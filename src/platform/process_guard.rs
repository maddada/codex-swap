use crate::cli::StopCodex;
use anyhow::{Context, Result, bail};
use std::{ffi::OsStr, path::Path, process::Command};

/// CDXC:AgentProviders 2026-09-06 WHY:
/// Codex does not cooperate with xswap's registry lock when refreshing its global login.
/// Refuse global credential changes while a current-user Codex process or launcher is visible, regardless of its unknown CODEX_HOME.
/// This is a process snapshot, not an exclusion lock: a separately started Codex can still race a later write, so callers recheck immediately before committing and users must keep Codex closed through the operation.
pub fn ensure_codex_stopped(configured_binary: &OsStr, stop: StopCodex) -> Result<()> {
    let configured = configured_name(configured_binary)?;
    let pids = codex_pids(configured_binary)?;
    super::codex_blockers::clear(
        &pids,
        &pids,
        stop,
        "must be closed before the global Codex login changes",
        || codex_pids(OsStr::new(configured)),
    )
}

/// Current-user Codex processes and launchers, sorted and without duplicates.
pub fn codex_pids(configured_binary: &OsStr) -> Result<Vec<u32>> {
    let mut pids = running_codex(configured_name(configured_binary)?)?;
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

fn configured_name(configured_binary: &OsStr) -> Result<&str> {
    Path::new(configured_binary)
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty())
        .context("cannot identify the configured Codex executable for the process check")
}

#[cfg(unix)]
fn basename(value: &str) -> &str {
    value.rsplit(['/', '\\']).next().unwrap_or(value)
}

#[cfg(unix)]
fn is_codex_name(value: &str, configured: &str) -> bool {
    let name = basename(value).to_ascii_lowercase();
    let configured = configured.to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    let configured = configured.strip_suffix(".exe").unwrap_or(&configured);
    name == "codex"
        || name.strip_prefix("codex-").is_some_and(|suffix| {
            ["aarch64", "x86_64", "arm64", "amd64", "x64"].iter().any(|arch| suffix.starts_with(arch))
                || suffix.as_bytes().first().is_some_and(u8::is_ascii_digit)
        })
        || matches!(name, "codex.js" | "codex.cmd" | "codex.bat" | "codex.ps1")
        || name == configured
        // Linux ps can truncate comm to the kernel's 15-character task name.
        || (name.len() >= 15 && configured.starts_with(name))
}

#[cfg(unix)]
fn is_launcher(value: &str) -> bool {
    matches!(
        basename(value).trim_start_matches('-'),
        "node" | "nodejs" | "sh" | "bash" | "zsh" | "fish" | "dash" | "pwsh" | "powershell" | "cmd"
    )
}

#[cfg(unix)]
fn command_mentions_codex(command: &str, executable: &str, configured: &str) -> bool {
    let command = command.trim();
    let arguments = command
        .strip_prefix(executable)
        .filter(|rest| rest.starts_with(char::is_whitespace))
        .or_else(|| {
            command
                .split_once(char::is_whitespace)
                .map(|(_, rest)| rest)
        })
        .unwrap_or("");
    let mut words = arguments
        .split_whitespace()
        .map(|word| word.trim_matches(['\'', '"']));
    let shell = matches!(
        basename(executable).trim_start_matches('-'),
        "sh" | "bash" | "zsh" | "fish" | "dash"
    );
    while let Some(word) = words.next() {
        if shell && word.starts_with('-') && word.contains('c') {
            let next = words.next().unwrap_or("");
            let command = if next == "exec" {
                words.next().unwrap_or("")
            } else {
                next
            };
            return is_codex_name(command, configured);
        }
        if matches!(
            word,
            "-e" | "--eval" | "-p" | "--print" | "-c" | "-Command" | "-EncodedCommand"
        ) {
            // Inline program text can mention Codex without launching it.
            return false;
        }
        if matches!(
            word,
            "-r" | "--require"
                | "--import"
                | "--loader"
                | "--experimental-loader"
                | "--conditions"
                | "--title"
        ) {
            words.next();
            continue;
        }
        if word == "--" {
            return words
                .next()
                .is_some_and(|word| is_codex_name(word, configured));
        }
        if word.starts_with('-') {
            continue;
        }
        // Inspect the entrypoint only: later arguments can be prompts or task text.
        return is_codex_name(word, configured);
    }
    false
}

#[cfg(unix)]
fn running_codex(configured: &str) -> Result<Vec<u32>> {
    use std::process::Stdio;
    let child = Command::new("/bin/ps")
        .args(["-e", "-o", "uid=", "-o", "pid=", "-o", "comm="])
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot enumerate processes with /bin/ps; keep Codex closed and restore process-query access before retrying")?;
    let scanner_pid = child.id();
    let listing = child
        .wait_with_output()
        .context("wait for process enumeration")?;
    if !listing.status.success() {
        bail!("cannot enumerate processes; /bin/ps failed, so global account changes were refused");
    }
    let listing = String::from_utf8(listing.stdout).context("cannot decode process enumeration")?;
    if listing.trim().is_empty() {
        bail!("process enumeration returned no processes; global account changes were refused");
    }
    let current_uid = unsafe { libc::geteuid() };
    let mut matches = Vec::new();
    for line in listing.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.trim_start().splitn(2, char::is_whitespace);
        let uid: i64 = fields
            .next()
            .context("missing process owner")?
            .parse()
            .context("invalid process owner")?;
        let rest = fields
            .next()
            .context("missing process details")?
            .trim_start();
        let mut fields = rest.splitn(2, char::is_whitespace);
        let pid: u32 = fields
            .next()
            .context("missing process PID")?
            .parse()
            .context("invalid process PID")?;
        let executable = fields.next().context("missing process executable")?.trim();
        if uid as u32 != current_uid || pid == std::process::id() || pid == scanner_pid {
            continue;
        }
        if is_codex_name(executable, configured) {
            matches.push(pid);
        } else if is_launcher(executable) {
            let output = Command::new("/bin/ps")
                .args(["-p", &pid.to_string(), "-ww", "-o", "args="])
                .env("LC_ALL", "C")
                .output()
                .with_context(|| {
                    format!("cannot inspect launcher PID {pid}; close Codex and retry")
                })?;
            if !output.status.success() {
                // ps returns 1 with no rows when a process exits between the queries.
                if output.status.code() == Some(1)
                    && output.stdout.is_empty()
                    && output.stderr.is_empty()
                {
                    continue;
                }
                bail!(
                    "cannot inspect launcher PID {pid}; close Codex and restore process-query access before retrying"
                );
            }
            let command = String::from_utf8(output.stdout)
                .with_context(|| format!("cannot decode launcher PID {pid}"))?;
            if command.trim().is_empty() {
                bail!(
                    "cannot read launcher PID {pid}; close Codex and restore process-query access before retrying"
                );
            }
            if command_mentions_codex(&command, executable, configured) {
                matches.push(pid);
            }
        }
    }
    Ok(matches)
}

#[cfg(windows)]
fn running_codex(configured: &str) -> Result<Vec<u32>> {
    let output = Command::new("powershell.exe")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", include_str!("process_guard.ps1")])
        .env("XSWAP_PROCESS_EXECUTABLE", configured)
        .env("XSWAP_PROCESS_PARENT", std::process::id().to_string())
        .output()
        .context("cannot inspect Windows processes; PowerShell and local CIM process queries are required before global account changes")?;
    if !output.status.success() {
        // Never render command lines or CIM error payloads, which may include credentials.
        if let Ok(pid) = serde_json::from_slice::<u32>(&output.stdout) {
            bail!(
                "cannot inspect Windows process PID {pid}; close that process or run xswap with the same Windows privileges, then retry. No global login was changed"
            );
        }
        bail!(
            "cannot inspect Windows process ownership or launcher arguments; close Codex and restore PowerShell/CIM process-query access before retrying"
        );
    }
    serde_json::from_slice(&output.stdout).context(
        "Windows process query returned invalid PID data; global account changes were refused",
    )
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn windows_guard_includes_codex_desktop_without_matching_chatgpt() {
        // Exercise the actual CIM script without relying on applications running on the host.
        let fixtures = r#"
function Get-CimInstance {
    param($ClassName, $Property, $OperationTimeoutSec, $Filter)
    $items = @(
        [pscustomobject]@{ Name = 'codex.exe'; ProcessId = 41001 },
        [pscustomobject]@{ Name = 'ChatGPT.exe'; ProcessId = 41002; ExecutablePath = 'C:\Program Files\WindowsApps\OpenAI.Codex_26.924.2738.0_x64__2p2nqsd0c76g0\app\ChatGPT.exe' },
        [pscustomobject]@{ Name = 'ChatGPT.exe'; ProcessId = 41003; ExecutablePath = 'C:\Program Files\WindowsApps\OpenAI.ChatGPT-Desktop_1.0_x64__2p2nqsd0c76g0\app\ChatGPT.exe' },
        [pscustomobject]@{ Name = 'ChatGPT.exe'; ProcessId = 41004; ExecutablePath = 'C:\Program Files\WindowsApps\OpenAI.Codex_26.924.2738.0_x64__2p2nqsd0c76g0\app\ChatGPT.exe' },
        [pscustomobject]@{ Name = 'node.exe'; ProcessId = 41005; CommandLine = 'node.exe C:\tools\codex.js' },
        [pscustomobject]@{ Name = 'node.exe'; ProcessId = 41006; CommandLine = 'node.exe -e "console.log(''codex.js'')"' }
    )
    if ($Filter) { return $items | Where-Object { "ProcessId = $($_.ProcessId)" -eq $Filter } }
    return $items
}
function Invoke-CimMethod {
    param($InputObject, $MethodName, $OperationTimeoutSec)
    $sid = if ($InputObject.ProcessId -eq 41004) { 'another-user' } else { [Security.Principal.WindowsIdentity]::GetCurrent().User.Value }
    return [pscustomobject]@{ ReturnValue = 0; Sid = $sid }
}
"#;
        let query = |fixtures: &str| {
            Command::new("powershell.exe")
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    &format!("{fixtures}\n{}", include_str!("process_guard.ps1")),
                ])
                .env("XSWAP_PROCESS_EXECUTABLE", "codex")
                .env("XSWAP_PROCESS_PARENT", "0")
                .output()
                .unwrap()
        };
        let output = query(fixtures);
        assert!(output.status.success(), "Windows process query failed");
        let pids: Vec<u32> = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(pids, [41001, 41002, 41005]);

        // Inaccessible elevated shells must block the switch with an actionable PID.
        let inaccessible = fixtures.replace("node.exe C:\\tools\\codex.js", "");
        let output = query(&inaccessible);
        assert!(!output.status.success());
        assert_eq!(
            serde_json::from_slice::<u32>(&output.stdout).unwrap(),
            41005
        );
        assert!(output.stderr.is_empty());
    }
}
