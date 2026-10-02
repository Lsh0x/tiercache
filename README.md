# tiercache

A key-value cache over an ordered list of stores you bring: an in-process LRU,
Redis, a database, anything that implements one small trait.

- **get** asks each level in order and stops at the first hit; the nearer
  levels are filled on the way back.
- **put** writes every level.
- **delete** removes the key from every level.

A failing level never fails a read: it counts as a miss, the next level is
asked, and the failure goes to your `on_error` hook. A failed delete is
returned, because a key left in one level would be served again.

No runtime, no dependency. The only store shipped is `Lru` (feature `memory`,
on by default, std only).

```rust
use tiercache::{Cache, Lru};

let cache = Cache::new(vec![
    Box::new(Lru::new(1024)), // in-process
    Box::new(my_redis),       // your CacheStore
    Box::new(my_database),    // your CacheStore
])
.on_error(|level, op, e| eprintln!("cache level {level} {op:?}: {e}"));

let hits = cache
    .get_or_compute("vi/hybrid/k10/niết bàn", || async { search().await })
    .await?;
```

## Bring a backend

```rust
use tiercache::{BoxError, BoxFuture, CacheStore};

struct MyStore { /* client */ }

impl CacheStore for MyStore {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, BoxError>> {
        Box::pin(async move { /* … */ Ok(None) })
    }
    fn put<'a>(&'a self, key: &'a str, value: &'a [u8]) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(async move { /* … */ Ok(()) })
    }
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(async move { /* … */ Ok(()) })
    }
}
```

Futures are boxed so stores of different types fit in one list. Keys are
strings and values bytes; encoding is yours. There is no TTL in the trait:
expiry belongs to the backend (Redis eviction, a database TTL policy), and
keys that embed a version of what produced them never go stale.

A `Cache` is itself a `CacheStore`, so a list can contain another list.

## License

MIT or Apache-2.0, at your option.
