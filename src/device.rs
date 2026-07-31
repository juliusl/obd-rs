//! Device lifecycle as a typestate: [`Device`] -> [`Live`] -> [`Mounted`].
//!
//! The ordering rules that the Python version could only document in comments
//! are compile errors here:
//!
//! * [`Mounted::dir`] hands out a **borrowed** [`Dir`], and [`Mounted::unmount`]
//!   consumes `self`, so the borrow checker refuses to let you unmount while a
//!   sandboxed handle is still alive. An open descriptor on the mount is
//!   exactly what makes `umount` return `EBUSY`.
//! * You cannot mount a device that is not up, or commit a layer while its
//!   device still exists, because the states are distinct types.

use std::path::{Path, PathBuf};
use std::time::Duration;

use cap_std::fs::Dir;

use crate::config::DeviceConfig;
use crate::configfs;
use crate::error::{Error, IoContext, Result};

#[cfg(target_os = "linux")]
use crate::configfs::MAX_DATA_AREA_MB;
#[cfg(target_os = "linux")]
use cap_std::ambient_authority;

/// Where overlaybd writes its log, used when reporting a launch failure.
pub const OVERLAYBD_LOG: &str = "/var/log/overlaybd.log";

/// How a device is mounted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Writable: the device must have an upper.
    Rw,
    /// `ro,noload`. All of a lower-only device's layers are read-only, so ext4
    /// cannot replay a journal; `noload` skips the attempt.
    Ro,
}

impl Mode {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn is_read_only(self) -> bool {
        self == Mode::Ro
    }
}

/// A device that has not been launched yet.
#[derive(Debug, Clone)]
pub struct Device {
    name: String,
    // Read only by the Linux launch path, but kept on every platform so the
    // type is identical everywhere.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    config_path: PathBuf,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    result_path: PathBuf,
    naa: String,
}

impl Device {
    /// Build a device.
    ///
    /// `name` must start with [`configfs::DEV_PREFIX`] so the cleanup sweep can
    /// recognise it. `naa_suffix` is appended to [`configfs::NAA_PREFIX`] and
    /// must be unique among live devices.
    pub fn new(
        name: impl Into<String>,
        config_path: impl Into<PathBuf>,
        result_path: impl Into<PathBuf>,
        naa_suffix: &str,
    ) -> Result<Self> {
        let name = name.into();
        if !name.starts_with(configfs::DEV_PREFIX) {
            return Err(Error::BadDeviceName {
                name,
                prefix: configfs::DEV_PREFIX,
            });
        }
        Ok(Device {
            name,
            config_path: config_path.into(),
            result_path: result_path.into(),
            naa: format!("{}{}", configfs::NAA_PREFIX, naa_suffix),
        })
    }

    /// Build a device from a [`DeviceConfig`], writing the config out first.
    pub fn from_config(
        name: impl Into<String>,
        config: &DeviceConfig,
        config_path: impl Into<PathBuf>,
        naa_suffix: &str,
    ) -> Result<Self> {
        let config_path = config_path.into();
        config.write(&config_path)?;
        Device::new(
            name,
            config_path,
            config.result_file().to_path_buf(),
            naa_suffix,
        )
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn naa(&self) -> &str {
        &self.naa
    }

    /// Launch the device and resolve its `/dev/sdX`.
    #[cfg(target_os = "linux")]
    pub fn up(self) -> Result<Live> {
        if let Some(parent) = self.result_path.parent() {
            std::fs::create_dir_all(parent).ctx(format!("creating {}", parent.display()))?;
        }
        let _ = std::fs::remove_file(&self.result_path);

        let backstore = configfs::backstore_path(&self.name);
        std::fs::create_dir_all(&backstore).ctx(format!("creating {}", backstore.display()))?;

        // The daemon registers the TCMU subtype "overlaybd"; everything after
        // the first '/' is taken as the absolute config path.
        let control = backstore.join("control");
        configfs::write_attr(
            &control,
            &format!("dev_config=overlaybd/{}", self.config_path.display()),
            1,
            Duration::ZERO,
        )?;
        configfs::write_attr(
            &control,
            &format!("max_data_area_mb={MAX_DATA_AREA_MB}"),
            1,
            Duration::ZERO,
        )?;
        configfs::write_attr(
            &backstore.join("enable"),
            "1",
            100,
            Duration::from_millis(50),
        )?;

        // overlaybd reports launch failures through resultFile, not through the
        // configfs write, so this must be checked explicitly.
        let result = configfs::await_result(&self.result_path, Duration::from_secs(10));
        if result != "success" {
            let mut live = Live {
                device: self,
                block_device: PathBuf::new(),
                scsi_address: None,
            };
            let failure = Error::LaunchFailed {
                device: live.device.name.clone(),
                result: if result.is_empty() {
                    "<empty>".to_string()
                } else {
                    result
                },
                log: PathBuf::from(OVERLAYBD_LOG),
                tail: configfs::log_tail(Path::new(OVERLAYBD_LOG), 25),
            };
            live.teardown();
            return Err(failure);
        }

        let lun0 = configfs::lun0_path(&self.naa);
        std::fs::create_dir_all(&lun0).ctx(format!("creating {}", lun0.display()))?;
        configfs::write_attr(
            &configfs::tpgt_path(&self.naa).join("nexus"),
            &self.naa,
            1,
            Duration::ZERO,
        )?;
        let link = configfs::lun_link_path(&self.naa, &self.name);
        if link.symlink_metadata().is_err() {
            std::os::unix::fs::symlink(&backstore, &link).ctx(format!(
                "linking {} -> {}",
                link.display(),
                backstore.display()
            ))?;
        }

        let (block_device, address) =
            configfs::resolve_block_device(&self.name, &self.naa, Duration::from_secs(30))?;

        Ok(Live {
            device: self,
            block_device,
            scsi_address: Some(address),
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn up(self) -> Result<Live> {
        Err(Error::unsupported())
    }
}

/// A launched device with a resolved block node, not yet mounted.
#[derive(Debug)]
pub struct Live {
    device: Device,
    block_device: PathBuf,
    scsi_address: Option<String>,
}

impl Live {
    /// The resolved `/dev/sdX`. Never hardcode this; it is recycled.
    pub fn block_device(&self) -> &Path {
        &self.block_device
    }

    pub fn name(&self) -> &str {
        &self.device.name
    }

    pub fn naa(&self) -> &str {
        &self.device.naa
    }

    /// Mount the device as ext4.
    ///
    /// Mount *before* handing the directory to anything that opens descriptors
    /// on it, and drop those descriptors before unmounting.
    #[cfg(target_os = "linux")]
    pub fn mount(self, mountpoint: impl Into<PathBuf>, mode: Mode) -> Result<Mounted> {
        use rustix::mount::{MountFlags, mount};

        let mountpoint = mountpoint.into();
        std::fs::create_dir_all(&mountpoint).ctx(format!("creating {}", mountpoint.display()))?;

        // ext4 cannot replay a journal on a device whose layers are all
        // read-only, so `noload` skips the attempt.
        let mut flags = MountFlags::empty();
        let data: Option<&std::ffi::CStr> = if mode.is_read_only() {
            flags |= MountFlags::RDONLY;
            Some(c"noload")
        } else {
            None
        };

        mount(&self.block_device, &mountpoint, "ext4", flags, data).map_err(|errno| {
            Error::io(
                format!(
                    "mounting {} at {} ({})",
                    self.block_device.display(),
                    mountpoint.display(),
                    if mode.is_read_only() {
                        "ro,noload"
                    } else {
                        "rw"
                    }
                ),
                std::io::Error::from_raw_os_error(errno.raw_os_error()),
            )
        })?;

        // The one ambient-authority call: from here on, everything reachable
        // through this handle is confined to the mount.
        let dir = Dir::open_ambient_dir(&mountpoint, ambient_authority())
            .map_err(|e| Error::io(format!("opening {} with cap-std", mountpoint.display()), e))?;

        Ok(Mounted {
            live: Some(self),
            mountpoint,
            dir: Some(dir),
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn mount(self, _mountpoint: impl Into<PathBuf>, _mode: Mode) -> Result<Mounted> {
        Err(Error::unsupported())
    }

    /// Tear the device down: LUN symlink, `lun_0`, `tpgt_1`, `naa.*`,
    /// backstore, HBA, then wait for the SCSI node to disappear.
    pub fn down(mut self) -> Result<()> {
        self.teardown();
        Ok(())
    }

    /// Idempotent teardown, strictly in reverse creation order.
    fn teardown(&mut self) {
        let name = &self.device.name;
        let naa = &self.device.naa;
        configfs::rm_symlink(&configfs::lun_link_path(naa, name));
        configfs::rmdir(&configfs::lun0_path(naa));
        configfs::rmdir(&configfs::tpgt_path(naa));
        configfs::rmdir(&configfs::configfs_root().join("loopback").join(naa));
        configfs::rmdir(&configfs::backstore_path(name));
        // Only succeeds once empty, which is what we want.
        configfs::rmdir(
            &configfs::configfs_root()
                .join("core")
                .join(configfs::CORE_HBA),
        );
        if let Some(address) = self.scsi_address.take() {
            configfs::wait_for_scsi_removal(&address, Duration::from_secs(15));
        }
    }

    /// Give up ownership without tearing down.
    ///
    /// Used by `obdctl device up`, which must leave the device running for a
    /// later process. Library callers normally want [`Live::down`] instead.
    pub fn persist(self) -> PersistedDevice {
        let persisted = PersistedDevice {
            name: self.device.name.clone(),
            naa_suffix: self
                .device
                .naa
                .strip_prefix(configfs::NAA_PREFIX)
                .unwrap_or(&self.device.naa)
                .to_string(),
            block_device: self.block_device.clone(),
            mountpoint: None,
        };
        std::mem::forget(self);
        persisted
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        // RAII teardown: a dropped device must not leave configfs entries that
        // need manual surgery. `persist()` opts out via mem::forget.
        self.teardown();
    }
}

/// A mounted device. Holds the cap-std handle to the mount.
#[derive(Debug)]
pub struct Mounted {
    // Both are Options purely so `unmount`/`persist` can move them out of
    // `self` without unsafe; they are always Some for a live `Mounted`.
    live: Option<Live>,
    mountpoint: PathBuf,
    dir: Option<Dir>,
}

impl Mounted {
    /// Sandboxed access to the mount.
    ///
    /// Borrowed on purpose: the borrow keeps [`Mounted::unmount`] - which
    /// consumes `self` - from being called while a descriptor on the mount is
    /// still open. That is the compile-time form of "drop it before
    /// unmounting".
    pub fn dir(&self) -> &Dir {
        self.dir
            .as_ref()
            .expect("dir is present for the lifetime of Mounted")
    }

    /// Create a subdirectory and return a handle confined to it.
    ///
    /// Prefer this over handing out the mount root: overlaybd's consumers wipe
    /// their output directory on every run, so the mount root would lose
    /// `lost+found` and anything else.
    pub fn create_subdir(&self, name: &str) -> Result<Dir> {
        let dir = self.dir();
        if dir.metadata(name).is_err() {
            dir.create_dir(name).ctx(format!(
                "creating {name} under {}",
                self.mountpoint.display()
            ))?;
        }
        dir.open_dir(name).ctx(format!(
            "opening {name} under {}",
            self.mountpoint.display()
        ))
    }

    /// Where the device is mounted. Needed for handing the path to a process
    /// that cannot take a file descriptor.
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    pub fn block_device(&self) -> &Path {
        self.live().block_device()
    }

    pub fn name(&self) -> &str {
        self.live().name()
    }

    fn live(&self) -> &Live {
        self.live
            .as_ref()
            .expect("live is present for the lifetime of Mounted")
    }

    /// sync, then unmount, returning the still-live device.
    #[cfg(target_os = "linux")]
    pub fn unmount(mut self) -> Result<Live> {
        use rustix::mount::{UnmountFlags, unmount};

        // Drop our own handle first, otherwise we are the thing holding it.
        self.dir.take();
        rustix::fs::sync();

        let mut last: Option<std::io::Error> = None;
        for _ in 0..20 {
            match unmount(&self.mountpoint, UnmountFlags::empty()) {
                Ok(()) => {
                    // Take the Live out so our Drop has nothing left to do.
                    return Ok(self
                        .live
                        .take()
                        .expect("live is present for the lifetime of Mounted"));
                }
                Err(errno) => {
                    last = Some(std::io::Error::from_raw_os_error(errno.raw_os_error()));
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
        Err(Error::Busy {
            path: self.mountpoint.clone(),
            source: last.unwrap_or_else(|| std::io::Error::other("unknown umount failure")),
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn unmount(self) -> Result<Live> {
        Err(Error::unsupported())
    }

    /// Unmount and tear the device down in one step.
    pub fn down(self) -> Result<()> {
        self.unmount()?.down()
    }

    /// Leave the device mounted for a later process; see [`Live::persist`].
    pub fn persist(mut self) -> PersistedDevice {
        self.dir.take();
        let mountpoint = self.mountpoint.clone();
        let live = self
            .live
            .take()
            .expect("live is present for the lifetime of Mounted");
        let mut persisted = live.persist();
        persisted.mountpoint = Some(mountpoint);
        persisted
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        self.dir.take();
        // `unmount()` and `persist()` both take `live`, so its absence means
        // this Mounted was handed off deliberately and must stay mounted.
        //
        // Written as a positive condition rather than an early return: the
        // block below is cfg'd out on non-Linux, which makes a `return` here
        // look redundant to clippy on those platforms even though removing it
        // silently unmounts persisted devices on Linux.
        if self.live.is_some() {
            #[cfg(target_os = "linux")]
            {
                use rustix::mount::{UnmountFlags, unmount};
                rustix::fs::sync();
                for _ in 0..20 {
                    if unmount(&self.mountpoint, UnmountFlags::empty()).is_ok() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }
}

/// Tear down one named device without having a [`Live`] handle.
///
/// `obdctl device up` and `obdctl device down` run in different processes, so
/// teardown cannot rely on `Drop`. Everything it needs is derivable from the
/// name and nexus suffix the caller already chose, which is why no state file
/// is involved. Idempotent: removing a device that is already gone succeeds.
pub fn teardown_named(name: &str, naa_suffix: &str) -> Result<()> {
    if !name.starts_with(configfs::DEV_PREFIX) {
        return Err(Error::BadDeviceName {
            name: name.to_string(),
            prefix: configfs::DEV_PREFIX,
        });
    }
    let naa = format!("{}{}", configfs::NAA_PREFIX, naa_suffix);

    let address = std::fs::read_to_string(configfs::tpgt_path(&naa).join("address"))
        .ok()
        .map(|s| s.trim().to_string());

    configfs::rm_symlink(&configfs::lun_link_path(&naa, name));
    configfs::rmdir(&configfs::lun0_path(&naa));
    configfs::rmdir(&configfs::tpgt_path(&naa));
    configfs::rmdir(&configfs::configfs_root().join("loopback").join(&naa));
    configfs::rmdir(&configfs::backstore_path(name));
    configfs::rmdir(
        &configfs::configfs_root()
            .join("core")
            .join(configfs::CORE_HBA),
    );

    if let Some(address) = address {
        configfs::wait_for_scsi_removal(&address, Duration::from_secs(15));
    }
    Ok(())
}

/// A device deliberately left running across process boundaries.
///
/// `obdctl device up` prints this so a later `obdctl device down` can find the
/// device again; nothing here is state that could not be re-derived from the
/// name and nexus suffix.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedDevice {
    pub name: String,
    pub naa_suffix: String,
    pub block_device: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mountpoint: Option<PathBuf>,
}
