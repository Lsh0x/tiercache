# Changelog

All notable changes to this project will be documented in this file.
Maintained by [release-plz](https://release-plz.dev/) from
[conventional commits](https://www.conventionalcommits.org/).

## [0.1.0] - Unreleased

### Added

- `CacheStore`: get, put and delete on one cache level, with boxed futures so
  backends of different types share one list.
- `Cache`: an ordered list of stores. Reads fall through and fill the nearer
  levels; writes and deletes reach every level; a failing level is a miss
  reported to `on_error`; a failed delete is returned. A `Cache` is itself a
  `CacheStore`.
- `Cache::get_or_compute`: the cached value, or the computed one written to
  every level.
- `Lru`: a std-only in-process store (feature `memory`, on by default).
