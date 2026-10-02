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
        assert!(lru.is_empty());

        let none = Lru::new(0);
        run(none.put("a", b"1")).unwrap();
        assert_eq!(run(none.get("a")).unwrap(), None);
    }
}
