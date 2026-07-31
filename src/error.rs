//! Error type for the overlaybd device lifecycle.

use std::path::PathBuf;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// configfs, TCMU and `mount(2)` are Linux-only. The types exist on other
    /// platforms so the crate can be developed and unit-tested there, but the
    /// operations refuse to run.
    #[error(
        "overlaybd device operations require Linux (configfs + target_core_user); \
         this binary was built for {os}"
    )]
    UnsupportedPlatform { os: &'static str },

    #[error(
        "overlaybd binary `{name}` not found in {searched:?}; \
         run scripts/install-overlaybd.sh, or set OVERLAYBD_BIN_DIR"
    )]
    ToolMissing {
        name: &'static str,
        searched: Vec<PathBuf>,
    },

    #[error("`{command}` failed ({code}):\n{output}")]
    ToolFailed {
        command: String,
        code: String,
        output: String,
    },

    /// overlaybd reports launch failures through the device's `resultFile`
    /// rather than through the configfs write, so this is checked explicitly.
    #[error("overlaybd refused to launch `{device}`: resultFile={result}\n--- {log} ---\n{tail}")]
    LaunchFailed {
        device: String,
        result: String,
        log: PathBuf,
        tail: String,
    },

    #[error(
        "no usable block device appeared for `{device}` at {path} (sysfs saw {seen}); \
         is udev running? check dmesg for tcm_loop errors"
    )]
    NoBlockDevice {
        device: String,
        path: PathBuf,
        seen: String,
    },

    #[error(
        "could not unmount {path}: {source}\n\
         an open file descriptor on the mount is the usual cause - drop any cap-std Dir \
         (and anything derived from it) before unmounting"
    )]
    Busy {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("device name must start with `{prefix}`: {name}")]
    BadDeviceName { name: String, prefix: &'static str },

    #[error("layer digest must be `sha256:...`: {digest}")]
    BadDigest { digest: String },

    #[error("remote lowers need a repo_blob_url (overlaybd rejects an empty repoBlobUrl)")]
    MissingRepoBlobUrl,

    #[error("refusing to overwrite existing layer {path}: overlaybd-create opens outputs O_EXCL")]
    LayerExists { path: PathBuf },

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

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
