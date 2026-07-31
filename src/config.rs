//! The overlaybd device config JSON.
//!
//! Field names and shape match what the daemon expects; see overlaybd's
//! "Standalone Usage" docs. `lowers` is bottom-up.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// One lower layer: either a local file or a blob streamed from a registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lower {
    /// A layer that exists as a file on this host.
    File(PathBuf),
    /// A layer overlaybd fetches from a registry.
    ///
    /// overlaybd resolves these as `<repoBlobUrl>/<digest>` and range-reads the
    /// blob; it never fetches a manifest. That is why the blob can be pushed as
    /// a plain OCI artifact rather than as a container image.
    Remote {
        digest: String,
        size: u64,
        /// Where overlaybd may persist what it has fetched. `None` leaves the
        /// layer purely streamed (it still uses the daemon's own block cache).
        cache_dir: Option<PathBuf>,
    },
}

impl Lower {
    /// A local file lower.
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Lower::File(path.into())
    }

    /// A streamed lower. The digest must be `sha256:...`, which is what
    /// overlaybd appends to `repoBlobUrl`.
    pub fn remote(digest: impl Into<String>, size: u64) -> Result<Self> {
        let digest = digest.into();
        if !digest.starts_with("sha256:") {
            return Err(Error::BadDigest { digest });
        }
        Ok(Lower::Remote {
            digest,
            size,
            cache_dir: None,
        })
    }

    /// Let overlaybd persist fetched blocks into `dir`.
    pub fn with_cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        if let Lower::Remote { cache_dir, .. } = &mut self {
            *cache_dir = Some(dir.into());
        }
        self
    }

    fn is_remote(&self) -> bool {
        matches!(self, Lower::Remote { .. })
    }
}

/// Serialised form of a lower. Untagged so a file lower emits `{"file": ...}`
/// and a remote one emits `{"digest": ..., "size": ...}`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum LowerRepr {
    File {
        file: String,
    },
    Remote {
        digest: String,
        size: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        dir: Option<String>,
    },
}

impl From<&Lower> for LowerRepr {
    fn from(lower: &Lower) -> Self {
        match lower {
            Lower::File(path) => LowerRepr::File {
                file: path.display().to_string(),
            },
            Lower::Remote {
                digest,
                size,
                cache_dir,
            } => LowerRepr::Remote {
                digest: digest.clone(),
                size: *size,
                dir: cache_dir.as_ref().map(|d| d.display().to_string()),
            },
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct UpperRepr {
    index: String,
    data: String,
}

/// The full device config, in the order overlaybd's examples use.
#[derive(Debug, Serialize, Deserialize)]
struct DeviceConfigRepr {
    #[serde(rename = "repoBlobUrl", skip_serializing_if = "Option::is_none")]
    repo_blob_url: Option<String>,
    lowers: Vec<LowerRepr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upper: Option<UpperRepr>,
    #[serde(rename = "resultFile")]
    result_file: String,
}

/// A device config: the lowers, an optional writable upper, and where the
/// daemon reports launch success.
#[derive(Debug, Clone)]
pub struct DeviceConfig {
    lowers: Vec<Lower>,
    upper: Option<(PathBuf, PathBuf)>,
    result_file: PathBuf,
    repo_blob_url: Option<String>,
}

impl DeviceConfig {
    /// Start a config. `result_file` is where overlaybd writes `success` or a
    /// failure reason.
    pub fn new(result_file: impl Into<PathBuf>) -> Self {
        DeviceConfig {
            lowers: Vec::new(),
            upper: None,
            result_file: result_file.into(),
            repo_blob_url: None,
        }
    }

    /// Append a lower. Order matters: bottom-up.
    pub fn lower(mut self, lower: Lower) -> Self {
        self.lowers.push(lower);
        self
    }

    /// Append several lowers, bottom-up.
    pub fn lowers(mut self, lowers: impl IntoIterator<Item = Lower>) -> Self {
        self.lowers.extend(lowers);
        self
    }

    /// Attach the sparse writable layer. Without one the device is read-only.
    pub fn upper(mut self, data: impl Into<PathBuf>, index: impl Into<PathBuf>) -> Self {
        self.upper = Some((data.into(), index.into()));
        self
    }

    /// Base URL for streamed lowers, e.g.
    /// `https://<registry>/v2/<repo>/blobs`.
    ///
    /// This is a single top-level field, so every remote lower in one device
    /// comes from the same repository.
    pub fn repo_blob_url(mut self, url: impl Into<String>) -> Self {
        self.repo_blob_url = Some(url.into().trim_end_matches('/').to_string());
        self
    }

    /// True if any lower is streamed.
    pub fn has_remote_lowers(&self) -> bool {
        self.lowers.iter().any(Lower::is_remote)
    }

    /// Where the daemon reports launch status.
    pub fn result_file(&self) -> &Path {
        &self.result_file
    }

    /// Render to JSON, validating the combination first.
    pub fn to_json(&self) -> Result<String> {
        if self.has_remote_lowers() && self.repo_blob_url.is_none() {
            return Err(Error::MissingRepoBlobUrl);
        }
        let repr = DeviceConfigRepr {
            repo_blob_url: self.repo_blob_url.clone(),
            lowers: self.lowers.iter().map(LowerRepr::from).collect(),
            upper: self.upper.as_ref().map(|(data, index)| UpperRepr {
                index: index.display().to_string(),
                data: data.display().to_string(),
            }),
            result_file: self.result_file.display().to_string(),
        };
        Ok(serde_json::to_string_pretty(&repr)?)
    }

    /// Write the config, creating parent directories as needed.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::io(format!("creating {}", parent.display()), e))?;
        }
        let mut json = self.to_json()?;
        json.push('\n');
        std::fs::write(path, json)
            .map_err(|e| Error::io(format!("writing {}", path.display()), e))?;
        Ok(())
    }
}
