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

mod controlled;
mod layers;
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
    "log",
    "logs",
    "tmp",
    "shell_snapshots",
];

fn case_insensitive(home: &Path) -> Result<bool> {
    // Query the account filesystem, rather than assuming macOS/Windows volume
    // defaults. The unique private probe never uses a runtime filename.
    let probe = tempfile::Builder::new()
        .prefix(".xswap-case-")
        .tempfile_in(home)?;
    let name = probe
        .path()
        .file_name()
        .context("case probe name")?
        .to_string_lossy();
    Ok(home
        .join(name.replacen(".xswap-case-", ".XSWAP-CASE-", 1))
        .exists())
}

fn path_prefix(path: &Path, prefix: &Path, insensitive: bool) -> bool {
    let mut components = path.components();
    prefix.components().all(|expected| {
        components.next().is_some_and(|actual| {
            actual == expected
                || (insensitive
                    && actual
                        .as_os_str()
                        .as_encoded_bytes()
                        .eq_ignore_ascii_case(expected.as_os_str().as_encoded_bytes()))
        })
    })
}

fn uncertain_unicode_alias(path: &Path, home: &Path, roots: &[PathBuf], insensitive: bool) -> bool {
    let non_ascii = |path: &Path| {
        path.strip_prefix(home)
            .unwrap_or(path)
            .components()
            .any(|part| !part.as_os_str().as_encoded_bytes().is_ascii())
    };
    insensitive && (non_ascii(path) || roots.iter().any(|root| non_ascii(root)))
}

fn runtime_path(path: &Path, home: &Path, roots: &[PathBuf], insensitive: bool) -> bool {
    let sqlite = path.parent() == Some(home)
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                let name = if insensitive {
                    name.to_ascii_lowercase()
                } else {
                    name.to_owned()
                };
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
    sqlite
        || roots.iter().any(|root| {
            path_prefix(path, root, insensitive) || path_prefix(root, path, insensitive)
        })
}

fn unresolved_runtime_alias(path: &Path) -> bool {
    path.ancestors()
        .any(|ancestor| fs::read_link(ancestor).is_ok() && !ancestor.exists())
}

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
            // Follow regular config symlinks, but never open an unused FIFO,
            // device or directory merely because its filename looks like a profile.
            && fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_file())
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
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
    case_insensitive: bool,
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
        let physical_destination = fsutil::absolute(link_destination)?;
        if fsutil::absolute(link_source)? != physical_destination {
            if !link_destination.starts_with(self.home) || link_destination == self.home {
                bail!(
                    "cannot preserve {field} reference {reference:?} from {} inside managed home {}; use an absolute path, or put agent role files in a shared subdirectory such as agents/",
                    source_config.display(),
                    self.home.display()
                );
            }
            let physical_destination =
                fsutil::resolve_config_path(&physical_destination, self.home, &self.user_home);
            if !physical_destination.starts_with(self.home) || physical_destination == self.home {
                bail!(
                    "{} resolves outside managed home {}; refusing to write through that link. Use an absolute config reference",
                    link_destination.display(),
                    self.home.display()
                );
            }
            let mut runtime_roots = self.runtime_roots.clone();
            for root in &self.runtime_roots {
                if unresolved_runtime_alias(root) {
                    bail!(
                        "cannot safely link {field} reference {reference:?}: account runtime path {} has an unresolved symlink alias; use an absolute asset reference or resolve the runtime alias before creating shared asset links",
                        root.display()
                    );
                }
                let physical = fsutil::absolute(root)?;
                runtime_roots.push(fsutil::resolve_config_path(
                    &physical,
                    self.home,
                    &self.user_home,
                ));
            }
            // Native Unicode case aliases can exist even before the destination
            // is created. Avoid guessing their fold with an ASCII comparison.
            if uncertain_unicode_alias(
                link_destination,
                self.home,
                &runtime_roots,
                self.case_insensitive,
            ) || uncertain_unicode_alias(
                &physical_destination,
                self.home,
                &runtime_roots,
                self.case_insensitive,
            ) {
                bail!(
                    "cannot safely link {field} reference {reference:?} from {}: non-ASCII path components can alias account runtime paths on this case-insensitive filesystem; use an absolute reference",
                    source_config.display()
                );
            }
            if runtime_path(
                link_destination,
                self.home,
                &runtime_roots,
                self.case_insensitive,
            ) || runtime_path(
                &physical_destination,
                self.home,
                &runtime_roots,
                self.case_insensitive,
            ) {
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
    share_at(source_home, home, args, &std::env::current_dir()?)
}

fn share_at(source_home: &Path, home: &Path, args: &[OsString], cwd: &Path) -> Result<()> {
    let invocation = layers::invocation(args, cwd);
    if !invocation.loads_user_config {
        return Ok(());
    }
    let cwd = &invocation.cwd;
    let user_home = fsutil::config_user_home()?;
    // Windows canonical paths may carry a namespace prefix that Codex strips.
    let home = fsutil::resolve_config_path(Path::new("."), home, &user_home);
    let profile = invocation.profile;
    let mut configs = vec![source_home.join("config.toml")];
    if let Some(profile) = profile {
        configs.push(source_home.join(format!("{profile}.config.toml")));
    }
    let mut assets = BTreeMap::new();
    let mut runtime_settings = BTreeMap::new();
    let mut discovery = toml::Value::Table(Default::default());
    for config in configs.into_iter().filter(|path| path.exists()) {
        let mut value = read_config(&config)?;
        for key in ["log_dir", "sqlite_home"] {
            if let Some(path) = value.get(key).and_then(toml::Value::as_str) {
                runtime_settings.insert(key, (path.to_owned(), home.clone()));
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
        layers::merge(&mut discovery, value);
    }
    let mut cli = invocation.overrides;
    layers::merge(&mut discovery, cli.clone());
    visit_paths(&mut cli, &mut |field, _, _| {
        assets.remove(field);
        Ok(())
    })?;
    if assets.values().any(|(config, reference, _)| {
        let source_base = config.parent().unwrap_or(source_home);
        fsutil::resolve_config_path(Path::new(reference), source_base, &user_home)
            != fsutil::resolve_config_path(Path::new(reference), &home, &user_home)
    }) {
        controlled::guard_relocation()?;
    }
    // Project paths keep their native project-layer base. Only user references
    // that survive the higher layers need relocation into the account home.
    for project in layers::project_configs(&discovery, cwd, &home)? {
        let mut value = read_config(&project)?;
        for key in ["log_dir", "sqlite_home"] {
            if let Some(path) = value.get(key).and_then(toml::Value::as_str) {
                runtime_settings.insert(
                    key,
                    (
                        path.to_owned(),
                        project
                            .parent()
                            .context("project config parent")?
                            .to_owned(),
                    ),
                );
            }
        }
        visit_paths(&mut value, &mut |field, _, _| {
            assets.remove(field);
            Ok(())
        })?;
    }
    for key in ["log_dir", "sqlite_home"] {
        if let Some(path) = cli.get(key).and_then(toml::Value::as_str) {
            runtime_settings.insert(key, (path.to_owned(), cwd.to_owned()));
        }
    }
    if assets.is_empty() {
        return Ok(());
    }
    let mut runtime_roots: Vec<_> = RUNTIME_ITEMS.iter().map(|name| home.join(name)).collect();
    for (reference, base) in runtime_settings.values() {
        let root = fsutil::resolve_config_path(Path::new(reference), base, &user_home);
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
        case_insensitive: case_insensitive(&home)?,
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
