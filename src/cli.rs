use clap::{Args, Parser, Subcommand};
use std::{ffi::OsString, path::PathBuf};

/// CDXC:AgentProviders 2026-09-06 DECISION:
/// The user requested standalone usage reporting, account backups, account management and native Windows support alongside the cswap account-launch workflow.
/// This supersedes the usage-polling exclusion; automatic switching policy remains owned by gxserver.
#[derive(Parser)]
#[command(
    name = "xswap",
    version,
    about = "Run Codex with separate accounts and shared conversations"
)]
pub struct Cli {
    /// Account registry directory (never put this inside a source repository).
    #[arg(long, env = "XSWAP_HOME", global = true)]
    pub data_dir: Option<PathBuf>,
    /// Main Codex home, remembered when the account registry is created.
    #[arg(long, env = "XSWAP_CODEX_HOME", global = true)]
    pub codex_home: Option<PathBuf>,
    /// Official Codex executable, resolved on PATH by default.
    #[arg(long, env = "XSWAP_CODEX_BIN", global = true)]
    pub codex_bin: Option<OsString>,
    #[command(subcommand)]
    pub command: Action,
}

#[derive(Subcommand)]
pub enum Action {
    /// Save an independent snapshot of the current login, or sign into a new account.
    Add(Add),
    /// List accounts, identities, login state and launch directories (no quota requests).
    List(Output),
    /// Fetch Codex quota percentages, reset windows and pacing.
    Usage {
        account: Option<String>,
        #[arg(long, conflicts_with = "account")]
        all: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Set or clear an account alias.
    #[command(visible_alias = "alias")]
    Rename {
        account: String,
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        alias: Option<String>,
        #[arg(long)]
        clear: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Move to a slot; swap accounts if that slot is occupied.
    Move {
        account: String,
        slot: u32,
        #[command(flatten)]
        output: Output,
    },
    /// Swap two accounts' slot numbers, keeping defaults attached to each account.
    Swap {
        account: String,
        other: String,
        #[command(flatten)]
        output: Output,
    },
    /// Allow an account to be selected implicitly.
    Enable {
        account: String,
        #[command(flatten)]
        output: Output,
    },
    /// Disable implicit selection; explicit launches remain available.
    Disable {
        account: String,
        #[command(flatten)]
        output: Output,
    },
    /// Export portable account credentials as a new plaintext JSON backup file.
    Export {
        file: PathBuf,
        #[arg(long)]
        account: Option<String>,
        #[command(flatten)]
        output: Output,
    },
    /// Import a portable backup into new managed account homes.
    Import {
        file: PathBuf,
        /// Reassign occupied slots; aliases and account identities must remain unique.
        #[arg(long)]
        remap_slots: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Map a directory (and its subfolders) to an account, or list mappings.
    Map {
        account: Option<String>,
        #[arg(requires = "account")]
        directory: Option<PathBuf>,
        #[command(flatten)]
        output: Output,
    },
    /// Remove the mapping on this exact directory (defaults to the current directory).
    Unmap {
        directory: Option<PathBuf>,
        #[command(flatten)]
        output: Output,
    },
    /// Read or change xswap preferences.
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
        #[command(flatten)]
        output: Output,
    },
    /// Upgrade through Homebrew, or the official Windows binary installer.
    Upgrade,
    /// Delete xswap credentials and managed homes, retaining original/adopted homes.
    Purge {
        /// Skip the interactive destructive-action confirmation.
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Show the saved global default account.
    Status(Output),
    /// Activate an account for bare Codex and future xswap launches; no account rotates to the next enabled account.
    Switch {
        /// Slot number, alias, email, or 'default' for the original Codex home.
        account: Option<String>,
        #[command(flatten)]
        output: Output,
    },
    /// Forget an account without deleting credentials or conversation files.
    Remove {
        account: String,
        #[command(flatten)]
        output: Output,
    },
    /// Sign in again using Codex's own interactive login.
    Login {
        account: String,
        #[arg(long)]
        device_auth: bool,
    },
    /// Launch the selected account; everything after -- goes directly to Codex.
    Run {
        account: Option<String>,
        /// Share transcripts, archives, history, session index and SQLite state.
        #[arg(long)]
        share_history: bool,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Show saved preferences and their defaults.
    List,
    /// Print the registry file containing these preferences.
    Path,
    /// Read codex-bin or default-account.
    Get { key: String },
    /// Set codex-bin or default-account (slot, alias, email or default).
    Set { key: String, value: String },
    /// Restore a preference to its default.
    Unset { key: String },
}

#[derive(Args)]
pub struct Add {
    #[arg(long)]
    pub alias: Option<String>,
    /// Assign an unused positive slot number.
    #[arg(long)]
    pub slot: Option<u32>,
    /// Snapshot a logged-in Codex home instead of the original home.
    #[arg(long, conflicts_with = "login")]
    pub home: Option<PathBuf>,
    /// Create a permanent account home and open Codex's sign-in flow.
    #[arg(long)]
    pub login: bool,
    #[arg(long, requires = "login")]
    pub device_auth: bool,
    #[arg(long)]
    pub share_history: bool,
    #[command(flatten)]
    pub output: Output,
}

#[derive(Args)]
pub struct Output {
    #[arg(long, global = true)]
    pub json: bool,
}

/// Accept the exact status/switch spellings used by the user's cs shell wrapper.
pub fn arguments() -> Vec<OsString> {
    let mut args: Vec<_> = std::env::args_os().collect();
    for arg in args.iter_mut().skip(1) {
        if arg == "--" {
            break;
        }
        if arg == "--status" {
            *arg = "status".into();
        } else if arg == "--switch-to" {
            *arg = "switch".into();
        }
    }
    args
}
