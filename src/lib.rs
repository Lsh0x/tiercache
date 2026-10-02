//! A key-value cache over an ordered list of stores you bring.
//!
//! The crate defines one trait, [`CacheStore`], and one type, [`Cache`], that
//! runs a list of stores as levels, nearest first:
//!
//! - **get** asks each level in order and stops at the first hit; the levels
//!   before it are filled on the way back, so the next read is near.
//! - **put** writes every level.
//! - **delete** removes the key from every level.
//!
//! A level that fails is never fatal to a read: it counts as a miss and the
//! next level is asked. Failures are not hidden either — [`Cache::on_error`]
//! receives each one with the index of its level. A failed delete is returned,
//! since a key left behind in one level would be served again.
//!
//! Backends are yours: implement [`CacheStore`] for Redis, a database, a file,
//! anything. The only store shipped is [`Lru`], an in-process LRU (feature
//! `memory`, on by default, std only). Keys are strings and values bytes; how
//! values are encoded is the caller's business.
//!
//! ```
//! # #[cfg(feature = "memory")] {
//! use tiercache::{Cache, Lru};
//! # let rt = |f| tiercache::block_on_ready(f);
//! let cache = Cache::new(vec![Box::new(Lru::new(1024))]);
//! rt(async {
//!     cache.put("vi/niết bàn", b"[...]").await;
//!     assert_eq!(cache.get("vi/niết bàn").await.as_deref(), Some(&b"[...]"[..]));
//! });
//! # }
//! ```
//!
//! The crate has no runtime and no dependency: the futures are boxed std
//! futures, run by whatever executor the caller uses.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

#[cfg(feature = "memory")]
mod lru;
#[cfg(feature = "memory")]
pub use lru::Lru;

/// Any error a store returns.
pub type BoxError = Box<dyn Error + Send + Sync>;

/// The future every store method returns: boxed, so stores of different
/// types can sit in one list.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One cache level. Implement it for any backend.
///
/// Methods return boxed futures so a `Vec<Box<dyn CacheStore>>` can mix
/// backends; an implementation is usually `Box::pin(async move { … })`.
pub trait CacheStore: Send + Sync {
    /// The value under `key`, or `None` when the store does not hold it.
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, BoxError>>;

    /// Stores `value` under `key`, replacing any previous value.
    fn put<'a>(&'a self, key: &'a str, value: &'a [u8]) -> BoxFuture<'a, Result<(), BoxError>>;

    /// Removes `key`. Removing a key that is not there is not an error.
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), BoxError>>;
}

/// What a level failed at, for [`Cache::on_error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Reading a key.
    Get,
    /// Writing a key, including filling a nearer level after a hit.
    Put,
    /// Removing a key.
    Delete,
}

/// The levels whose delete failed: the key may still be served from them.
#[derive(Debug)]
pub struct DeleteError {
    /// `(level index, error)` for each failing level, nearest first.
    pub failures: Vec<(usize, BoxError)>,
}

impl fmt::Display for DeleteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "delete failed on {} level(s):", self.failures.len())?;
        for (level, e) in &self.failures {
            write!(f, " [{level}] {e};")?;
        }
        Ok(())
    }
}

impl Error for DeleteError {}

type ErrorHook = Box<dyn Fn(usize, Op, &BoxError) + Send + Sync>;

/// An ordered list of [`CacheStore`]s, nearest first. See the crate docs.
///
/// A `Cache` is itself a `CacheStore`, so a list can hold another list.
pub struct Cache {
    levels: Vec<Box<dyn CacheStore>>,
    on_error: Option<ErrorHook>,
}

impl fmt::Debug for Cache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cache")
            .field("levels", &self.levels.len())
            .finish_non_exhaustive()
    }
}

impl Cache {
    /// A cache over `levels`, nearest (fastest) first. An empty list is a
    /// cache that never hits.
    #[must_use]
    pub fn new(levels: Vec<Box<dyn CacheStore>>) -> Self {
        Self {
            levels,
            on_error: None,
        }
    }

    /// Calls `hook(level, op, error)` for every failure of a level, including
    /// the ones a read absorbs as a miss. Typically a log line.
    #[must_use]
    pub fn on_error(mut self, hook: impl Fn(usize, Op, &BoxError) + Send + Sync + 'static) -> Self {
        self.on_error = Some(Box::new(hook));
        self
    }

    /// How many levels the cache runs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.levels.len()
    }

    /// Whether the cache has no level at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    fn report(&self, level: usize, op: Op, e: &BoxError) {
        if let Some(hook) = &self.on_error {
            hook(level, op, e);
        }
    }

    /// The value under `key` from the nearest level that holds it, after
    /// copying it into every nearer level. A failing level is a miss.
    pub async fn get(&self, key: &str) -> Option<Vec<u8>> {
        for (i, level) in self.levels.iter().enumerate() {
            match level.get(key).await {
                Ok(Some(value)) => {
                    for (j, nearer) in self.levels[..i].iter().enumerate() {
                        if let Err(e) = nearer.put(key, &value).await {
                            self.report(j, Op::Put, &e);
                        }
                    }
                    return Some(value);
                }
                Ok(None) => {}
                Err(e) => self.report(i, Op::Get, &e),
            }
        }
        None
    }

    /// Writes `value` under `key` in every level. A failing level is
    /// reported and skipped: the others still hold the value.
    pub async fn put(&self, key: &str, value: &[u8]) {
        for (i, level) in self.levels.iter().enumerate() {
            if let Err(e) = level.put(key, value).await {
                self.report(i, Op::Put, &e);
            }
        }
    }

    /// Removes `key` from every level, trying all of them even after a
    /// failure.
    ///
    /// # Errors
    /// The levels that failed, since they may still serve the key.
    pub async fn delete(&self, key: &str) -> Result<(), DeleteError> {
        let mut failures = Vec::new();
        for (i, level) in self.levels.iter().enumerate() {
            if let Err(e) = level.delete(key).await {
                self.report(i, Op::Delete, &e);
                failures.push((i, e));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(DeleteError { failures })
        }
    }

    /// The cached value under `key`, or the one `compute` returns, which is
    /// then written to every level. A cache that fails entirely just means
    /// `compute` runs.
    ///
    /// # Errors
    /// Whatever `compute` returns; nothing is cached then.
    pub async fn get_or_compute<F, Fut, E>(&self, key: &str, compute: F) -> Result<Vec<u8>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Vec<u8>, E>>,
    {
        if let Some(value) = self.get(key).await {
            return Ok(value);
        }
        let value = compute().await?;
        self.put(key, &value).await;
        Ok(value)
    }
}

impl CacheStore for Cache {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, BoxError>> {
        Box::pin(async move { Ok(Cache::get(self, key).await) })
    }

    fn put<'a>(&'a self, key: &'a str, value: &'a [u8]) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(async move {
            Cache::put(self, key, value).await;
            Ok(())
        })
    }

    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(async move {
            Cache::delete(self, key)
                .await
                .map_err(|e| Box::new(e) as BoxError)
        })
    }
}

/// Runs a future that completes without waiting, such as one over [`Lru`]
/// alone. For doc examples and tests; real backends need a real executor.
///
/// # Panics
/// If the future is not ready on its first poll.
#[doc(hidden)]
pub fn block_on_ready<F: Future>(fut: F) -> F::Output {
    let mut fut = std::pin::pin!(fut);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    match fut.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(v) => v,
        std::task::Poll::Pending => panic!("block_on_ready: the future was not ready"),
    }
}
