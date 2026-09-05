//! A private staging directory for in-flight downloads (DESIGN.md §3.6).
//!
//! Every asset is written to `<name>.part` under a 0700 directory and only
//! renamed to its final (sanitised) name once the transfer completes
//! cleanly and its SHA-256 has been computed. A partial download therefore
//! never carries a name a bootstrap script might pick up by accident, and
//! dropping the sink or the directory before `finish()`/explicit `keep`
//! leaves nothing behind.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;

use async_trait::async_trait;
use rand::Rng;

use super::filename::sanitize_filename;
use super::hash::Sha256Writer;
use crate::error::{Result, WargameError};
use crate::ports::wargame_provider::{AssetSink, DownloadedFile};

/// A private (0700) scratch directory for downloads in flight. Removed on
/// drop unless constructed with `keep = true` (`config.debug.keep_staging`).
pub struct StagingDir {
    pub path: PathBuf,
    keep: bool,
}

impl StagingDir {
    /// Creates a fresh randomly-named directory under `root/staging/`.
    pub fn create(root: &std::path::Path, keep: bool) -> Result<Self> {
        let staging_root = root.join("staging");
        std::fs::create_dir_all(&staging_root)?;

        let suffix: String = {
            let mut rng = rand::thread_rng();
            (0..16)
                .map(|_| char::from(rng.sample(rand::distributions::Alphanumeric)))
                .collect()
        };
        let path = staging_root.join(suffix);
        std::fs::create_dir(&path)?;
        set_mode_0700(&path)?;

        Ok(StagingDir { path, keep })
    }

    /// A sink that will stage the asset identified by `asset_id` into this
    /// directory.
    pub fn sink(&self, asset_id: &str) -> StagingSink {
        StagingSink {
            dir: self.path.clone(),
            asset_id: asset_id.to_string(),
            open: None,
        }
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(unix)]
fn set_mode_0700(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode_0700(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_mode_0600(file: &File) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode_0600(_file: &File) -> Result<()> {
    Ok(())
}

struct OpenState {
    safe_name: String,
    part_path: PathBuf,
    writer: Sha256Writer<File>,
}

/// An [`AssetSink`] that streams into a `.part` file inside a [`StagingDir`]
/// and only reveals the final (sanitised) filename once `finish()` succeeds.
pub struct StagingSink {
    dir: PathBuf,
    asset_id: String,
    open: Option<OpenState>,
}

#[async_trait]
impl AssetSink for StagingSink {
    async fn begin(&mut self, server_filename: &str) -> Result<()> {
        let safe_name = sanitize_filename(server_filename, &self.asset_id);
        let part_path = self.dir.join(format!("{safe_name}.part"));
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&part_path)?;
        set_mode_0600(&file)?;
        self.open = Some(OpenState {
            safe_name,
            part_path,
            writer: Sha256Writer::new(file),
        });
        Ok(())
    }

    async fn write(&mut self, chunk: &[u8]) -> Result<()> {
        let open = self.open.as_mut().ok_or_else(|| {
            WargameError::HashOrWrite("write() called before begin()".to_string())
        })?;
        open.writer
            .write_all(chunk)
            .map_err(|e| WargameError::HashOrWrite(e.to_string()))?;
        Ok(())
    }

    async fn finish(&mut self) -> Result<DownloadedFile> {
        let open = self.open.take().ok_or_else(|| {
            WargameError::HashOrWrite("finish() called before begin()".to_string())
        })?;
        let (file, sha256, size) = open.writer.finish();
        file.sync_all()
            .map_err(|e| WargameError::HashOrWrite(e.to_string()))?;
        drop(file);
        let final_path = self.dir.join(&open.safe_name);
        std::fs::rename(&open.part_path, &final_path)
            .map_err(|e| WargameError::HashOrWrite(e.to_string()))?;
        Ok(DownloadedFile {
            filename: open.safe_name,
            path: final_path,
            sha256,
            size,
        })
    }
}

impl Drop for StagingSink {
    fn drop(&mut self) {
        if let Some(open) = self.open.take() {
            let _ = std::fs::remove_file(&open.part_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn staging_dir_create_is_0700() {
        let root = tempfile::tempdir().unwrap();
        let staging = StagingDir::create(root.path(), false).unwrap();
        assert!(staging.path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&staging.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700);
        }
    }

    #[test]
    fn staging_dir_drop_removes_unless_keep() {
        let root = tempfile::tempdir().unwrap();
        let path = {
            let staging = StagingDir::create(root.path(), false).unwrap();
            staging.path.clone()
        };
        assert!(!path.exists());

        let path_keep = {
            let staging = StagingDir::create(root.path(), true).unwrap();
            staging.path.clone()
        };
        assert!(path_keep.exists());
    }

    #[tokio::test]
    async fn sink_writes_part_then_final_name_with_matching_sha256() {
        let root = tempfile::tempdir().unwrap();
        let staging = StagingDir::create(root.path(), true).unwrap();
        let mut sink = staging.sink("7");

        sink.begin("../../evil/report.pdf").await.unwrap();
        let part_path = staging.path.join("report.pdf.part");
        assert!(part_path.exists());

        sink.write(b"PK\x03\x04fake").await.unwrap();
        let file = sink.finish().await.unwrap();

        assert!(!part_path.exists());
        assert_eq!(file.filename, "report.pdf");
        assert!(file.path.exists());

        let mut hasher = Sha256::new();
        hasher.update(b"PK\x03\x04fake");
        assert_eq!(file.sha256, hex::encode(hasher.finalize()));
        assert_eq!(file.size, 8);
    }

    #[tokio::test]
    async fn dropping_sink_mid_write_leaves_no_part_file() {
        let root = tempfile::tempdir().unwrap();
        let staging = StagingDir::create(root.path(), true).unwrap();
        let part_path;
        {
            let mut sink = staging.sink("7");
            sink.begin("asset.bin").await.unwrap();
            sink.write(b"partial").await.unwrap();
            part_path = staging.path.join("asset.bin.part");
            assert!(part_path.exists());
            // sink dropped here without finish()
        }
        assert!(!part_path.exists());
    }
}
