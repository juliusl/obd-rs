# Changelog

Notable changes to the `obd-rs` crate, the `obdctl` binary and the packages
built from them. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each released version is a `vX.Y.Z` tag. That one tag builds the deb and the
rpm, publishes the GitHub release, and publishes the crate to crates.io.

## [Unreleased]

## [0.1.1] - 2026-08-04

### Added

- `obdctl config --remote-lower-dir PATH`, so a streamed lower can name the
  directory overlaybd persists its fetched blocks into. The library could
  already express this through `Lower::with_cache_dir`; nothing reached it from
  the command line, so a config written by `obdctl` could never carry a `dir`.
  The flag pairs positionally with `--remote-lower`, all or nothing, because a
  partial list would attach a cache directory to the wrong layer.

### Changed

- The crate is published by `.github/workflows/publish.yml` rather than by
  hand. It authenticates to crates.io over OIDC and holds no token: see
  [docs/internal/publish.md](docs/internal/publish.md).

## [0.1.0] - 2026-08-04

First release. Published to crates.io by hand, which is what crates.io requires
before a trusted publisher can be configured.

### Added

- The device lifecycle as a typestate — `Device` → `Live` → `Mounted` — so the
  ordering rule that matters is a compile error rather than an `EBUSY`:
  `Mounted::dir` borrows and `Mounted::unmount` consumes `self`, so a device
  cannot be unmounted while a handle on the mount is alive.
- Filesystem access to a mounted device handed out as a
  [`cap_std::fs::Dir`](https://docs.rs/cap-std) confined to the mount, so a
  caller given that handle cannot walk out of it.
- `obdctl`: `preflight`, `layer create`, `layer commit`, `config`, `device up`,
  `device down` and `cleanup`, with `--json` on stdout and diagnostics on
  stderr.
- Streamed lowers: `Lower::remote` plus a top-level `repoBlobUrl`, so a device
  can consume a layer straight out of a registry.
- Deb and rpm packages that complete the `containerd-overlaybd` install they
  depend on: the daemon config it omits, a locally generated `ext4_64`
  baselayer, both kernel modules, and a drop-in correcting the `ExecStart` of
  the unit it ships — a stock install otherwise fails with `status=203/EXEC`.
- `tracing` instrumentation with no subscriber installed, so it costs nothing
  until a binary opts in.

[Unreleased]: https://github.com/juliusl/obd-rs/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/juliusl/obd-rs/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/juliusl/obd-rs/releases/tag/v0.1.0
