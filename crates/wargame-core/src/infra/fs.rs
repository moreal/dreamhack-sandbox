//! `$WARGAME_HOME` resolution and disk-space checks (DESIGN.md §3.5).

use std::path::{Path, PathBuf};

use crate::error::{Result, WargameError};

/// The root of everything wargame-vm writes to disk, and the well-known
/// subdirectories under it (DESIGN.md §3.5).
pub struct AppDirs {
    pub root: PathBuf,
}

impl AppDirs {
    /// Resolves the root directory: `$WARGAME_HOME` if set, else
    /// `~/Library/Application Support/wargame-vm` on macOS, else
    /// `~/.local/share/wargame-vm`.
    pub fn resolve() -> Result<Self> {
        if let Ok(home) = std::env::var("WARGAME_HOME") {
            return Ok(AppDirs {
                root: PathBuf::from(home),
            });
        }

        let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            WargameError::Config(
                "could not determine the home directory ($HOME is unset)".to_string(),
            )
        })?;

        let root = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/wargame-vm")
        } else {
            home.join(".local/share/wargame-vm")
        };
        Ok(AppDirs { root })
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config").join("config.toml")
    }

    pub fn profiles_dir(&self) -> PathBuf {
        self.root.join("profiles")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn keys_dir(&self) -> PathBuf {
        self.root.join("keys")
    }

    /// Creates every well-known subdirectory (mode 0700 on unix).
    pub fn ensure_layout(&self) -> Result<()> {
        for dir in [
            self.root.clone(),
            self.root.join("config"),
            self.profiles_dir(),
            self.sessions_dir(),
            self.cache_dir(),
            self.logs_dir(),
            self.keys_dir(),
        ] {
            std::fs::create_dir_all(&dir)?;
            set_mode_0700(&dir)?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn set_mode_0700(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode_0700(_dir: &Path) -> Result<()> {
    Ok(())
}

/// Bytes free on the filesystem containing `path`.
pub fn disk_free_bytes(path: &Path) -> Result<u64> {
    Ok(fs2::available_space(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests mutate the process-wide WARGAME_HOME env var, so serialize them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn resolve_honours_wargame_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("WARGAME_HOME", dir.path());
        let dirs = AppDirs::resolve().unwrap();
        assert_eq!(dirs.root, dir.path());
        std::env::remove_var("WARGAME_HOME");
    }

    #[test]
    fn ensure_layout_creates_all_subdirs() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("WARGAME_HOME", dir.path());
        let dirs = AppDirs::resolve().unwrap();
        dirs.ensure_layout().unwrap();
        assert!(dirs.profiles_dir().is_dir());
        assert!(dirs.sessions_dir().is_dir());
        assert!(dirs.cache_dir().is_dir());
        assert!(dirs.logs_dir().is_dir());
        assert!(dirs.keys_dir().is_dir());
        std::env::remove_var("WARGAME_HOME");
    }

    #[test]
    fn disk_free_bytes_is_nonzero_for_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let free = disk_free_bytes(dir.path()).unwrap();
        assert!(free > 0);
    }
}
