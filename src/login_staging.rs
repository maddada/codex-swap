//! Login homes are temporary, but their leases must also survive interrupted launchers.
use crate::{fsutil, store::Store};
use anyhow::{Context, Result, bail};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

pub struct LoginStaging {
    // Remove the directory before releasing its lease, including on an ordinary error.
    directory: tempfile::TempDir,
    _lease: File,
}

impl LoginStaging {
    /// Call while Store holds the registry lock, so purge cannot miss a new login.
    pub fn new(store: &Store, prefix: &str) -> Result<Self> {
        let directory = fsutil::private_tempdir(&store.root, prefix)?;
        let lease = store.lease(directory.path(), true)?;
        crate::platform::keep_lease_across_exec(&lease)?;
        Ok(Self {
            directory,
            _lease: lease,
        })
    }

    pub fn path(&self) -> &Path {
        self.directory.path()
    }
}

fn validate(root: &Path, directory: &Path, protected: &[&Path]) -> Result<()> {
    if directory.parent() != Some(root) {
        bail!("purge refused: login staging is outside the owned root");
    }
    let metadata = fs::symlink_metadata(directory)?;
    #[cfg(unix)]
    let owned_directory = {
        use std::os::unix::fs::MetadataExt;
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0
    };
    #[cfg(windows)]
    let owned_directory = {
        use std::os::windows::fs::MetadataExt;
        metadata.is_dir()
            && metadata.file_attributes()
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                == 0
    };
    if !owned_directory || directory.canonicalize()? != directory {
        bail!(
            "purge refused: login staging must be a real, private owned directory ({})",
            directory.display()
        );
    }
    #[cfg(windows)]
    crate::platform::owned(directory)?;
    for home in protected {
        let home = fsutil::absolute(home)?;
        if directory.starts_with(&home) || home.starts_with(directory) {
            bail!(
                "purge refused: login staging overlaps an original or adopted Codex home ({})",
                home.display()
            );
        }
    }
    Ok(())
}

/// Discover legacy and current staging while Store excludes new stage creation.
pub fn directories(store: &Store, protected: &[&Path]) -> Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    for entry in fs::read_dir(&store.root)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("login-") || name.starts_with("new-login-"))
        {
            let directory = entry.path();
            // An ordinary login may finish and remove its TempDir during enumeration.
            match validate(&store.root, &directory, protected) {
                Ok(()) => directories.push(directory),
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                        && matches!(fs::symlink_metadata(&directory), Err(missing) if missing.kind() == std::io::ErrorKind::NotFound) =>
                    {}
                Err(error) => {
                    return Err(error)
                        .context(format!("inspect login staging {}", directory.display()));
                }
            }
        }
    }
    Ok(directories)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_validation_refuses_outside_paths() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        assert!(validate(root.path(), outside.path(), &[]).is_err());
        assert!(outside.path().exists());
    }
}
