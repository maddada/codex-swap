//! Read the portions of Codex's user/project/session layers needed for asset links.
use super::fsutil;
use anyhow::{Context, Result};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

pub(super) fn cwd(args: &[OsString], current: &Path) -> PathBuf {
    let mut directory = None;
    let mut command = None;
    let mut debug_command = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--" {
            break;
        }
        let arg = args[index].to_str().unwrap_or("");
        if arg == "--cd" || arg == "-C" {
            index += 1;
            directory = args.get(index).map(PathBuf::from);
        } else if let Some(path) = arg.strip_prefix("--cd=").or_else(|| arg.strip_prefix("-C")) {
            directory = Some(PathBuf::from(path.strip_prefix('=').unwrap_or(path)));
        } else if [
            "-c",
            "--config",
            "-p",
            "--profile",
            "-m",
            "--model",
            "-s",
            "--sandbox",
            "-a",
            "--ask-for-approval",
            "--local-provider",
            "--remote",
            "--remote-auth-token-env",
            "--enable",
            "--disable",
        ]
        .contains(&arg)
        {
            index += 1;
        } else if arg == "-i" || arg == "--image" {
            while args
                .get(index + 1)
                .and_then(|arg| arg.to_str())
                .is_some_and(|arg| !arg.starts_with('-'))
            {
                index += 1;
            }
        } else if !arg.is_empty() && !arg.starts_with('-') {
            if command.is_none() {
                command = Some(arg);
            } else if command == Some("debug") && debug_command.is_none() {
                debug_command = Some(arg);
            }
        }
        index += 1;
    }
    // main.rs forwards root cwd only to runtime commands. MCP/features use a
    // default ConfigBuilder at the process cwd, even with a root --cd option.
    let ignored = matches!(
        command,
        Some(
            "mcp"
                | "features"
                | "plugin"
                | "login"
                | "logout"
                | "cloud"
                | "app-server"
                | "completion"
                | "exec-server"
                | "help"
        )
    ) || (command == Some("debug") && debug_command != Some("prompt-input"));
    if ignored {
        current.to_owned()
    } else {
        directory
            .map(|path| current.join(path))
            .unwrap_or_else(|| current.to_owned())
    }
}

pub(super) fn merge(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                if let Some(existing) = base.get_mut(&key) {
                    merge(existing, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

pub(super) fn cli_overrides(args: &[OsString]) -> toml::Value {
    let mut root = toml::Value::Table(Default::default());
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            break;
        }
        let arg = arg.to_str().unwrap_or("");
        let raw = if arg == "-c" || arg == "--config" {
            index += 1;
            args.get(index).and_then(|value| value.to_str())
        } else {
            arg.strip_prefix("--config=").or_else(|| {
                arg.strip_prefix("-c")
                    .map(|value| value.strip_prefix('=').unwrap_or(value))
            })
        };
        index += 1;
        let Some((key, raw)) = raw.and_then(|raw| raw.split_once('=')) else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        // Match utils/cli/config_override.rs: scalar TOML, then trimmed raw string.
        let value = toml::from_str::<toml::Table>(&format!("_x_ = {}", raw.trim()))
            .ok()
            .and_then(|mut table| table.remove("_x_"))
            .unwrap_or_else(|| {
                toml::Value::String(raw.trim().trim_matches(['\'', '"']).to_owned())
            });
        let mut current = &mut root;
        let mut segments = key.split('.').peekable();
        while let Some(segment) = segments.next() {
            if !current.is_table() {
                *current = toml::Value::Table(Default::default());
            }
            let table = current.as_table_mut().expect("override table");
            if segments.peek().is_none() {
                table.insert(segment.to_owned(), value.clone());
                break;
            }
            current = table
                .entry(segment.to_owned())
                .or_insert_with(|| toml::Value::Table(Default::default()));
        }
    }
    root
}

fn trust(config: &toml::Value, directory: &Path) -> Option<bool> {
    let projects = config.get("projects")?.as_table()?;
    // Native trust lookup prefers canonical, then original spelling. Only Windows
    // folds case in trust-map keys; filesystem runtime-alias checks are separate.
    for key in directory
        .canonicalize()
        .ok()
        .into_iter()
        .chain(std::iter::once(directory.to_owned()))
    {
        let key = key.to_string_lossy();
        let entry = projects.get(key.as_ref()).or_else(|| {
            if cfg!(windows) {
                projects
                    .iter()
                    .find(|(candidate, _)| candidate.eq_ignore_ascii_case(&key))
                    .map(|(_, value)| value)
            } else {
                None
            }
        });
        if let Some(level) = entry
            .and_then(|entry| entry.get("trust_level"))
            .and_then(toml::Value::as_str)
        {
            return Some(level == "trusted");
        }
    }
    None
}

fn git_marker(directory: &Path) -> bool {
    let git = directory.join(".git");
    git.exists() && (!git.is_dir() || git.join("HEAD").exists())
}

fn metadata_path(file: &Path, prefix: &str) -> Option<PathBuf> {
    let metadata = fs::symlink_metadata(file).ok()?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return None;
    }
    let bytes = fs::read(file).ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let contents = std::str::from_utf8(&bytes)
        .ok()?
        .trim()
        .strip_prefix(prefix)?
        .trim();
    if contents.is_empty() {
        return None;
    }
    Some(file.parent()?.join(contents))
}

// Port the bounded filesystem checks in git-utils/trust.rs. Do not use `git`
// commands or inherited GIT_* variables to decide which project config is trusted.
fn git_trust_root(cwd: &Path) -> Option<PathBuf> {
    let checkout = cwd.ancestors().find(|directory| git_marker(directory))?;
    let dot_git = checkout.join(".git");
    if dot_git.is_dir() {
        return Some(checkout.to_owned());
    }
    let git_dir = metadata_path(&dot_git, "gitdir:")?;
    if !fs::symlink_metadata(&git_dir).ok()?.is_dir() {
        return None;
    }
    let canonical_git_dir = git_dir.canonicalize().ok()?;
    let worktrees = canonical_git_dir.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let common = worktrees.parent()?;
    let registered = metadata_path(&canonical_git_dir.join("gitdir"), "")?;
    if registered.file_name()? != ".git"
        || registered.parent()?.canonicalize().ok()? != checkout.canonicalize().ok()?
    {
        return None;
    }
    if metadata_path(&canonical_git_dir.join("commondir"), "")?
        .canonicalize()
        .ok()?
        != common
    {
        return None;
    }
    let main = git_dir.parent()?.parent()?.parent()?;
    let main_git = main.join(".git");
    let owned_git = if main_git.is_dir() {
        main_git
    } else {
        metadata_path(&main_git, "gitdir:")?
    };
    (owned_git.canonicalize().ok()? == common).then(|| main.to_owned())
}

pub(super) fn project_configs(
    config: &toml::Value,
    cwd: &Path,
    home: &Path,
) -> Result<Vec<PathBuf>> {
    let default = vec![toml::Value::String(".git".to_owned())];
    let markers = config
        .get("project_root_markers")
        .map(|value| {
            value
                .as_array()
                .context("project_root_markers must be an array of strings")
        })
        .transpose()?
        .unwrap_or(&default);
    let markers: Vec<_> = markers
        .iter()
        .map(|value| {
            value
                .as_str()
                .context("project_root_markers must be strings")
        })
        .collect::<Result<_>>()?;
    let root = cwd
        .ancestors()
        .find(|directory| {
            markers.iter().any(|marker| {
                if *marker == ".git" {
                    git_marker(directory)
                } else {
                    directory.join(marker).exists()
                }
            })
        })
        .unwrap_or(cwd);
    let git_root = git_trust_root(cwd);
    let mut directories: Vec<_> = cwd
        .ancestors()
        .take_while(|directory| directory.starts_with(root))
        .collect();
    directories.reverse();
    let mut configs = Vec::new();
    for directory in directories {
        let enabled = trust(config, directory)
            .or_else(|| trust(config, root))
            .or_else(|| git_root.as_deref().and_then(|root| trust(config, root)))
            .unwrap_or(false);
        let file = directory.join(".codex/config.toml");
        if enabled
            && file.exists()
            && fsutil::absolute(file.parent().context("project config parent")?)?
                != fsutil::absolute(home)?
        {
            configs.push(file);
        }
    }
    Ok(configs)
}
