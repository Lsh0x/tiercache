//! [`Lru`]: an in-process least-recently-used store, std only.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use crate::{BoxError, BoxFuture, CacheStore};

/// An in-process store holding at most `capacity` entries; the least
/// recently read or written one goes first. Every operation is `O(log n)`.
///
/// Capacity counts entries, not bytes. A capacity of 0 holds nothing.
#[derive(Debug)]
pub struct Lru {
    capacity: usize,
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// key → (value, last use)
    entries: HashMap<String, (Vec<u8>, u64)>,
    /// last use → key: the oldest is first.
    order: BTreeMap<u64, String>,
    clock: u64,
}

impl Inner {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn get(&mut self, key: &str) -> Option<Vec<u8>> {
        let now = self.tick();
        let (value, used) = self.entries.get_mut(key)?;
        let old = std::mem::replace(used, now);
        let value = value.clone();
        if let Some(k) = self.order.remove(&old) {
            self.order.insert(now, k);
        }
        Some(value)
    }

    fn put(&mut self, key: &str, value: &[u8], capacity: usize) {
        if capacity == 0 {
            return;
        }
        let now = self.tick();
        if let Some((_, old)) = self.entries.insert(key.to_owned(), (value.to_vec(), now)) {
            self.order.remove(&old);
        }
        self.order.insert(now, key.to_owned());
        while self.entries.len() > capacity {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }

    fn delete(&mut self, key: &str) {
        if let Some((_, used)) = self.entries.remove(key) {
            self.order.remove(&used);
        }
    }
}

impl Lru {
    /// An empty store for at most `capacity` entries.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Entries held now.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    /// Whether the store holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves a consistent map (every
        // mutation above completes or does nothing), so poisoning is ignored.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl CacheStore for Lru {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, BoxError>> {
        let value = self.lock().get(key);
        Box::pin(async move { Ok(value) })
    }

    fn put<'a>(&'a self, key: &'a str, value: &'a [u8]) -> BoxFuture<'a, Result<(), BoxError>> {
        self.lock().put(key, value, self.capacity);
        Box::pin(async { Ok(()) })
    }

    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), BoxError>> {
        self.lock().delete(key);
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_on_ready as run;

    #[test]
    fn evicts_the_least_recently_used() {
        let lru = Lru::new(2);
        run(lru.put("a", b"1")).unwrap();
        run(lru.put("b", b"2")).unwrap();
        // Reading `a` makes `b` the oldest.
        assert_eq!(run(lru.get("a")).unwrap().as_deref(), Some(&b"1"[..]));
        run(lru.put("c", b"3")).unwrap();
        assert_eq!(run(lru.get("b")).unwrap(), None);
        assert!(run(lru.get("a")).unwrap().is_some());
        assert!(run(lru.get("c")).unwrap().is_some());
        assert_eq!(lru.len(), 2);
    }

    #[test]
    fn overwrite_refreshes_and_keeps_one_entry() {
        let lru = Lru::new(2);
        run(lru.put("a", b"1")).unwrap();
        run(lru.put("b", b"2")).unwrap();
        run(lru.put("a", b"9")).unwrap();
        run(lru.put("c", b"3")).unwrap();
        assert_eq!(run(lru.get("a")).unwrap().as_deref(), Some(&b"9"[..]));
        assert_eq!(run(lru.get("b")).unwrap(), None);
        assert_eq!(lru.len(), 2);
    }

    #[test]
    fn delete_and_zero_capacity() {
        let lru = Lru::new(4);
        run(lru.put("a", b"1")).unwrap();
        run(lru.delete("a")).unwrap();
        run(lru.delete("never")).unwrap();
        assert_eq!(lru.len(), 0);

        let none = Lru::new(0);
        run(none.put("a", b"1")).unwrap();
        assert_eq!(run(none.get("a")).unwrap(), None);
    }
}

#[cfg(test)]
mod model {
    //! `Lru` against the obvious model — a Vec ordered by last use — over
    //! thousands of random operations, for several capacities.
    use super::*;
    use crate::block_on_ready as run;

    /// xorshift64*: deterministic, no dependency.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Most recently used last.
    #[derive(Default)]
    struct Model(Vec<(String, Vec<u8>)>);
    impl Model {
        fn get(&mut self, k: &str) -> Option<Vec<u8>> {
            let i = self.0.iter().position(|(key, _)| key == k)?;
            let e = self.0.remove(i);
            let v = e.1.clone();
            self.0.push(e);
            Some(v)
        }
        fn put(&mut self, k: &str, v: &[u8], cap: usize) {
            if cap == 0 {
                return;
            }
            self.0.retain(|(key, _)| key != k);
            self.0.push((k.to_owned(), v.to_vec()));
            while self.0.len() > cap {
                self.0.remove(0);
            }
        }
        fn delete(&mut self, k: &str) {
            self.0.retain(|(key, _)| key != k);
        }
    }

    #[test]
    fn behaves_like_the_model() {
        for (seed, cap) in [(1u64, 0usize), (2, 1), (3, 2), (4, 7), (5, 64)] {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let lru = Lru::new(cap);
            let mut model = Model::default();
            for step in 0..5_000u32 {
                let key = format!("k{}", rng.below(cap as u64 * 2 + 3));
                match rng.below(10) {
                    0..=4 => assert_eq!(
                        run(lru.get(&key)).unwrap(),
                        model.get(&key),
                        "cap {cap}, step {step}, get {key}"
                    ),
                    5..=8 => {
                        let v = step.to_le_bytes();
                        run(lru.put(&key, &v)).unwrap();
                        model.put(&key, &v, cap);
                    }
                    _ => {
                        run(lru.delete(&key)).unwrap();
                        model.delete(&key);
                    }
                }
                assert_eq!(lru.len(), model.0.len(), "cap {cap}, step {step}");
            }
        }
    }

    #[test]
    fn stays_bounded_and_consistent_under_threads() {
        let lru = std::sync::Arc::new(Lru::new(50));
        let threads: Vec<_> = (0..8u64)
            .map(|t| {
                let lru = lru.clone();
                std::thread::spawn(move || {
                    let mut rng = Rng(t + 1);
                    for i in 0..2_000u64 {
                        let key = format!("k{}", rng.below(200));
                        match rng.below(3) {
                            0 => {
                                if let Some(v) = run(lru.get(&key)).unwrap() {
                                    assert_eq!(v.len(), 8, "a value is never torn");
                                }
                            }
                            1 => run(lru.put(&key, &i.to_le_bytes())).unwrap(),
                            _ => run(lru.delete(&key)).unwrap(),
                        }
                        assert!(lru.len() <= 50);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let inner = lru.lock();
        assert_eq!(
            inner.entries.len(),
            inner.order.len(),
            "map and order agree"
        );
        for (key, (_, used)) in &inner.entries {
            assert_eq!(inner.order.get(used), Some(key));
        }
    }
}
