use crate::{
    auth::Identity,
    cli::{Cli, Output},
    store::{Account, Store},
    usage_client,
    usage_model::{self, Usage},
};
use anyhow::{Result, bail};
use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountUsage {
    number: Option<u32>,
    alias: Option<String>,
    email: Option<String>,
    account_id: Option<String>,
    fetched_at: String,
    usage: Option<Usage>,
    error: Option<String>,
}

struct Target {
    number: Option<u32>,
    alias: Option<String>,
    home: PathBuf,
    identity: Option<Identity>,
}

impl From<Account> for Target {
    fn from(account: Account) -> Self {
        Self {
            number: Some(account.number),
            alias: account.alias,
            home: account.home,
            identity: account.identity,
        }
    }
}

fn duration(seconds: i64) -> String {
    if seconds >= 86_400 {
        format!("{}d {}h", seconds / 86_400, seconds % 86_400 / 3600)
    } else if seconds >= 3600 {
        format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
    } else if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

fn human(report: &AccountUsage) {
    let name = report
        .alias
        .as_deref()
        .or(report.email.as_deref())
        .unwrap_or("original Codex home");
    let slot = report
        .number
        .map(|number| format!("{number} "))
        .unwrap_or_default();
    println!("{slot}{}", name.escape_default());
    if let Some(error) = &report.error {
        println!("  Error: {}", error.escape_default());
        return;
    }
    let Some(usage) = &report.usage else { return };
    if let Some(plan) = &usage.plan {
        println!("  Plan: {}", plan.escape_default());
    }
    if usage.allowed == Some(false) || usage.limit_reached == Some(true) {
        println!("  Account quota is currently limited.");
    }
    if usage.windows.is_empty() {
        println!("  No quota windows reported by Codex.");
    }
    for window in &usage.windows {
        let period = window
            .window_seconds
            .map(duration)
            .unwrap_or_else(|| "duration unknown".into());
        println!(
            "  {} {} ({period}): {:.1}% used, {:.1}% left",
            window.scope.escape_default(),
            window.kind,
            window.used_percent,
            window.remaining_percent
        );
        match (&window.resets_at, window.reset_after_seconds) {
            (Some(reset), Some(seconds)) => {
                println!("    Resets {reset} (in {})", duration(seconds))
            }
            _ => println!("    Reset time not reported"),
        }
        if let Some(pacing) = &window.pacing {
            let status = match pacing.status {
                "ahead" => "ahead (at least 10% projected spare)",
                "on_track" => "on track (less than 10% projected spare)",
                _ => "behind (quota exhausted before reset)",
            };
            println!(
                "    Estimated pace: {status}; projected {:.1}% used at reset",
                pacing.projected_used_percent
            );
            if let Some(seconds) = pacing.exhausts_in_seconds {
                println!(
                    "    Estimated exhaustion in {} at the current pace",
                    duration(seconds)
                );
            }
        } else {
            println!(
                "    Pace: not available (needs usage and an active, sufficiently elapsed window)"
            );
        }
    }
    if let Some(credits) = &usage.credits {
        if credits.unlimited == Some(true) {
            println!("  Extra usage credits: unlimited");
        } else if let Some(balance) = credits.balance {
            println!("  Extra usage credits: {balance}");
        } else if credits.has_credits == Some(false) {
            println!("  Extra usage credits: none");
        }
    }
    println!("  Fetched {}", report.fetched_at);
}

/// CDXC:AgentProviders 2026-09-06 DECISION:
/// User: implement standalone Codex usage percentages, quota windows, reset times and pacing using OpenUsage's Codex integration as the reference.
pub fn show(cli: &Cli, identifier: Option<&str>, all: bool, output: &Output) -> Result<()> {
    let store = Store::open(cli)?;
    let targets: Vec<Target> = if all {
        store
            .data
            .accounts
            .iter()
            .cloned()
            .map(Into::into)
            .collect()
    } else {
        vec![match store.selected(identifier)? {
            Some(account) => account.into(),
            None => Target {
                number: None,
                alias: None,
                home: store.data.main_home.clone(),
                identity: None,
            },
        }]
    };
    let leases: Vec<_> = targets
        .iter()
        .map(|target| store.lease(&target.home, false))
        .collect::<Result<_>>()?;
    drop(store);
    let client = usage_client::client()?;
    let mut reports = Vec::new();
    for target in targets {
        let result = usage_client::fetch(&client, &target.home, &target.identity);
        let fetched = Utc::now();
        let (identity, usage, error) = match result {
            Ok((identity, response)) => match usage_model::parse(&response, fetched) {
                Ok(usage) => (Some(identity), Some(usage), None),
                Err(error) => (Some(identity), None, Some(format!("{error:#}"))),
            },
            Err(error) => (target.identity, None, Some(format!("{error:#}"))),
        };
        reports.push(AccountUsage {
            number: target.number,
            alias: target.alias,
            email: identity
                .as_ref()
                .and_then(|identity| identity.email.clone()),
            account_id: identity.map(|identity| identity.account_id),
            fetched_at: usage_model::timestamp(fetched),
            usage,
            error,
        });
    }
    drop(leases);
    let failures = reports
        .iter()
        .filter(|report| report.error.is_some())
        .count();
    if output.json {
        serde_json::to_writer_pretty(
            std::io::stdout().lock(),
            &json!({"schemaVersion": 1, "accounts": reports}),
        )?;
        println!();
    } else if reports.is_empty() {
        println!("No saved accounts. Use xswap add or xswap add --login.");
    } else {
        for report in &reports {
            human(report);
        }
    }
    if failures > 0 {
        bail!("usage unavailable for {failures} account(s)");
    }
    Ok(())
}
