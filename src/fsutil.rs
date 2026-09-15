use anyhow::{Context, Result, bail};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
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

#[cfg(windows)]
pub use crate::platform::config_user_home;
#[cfg(unix)]
pub use user_home as config_user_home;

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

/// Resolve a Codex config path against its original directory, without touching
/// the filesystem. The base and user home must already be absolute.
/// Matches Codex's `utils/absolute-path` home expansion and lexical resolution;
/// Windows drive-relative and namespace paths need the same platform handling.
pub fn resolve_config_path(path: &Path, base: &Path, home: &Path) -> PathBuf {
    let expanded = match path.to_str().and_then(|path| path.strip_prefix('~')) {
        Some("") => home.to_owned(),
        Some(rest) if rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        Some(rest) if cfg!(windows) && rest.starts_with('\\') => {
            home.join(rest.trim_start_matches('\\'))
        }
        _ => path.to_owned(),
    };
    let joined = config_path_with_base(&expanded, base);
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => (),
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

#[cfg(not(windows))]
fn config_path_with_base(path: &Path, base: &Path) -> PathBuf {
    base.join(path)
}

#[cfg(windows)]
fn config_path_with_base(path: &Path, base: &Path) -> PathBuf {
    let path = normalize_windows_config_path(path);
    let base = normalize_windows_config_path(base);
    if path.is_absolute() || path.has_root() {
        return base.join(path);
    }
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return base.join(path);
    };
    let mut joined = PathBuf::from(prefix.as_os_str());
    if components.clone().next().is_none() {
        joined.push(std::path::MAIN_SEPARATOR_STR);
        return joined;
    }
    let skip_prefix = matches!(base.components().next(), Some(Component::Prefix(_)));
    for component in base
        .components()
        .skip(usize::from(skip_prefix))
        .chain(components)
    {
        joined.push(component.as_os_str());
    }
    joined
}

#[cfg(windows)]
fn normalize_windows_config_path(path: &Path) -> PathBuf {
    if let Some(text) = path.to_str() {
        if let Some(unc) = text
            .strip_prefix(r"\\?\UNC\")
            .or_else(|| text.strip_prefix(r"\\.\UNC\"))
        {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        if let Some(drive) = text
            .strip_prefix(r"\\?\")
            .or_else(|| text.strip_prefix(r"\\.\"))
        {
            let bytes = drive.as_bytes();
            if bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'\\' | b'/')
            {
                return PathBuf::from(drive);
            }
        }
    }
    path.to_owned()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_paths_expand_home_and_normalize_without_creating_directories() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("missing/main");
        let home = root.path().join("missing/user");
        for (value, expected) in [
            ("~", home.clone()),
            ("~/state", home.join("state")),
            (
                "~///state/./missing/../数据库 with spaces",
                home.join("state/数据库 with spaces"),
            ),
            ("~someone/state", base.join("~someone/state")),
            (
                "state/./missing/../数据库 with spaces",
                base.join("state/数据库 with spaces"),
            ),
            ("../state", root.path().join("missing/state")),
            ("", base.clone()),
            (".", base.clone()),
        ] {
            assert_eq!(
                resolve_config_path(Path::new(value), &base, &home),
                expected
            );
        }
        let absolute = root.path().join("absolute/missing/../state");
        assert_eq!(
            resolve_config_path(&absolute, &base, &home),
            root.path().join("absolute/state")
        );
        assert!(!root.path().join("missing").exists());
        assert!(!root.path().join("absolute").exists());
    }

    #[cfg(unix)]
    #[test]
    fn config_paths_preserve_symlinks_and_unix_backslashes() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("main");
        let target = root.path().join("target");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &base).unwrap();
        assert_eq!(
            resolve_config_path(Path::new("state"), &base, root.path()),
            base.join("state")
        );
        assert_eq!(
            resolve_config_path(Path::new(r"~\state"), &base, root.path()),
            base.join(r"~\state")
        );
        assert_eq!(
            resolve_config_path(Path::new("../../state"), Path::new("/"), root.path()),
            PathBuf::from("/state")
        );
    }

    #[cfg(windows)]
    #[test]
    fn config_home_uses_native_profile_when_userprofile_differs() {
        if let Some(expected) = std::env::var_os("XSWAP_TEST_NATIVE_CONFIG_HOME") {
            let home = config_user_home().unwrap();
            assert_eq!(home, PathBuf::from(expected));
            assert_ne!(home, user_home().unwrap());
            assert_eq!(resolve_config_path(Path::new("~"), &home, &home), home);
            return;
        }
        let home = config_user_home().unwrap();
        let spoofed = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "fsutil::tests::config_home_uses_native_profile_when_userprofile_differs",
                "--exact",
            ])
            .env("XSWAP_TEST_NATIVE_CONFIG_HOME", home)
            .env("USERPROFILE", spoofed.path())
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(windows)]
    #[test]
    fn config_paths_follow_windows_home_drive_and_namespace_semantics() {
        let base = Path::new(r"\\?\C:\base\cwd");
        let home = Path::new(r"C:\Users\fixture");
        for (value, expected) in [
            (
                r"~\\state\missing\..\数据库 with spaces",
                r"C:\Users\fixture\state\数据库 with spaces",
            ),
            (r"\state", r"C:\state"),
            (r"D:state", r"D:\base\cwd\state"),
            (r"D:", r"D:\"),
            (r"state\missing\..\final", r"C:\base\cwd\state\final"),
            (r"\\?\D:\missing\..\state", r"D:\state"),
            (r"\\.\D:\missing\..\state", r"D:\state"),
            (
                r"\\?\UNC\server\share\missing\..\state",
                r"\\server\share\state",
            ),
            (
                r"\\.\UNC\server\share\missing\..\state",
                r"\\server\share\state",
            ),
        ] {
            assert_eq!(
                resolve_config_path(Path::new(value), base, home),
                PathBuf::from(expected)
            );
        }
    }
}
