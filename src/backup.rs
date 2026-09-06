use crate::{
    auth,
    cli::{Cli, Output},
    fsutil, launch, sharing,
    store::{Account, Store},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Backup {
    format: String,
    schema_version: u32,
    default: Option<u32>,
    accounts: Vec<BackupAccount>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupAccount {
    number: u32,
    alias: Option<String>,
    enabled: bool,
    share_history: bool,
    auth: Value,
}

fn emit(value: Value, output: &Output, message: String) -> Result<()> {
    if output.json {
        serde_json::to_writer_pretty(std::io::stdout().lock(), &value)?;
        println!();
    } else {
        println!("{message}");
    }
    Ok(())
}

/// CDXC:AgentProviders 2026-09-06 DECISION:
/// The user requested portable account export/import for backup and migration.
/// Backups contain plaintext credentials and portable account metadata; imports create fresh homes without transplanting machine-local paths, configuration or conversation history.
pub fn export(cli: &Cli, file: &Path, identifier: Option<&str>, output: &Output) -> Result<()> {
    let store = Store::open(cli)?;
    let accounts = match identifier {
        Some(identifier) => vec![store.resolve(identifier)?],
        None => store.data.accounts.clone(),
    };
    if accounts.is_empty() {
        bail!("no accounts to export");
    }
    let _leases: Vec<_> = accounts
        .iter()
        .map(|account| store.lease(&account.home, true))
        .collect::<Result<_>>()?;
    let mut exported = Vec::new();
    for account in accounts {
        auth::verify(&account.home, &account.identity)?;
        let bytes = fsutil::optional_bytes(&account.home.join("auth.json"))?
            .context("account credentials disappeared")?;
        let auth = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid account credentials (contents omitted)"))?;
        exported.push(BackupAccount {
            number: account.number,
            alias: account.alias,
            enabled: account.enabled,
            share_history: account.share_history,
            auth,
        });
    }
    let selected_default = store
        .data
        .default
        .or_else(|| store.main_account().map(|account| account.number));
    let default = selected_default.filter(|n| exported.iter().any(|a| a.number == *n));
    let count = exported.len();
    fsutil::create_json(
        file,
        &Backup {
            format: "codex-swap-account-backup".into(),
            schema_version: 1,
            default,
            accounts: exported,
        },
    )?;
    eprintln!(
        "Backup contains plaintext login credentials. Keep it private; settings and conversation history are not included."
    );
    emit(
        json!({"schemaVersion": 1, "exported": count, "file": file, "containsPlaintextCredentials": true}),
        output,
        format!("Exported {count} account(s) to {}", file.display()),
    )
}

pub fn import(cli: &Cli, file: &Path, remap_slots: bool, output: &Output) -> Result<()> {
    fsutil::regular(file)?;
    if std::fs::metadata(file)?.len() > 32 * 1024 * 1024 {
        bail!("account backup exceeds the 32 MiB size limit");
    }
    let bytes = fsutil::optional_bytes(file)?.context("backup file is missing")?;
    let backup: Backup = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid xswap backup (contents omitted)"))?;
    if backup.format != "codex-swap-account-backup" || backup.schema_version != 1 {
        bail!("unsupported account backup format or version");
    }
    if backup.accounts.is_empty() {
        bail!("backup contains no accounts");
    }
    let mut seen = std::collections::HashSet::new();
    for account in &backup.accounts {
        if account.number == 0 || account.number == u32::MAX || !seen.insert(account.number) {
            bail!("backup contains an invalid or duplicate slot");
        }
    }
    if backup.default.is_some_and(|number| !seen.contains(&number)) {
        bail!("backup default refers to a missing account");
    }
    let mut store = Store::open(cli)?;
    let was_empty = store.data.accounts.is_empty();
    let profiles = store.root.join("accounts");
    fsutil::private_dir(&profiles)?;
    // Stage each auth file in a private temporary home so the ordinary auth parser
    // validates it. TempDir removes all staged credentials if any account conflicts.
    let mut staged = Vec::new();
    let mut mappings = Vec::new();
    let mut imported_default = None;
    let mut used: std::collections::HashSet<_> =
        store.data.accounts.iter().map(|a| a.number).collect();
    if remap_slots {
        used.extend(backup.accounts.iter().map(|a| a.number));
    }
    for entry in backup.accounts {
        store.validate_alias(&entry.alias)?;
        let occupied = store.data.accounts.iter().any(|a| a.number == entry.number);
        let number = if occupied {
            if !remap_slots {
                bail!(
                    "slot {} is already occupied; use --remap-slots to allocate new slots",
                    entry.number
                );
            }
            let mut candidate = store.data.next_number;
            while used.contains(&candidate) {
                candidate = candidate.checked_add(1).context("no free account slots")?;
            }
            candidate
        } else {
            entry.number
        };
        let next_number = number.checked_add(1).context("slot number is too large")?;
        used.insert(number);
        let dir = fsutil::private_tempdir(&profiles, &format!("{number}-"))?;
        fsutil::atomic_json(&dir.path().join("auth.json"), &entry.auth)?;
        let identity = auth::require(dir.path())?;
        store.ensure_unique_identity(&identity, number)?;
        if backup.default == Some(entry.number) {
            imported_default = Some(number);
        }
        store.data.accounts.push(Account {
            number,
            alias: entry.alias,
            enabled: entry.enabled,
            share_history: entry.share_history,
            home: dir.path().to_path_buf(),
            managed: true,
            identity: Some(identity),
        });
        store.data.next_number = store.data.next_number.max(next_number);
        mappings.push(json!({"from": entry.number, "to": number}));
        staged.push(dir);
    }
    launch::prepare_main(&store.data.main_home)?;
    for dir in &staged {
        let account = store
            .data
            .accounts
            .iter()
            .find(|a| a.home == dir.path())
            .context("staged account missing")?;
        sharing::config(&store.data.main_home, dir.path())?;
        if account.share_history {
            sharing::history(&store.data.main_home, dir.path())?;
        }
    }
    // Importing into an established installation never changes its launch default.
    if was_empty {
        store.data.default = imported_default;
    }
    store.data.accounts.sort_by_key(|a| a.number);
    store.save()?;
    let count = staged.len();
    for dir in staged {
        let _ = dir.keep();
    }
    emit(
        json!({"schemaVersion": 1, "imported": count, "slots": mappings}),
        output,
        format!(
            "Imported {count} account(s). Settings and shared history use this computer's main Codex home."
        ),
    )
}
