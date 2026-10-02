# Security Policy

## Supported versions

Only the latest published `0.1.x` release receives fixes. `tiercache` is
pre-1.0: there are no long-term support branches yet.

## Reporting a vulnerability

Report privately through GitHub's
[security advisories](https://github.com/Lsh0x/tiercache/security/advisories/new)
form. Please do **not** open a public issue for an unfixed vulnerability.

Expect an acknowledgement within 7 days and an assessment within 30.

## Threat model

`tiercache` does no I/O of its own: it orders calls to stores the
application provides, and ships one in-process store (`Lru`). It has no
dependency and is `#![forbid(unsafe_code)]`.

In scope:

- Panics, deadlocks, or unbounded memory growth reachable through `Cache` or
  `Lru` (an `Lru` holding more entries than its capacity is a bug).
- A key served after `Cache::delete` returned `Ok`.

Out of scope:

- The behaviour of stores you implement, including what they do with keys
  and values.
- `Lru` capacity counting entries, not bytes: bound value sizes yourself.
