use crate::{
    auth::Identity,
    cli::{Cli, Output},
    store::{Account, Store, require_registered_identity},
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

fn reports(
    targets: Vec<Target>,
    mut fetch: impl FnMut(&Target) -> Result<(Identity, usage_client::Response)>,
) -> Vec<AccountUsage> {
    let mut reports = Vec::new();
    for target in targets {
        let result = match target.number {
            Some(number) => {
                require_registered_identity(number, &target.identity).and_then(|_| fetch(&target))
            }
            None => fetch(&target),
        };
        let fetched = Utc::now();
        let (identity, usage, error) = match result {
            Ok((identity, response)) => match usage_model::parse(&response, fetched) {
                Ok(usage) => (Some(identity), Some(usage), None),
                Err(error) => (Some(identity), None, Some(format!("{error:#}"))),
            },
            Err(error) => (
                target
                    .identity
                    .clone()
                    .filter(|identity| identity.has_owner()),
                None,
                Some(format!("{error:#}")),
            ),
        };
        let labels = identity.as_ref().or(target.identity.as_ref());
        reports.push(AccountUsage {
            number: target.number,
            alias: target.alias,
            email: labels.and_then(|identity| identity.email.clone()),
            account_id: identity.map(|identity| identity.account_id),
            fetched_at: usage_model::timestamp(fetched),
            usage,
            error,
        });
    }
    reports
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
    let live = store.observe_live_account();
    let targets: Vec<Target> = if all {
        store
            .data
            .accounts
            .iter()
            .map(|account| store.effective_account(account, live.as_ref()).into())
            .collect()
    } else {
        vec![match store.selected(identifier)? {
            Some(account) => store.effective_account(&account, live.as_ref()).into(),
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
    let reports = reports(targets, |target| {
        usage_client::fetch(&client, &target.home, &target.identity)
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_usage_batch_reports_incomplete_setup_and_keeps_healthy_results() {
        let identity = Identity {
            account_id: "workspace-1".into(),
            user_id: Some("user-1".into()),
            email: Some("first@example.test".into()),
            plan: None,
            legacy_hint_unusable: false,
        };
        let mut ownerless = identity.clone();
        ownerless.user_id = None;
        ownerless.email = None;
        let mut unresolved = ownerless.clone();
        unresolved.email = Some("stored-label@example.test".into());
        unresolved.legacy_hint_unusable = true;
        let mut targets = Vec::new();
        for (number, owner) in [
            (1, Some(identity.clone())),
            (2, None),
            (3, Some(ownerless)),
            (4, Some(unresolved)),
        ] {
            targets.push(
                Account {
                    number,
                    alias: Some(format!("slot-{number}")),
                    home: PathBuf::from("unused-synthetic-home"),
                    managed: true,
                    share_history: false,
                    identity: owner,
                    enabled: true,
                }
                .into(),
            );
        }
        targets.push(Target {
            number: None,
            alias: None,
            home: PathBuf::from("unused-main-home"),
            identity: None,
        });
        let mut fetched = Vec::new();
        let reports = reports(targets, |target| {
            fetched.push(target.number);
            Ok((
                identity.clone(),
                usage_client::Response {
                    body: json!({"rate_limit": {"allowed": true, "primary_window": {"used_percent": 12.0, "limit_window_seconds": 18000}}}),
                    headers: Default::default(),
                },
            ))
        });
        assert_eq!(fetched, vec![Some(1), None]);
        assert_eq!(reports.len(), 5);
        assert!(reports[0].error.is_none());
        assert_eq!(
            reports[0].usage.as_ref().unwrap().windows[0].used_percent,
            12.0
        );
        for report in &reports[1..4] {
            assert!(
                report
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("setup is incomplete")
            );
            assert!(report.usage.is_none());
            assert!(report.account_id.is_none());
        }
        assert!(reports[1].email.is_none());
        assert!(reports[2].email.is_none());
        assert_eq!(
            reports[3].email.as_deref(),
            Some("stored-label@example.test")
        );
        assert!(reports[4].error.is_none());
        assert!(reports[4].usage.is_some());
    }
}

#[cfg(test)]
mod credit_header_output_regressions {
    use super::*;
    use reqwest::header::HeaderMap;
    use std::process::Command;

    #[test]
    fn credit_header_reaches_json_and_human_output() {
        const CHILD: &str = "XSWAP_TEST_CREDIT_HEADER_OUTPUT";
        if std::env::var_os(CHILD).is_some() {
            let mut headers = HeaderMap::new();
            headers.insert("x-codex-credits-balance", "12.5".parse().unwrap());
            let fetched = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
            let report = AccountUsage {
                number: None,
                alias: Some("offline fixture".into()),
                user_id: None,
                email: None,
                account_id: None,
                fetched_at: usage_model::timestamp(fetched),
                usage: Some(
                    usage_model::parse(
                        &usage_client::Response {
                            body: json!({}),
                            headers,
                        },
                        fetched,
                    )
                    .unwrap(),
                ),
                error: None,
            };
            human(&report);
            println!(
                "CREDIT_HEADER_JSON:{}",
                serde_json::to_string(&json!({"schemaVersion": 1, "accounts": [report]})).unwrap()
            );
            return;
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "usage::credit_header_output_regressions::credit_header_reaches_json_and_human_output", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("  Extra usage credits: 12.5"), "{stdout}");
        let json = stdout
            .lines()
            .find_map(|line| line.strip_prefix("CREDIT_HEADER_JSON:"))
            .unwrap();
        let output: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(output["schemaVersion"], 1);
        assert_eq!(output["accounts"][0]["usage"]["credits"]["balance"], 12.5);
        assert!(output["accounts"][0]["usage"]["credits"]["hasCredits"].is_null());
        assert!(output["accounts"][0]["usage"]["credits"]["unlimited"].is_null());
        assert_eq!(
            output["accounts"][0]["fetchedAt"],
            usage_model::timestamp(chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap())
        );
    }
}
