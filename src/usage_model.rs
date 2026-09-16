use crate::usage_client::Response;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub plan: Option<String>,
    pub allowed: Option<bool>,
    pub limit_reached: Option<bool>,
    pub windows: Vec<Window>,
    pub credits: Option<Credits>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Credits {
    pub balance: Option<f64>,
    pub has_credits: Option<bool>,
    pub unlimited: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    pub scope: String,
    pub kind: &'static str,
    pub used_percent: f64,
    pub remaining_percent: f64,
    pub window_seconds: Option<i64>,
    pub resets_at: Option<String>,
    pub resets_at_epoch_seconds: Option<i64>,
    pub reset_after_seconds: Option<i64>,
    pub pacing: Option<Pacing>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pacing {
    pub status: &'static str,
    pub elapsed_percent: f64,
    pub projected_used_percent: f64,
    pub projected_remaining_percent: f64,
    pub exhausts_in_seconds: Option<i64>,
}

pub fn timestamp(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        .filter(|value| value.is_finite())
}

fn integer(value: &Value, field: &str) -> Result<Option<i64>> {
    if value.is_null() {
        return Ok(None);
    }
    let value = number(value).with_context(|| format!("invalid Codex usage {field}"))?;
    if value < 0.0 || value >= i64::MAX as f64 || value.fract() != 0.0 {
        bail!("invalid Codex usage {field}");
    }
    Ok(Some(value as i64))
}

/// OpenUsage's pacing model projects the current window's burn rate, waiting at least 60 seconds or 1% of the window before projecting.
fn pace(used: f64, seconds: i64, reset: i64, now: i64) -> Option<Pacing> {
    let elapsed = seconds as f64 - (reset - now) as f64;
    if seconds <= 0 || used <= 0.0 || now >= reset || elapsed < 60.0_f64.max(seconds as f64 * 0.01)
    {
        return None;
    }
    let projected = used / elapsed * seconds as f64;
    let status = if used >= 100.0 || projected > 100.0 {
        "behind"
    } else if projected > 90.0 {
        "on_track"
    } else {
        "ahead"
    };
    let eta = (100.0 - used) / (used / elapsed);
    Some(Pacing {
        status,
        elapsed_percent: elapsed / seconds as f64 * 100.0,
        projected_used_percent: projected,
        projected_remaining_percent: (100.0 - projected).max(0.0),
        exhausts_in_seconds: (status == "behind" && eta > 0.0 && eta < (reset - now) as f64)
            .then_some(eta.ceil() as i64),
    })
}

fn windows(
    scope: &str,
    rate: &Value,
    response: Option<&Response>,
    now: DateTime<Utc>,
) -> Result<Vec<Window>> {
    if !rate.is_null() && !rate.is_object() {
        bail!("Codex usage contains an invalid rate limit");
    }
    let mut windows = Vec::new();
    for slot in ["primary", "secondary"] {
        let value = &rate[format!("{slot}_window")];
        if !value.is_null() && !value.is_object() {
            bail!("Codex usage contains an invalid quota window");
        }
        let used = number(&value["used_percent"]).or_else(|| {
            response.and_then(|response| {
                response
                    .headers
                    .get(format!("x-codex-{slot}-used-percent"))?
                    .to_str()
                    .ok()?
                    .parse::<f64>()
                    .ok()
            })
        });
        let Some(used) = used else {
            if !value["used_percent"].is_null() {
                bail!("Codex usage contains an invalid used percentage");
            }
            continue;
        };
        if !used.is_finite() || used < 0.0 {
            bail!("Codex usage contains an invalid used percentage");
        }
        let seconds = integer(&value["limit_window_seconds"], "window duration")?;
        if seconds == Some(0) {
            bail!("Codex usage window duration must be positive");
        }
        let reset = match integer(&value["reset_at"], "reset time")? {
            Some(reset) => Some(reset),
            None => integer(&value["reset_after_seconds"], "reset duration")?
                .map(|seconds| {
                    now.timestamp()
                        .checked_add(seconds)
                        .context("Codex usage reset time is too large")
                })
                .transpose()?,
        };
        let resets_at = reset
            .map(|reset| {
                DateTime::from_timestamp(reset, 0)
                    .map(timestamp)
                    .context("Codex usage reset time is outside the supported range")
            })
            .transpose()?;
        // CDXC:AgentProviders 2026-09-06 WHY:
        // Codex can put a sole weekly quota in primary_window, so the reported duration owns its label; missing durations stay unknown instead of inventing a five-hour window.
        let kind = match seconds {
            Some(18_000) => "session",
            Some(604_800) => "weekly",
            _ => slot,
        };
        windows.push(Window {
            scope: scope.to_owned(),
            kind,
            used_percent: used,
            remaining_percent: (100.0 - used).max(0.0),
            window_seconds: seconds,
            resets_at,
            resets_at_epoch_seconds: reset,
            reset_after_seconds: reset.map(|reset| (reset - now.timestamp()).max(0)),
            pacing: seconds
                .zip(reset)
                .and_then(|(seconds, reset)| pace(used, seconds, reset, now.timestamp())),
        });
    }
    Ok(windows)
}

pub fn parse(response: &Response, now: DateTime<Utc>) -> Result<Usage> {
    let body = &response.body;
    let rate = &body["rate_limit"];
    let mut all = windows("codex", rate, Some(response), now)?;
    for key in ["additional_rate_limits", "code_review_rate_limit"] {
        if key == "code_review_rate_limit" {
            all.extend(windows("code_review", &body[key], None, now)?);
            continue;
        }
        if body[key].is_null() {
            continue;
        }
        let limits = body[key]
            .as_array()
            .context("Codex usage contains invalid additional limits")?;
        for (index, limit) in limits.iter().enumerate() {
            if !limit.is_object() {
                bail!("Codex usage contains an invalid additional limit");
            }
            let scope = limit["limit_name"]
                .as_str()
                .or_else(|| limit["metered_feature"].as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("additional_{}", index + 1));
            all.extend(windows(&scope, &limit["rate_limit"], None, now)?);
        }
    }
    let has_credits = body["credits"]["has_credits"].as_bool();
    let unlimited = body["credits"]["unlimited"].as_bool();
    let balance = number(&body["credits"]["balance"])
        .or_else(|| (has_credits == Some(false)).then_some(0.0))
        .or_else(|| {
            let header = response
                .headers
                .get("x-codex-credits-balance")?
                .to_str()
                .ok()?;
            number(&Value::String(header.to_owned()))
        });
    let has_credit_data = balance.is_some() || has_credits.is_some() || unlimited.is_some();
    let credits = (body["credits"].is_object() || has_credit_data).then_some(Credits {
        balance,
        has_credits,
        unlimited,
    });
    if all.is_empty() && !has_credit_data && !rate.is_object() {
        bail!("Codex usage API returned no recognized usage data");
    }
    Ok(Usage {
        plan: body["plan_type"].as_str().map(str::to_owned),
        allowed: rate["allowed"].as_bool(),
        limit_reached: rate["limit_reached"].as_bool(),
        windows: all,
        credits,
    })
}

#[cfg(test)]
mod credit_header_regressions {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};
    use serde_json::json;

    const INVALID_HEADERS: [Option<&str>; 9] = [
        None,
        Some(""),
        Some(" "),
        Some("invalid"),
        Some("NaN"),
        Some("inf"),
        Some("-inf"),
        Some("Infinity"),
        Some("1e999"),
    ];

    fn response(body: Value, header: Option<&str>) -> Response {
        let mut headers = HeaderMap::new();
        if let Some(header) = header {
            headers.insert("x-codex-credits-balance", header.parse().unwrap());
        }
        Response { body, headers }
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap()
    }

    #[test]
    fn credit_header_is_retained_beside_core_quota() {
        let usage = parse(
            &response(
                json!({"rate_limit": {"primary_window": {
                    "used_percent": 20,
                    "limit_window_seconds": 18000,
                    "reset_after_seconds": 9000
                }}}),
                Some("12.5"),
            ),
            now(),
        )
        .unwrap();
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].kind, "session");
        assert_eq!(usage.windows[0].remaining_percent, 80.0);
        let credits = usage.credits.unwrap();
        assert_eq!(credits.balance, Some(12.5));
        assert_eq!(credits.has_credits, None);
        assert_eq!(credits.unlimited, None);
    }

    #[test]
    fn credit_header_counts_as_recognized_usage() {
        let usage = parse(&response(json!({}), Some("12.5")), now()).unwrap();
        assert!(usage.windows.is_empty());
        assert_eq!(usage.credits.unwrap().balance, Some(12.5));
    }

    #[test]
    fn valid_body_balance_wins_over_header_and_false_flag() {
        for (balance, expected) in [
            (json!(0), 0.0),
            (json!(12.75), 12.75),
            (json!("-12.75"), -12.75),
            (json!("1e20"), 1e20),
        ] {
            let usage = parse(
                &response(
                    json!({"credits": {
                        "balance": balance,
                        "has_credits": false,
                        "unlimited": true
                    }}),
                    Some("99"),
                ),
                now(),
            )
            .unwrap();
            let credits = usage.credits.unwrap();
            assert_eq!(credits.balance, Some(expected));
            assert_eq!(credits.has_credits, Some(false));
            assert_eq!(credits.unlimited, Some(true));
        }
    }

    #[test]
    fn credit_header_preserves_body_flags() {
        let usage = parse(
            &response(
                json!({"credits": {"has_credits": true, "unlimited": false}}),
                Some("12.5"),
            ),
            now(),
        )
        .unwrap();
        let credits = usage.credits.unwrap();
        assert_eq!(credits.balance, Some(12.5));
        assert_eq!(credits.has_credits, Some(true));
        assert_eq!(credits.unlimited, Some(false));
    }

    #[test]
    fn credit_header_fills_missing_or_unusable_body_balance() {
        for body in [
            json!({}),
            json!({"credits": null}),
            json!({"credits": {}}),
            json!({"credits": {"balance": null}}),
            json!({"credits": {"balance": false}}),
            json!({"credits": {"balance": {}}}),
            json!({"credits": {"balance": "invalid"}}),
            json!({"credits": {"balance": "NaN"}}),
            json!({"credits": {"balance": "Infinity"}}),
            json!({"credits": {"balance": "1e999"}}),
        ] {
            let usage = parse(&response(body, Some("12.5")), now()).unwrap();
            assert_eq!(usage.credits.unwrap().balance, Some(12.5));
        }
    }

    #[test]
    fn explicit_false_supplies_zero_before_credit_header() {
        for balance in [json!(null), json!("invalid"), json!("NaN")] {
            let usage = parse(
                &response(
                    json!({"credits": {
                        "balance": balance,
                        "has_credits": false,
                        "unlimited": false
                    }}),
                    Some("99"),
                ),
                now(),
            )
            .unwrap();
            let credits = usage.credits.unwrap();
            assert_eq!(credits.balance, Some(0.0));
            assert_eq!(credits.has_credits, Some(false));
            assert_eq!(credits.unlimited, Some(false));
        }
    }

    #[test]
    fn meaningful_flags_preserve_unknown_balance() {
        for (body, has_credits, unlimited) in [
            (json!({"credits": {"has_credits": true}}), Some(true), None),
            (json!({"credits": {"unlimited": true}}), None, Some(true)),
            (json!({"credits": {"unlimited": false}}), None, Some(false)),
        ] {
            let usage = parse(&response(body, Some("NaN")), now()).unwrap();
            let credits = usage.credits.unwrap();
            assert_eq!(credits.balance, None);
            assert_eq!(credits.has_credits, has_credits);
            assert_eq!(credits.unlimited, unlimited);
        }
    }

    #[test]
    fn header_balances_preserve_raw_decimal_values() {
        for (header, expected) in [("12.75", 12.75), ("-12.75", -12.75), ("1e20", 1e20)] {
            let usage = parse(&response(json!({}), Some(header)), now()).unwrap();
            assert_eq!(usage.credits.unwrap().balance, Some(expected));
        }
    }

    #[test]
    fn invalid_headers_do_not_invent_credit_balance() {
        for header in INVALID_HEADERS {
            let usage = parse(
                &response(
                    json!({"rate_limit": {"primary_window": {"used_percent": 20}}}),
                    header,
                ),
                now(),
            )
            .unwrap();
            assert!(usage.credits.is_none(), "header {header:?}");
        }
    }

    #[test]
    fn missing_or_invalid_header_without_meaningful_data_is_rejected() {
        for header in INVALID_HEADERS {
            for body in [
                json!({}),
                json!({"credits": {}}),
                json!({"credits": {"balance": "NaN", "has_credits": "false", "unlimited": 1}}),
            ] {
                let error = parse(&response(body, header), now())
                    .err()
                    .expect("no recognized data");
                assert_eq!(
                    error.to_string(),
                    "Codex usage API returned no recognized usage data"
                );
            }
        }
    }

    #[test]
    fn non_text_credit_header_is_not_recognized() {
        let mut response = response(json!({}), None);
        response.headers.insert(
            "x-codex-credits-balance",
            HeaderValue::from_bytes(b"\xff").unwrap(),
        );
        assert!(parse(&response, now()).is_err());
    }
}
