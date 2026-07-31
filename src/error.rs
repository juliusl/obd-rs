//! Error type for the overlaybd device lifecycle.

use std::path::PathBuf;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong in the device lifecycle.
///
/// The variants carry the state needed to act on them - which device, which
/// path, which errno - because the remedy is usually outside this process.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// configfs, TCMU and `mount(2)` are Linux-only. The types exist on other
    /// platforms so the crate can be developed and unit-tested there, but the
    /// operations refuse to run.
    /// Device operations were attempted on a platform that has no configfs.
    #[error(
        "overlaybd device operations require Linux (configfs + target_core_user); \
         this binary was built for {os}"
    )]
    UnsupportedPlatform {
        /// The platform this binary was built for.
        os: &'static str,
    },

    /// An `overlaybd-*` binary is not installed where this crate looks.
    #[error(
        "overlaybd binary `{name}` not found in {searched:?}; \
         run scripts/install-overlaybd.sh, or set OVERLAYBD_BIN_DIR"
    )]
    ToolMissing {
        /// The binary that could not be found, e.g. `overlaybd-create`.
        name: &'static str,
        /// Every path that was tried, in order.
        searched: Vec<PathBuf>,
    },

    /// An `overlaybd-*` binary ran but exited non-zero.
    #[error("`{command}` failed ({code}):\n{output}")]
    ToolFailed {
        /// The command line that was run.
        command: String,
        /// Exit status, or `signal` when it was killed.
        code: String,
        /// Combined stdout and stderr, trimmed.
        output: String,
    },

    /// overlaybd reports launch failures through the device's `resultFile`
    /// rather than through the configfs write, so this is checked explicitly.
    /// The daemon refused to attach the device.
    #[error("overlaybd refused to launch `{device}`: resultFile={result}\n--- {log} ---\n{tail}")]
    LaunchFailed {
        /// Backstore name of the device that failed to launch.
        device: String,
        /// What the daemon wrote to `resultFile`.
        result: String,
        /// Path to the overlaybd log the tail came from.
        log: PathBuf,
        /// Last lines of that log, which is where the real cause appears.
        tail: String,
    },

    /// The device attached but no usable `/dev/sdX` appeared in time.
    #[error(
        "no usable block device appeared for `{device}` at {path} (sysfs saw {seen}); \
         is udev running? check dmesg for tcm_loop errors"
    )]
    NoBlockDevice {
        /// Backstore name of the device whose node never appeared.
        device: String,
        /// The sysfs directory that was watched.
        path: PathBuf,
        /// The last node name sysfs reported, or `<nothing>`.
        seen: String,
    },

    /// `umount(2)` kept returning busy, so something still holds the mount.
    #[error(
        "could not unmount {path}: {source}\n\
         an open file descriptor on the mount is the usual cause - drop any cap-std Dir \
         (and anything derived from it) before unmounting"
    )]
    Busy {
        /// The mountpoint that could not be unmounted.
        path: PathBuf,
        /// The final `umount(2)` error, usually `EBUSY`.
        #[source]
        source: std::io::Error,
    },

    /// A device name would be invisible to the cleanup sweep.
    #[error("device name must start with `{prefix}`: {name}")]
    BadDeviceName {
        /// The rejected name.
        name: String,
        /// The prefix the cleanup sweep requires.
        prefix: &'static str,
    },

    /// A remote layer digest is not in the form overlaybd expects.
    #[error("layer digest must be `sha256:...`: {digest}")]
    BadDigest {
        /// The rejected digest.
        digest: String,
    },

    /// A streamed lower was given without the URL needed to fetch it.
    #[error("remote lowers need a repo_blob_url (overlaybd rejects an empty repoBlobUrl)")]
    MissingRepoBlobUrl,

    /// A layer output path already exists; the tools open outputs `O_EXCL`.
    #[error("refusing to overwrite existing layer {path}: overlaybd-create opens outputs O_EXCL")]
    LayerExists {
        /// The path that already exists.
        path: PathBuf,
    },

    /// A filesystem operation failed, with context about what was attempted.
    #[error("{context}: {source}")]
    Io {
        /// What was being attempted, e.g. `creating /var/lib/x`.
        context: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The device config could not be rendered to JSON.
    #[error("could not serialise the device config: {0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }

    /// The current platform, for `UnsupportedPlatform`. Only constructed off
    /// Linux, where the device operations refuse to run.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub(crate) fn unsupported() -> Self {
        Error::UnsupportedPlatform {
            os: std::env::consts::OS,
        }
    }
}

/// Lets callers `?` the `io::Result` that cap-std's [`Dir`](cap_std::fs::Dir)
/// operations return straight into this crate's error type.
impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io {
            context: "io".to_string(),
            source,
        }
    }
}

/// Add context to an `io::Result` without dragging in anyhow.
pub(crate) trait IoContext<T> {
    fn ctx(self, context: impl Into<String>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn ctx(self, context: impl Into<String>) -> Result<T> {
        self.map_err(|source| Error::io(context, source))
    }
}
