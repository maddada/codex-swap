//! Login homes are temporary, but their leases must also survive interrupted launchers.
use crate::{fsutil, store::Store};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

const MARKER: &str = ".xswap-login-staging.json";

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DirectoryIdentity {
    volume: u64,
    file: [u8; 16],
}

impl DirectoryIdentity {
    fn read(directory: &File) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = directory.metadata()?;
            Ok(Self {
                volume: metadata.dev(),
                file: u128::from(metadata.ino()).to_le_bytes(),
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
            };
            let mut information = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
            if unsafe {
                GetFileInformationByHandleEx(
                    directory.as_raw_handle(),
                    FileIdInfo,
                    information.as_mut_ptr().cast(),
                    size_of::<FILE_ID_INFO>() as u32,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error()).context("identify login staging");
            }
            let information = unsafe { information.assume_init() };
            Ok(Self {
                volume: information.VolumeSerialNumber,
                file: information.FileId.Identifier,
            })
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Marker {
    schema_version: u32,
    directory_name: String,
    directory_identity: DirectoryIdentity,
}

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
        mark(directory.path())?;
        Ok(Self {
            directory,
            _lease: lease,
        })
    }

    pub fn path(&self) -> &Path {
        self.directory.path()
    }
}

fn mark(path: &Path) -> Result<()> {
    fsutil::create_json(
        &path.join(MARKER),
        &Marker {
            schema_version: 1,
            directory_name: path
                .file_name()
                .context("login staging has no directory name")?
                .to_string_lossy()
                .into_owned(),
            directory_identity: DirectoryIdentity::read(&open_directory(path)?)?,
        },
    )
}

fn open_directory(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let directory = options.open(path).with_context(|| {
        format!(
            "purge refused: login staging must be a real, private owned directory ({})",
            path.display()
        )
    })?;
    let metadata = directory.metadata()?;
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
    if !owned_directory {
        bail!(
            "purge refused: login staging must be a real, private owned directory ({})",
            path.display()
        );
    }
    #[cfg(windows)]
    crate::platform::owned(path)?;
    Ok(directory)
}

/// Holds the discovered object open so its filesystem identity cannot be reused.
pub struct StagingDirectory {
    path: PathBuf,
    identity: DirectoryIdentity,
    _directory: File,
}

impl StagingDirectory {
    fn capture(root: &Path, path: &Path, protected: &[&Path]) -> Result<Self> {
        if path.parent() != Some(root) {
            bail!("purge refused: login staging is outside the owned root");
        }
        let directory = open_directory(path)?;
        if path.canonicalize()? != path {
            bail!("purge refused: login staging path is not canonical");
        }
        for home in protected {
            let home = fsutil::absolute(home)?;
            if path.starts_with(&home) || home.starts_with(path) {
                bail!(
                    "purge refused: login staging overlaps an original or adopted Codex home ({})",
                    home.display()
                );
            }
        }
        let identity = DirectoryIdentity::read(&directory)?;
        let captured = Self {
            path: path.to_owned(),
            identity,
            _directory: directory,
        };
        captured.verify_marker()?;
        Ok(captured)
    }

    fn verify_marker(&self) -> Result<()> {
        let marker = fsutil::optional_bytes(&self.path.join(MARKER))?
            .and_then(|bytes| serde_json::from_slice::<Marker>(&bytes).ok());
        if !marker.is_some_and(|marker| {
            marker.schema_version == 1
                && self.path.file_name() == Some(marker.directory_name.as_ref())
                && marker.directory_identity == self.identity
        }) {
            bail!(
                "purge refused: cannot verify xswap login staging provenance ({}). This may be an unrelated folder or legacy staging. Inspect it and move it outside the data directory before retrying; remove it manually only if you confirm it is abandoned sign-in data",
                self.path.display()
            );
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn remove(self, root: &Path) -> Result<bool> {
        // Never recursively clean up quarantine on error: it may contain a replacement object.
        let quarantine = fsutil::private_tempdir(root, ".purge-login-")?.keep();
        let quarantine_handle = open_directory(&quarantine)?;
        let quarantine_identity = DirectoryIdentity::read(&quarantine_handle)?;
        let destination = quarantine.join("staging");
        if let Err(error) = fs::rename(&self.path, &destination) {
            fs::remove_dir(&quarantine)?;
            if error.kind() == std::io::ErrorKind::NotFound
                && matches!(fs::symlink_metadata(&self.path), Err(missing) if missing.kind() == std::io::ErrorKind::NotFound)
            {
                return Ok(false);
            }
            return Err(error)
                .with_context(|| format!("quarantine login staging {}", self.path.display()));
        }
        let result = (|| {
            if DirectoryIdentity::read(&open_directory(&quarantine)?)? != quarantine_identity {
                bail!("purge refused: login staging quarantine changed");
            }
            let directory = open_directory(&destination)?;
            if DirectoryIdentity::read(&directory)? != self.identity {
                bail!("purge refused: login staging changed after inspection");
            }
            // Release handles after verification so Windows can finish directory deletion.
            drop(directory);
            drop(self._directory);
            // The original pathname is no longer the deletion target. Advisory leases and a
            // fresh private quarantine protect cooperating xswap operations, not hostile code
            // running as this same OS user and able to mutate the quarantine itself.
            fs::remove_dir_all(&destination).context("remove verified login staging")?;
            drop(quarantine_handle);
            fs::remove_dir(&quarantine)?;
            Ok(true)
        })();
        result.with_context(|| {
            format!(
                "remove login staging {}; quarantined data retained at {} on failure. Inspect that path and recover or remove it manually before retrying",
                self.path.display(),
                destination.display()
            )
        })
    }
}

/// Discover marked staging while Store excludes new stage creation. Prefixes alone are ambiguous.
pub fn directories(store: &Store, protected: &[&Path]) -> Result<Vec<StagingDirectory>> {
    let mut directories = Vec::new();
    for entry in fs::read_dir(&store.root)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(".purge-login-"))
        {
            bail!(
                "purge refused: a previous cleanup quarantine remains ({}). Inspect it and recover or remove its contents manually, or move it outside the data directory before retrying",
                entry.path().display()
            );
        }
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("login-") || name.starts_with("new-login-"))
        {
            let directory = entry.path();
            // An ordinary login may finish and remove its TempDir during enumeration.
            match StagingDirectory::capture(&store.root, &directory, protected) {
                Ok(directory) => directories.push(directory),
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
        assert!(StagingDirectory::capture(root.path(), outside.path(), &[]).is_err());
        assert!(outside.path().exists());
    }

    #[test]
    fn copied_marker_cannot_authorize_a_replacement_directory() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let path = root.join("login-staging");
        fsutil::private_dir(&path).unwrap();
        mark(&path).unwrap();
        let held = open_directory(&path).unwrap();
        let moved = root.join("original");
        fs::rename(&path, &moved).unwrap();
        fsutil::private_dir(&path).unwrap();
        fs::copy(moved.join(MARKER), path.join(MARKER)).unwrap();
        fs::write(path.join("user-data"), b"retained").unwrap();
        assert!(StagingDirectory::capture(&root, &path, &[]).is_err());
        assert!(path.join("user-data").exists());
        drop(held);
    }

    #[test]
    fn replacement_between_capture_and_removal_is_retained_in_quarantine() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let path = root.join("login-staging");
        fsutil::private_dir(&path).unwrap();
        mark(&path).unwrap();
        let captured = StagingDirectory::capture(&root, &path, &[]).unwrap();
        let moved = root.join("original");
        fs::rename(&path, &moved).unwrap();
        fsutil::private_dir(&path).unwrap();
        fs::write(path.join("user-data"), b"retained").unwrap();
        let error = captured.remove(&root).unwrap_err();
        assert!(format!("{error:#}").contains("changed after inspection"));
        assert!(moved.join(MARKER).exists());
        let quarantine = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".purge-login-")
            })
            .unwrap();
        assert_eq!(
            fs::read(quarantine.join("staging/user-data")).unwrap(),
            b"retained"
        );
    }
}
