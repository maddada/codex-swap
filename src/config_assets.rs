//! Preserve the source base of files referenced by shared or copied user config.
use crate::{fsutil, sharing};
use anyhow::{Context, Result, bail};
use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[cfg(test)]
mod tests;

const FILE_SETTINGS: &[&str] = &[
    "model_instructions_file",
    "model_catalog_json",
    "experimental_compact_prompt_file",
];
const RUNTIME_ITEMS: &[&str] = &[
    "auth.json",
    "sessions",
    "archived_sessions",
    "thread-writer-locks",
    "history.jsonl",
    "session_index.jsonl",
    "logs",
    "tmp",
    "shell_snapshots",
];

fn configs(home: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    if home.join("config.toml").exists() {
        paths.push(home.join("config.toml"));
    }
    let entries = match fs::read_dir(home) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(paths),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(".config.toml"))
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn selected_profile(args: &[OsString]) -> Option<&str> {
    let mut selected = None;
    for (index, arg) in args.iter().enumerate() {
        if arg == "--" {
            break;
        }
        let Some(arg) = arg.to_str() else {
            continue;
        };
        let name = if arg == "--profile" || arg == "-p" {
            args.get(index + 1)?.to_str()?
        } else if let Some(name) = arg.strip_prefix("--profile=") {
            name
        } else if let Some(name) = arg.strip_prefix("-p") {
            name.strip_prefix('=').unwrap_or(name)
        } else {
            continue;
        };
        // Leave invalid names to Codex, without turning them into filesystem paths.
        selected = (!name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
        .then_some(name);
    }
    // Commands with their own shared options override the root profile option.
    selected
}

fn read_config(path: &Path) -> Result<toml::Value> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("read source Codex config {}", path.display()))?;
    toml::from_str(&contents)
        .map_err(|_| anyhow::anyhow!("invalid Codex config {} (contents omitted)", path.display()))
}

fn visit_paths(
    config: &mut toml::Value,
    visit: &mut impl FnMut(&str, &mut toml::Value, bool) -> Result<()>,
) -> Result<()> {
    // Role directories come first, so their other assets can reuse the directory link.
    if let Some(roles) = config.get_mut("agents").and_then(toml::Value::as_table_mut) {
        for (name, role) in roles {
            if let Some(path) = role.get_mut("config_file") {
                visit(&format!("agents.{name}.config_file"), path, true)?;
            }
        }
    }
    for key in FILE_SETTINGS {
        if let Some(path) = config.get_mut(key) {
            visit(key, path, false)?;
        }
    }
    Ok(())
}

struct AssetLink {
    source: PathBuf,
    destination: PathBuf,
    directory: bool,
}

struct SharedAssets<'a> {
    home: &'a Path,
    user_home: PathBuf,
    visited: HashSet<(PathBuf, PathBuf)>,
    links: Vec<AssetLink>,
    runtime_roots: Vec<PathBuf>,
}

impl SharedAssets<'_> {
    fn collect_config(&mut self, source_config: &Path, destination_config: &Path) -> Result<()> {
        if !self
            .visited
            .insert((source_config.to_owned(), destination_config.to_owned()))
        {
            return Ok(());
        }
        // Codex may shadow this role with a project layer. Leave missing/invalid
        // role contents to its effective-config validation, just like file assets.
        let Ok(mut config) = read_config(source_config) else {
            return Ok(());
        };
        visit_paths(&mut config, &mut |field, value, role| {
            if let Some(reference) = value.as_str() {
                self.collect_asset(source_config, destination_config, field, reference, role)?;
            }
            Ok(())
        })
    }

    fn collect_asset(
        &mut self,
        source_config: &Path,
        destination_config: &Path,
        field: &str,
        reference: &str,
        role: bool,
    ) -> Result<()> {
        let source_base = source_config.parent().context("source config parent")?;
        let destination_base = destination_config
            .parent()
            .context("shared config parent")?;
        let source =
            fsutil::resolve_config_path(Path::new(reference), source_base, &self.user_home);
        let destination =
            fsutil::resolve_config_path(Path::new(reference), destination_base, &self.user_home);
        // Absolute and home-relative values already retain their original base.
        if source == destination {
            return Ok(());
        }
        let (link_source, link_destination) = if role {
            (
                source.parent().context("role config parent")?,
                destination.parent().context("shared role config parent")?,
            )
        } else {
            (source.as_path(), destination.as_path())
        };
        if fsutil::absolute(link_source)? != fsutil::absolute(link_destination)? {
            if !link_destination.starts_with(self.home) || link_destination == self.home {
                bail!(
                    "cannot preserve {field} reference {reference:?} from {} inside managed home {}; use an absolute path, or put agent role files in a shared subdirectory such as agents/",
                    source_config.display(),
                    self.home.display()
                );
            }
            let sqlite_file = link_destination.parent() == Some(self.home)
                && link_destination
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        [
                            "state_",
                            "logs_",
                            "goals_",
                            "memories_",
                            "queue_",
                            "thread_history_",
                        ]
                        .iter()
                        .any(|prefix| name.starts_with(prefix))
                            && [".sqlite", ".sqlite-wal", ".sqlite-shm", ".sqlite-journal"]
                                .iter()
                                .any(|suffix| name.ends_with(suffix))
                    });
            if sqlite_file
                || self.runtime_roots.iter().any(|root| {
                    link_destination.starts_with(root) || root.starts_with(link_destination)
                })
            {
                bail!(
                    "cannot link {field} reference {reference:?} from {}: {} is an account runtime path; use an absolute reference to preserve private credentials, logs and history",
                    source_config.display(),
                    link_destination.display()
                );
            }
            self.links.push(AssetLink {
                source: link_source.to_owned(),
                destination: link_destination.to_owned(),
                directory: role,
            });
        }
        if role {
            self.collect_config(&source, &destination)?;
        }
        Ok(())
    }

    fn link(mut self) -> Result<()> {
        // Link containing role directories before individual files, across all profiles.
        self.links
            .sort_by_key(|link| (!link.directory, link.destination.components().count()));
        for link in self.links {
            if fsutil::absolute(&link.source)? == fsutil::absolute(&link.destination)? {
                continue;
            }
            let parent = link.destination.parent().context("asset link parent")?;
            let physical_parent = fsutil::absolute(parent)?;
            let physical_parent =
                fsutil::resolve_config_path(&physical_parent, self.home, &self.user_home);
            if !physical_parent.starts_with(self.home) {
                bail!(
                    "{} resolves outside managed home {}; refusing to write through that link. Use an absolute config reference",
                    link.destination.display(),
                    self.home.display()
                );
            }
            if !parent.exists() {
                fsutil::private_dir(parent)?;
            }
            sharing::link_with_kind(&link.source, &link.destination, link.directory)?;
        }
        Ok(())
    }
}

/// Keep the config symlinks editable and let Codex retain its normal layer precedence.
pub fn share(source_home: &Path, home: &Path, args: &[OsString]) -> Result<()> {
    let user_home = fsutil::config_user_home()?;
    // Windows canonical paths may carry a namespace prefix that Codex strips.
    let home = fsutil::resolve_config_path(Path::new("."), home, &user_home);
    let profile = selected_profile(args);
    let mut configs = vec![source_home.join("config.toml")];
    if let Some(profile) = profile {
        configs.push(source_home.join(format!("{profile}.config.toml")));
    }
    let mut assets = BTreeMap::new();
    let mut runtime_settings = BTreeMap::new();
    for config in configs.into_iter().filter(|path| path.exists()) {
        let mut value = read_config(&config)?;
        for key in ["log_dir", "sqlite_home"] {
            if let Some(path) = value.get(key).and_then(toml::Value::as_str) {
                runtime_settings.insert(key, path.to_owned());
            }
        }
        visit_paths(&mut value, &mut |field, value, role| {
            if let Some(reference) = value.as_str() {
                // A named profile overrides the same base-config path key.
                assets.insert(
                    field.to_owned(),
                    (config.clone(), reference.to_owned(), role),
                );
            }
            Ok(())
        })?;
    }
    let mut runtime_roots: Vec<_> = RUNTIME_ITEMS.iter().map(|name| home.join(name)).collect();
    for reference in runtime_settings.values() {
        let root = fsutil::resolve_config_path(Path::new(reference), &home, &user_home);
        // SQLite/log files in the home root do not own its config subdirectories.
        if root != home {
            runtime_roots.push(root);
        }
    }
    let mut shared = SharedAssets {
        home: &home,
        user_home,
        visited: HashSet::new(),
        links: Vec::new(),
        runtime_roots,
    };
    for (field, (config, reference, role)) in assets {
        let destination = home.join(config.file_name().context("config file name")?);
        shared.collect_asset(&config, &destination, &field, &reference, role)?;
    }
    shared.link()
}

/// Copy user configs independently; referenced role files retain their own source base.
pub fn copy_for_login(home: &Path, shared_from: Option<&Path>, staging: &Path) -> Result<()> {
    let source_home = shared_from.unwrap_or(home);
    let user_home = fsutil::config_user_home()?;
    if source_home != home {
        sharing::check_link(&source_home.join("config.toml"), &home.join("config.toml"))
            .context("divergent config; preserve/merge shared settings before login")?;
    }
    for source in configs(source_home)? {
        let name = source.file_name().context("config file name")?;
        let bytes = match fs::read(&source) {
            Ok(bytes) => bytes,
            // Login never selects named profiles; unrelated dangling links,
            // unreadable files or directories must not block its base config.
            Err(_) if name != "config.toml" => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read source login config {}", source.display()));
            }
        };
        if source_home != home {
            sharing::check_link(&source, &home.join(name))
                .context("divergent config; preserve/merge shared settings before login")?;
        }
        let parsed = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| toml::from_str::<toml::Value>(text).ok());
        let mut config = match parsed {
            Some(config) => config,
            // Native login does not select named profiles. Keep an unused invalid
            // profile independent too, without making it break base-config login.
            None if name != "config.toml" => {
                let mut copy = tempfile::NamedTempFile::new_in(staging)?;
                copy.write_all(&bytes)?;
                copy.persist_noclobber(staging.join(name))?;
                continue;
            }
            None => bail!(
                "invalid Codex config {} (contents omitted)",
                source.display()
            ),
        };
        let base = source.parent().context("source config parent")?;
        visit_paths(&mut config, &mut |_, value, _| {
            if let Some(reference) = value.as_str() {
                let resolved = fsutil::resolve_config_path(Path::new(reference), base, &user_home);
                *value = toml::Value::String(resolved.to_string_lossy().into_owned());
            }
            Ok(())
        })?;
        let mut copy = tempfile::NamedTempFile::new_in(staging)?;
        copy.write_all(toml::to_string(&config)?.as_bytes())?;
        copy.persist_noclobber(staging.join(name))
            .context("copy login config; destination must not exist")?;
    }
    Ok(())
}
