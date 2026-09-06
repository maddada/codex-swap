use anyhow::{Context, Result, bail};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

pub fn user_home() -> Result<PathBuf> {
    #[cfg(unix)]
    let variable = "HOME";
    #[cfg(windows)]
    let variable = "USERPROFILE";
    let home = std::env::var_os(variable)
        .map(PathBuf::from)
        .with_context(|| format!("{variable} is not set"))?;
    if !home.is_absolute() {
        bail!("{variable} must be an absolute path");
    }
    Ok(home)
}

pub fn default_data_dir(home: &Path) -> Result<PathBuf> {
    #[cfg(unix)]
    let root = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"));
    #[cfg(windows)]
    let root = {
        let _ = home;
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .context("LOCALAPPDATA must be an absolute path")?
    };
    Ok(root.join("codex-swap"))
}

pub fn absolute(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    if path.exists() {
        return path.canonicalize().context("resolve directory");
    }
    let parent = path.parent().context("directory has no parent")?;
    Ok(absolute(parent)?.join(path.file_name().context("directory has no name")?))
}

pub fn private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            bail!(
                "{} must be a real directory owned by you with mode 0700",
                path.display()
            );
        }
    } else {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    #[cfg(windows)]
    {
        match fs::symlink_metadata(path) {
            Ok(meta) if !meta.is_dir() || is_reparse_point(&meta) => {
                bail!("{} must be a real directory", path.display())
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(path)?;
                crate::platform::own_new(path)?;
            }
            Err(e) => return Err(e.into()),
        }
        crate::platform::private_permissions(path, true)?;
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
        != 0
}

pub fn regular(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    #[cfg(unix)]
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
        bail!("{} must be a regular file owned by you", path.display());
    }
    #[cfg(windows)]
    {
        if !meta.is_file() || is_reparse_point(&meta) {
            bail!("{} must be a regular file", path.display());
        }
        crate::platform::owned(path)?;
    }
    Ok(())
}

pub fn atomic_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        regular(path)?;
    }
    write_json(path, value, false)
}

/// Creates a private JSON file without replacing an existing backup.
pub fn create_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    write_json(path, value, true)
}

fn write_json(path: &Path, value: &impl serde::Serialize, no_clobber: bool) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    #[cfg(windows)]
    {
        crate::platform::own_new(temp.path())?;
        crate::platform::private_permissions(temp.path(), false)?;
    }
    serde_json::to_writer_pretty(&mut temp, value)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    if no_clobber {
        temp.persist_noclobber(path)
            .context("create private JSON file; destination must not exist")?;
    } else {
        temp.persist(path).context("commit private JSON file")?;
    }
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    #[cfg(windows)]
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?
        .sync_all()?;
    Ok(())
}

pub fn lock(path: &Path, exclusive: bool, wait: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    let file = options.open(path)?;
    #[cfg(windows)]
    let file = match options.create_new(true).open(path) {
        Ok(file) => {
            crate::platform::own_new(path)?;
            file
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            options.create_new(false).create(false).open(path)?
        }
        Err(e) => return Err(e.into()),
    };
    regular(path)?;
    #[cfg(windows)]
    crate::platform::private_permissions(path, false)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let result = if exclusive {
            file.try_lock_exclusive()
        } else {
            FileExt::try_lock_shared(&file)
        };
        match result {
            Ok(()) => return Ok(file),
            Err(err)
                if wait
                    && err.raw_os_error() == fs2::lock_contended_error().raw_os_error()
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(err) => {
                return Err(err).context(
                    "account or registry is busy; finish its current operation and try again",
                );
            }
        }
    }
}

pub fn optional_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            regular(path)?;
            Ok(Some(fs::read(path)?))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub fn private_tempdir(parent: &Path, prefix: &str) -> Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    builder.permissions(fs::Permissions::from_mode(0o700));
    let dir = builder.tempdir_in(parent)?;
    #[cfg(windows)]
    crate::platform::own_new(dir.path())?;
    private_dir(dir.path())?;
    Ok(dir)
}
