use anyhow::{Context, Result, bail};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

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
    Ok(())
}

pub fn regular(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
        bail!("{} must be a regular file owned by you", path.display());
    }
    Ok(())
}

pub fn atomic_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        regular(path)?;
    }
    let parent = path.parent().context("file has no parent")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer_pretty(&mut temp, value)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist(path).context("commit account registry")?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn lock(path: &Path, exclusive: bool, wait: bool) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    regular(path)?;
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
                    && err.kind() == std::io::ErrorKind::WouldBlock
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
