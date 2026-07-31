# obd-rs

The overlaybd device lifecycle in Rust: create layers, launch a device over
TCMU/configfs, mount it, commit the result, and tear it all down again.

This is a port of `overlaybd_device.py` from the hyperlight-overlaybd PoC, with
two things the Python could not do:

- **Sandboxed filesystem access.** A mounted device hands back a
  [`cap_std::fs::Dir`](https://docs.rs/cap-std), so code given that handle
  cannot walk out of the mount.
- **Ordering rules enforced by the compiler.** The lifecycle is a typestate, so
  "drop the directory handle before unmounting" is a compile error rather than a
  comment and a retry loop.

```
  Device  --up()-->  Live  --mount()-->  Mounted
                      |                    |
                      |                 dir() -> &Dir   (confined to the mount)
                      |                    |
                      +<--- unmount() -----+
                      |
                    down()
```

## Quick start

```bash
sudo ./scripts/install-overlaybd.sh   # PMC install + /opt/overlaybd wiring + baselayer
cargo build
sudo ./target/debug/obdctl preflight
```

Library use:

```rust
use obd::{Device, DeviceConfig, Lower, Mode, tools};

tools::create_sparse_layer("/var/lib/x/u.data".as_ref(), "/var/lib/x/u.index".as_ref(), 64)?;

let config = DeviceConfig::new("/var/lib/x/result-a")
    .lower(Lower::file(tools::DEFAULT_BASELAYER))
    .upper("/var/lib/x/u.data", "/var/lib/x/u.index");

let device  = Device::from_config("poc_a", &config, "/var/lib/x/device-a.json", "0021")?;
let mounted = device.up()?.mount("/mnt/obd-a", Mode::Rw)?;

let out = mounted.create_subdir("job-out")?;   // sandboxed handle
out.write("result.json", b"{}")?;
drop(out);                                      // must end before unmount

mounted.unmount()?.down()?;
tools::commit_layer(
    "/var/lib/x/u.data".as_ref(),
    "/var/lib/x/u.index".as_ref(),
    "/var/lib/x/job.commit".as_ref(),
    "job 1",
)?;
```

## What cap-std does and does not cover

`Mounted::dir()` and `Mounted::create_subdir()` return handles confined to the
mount: `..` and absolute paths are refused, so a component handed one cannot
reach the rest of the host. That covers the **data plane** - job input and
output, which is the part that handles untrusted content.

It deliberately does **not** cover the **control plane**. configfs writes,
`mount(2)` and the `overlaybd-*` binaries are ambient, root-only, whole-host
operations that cap-std cannot confine. This crate does not pretend otherwise.

## `obdctl`

The CLI exists so shell and Python callers can drop `overlaybd_device.py`.

| Was (`overlaybd_device.py`) | Now (`obdctl`) |
| --- | --- |
| `create_sparse_layer(data, index, gb)` | `obdctl layer create --data D --index I --size-gb 64` |
| `commit_layer(data, index, out, msg)` | `obdctl layer commit --data D --index I --out O --message M` |
| `write_device_config(...)` | `obdctl config --out C --result-file R --lower L [--upper-data D --upper-index I]` |
| `RemoteLayer(digest, size)` | `obdctl config ... --remote-lower sha256:abc=167936 --repo-blob-url URL` |
| `OverlaybdDevice(...).up()` + `.mount(p)` | `obdctl device up --name poc_a --config C --result-file R --naa-suffix 0021 --mount P [--read-only] [--subdir job-out]` |
| `.down()` | `obdctl device down --name poc_a --naa-suffix 0021 --mount P` |
| `force_cleanup([a, b])` | `obdctl cleanup --mount A --mount B` |
| `preflight_paths()` | `obdctl preflight` |

Add `--json` to any command for machine-readable output:

```console
$ obdctl --json device up --name poc_a --config c.json --result-file r \
    --naa-suffix 0021 --mount /mnt/obd-a --subdir job-out
{
  "name": "poc_a",
  "naa_suffix": "0091",
  "block_device": "/dev/sda",
  "mountpoint": "/mnt/obd-a"
}
```

### `obdctl` is stateless, and `device up` leaves the device running

The Python object lived across mount → run job → unmount inside one process. A
CLI exits in between, so:

- **Teardown needs no state file.** Everything is derivable from the name and
  nexus suffix the caller already chose, which is what `device down` takes.
- **`device up` deliberately does not tear down on exit.** The library uses RAII
  (`Drop` unmounts and removes the configfs entries), but the CLI calls
  `persist()` to defuse it - otherwise `obdctl device up` would destroy the
  device as it exited. If you write library code that must outlive its handle,
  use `persist()` too.

## Diagnostics

The library emits [`tracing`](https://docs.rs/tracing) events and installs no
subscriber, so it costs nothing until a binary opts in. `obdctl` installs one,
plus [`color-eyre`](https://docs.rs/color-eyre) as the error report handler.

Severities are used deliberately rather than by feel:

| Level | What it is for | Examples here |
| --- | --- | --- |
| `error` | Non-recoverable failures, especially syscall edges whose remedy is outside this process | `mount(2)` failed, configfs rejected a write, no block device appeared |
| `warn` | Defensive code that actually fired | EAGAIN retry covered for a daemon still attaching, unmount succeeded only after retries, a recycled device node was skipped, teardown ran from `Drop` |
| `info` | The audit trail | Every layer created or committed, config written, device launched, mounted, unmounted or removed |
| `debug` | Flow and state transitions | Which binary was found where, each configfs write, each teardown step |
| `trace` | Timing and counts from the polling loops | `configfs-write`, `result-file`, `node-resolved`, `unmount`, `device-up` |

`info` alone is a complete record of what this crate did to the host, which is
what makes it worth leaving on.

```console
$ obdctl device up --name poc_a --config d.json --result-file r --naa-suffix 0021 --mount /mnt/obd-a
 INFO obdctl::device: launched an overlaybd device device=poc_a block_device=/dev/sda naa=naa.5001405e0b0d0021 scsi_address=0:0:1
 INFO obdctl::device: mounted an overlaybd device mountpoint=/mnt/obd-a mode=rw
```

Verbosity is `-v` for debug and `-vv` for trace, or `RUST_LOG` for full control.
The traces are concentrated in `obd::configfs`, so the waits can be measured
without turning anything else on:

```console
$ RUST_LOG=obd::configfs=trace obdctl device up ...
TRACE configfs-write attempts=1 elapsed_us=1126
TRACE result-file polls=1 elapsed_ms=0
TRACE resolve_block_device{device=poc_a naa=naa.5001405e0b0d0021}: node-resolved polls=2 recycled=0 not_ready=0 elapsed_ms=206
```

Failures come back as a `color-eyre` report: the context chain, where it
happened, and the span trace showing which operation was running.

```console
$ obdctl layer commit --data missing.data --index missing.index --out out.commit
Error:
   0: committing missing.data into the read-only layer out.commit
   1: overlaybd binary `overlaybd-commit` not found in [...]; run scripts/install-overlaybd.sh, or set OVERLAYBD_BIN_DIR

Location:
   src/bin/obdctl.rs:311

  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ SPANTRACE ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   0: obdctl::obdctl::layer
      at src/bin/obdctl.rs:276
```

`RUST_BACKTRACE=1` adds the backtrace, `RUST_BACKTRACE=full` adds source
snippets. Logs go to stderr, so `--json` output on stdout stays parseable.

The library keeps typed [`thiserror`](https://docs.rs/thiserror) errors and does
**not** pull in eyre: picking a report handler is the binary's call, not a
library's.

## Layout

| Path | Purpose |
| --- | --- |
| `src/lib.rs` | Public API, `preflight()` |
| `src/device.rs` | The `Device` → `Live` → `Mounted` typestate |
| `src/configfs.rs` | Backstore/nexus/LUN choreography, block-device resolution |
| `src/config.rs` | Device config JSON, local and streamed lowers |
| `src/tools.rs` | `overlaybd-create` / `overlaybd-commit`, binary discovery |
| `src/cleanup.rs` | Convention sweep, signal handling |
| `src/bin/obdctl.rs` | The CLI, plus the subscriber and color-eyre setup |
| `build.rs` | Build-time probe (warns only) |
| `scripts/install-overlaybd.sh` | PMC install and layout wiring |
| `tests/api.rs` | Tests that run anywhere, including macOS |
| `tests/linux_device.rs` | Library device lifecycle; needs Linux and root |
| `tests/lima-e2e.sh` | `obdctl` device lifecycle; needs Linux and root |
| `lima-dev.yaml` | Lima VM for developing from macOS |

## Why the install script does more than `apt install`

The PMC `containerd-overlaybd` package installs binaries under
`/usr/bin/overlaybd` and ships nothing else: no `/etc/overlaybd/overlaybd.json`,
no `cred.json`, no `ext4_64` baselayer. Its own systemd unit still hardcodes
`ExecStart=/opt/overlaybd/bin/overlaybd-tcmu`, so a stock install fails to start
with `status=203/EXEC`. `scripts/install-overlaybd.sh` symlinks the binaries into
`/opt/overlaybd/bin`, seeds the two config files, and fetches the baselayer from
the overlaybd source tree, where it is a checked-in artifact rather than a build
output.

`build.rs` only **warns** when it cannot find those binaries. The build host and
the run host need not be the same machine - this crate is developed on macOS and
run on Linux - so a hard failure there would break `cargo check`, `clippy` and
rust-analyzer for no good reason. `obdctl preflight` is the authoritative check.

## Platform

configfs, TCMU and `mount(2)` are Linux-only. The types compile everywhere so
the crate can be developed and unit-tested on macOS, but device operations
return `Error::UnsupportedPlatform` off Linux.

## Testing

```bash
cargo test                                  # anywhere, including macOS

limactl start --name=obd-dev --mount "$PWD" ./lima-dev.yaml
limactl shell obd-dev
sudo ./scripts/install-overlaybd.sh

# Real devices, two ways:
sudo -E OBD_DEVICE_TESTS=1 cargo test --test linux_device -- --test-threads=1
cargo build && sudo ./tests/lima-e2e.sh
```

`tests/linux_device.rs` drives the **library** API, and `tests/lima-e2e.sh`
drives `obdctl`. Both are needed: the CLI always hands devices off with
`persist()`, so the RAII paths - an explicit `down()`, and teardown from `Drop` -
are only covered by the library tests. They mutate global host state, hence
`--test-threads=1`, and skip unless `OBD_DEVICE_TESTS` is set so a plain
`cargo test` stays green anywhere.

overlaybd is architecture independent, so the Lima VM works on Apple Silicon;
nothing here needs x86_64.

`tests/lima-e2e.sh` covers what `cargo test` cannot: launching a device,
mounting it, writing, committing, restacking the committed layer read-only,
asserting the read-only mount refuses writes, three back-to-back reruns (which
is what exercises the `tcm_loop` SCSI-triple recycling waits), and an idempotent
cleanup that leaves no configfs entries behind.

## Ordering rules and pitfalls

Carried over from the Python, because getting any of them wrong produces
confusing failures. The first is now enforced by the compiler; the rest are
enforced at runtime.

1. **Drop every handle on the mount before unmounting.** An open descriptor is
   what makes `umount` return `EBUSY`. `Mounted::dir()` borrows, and
   `Mounted::unmount()` consumes `self`, so this cannot compile wrong.
2. **Never point a job's output at the mount root.** Consumers wipe their output
   directory on every run, so the root would lose `lost+found`. Use
   `create_subdir`.
3. **sync, unmount, tear the device down, *then* commit.** `overlaybd-commit`
   opens the data file `O_RDWR` and will happily capture a torn filesystem from
   a device the daemon still has open.
4. **Mount a lower-only device `ro,noload`.** All its layers are read-only, so
   ext4 cannot replay a journal. `Mode::Ro` does this.
5. **configfs teardown is order sensitive**: LUN symlink → `lun_0` → `tpgt_1` →
   `naa.*` → backstore → HBA.
6. **Check `resultFile` after enabling a device.** overlaybd reports launch
   failures there, not through the configfs write.
7. **Never hardcode `/dev/sdX`.** It is resolved from the loopback nexus, and
   `tcm_loop` *recycles* SCSI triples - hence the waits for the node's `dev_t` to
   match sysfs and for the node to be readable, plus the wait for the old device
   to disappear on teardown.
8. **`overlaybd-create` refuses to overwrite.** Its outputs are opened
   `O_EXCL|O_CREAT`, so each run needs a fresh directory.

## Not included

Registry operations - push, pull, login - are deliberately out of scope for now.
`Lower::remote` and `repo_blob_url` are present so a config *can* describe a
streamed layer, but getting the blob into a registry and authenticating to it
lives elsewhere.
