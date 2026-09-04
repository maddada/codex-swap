use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::ErrorKind,
    os::unix::fs::{OpenOptionsExt, symlink},
    path::Path,
};

const CONFIG_ITEMS: &[&str] = &[
    "config.toml",
    "AGENTS.md",
    "AGENTS.override.md",
    "skills",
    "hooks",
    "hooks.json",
    "rules",
    "agents",
];
const HISTORY_DIRS: &[&str] = &["sessions", "archived_sessions", "thread-writer-locks"];
const HISTORY_FILES: &[&str] = &["history.jsonl", "session_index.jsonl"];

fn link(source: &Path, dest: &Path) -> Result<()> {
    match fs::symlink_metadata(dest) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let target = fs::read_link(dest)?;
            // Resolve relative links, including existing links from hand-made profiles.
            let target = if target.is_absolute() {
                target
            } else {
                dest.parent().context("link parent")?.join(target)
            };
            if crate::fsutil::absolute(&target)? != crate::fsutil::absolute(source)? {
                bail!(
                    "{} already links elsewhere; refusing to replace it",
                    dest.display()
                );
            }
        }
        Ok(_) => bail!(
            "{} contains private data; sharing requires an empty destination. Preserve/merge it into {} first",
            dest.display(),
            source.display()
        ),
        Err(err) if err.kind() == ErrorKind::NotFound => symlink(source, dest)?,
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

pub fn config(main: &Path, home: &Path) -> Result<()> {
    // Establish these links before Codex can create independent defaults in a new home.
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(main.join("config.toml"))?;
    for name in ["skills", "hooks", "rules", "agents"] {
        fs::create_dir_all(main.join(name))?;
    }
    for item in CONFIG_ITEMS {
        let source = main.join(item);
        if source.exists() {
            link(&source, &home.join(item))?;
        }
    }
    // Named Codex configuration profiles are files next to config.toml.
    for entry in fs::read_dir(main)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.ends_with(".config.toml"))
        {
            link(&entry.path(), &home.join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// CDXC:AgentProviders 2026-09-05 DECISION:
/// Accounts share conversations so the user can resume or fork the same Codex session under another login, matching their cswap --share-history wrappers.
/// SQLite uses one canonical directory via the launch override, not individual database symlinks, so WAL and SHM files stay together.
/// Codex's thread-writer-locks directory is shared too, preventing two account homes from independently owning the same conversation writer.
pub fn history(main: &Path, home: &Path) -> Result<()> {
    if main == home {
        return Ok(());
    }
    // Check every destination before creating links, avoiding a half-shared profile on ordinary conflicts.
    for name in HISTORY_DIRS.iter().chain(HISTORY_FILES) {
        let dest = home.join(name);
        if let Ok(meta) = fs::symlink_metadata(&dest) {
            if !meta.file_type().is_symlink() {
                bail!(
                    "{} already contains private history; merge/preserve it before enabling shared history",
                    dest.display()
                );
            }
            let target = fs::read_link(&dest)?;
            let target = if target.is_absolute() {
                target
            } else {
                home.join(target)
            };
            if crate::fsutil::absolute(&target)? != crate::fsutil::absolute(&main.join(name))? {
                bail!("{} links to different history", dest.display());
            }
        }
    }
    for name in HISTORY_DIRS {
        let source = main.join(name);
        fs::create_dir_all(&source)?;
        link(&source, &home.join(name))?;
    }
    for name in HISTORY_FILES {
        let source = main.join(name);
        // Create without truncating: Codex may be appending to the shared index right now.
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&source)?;
        link(&source, &home.join(name))?;
    }
    Ok(())
}

pub fn sqlite_home(main: &Path) -> Result<std::path::PathBuf> {
    let config = main.join("config.toml");
    let text = match fs::read_to_string(&config) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let config: toml::Value = toml::from_str(&text)
        .map_err(|_| anyhow::anyhow!("invalid main Codex config.toml (contents omitted)"))?;
    if let Some(value) = config.get("sqlite_home") {
        let path = Path::new(
            value
                .as_str()
                .context("sqlite_home must be a path string")?,
        );
        return crate::fsutil::absolute(&if path.is_absolute() {
            path.to_owned()
        } else {
            main.join(path)
        });
    }
    Ok(main.to_owned())
}
