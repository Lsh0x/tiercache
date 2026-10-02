//! The level semantics of `Cache`: fall-through reads with fill-back, writes
//! and deletes on every level, failures absorbed on reads and reported.

use std::sync::{Arc, Mutex};

use tiercache::{BoxError, BoxFuture, Cache, CacheStore, Lru, Op, block_on_ready as run};

/// A store that records what it was asked, and can be told to fail.
struct Probe {
    inner: Lru,
    failing: bool,
    calls: Mutex<Vec<String>>,
}

impl Default for Probe {
    fn default() -> Self {
        Self {
            inner: Lru::new(16),
            failing: false,
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl Probe {
    fn failing() -> Self {
        Self {
            failing: true,
            ..Self::default()
        }
    }
    fn log(&self, what: String) {
        self.calls.lock().unwrap().push(what);
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

fn down() -> BoxError {
    "store is down".into()
}

impl CacheStore for Probe {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, BoxError>> {
        self.log(format!("get {key}"));
        if self.failing {
            return Box::pin(async { Err(down()) });
        }
        self.inner.get(key)
    }
    fn put<'a>(&'a self, key: &'a str, value: &'a [u8]) -> BoxFuture<'a, Result<(), BoxError>> {
        self.log(format!("put {key}"));
        if self.failing {
            return Box::pin(async { Err(down()) });
        }
        self.inner.put(key, value)
    }
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), BoxError>> {
        self.log(format!("delete {key}"));
        if self.failing {
            return Box::pin(async { Err(down()) });
        }
        self.inner.delete(key)
    }
}

/// `Arc<Probe>` as a level, so the test keeps a handle to inspect it.
struct Shared(Arc<Probe>);

impl CacheStore for Shared {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, BoxError>> {
        self.0.get(key)
    }
    fn put<'a>(&'a self, key: &'a str, value: &'a [u8]) -> BoxFuture<'a, Result<(), BoxError>> {
        self.0.put(key, value)
    }
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), BoxError>> {
        self.0.delete(key)
    }
}

fn levels(probes: &[Arc<Probe>]) -> Vec<Box<dyn CacheStore>> {
    probes
        .iter()
        .map(|p| Box::new(Shared(p.clone())) as Box<dyn CacheStore>)
        .collect()
}

#[test]
fn a_far_hit_fills_every_nearer_level_and_stops_there() {
    let (near, mid, far, beyond) = (
        Arc::new(Probe::default()),
        Arc::new(Probe::default()),
        Arc::new(Probe::default()),
        Arc::new(Probe::default()),
    );
    run(far.inner.put("k", b"v")).unwrap();
    let cache = Cache::new(levels(&[
        near.clone(),
        mid.clone(),
        far.clone(),
        beyond.clone(),
    ]));

    assert_eq!(run(cache.get("k")).as_deref(), Some(&b"v"[..]));
    assert_eq!(near.calls(), ["get k", "put k"]);
    assert_eq!(mid.calls(), ["get k", "put k"]);
    assert_eq!(far.calls(), ["get k"]);
    assert!(
        beyond.calls().is_empty(),
        "a level after the hit is never asked"
    );

    // The next read is served by the nearest level alone.
    assert_eq!(run(cache.get("k")).as_deref(), Some(&b"v"[..]));
    assert_eq!(mid.calls().len(), 2);
}

#[test]
fn put_and_delete_reach_every_level() {
    let (a, b) = (Arc::new(Probe::default()), Arc::new(Probe::default()));
    let cache = Cache::new(levels(&[a.clone(), b.clone()]));
    run(cache.put("k", b"v"));
    assert!(run(b.inner.get("k")).unwrap().is_some());
    run(cache.delete("k")).unwrap();
    assert_eq!(run(cache.get("k")), None);
    assert_eq!(a.calls(), ["put k", "delete k", "get k"]);
    assert_eq!(b.calls(), ["put k", "delete k", "get k"]);
}

#[test]
fn a_failing_level_is_a_reported_miss_on_read() {
    let (near, broken, far) = (
        Arc::new(Probe::default()),
        Arc::new(Probe::failing()),
        Arc::new(Probe::default()),
    );
    run(far.inner.put("k", b"v")).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let cache = Cache::new(levels(&[near.clone(), broken.clone(), far.clone()]))
        .on_error(move |level, op, e| log.lock().unwrap().push((level, op, e.to_string())));

    assert_eq!(run(cache.get("k")).as_deref(), Some(&b"v"[..]));
    assert!(
        run(near.inner.get("k")).unwrap().is_some(),
        "near level still filled"
    );
    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        [
            (1, Op::Get, "store is down".to_owned()),
            (1, Op::Put, "store is down".to_owned()),
        ]
    );
}

#[test]
fn a_failed_delete_is_returned_with_its_level() {
    let (ok, broken) = (Arc::new(Probe::default()), Arc::new(Probe::failing()));
    let cache = Cache::new(levels(&[ok.clone(), broken.clone()]));
    let err = run(cache.delete("k")).unwrap_err();
    assert_eq!(err.failures.len(), 1);
    assert_eq!(err.failures[0].0, 1);
    assert_eq!(
        ok.calls(),
        ["delete k"],
        "the healthy level is still cleared"
    );
}

#[test]
fn get_or_compute_computes_once() {
    let cache = Cache::new(vec![Box::new(Lru::new(8))]);
    let mut runs = 0;
    for _ in 0..3 {
        let v: Result<Vec<u8>, ()> = run(cache.get_or_compute("k", || {
            runs += 1;
            async { Ok(b"v".to_vec()) }
        }));
        assert_eq!(v.unwrap(), b"v");
    }
    assert_eq!(runs, 1);

    let err: Result<Vec<u8>, &str> = run(cache.get_or_compute("bad", || async { Err("nope") }));
    assert_eq!(err.unwrap_err(), "nope");
    assert_eq!(
        run(cache.get("bad")),
        None,
        "a failed computation is not cached"
    );
}

#[test]
fn caches_nest_and_an_empty_cache_never_hits() {
    let inner = Cache::new(vec![Box::new(Lru::new(8))]);
    let outer = Cache::new(vec![Box::new(Lru::new(8)), Box::new(inner)]);
    run(outer.put("k", b"v"));
    assert_eq!(run(outer.get("k")).as_deref(), Some(&b"v"[..]));

    let empty = Cache::new(Vec::new());
    assert!(empty.is_empty());
    run(empty.put("k", b"v"));
    assert_eq!(run(empty.get("k")), None);
}
