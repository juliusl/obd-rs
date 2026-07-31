//! Idempotent cleanup of leftover devices, plus signal handling.
//!
//! TCMU/configfs leftovers otherwise need manual surgery, so the sweep works by
//! naming convention rather than only from in-process state: a run killed with
//! `SIGKILL` can still be cleaned up afterwards.

use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::time::Duration;

use tracing::{debug, info, instrument, warn};

use crate::configfs::{self, DEV_PREFIX, NAA_PREFIX};
use crate::error::Result;

/// What a sweep removed, for reporting.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Swept {
    /// `tcm_loop` nexuses removed, by NAA name.
    pub nexuses: Vec<String>,
    /// `target_core_user` backstores removed, by device name.
    pub backstores: Vec<String>,
    /// Mountpoints that were mounted and are no longer.
    pub unmounted: Vec<PathBuf>,
}

impl Swept {
    /// True when the sweep found nothing to remove, which is the expected
    /// result on a host with no interrupted runs.
    pub fn is_empty(&self) -> bool {
        self.nexuses.is_empty() && self.backstores.is_empty() && self.unmounted.is_empty()
    }
}

/// True if `target` is a mount point, per `/proc/mounts`.
pub fn is_mounted(target: &Path) -> bool {
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
        return false;
    };
    let target = target.to_string_lossy();
    mounts.lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next();
        fields.next() == Some(target.as_ref())
    })
}

/// Unmount a path if it is mounted, retrying while it is busy.
#[cfg(target_os = "linux")]
pub fn unmount_path(target: &Path) -> bool {
    use rustix::mount::{UnmountFlags, unmount};

    if !is_mounted(target) {
        debug!("{} is not mounted; nothing to unmount", target.display());
        return false;
    }
    rustix::fs::sync();
    for attempt in 1..=20u32 {
        if unmount(target, UnmountFlags::empty()).is_ok() {
            // Auditable: a filesystem is no longer visible on this host.
            info!(mountpoint = %target.display(), attempts = attempt, "unmounted during cleanup");
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    // Cleanup is best effort by design, but leaving a mount behind blocks the
    // configfs teardown that follows, so it must not pass silently.
    warn!(
        mountpoint = %target.display(),
        "could not unmount during cleanup; the device behind it cannot be torn down"
    );
    false
}

/// Always false off Linux, where nothing can be mounted by this crate.
#[cfg(not(target_os = "linux"))]
pub fn unmount_path(_target: &Path) -> bool {
    false
}

/// Remove every device this crate could have created, including leftovers from
/// a previous crashed run.
///
/// Safe to call repeatedly, and safe to call when nothing is left.
#[instrument(level = "debug", skip_all)]
pub fn cleanup_all() -> Swept {
    let mut swept = Swept::default();
    debug!(
        "sweeping configfs for backstores prefixed '{DEV_PREFIX}' and nexuses prefixed '{NAA_PREFIX}'"
    );

    let loopback = configfs::configfs_root().join("loopback");
    if loopback.is_dir()
        && let Ok(entries) = std::fs::read_dir(&loopback)
    {
        let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        dirs.sort();
        for naa_dir in dirs {
            let naa = naa_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if !naa.starts_with(NAA_PREFIX) {
                continue;
            }
            let lun0 = configfs::lun0_path(&naa);
            if lun0.is_dir()
                && let Ok(links) = std::fs::read_dir(&lun0)
            {
                for link in links.filter_map(|e| e.ok()) {
                    if link
                        .path()
                        .symlink_metadata()
                        .map(|m| m.is_symlink())
                        .unwrap_or(false)
                    {
                        configfs::rm_symlink(&link.path());
                    }
                }
            }
            configfs::rmdir(&lun0);
            configfs::rmdir(&configfs::tpgt_path(&naa));
            configfs::rmdir(&naa_dir);
            // Auditable: removing host state that outlived its process.
            info!(nexus = %naa, "removed a stale tcm_loop nexus");
            swept.nexuses.push(naa);
        }
    }

    let hba = configfs::configfs_root()
        .join("core")
        .join(configfs::CORE_HBA);
    if hba.is_dir()
        && let Ok(entries) = std::fs::read_dir(&hba)
    {
        let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        dirs.sort();
        for backstore in dirs {
            let name = backstore
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if backstore.is_dir() && name.starts_with(DEV_PREFIX) {
                configfs::rmdir(&backstore);
                info!(backstore = %name, "removed a stale overlaybd backstore");
                swept.backstores.push(name);
            }
        }
    }
    configfs::rmdir(&hba);

    debug!(
        "sweep removed {} nexus(es) and {} backstore(s)",
        swept.nexuses.len(),
        swept.backstores.len()
    );
    swept
}

/// Unmount the given mountpoints, then sweep configfs.
///
/// This is what `obdctl cleanup` runs: the mounts have to go first, since a
/// mounted device cannot be torn down.
#[instrument(level = "debug", skip_all, fields(mountpoints = mountpoints.len()))]
pub fn force_cleanup(mountpoints: &[PathBuf]) -> Result<Swept> {
    let mut unmounted = Vec::new();
    for target in mountpoints {
        if unmount_path(target) {
            unmounted.push(target.clone());
        }
    }
    let mut swept = cleanup_all();
    swept.unmounted = unmounted;
    Ok(swept)
}

/// Run the cleanup sweep on SIGINT/SIGTERM/SIGHUP, then exit.
///
/// The work happens on a dedicated listener thread rather than inside a signal
/// handler, so the configfs and mount operations do not have to be
/// async-signal-safe.
#[cfg(target_os = "linux")]
pub fn install_signal_handler(mountpoints: Vec<PathBuf>) -> Result<()> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])
        .map_err(|e| crate::error::Error::io("installing signal handler", e))?;

    debug!("installed a cleanup handler for SIGINT, SIGTERM and SIGHUP");
    std::thread::spawn(move || {
        if let Some(signal) = signals.forever().next() {
            // Deliberately not inside the signal handler: this does filesystem
            // and configfs work that is not async-signal-safe.
            warn!(
                signal,
                "caught a termination signal; sweeping devices before exiting"
            );
            let _ = force_cleanup(&mountpoints);
            std::process::exit(128 + signal);
        }
    });
    Ok(())
}

/// No-op off Linux: there are no devices to clean up.
#[cfg(not(target_os = "linux"))]
pub fn install_signal_handler(_mountpoints: Vec<PathBuf>) -> Result<()> {
    Ok(())
}
