# Contributing to tiercache

## Scope

`tiercache` is the level logic of a key-value cache: one trait, one list of
stores, one in-process store. Backends (Redis, databases, files, HTTP) belong
to the applications that use them, not here. TTLs, serialization and
statistics are out of scope too.

## Before opening a pull request

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test && cargo test --no-default-features && cargo test --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

CI also runs an MSRV check (Rust 1.85), `cargo-deny`, and coverage.

## What a change is expected to carry

- **Tests**, including the failure paths: a level that errors on get, put or
  delete. `tests/levels.rs` has a recording, failable store for that.
- **Docs**: `#![warn(missing_docs)]` is enforced.
- **No dependency** without a strong case argued in an issue first.
