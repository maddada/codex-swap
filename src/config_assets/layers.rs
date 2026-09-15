//! Read the portions of Codex's user/project/session layers needed for asset links.
use super::fsutil;
use anyhow::{Context, Result};
use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
};

pub(super) struct Invocation<'a> {
    pub cwd: PathBuf,
    pub profile: Option<&'a str>,
    pub overrides: toml::Value,
    pub loads_user_config: bool,
}

#[derive(Clone, Copy)]
enum ValueOption {
    Config,
    Profile,
    Cwd,
    Image,
    Other,
}

// Advertised root value options in SharedCliOptions, TuiCli,
// CliConfigOverrides, FeatureToggles and InteractiveRemoteOptions.
const VALUE_OPTIONS: &[(&str, &str, ValueOption)] = &[
    ("--config", "-c", ValueOption::Config),
    ("--profile", "-p", ValueOption::Profile),
    ("--cd", "-C", ValueOption::Cwd),
    ("--image", "-i", ValueOption::Image),
    ("--model", "-m", ValueOption::Other),
    ("--sandbox", "-s", ValueOption::Other),
    ("--ask-for-approval", "-a", ValueOption::Other),
    ("--local-provider", "", ValueOption::Other),
    ("--add-dir", "", ValueOption::Other),
    ("--remote", "", ValueOption::Other),
    ("--remote-auth-token-env", "", ValueOption::Other),
    ("--enable", "", ValueOption::Other),
    ("--disable", "", ValueOption::Other),
];

fn value_option(arg: &str) -> Option<(ValueOption, Option<&str>)> {
    for &(long, short, kind) in VALUE_OPTIONS {
        if arg == long || (!short.is_empty() && arg == short) {
            return Some((kind, None));
        }
        if let Some(value) = arg
            .strip_prefix(long)
            .and_then(|value| value.strip_prefix('='))
        {
            return Some((kind, Some(value)));
        }
        if !short.is_empty() {
            if let Some(value) = arg.strip_prefix(short).filter(|value| !value.is_empty()) {
                return Some((kind, Some(value.strip_prefix('=').unwrap_or(value))));
            }
        }
    }
    None
}

pub(super) fn invocation<'a>(args: &'a [OsString], current: &Path) -> Invocation<'a> {
    let mut directory = None;
    let mut command = None;
    let mut debug_command = None;
    let mut profile = None;
    let mut overrides = toml::Value::Table(Default::default());
    let mut display = false;
    let mut ignore_user_config = false;
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--" {
            break;
        }
        let arg = args[index].to_str().unwrap_or("");
        // Short display flags take no value and also terminate short clusters.
        if matches!(arg, "--help" | "--version") || arg.starts_with("-h") || arg.starts_with("-V") {
            display = true;
        } else if arg == "--ignore-user-config" {
            ignore_user_config = true;
        } else if let Some((kind, attached)) = value_option(arg) {
            let value = attached.map(OsStr::new).or_else(|| {
                let value = args.get(index + 1)?;
                if value.as_encoded_bytes().starts_with(b"-") {
                    return None;
                }
                index += 1;
                Some(value.as_os_str())
            });
            match kind {
                ValueOption::Cwd => directory = value.map(PathBuf::from),
                ValueOption::Profile => {
                    profile = value.and_then(OsStr::to_str).filter(|name| {
                        !name.is_empty()
                            && name.bytes().all(|byte| {
                                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
                            })
                    });
                }
                ValueOption::Config => {
                    if let Some(raw) = value.and_then(OsStr::to_str) {
                        add_override(&mut overrides, raw);
                    }
                }
                ValueOption::Image => {
                    while attached.is_none()
                        && args
                            .get(index + 1)
                            .is_some_and(|arg| !arg.as_encoded_bytes().starts_with(b"-"))
                    {
                        index += 1;
                    }
                }
                ValueOption::Other => {}
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
    let cwd = if ignored {
        current.to_owned()
    } else {
        directory
            .map(|path| current.join(path))
            .unwrap_or_else(|| current.to_owned())
    };
    // exec's LoaderOverrides skips both base and named user config. Help and
    // version are handled by clap before config loading. Tokens after -- are literal.
    let loads_user_config = !display
        && command != Some("help")
        && !(matches!(command, Some("exec" | "e" | "x")) && ignore_user_config);
    Invocation {
        cwd,
        profile,
        overrides,
        loads_user_config,
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

fn add_override(root: &mut toml::Value, raw: &str) {
    let Some((key, raw)) = raw.split_once('=') else {
        return;
    };
    let key = key.trim();
    if key.is_empty() {
        return;
    }
    // Match utils/cli/config_override.rs: scalar TOML, then trimmed raw string.
    let value = toml::from_str::<toml::Table>(&format!("_x_ = {}", raw.trim()))
        .ok()
        .and_then(|mut table| table.remove("_x_"))
        .unwrap_or_else(|| toml::Value::String(raw.trim().trim_matches(['\'', '"']).to_owned()));
    let mut current = root;
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
