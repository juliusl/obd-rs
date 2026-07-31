//! overlaybd device lifecycle: create, launch over TCMU, mount, tear down,
//! commit.
//!
//! The whole lifecycle is driven through configfs exactly as documented in
//! containerd/overlaybd's "Standalone Usage" section: a `target_core_user`
//! backstore whose `dev_config` points at an overlaybd JSON config, plus a
//! `tcm_loop` nexus that turns it into a real `/dev/sdX`.
//!
//! # Sandboxing
//!
//! Filesystem access to a mounted device is handed out as a [`cap_std::fs::Dir`],
//! so a caller given that handle cannot walk out of the mount - `..` and
//! absolute paths are refused. This covers the **data plane**: the contents of
//! the mount, which is where job input and output live.
//!
//! It deliberately does *not* cover the **control plane**. configfs writes,
//! `mount(2)` and the `overlaybd-*` binaries are ambient, root-only, whole-host
//! operations; cap-std cannot confine them and this crate does not pretend
//! otherwise.
//!
//! # Ordering
//!
//! The lifecycle is a typestate ([`Device`] -> [`Live`] -> [`Mounted`]) so the
//! rules that matter are enforced by the compiler rather than by convention:
//!
//! ```no_run
//! use obd::{Device, DeviceConfig, Lower, Mode, tools};
//!
//! # fn main() -> obd::Result<()> {
//! tools::create_sparse_layer("/var/lib/x/u.data".as_ref(), "/var/lib/x/u.index".as_ref(), 64)?;
//! let config = DeviceConfig::new("/var/lib/x/result-a")
//!     .lower(Lower::file(tools::DEFAULT_BASELAYER))
//!     .upper("/var/lib/x/u.data", "/var/lib/x/u.index");
//!
//! let device = Device::from_config("poc_a", &config, "/var/lib/x/device-a.json", "0021")?;
//! let mounted = device.up()?.mount("/mnt/obd-a", Mode::Rw)?;
//!
//! // Sandboxed: confined to the mount.
//! let out = mounted.create_subdir("job-out")?;
//! out.write("result.json", b"{}")?;
//! drop(out);
//!
//! // `unmount` consumes `mounted`, so it will not compile while a borrowed
//! // `dir()` is still alive - which is exactly what would make umount fail.
//! mounted.unmount()?.down()?;
//!
//! tools::commit_layer(
//!     "/var/lib/x/u.data".as_ref(),
//!     "/var/lib/x/u.index".as_ref(),
//!     "/var/lib/x/job.commit".as_ref(),
//!     "job 1",
//! )?;
//! # Ok(())
//! # }
//! ```
//!
//! # Platform
//!
//! configfs, TCMU and `mount(2)` are Linux-only. The types compile everywhere
//! so the crate can be developed and unit-tested on other platforms, but the
//! device operations return [`Error::UnsupportedPlatform`] off Linux.
//!
//! # Tracing
//!
//! This crate emits [`tracing`] events and installs no subscriber, so it costs
//! nothing until a binary opts in. Severities are used deliberately:
//!
//! | Level | What it is for |
//! | --- | --- |
//! | `error` | Non-recoverable failures, especially syscall edges whose remedy is outside this process |
//! | `warn` | Defensive code that actually fired: an EAGAIN retry, a busy unmount, teardown from `Drop` |
//! | `info` | The audit trail. Every layer written, config written, device launched, mounted, unmounted or removed |
//! | `debug` | Flow and state transitions, enough to reason about a run after the fact |
//! | `trace` | Timing and counts from the polling loops, concentrated in [`configfs`] |
//!
//! The `info` level alone is a complete record of what this crate did to the
//! host, which is what makes it useful to keep on in production. To debug a
//! device that will not attach, `obd=debug` shows the lifecycle; to investigate
//! slow attaches or the `tcm_loop` recycling race, `obd::configfs=trace` adds
//! per-poll timings without the rest.

// The repo convention: every public API carries a doc-header and every public
// field is documented. This lint keeps that honest.
#![warn(missing_docs)]

pub mod cleanup;
pub mod config;
pub mod configfs;
pub mod device;
pub mod error;
pub mod tools;

pub use cleanup::{Swept, cleanup_all, force_cleanup, install_signal_handler};
pub use config::{DeviceConfig, Lower};
pub use device::{Device, Live, Mode, Mounted, PersistedDevice, teardown_named};
pub use error::{Error, Result};

/// Re-exported so callers can accept the sandboxed handle without depending on
/// cap-std directly.
pub use cap_std::fs::Dir;

use std::path::PathBuf;

/// One preflight finding.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Check {
    /// What was checked, e.g. `module tcm_loop`, phrased so it reads the same
    /// whether it passed or failed.
    pub name: String,
    /// Whether the requirement is satisfied.
    pub ok: bool,
    /// The remedy, present only when the check failed. Written as a command
    /// the operator can run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Check {
    fn pass(name: impl Into<String>) -> Self {
        Check {
            name: name.into(),
            ok: true,
            hint: None,
        }
    }
    fn fail(name: impl Into<String>, hint: impl Into<String>) -> Self {
        Check {
            name: name.into(),
            ok: false,
            hint: Some(hint.into()),
        }
    }
}

/// Check that this host can actually drive overlaybd devices.
///
/// This is the authoritative check; `build.rs` only warns, because the build
/// host and the run host need not be the same machine.
pub fn preflight() -> Vec<Check> {
    let mut checks = Vec::new();

    if cfg!(target_os = "linux") {
        checks.push(Check::pass("linux host"));
    } else {
        checks.push(Check::fail(
            "linux host",
            format!(
                "configfs and TCMU are Linux-only; this binary is built for {}",
                std::env::consts::OS
            ),
        ));
    }

    for binary in ["overlaybd-create", "overlaybd-commit"] {
        match tools::find(binary) {
            Ok(path) => checks.push(Check::pass(format!("{binary} at {}", path.display()))),
            Err(_) => checks.push(Check::fail(
                binary,
                "sudo ./scripts/install-overlaybd.sh (installs containerd-overlaybd from PMC \
                 and wires /opt/overlaybd)",
            )),
        }
    }

    let baselayer = std::env::var("OVERLAYBD_BASELAYER")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(tools::DEFAULT_BASELAYER));
    if baselayer.is_file() {
        checks.push(Check::pass(format!("baselayer {}", baselayer.display())));
    } else {
        checks.push(Check::fail(
            format!("baselayer {}", baselayer.display()),
            "sudo ./scripts/install-overlaybd.sh, or set OVERLAYBD_BASELAYER to an ext4 layer",
        ));
    }

    let target = std::path::Path::new(configfs::CONFIGFS);
    if target.is_dir() {
        checks.push(Check::pass(format!("configfs at {}", target.display())));
    } else {
        checks.push(Check::fail(
            format!("configfs at {}", target.display()),
            "sudo modprobe target_core_user tcm_loop && \
             sudo mount -t configfs none /sys/kernel/config",
        ));
    }

    for module in ["target_core_user", "tcm_loop"] {
        let loaded = std::path::Path::new("/sys/module").join(module).is_dir();
        if loaded {
            checks.push(Check::pass(format!("module {module}")));
        } else {
            checks.push(Check::fail(
                format!("module {module}"),
                format!("sudo modprobe {module}"),
            ));
        }
    }

    #[cfg(target_os = "linux")]
    {
        let root = rustix::process::geteuid().is_root();
        if root {
            checks.push(Check::pass("running as root"));
        } else {
            checks.push(Check::fail(
                "running as root",
                "configfs and mount(2) need root; re-run with sudo",
            ));
        }
    }

    checks
}
