use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

use crate::config::Config;
use crate::format::{format_percent, format_reset_detail, parse_iso8601};
use crate::http;
use crate::models::{DetailLine, PollError, ProviderId, Reading, State, Value as ReadingValue};

const USER_URL: &str = "https://api.github.com/copilot_internal/user";
const PREMIUM_QUOTA: &str = "premium_interactions";

#[derive(Deserialize)]
struct CopilotResponse {
    copilot_plan: Option<String>,
    quota_reset_date_utc: Option<String>,
    quota_reset_date: Option<String>,
    #[serde(default)]
    quota_snapshots: BTreeMap<String, Value>,
    #[serde(default)]
    limited_user_quotas: BTreeMap<String, Value>,
    #[serde(default)]
    monthly_quotas: BTreeMap<String, Value>,
    limited_user_reset_date: Option<String>,
}

pub fn poll(_config: &Config) -> Reading {
    let Some(token) = token_candidates().into_iter().next() else {
        return error("Нет входа GitHub Copilot");
    };
    let headers = [
        ("Authorization", format!("token {token}")),
        ("Accept", "application/json".to_string()),
        ("User-Agent", "agent-limit-monitor".to_string()),
    ];
    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    let response: CopilotResponse = match http::get_json(USER_URL, &header_refs) {
        Ok(response) => response,
        Err(PollError::Auth) => return error("Требуется вход GitHub Copilot"),
        Err(_) => return error("Не удалось запросить лимиты Copilot"),
    };

    let reset = response
        .quota_reset_date_utc
        .as_deref()
        .or(response.quota_reset_date.as_deref())
        .or(response.limited_user_reset_date.as_deref())
        .and_then(parse_iso8601);

    let mut detail = Vec::new();
    let mut selected: Option<(String, f64, bool)> = None;

    for (key, value) in &response.quota_snapshots {
        let Some(snapshot) = value.as_object() else {
            continue;
        };
        let unlimited = snapshot.get("unlimited").and_then(Value::as_bool).unwrap_or(false);
        let remaining = if unlimited {
            100.0
        } else if let Some(percent) = snapshot.get("percent_remaining").and_then(Value::as_f64) {
            percent
        } else {
            let entitlement = snapshot.get("entitlement").and_then(Value::as_f64);
            let left = snapshot.get("remaining").and_then(Value::as_f64);
            match (entitlement, left) {
                (Some(entitlement), Some(left)) if entitlement > 0.0 => {
                    (left / entitlement * 100.0).clamp(0.0, 100.0)
                }
                _ => continue,
            }
        };
        let label = quota_label(key);
        detail.push(DetailLine {
            label: label.clone(),
            value: format!("{} {}", crate::i18n::s().remaining, format_percent(remaining)),
            reset: format_reset_detail(reset),
        });
        let prefer = key == PREMIUM_QUOTA
            || selected
                .as_ref()
                .map(|(_, current, _)| remaining < *current)
                .unwrap_or(true);
        if prefer {
            selected = Some((label, remaining, remaining <= 0.0));
        }
    }

    if selected.is_none() {
        for (key, left) in &response.limited_user_quotas {
            let Some(entitlement) = response.monthly_quotas.get(key).and_then(Value::as_f64) else {
                continue;
            };
            let Some(left) = left.as_f64() else {
                continue;
            };
            if entitlement <= 0.0 {
                continue;
            }
            let remaining = (left / entitlement * 100.0).clamp(0.0, 100.0);
            let label = quota_label(key);
            detail.push(DetailLine {
                label: label.clone(),
                value: format!("{} {}", crate::i18n::s().remaining, format_percent(remaining)),
                reset: format_reset_detail(reset),
            });
            if selected.is_none() {
                selected = Some((label, remaining, remaining <= 0.0));
            }
        }
    }

    let Some((_label, remaining, exhausted)) = selected else {
        return error("Copilot не вернул квоты");
    };
    let state = if exhausted {
        State::Critical
    } else if remaining <= 25.0 {
        State::Warning
    } else {
        State::Ok
    };
    let plan = response.copilot_plan.as_deref().unwrap_or("Copilot");

    Reading {
        provider: ProviderId::Copilot,
        value: ReadingValue::Percent(remaining),
        reset_at: reset,
        state,
        title: format!("GitHub Copilot · {plan}"),
        windows: Vec::new(),
        detail,
    }
}

fn quota_label(key: &str) -> String {
    match key {
        "premium_interactions" => crate::i18n::s().premium_requests.to_string(),
        "chat" => crate::i18n::s().chat.to_string(),
        "completions" => crate::i18n::s().completions.to_string(),
        other => {
            let mut label = other.replace('_', " ");
            if let Some(first) = label.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            label
        }
    }
}

fn token_candidates() -> Vec<String> {
    let mut tokens = Vec::new();
    for name in ["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(name) {
            let value = value.trim();
            if !value.is_empty() {
                tokens.push(value.to_string());
            }
        }
    }
    if let Some(token) = gh_auth_token() {
        tokens.push(token);
    }
    if let Some(token) = gh_hosts_token() {
        tokens.push(token);
    }
    tokens.sort();
    tokens.dedup();
    tokens
}

fn gh_auth_token() -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("gh")
        .args(["auth", "token"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!token.is_empty()).then_some(token)
}

fn gh_hosts_token() -> Option<String> {
    let path = gh_hosts_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        if let Some(value) = line.trim().strip_prefix("oauth_token:") {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn gh_hosts_path() -> Option<PathBuf> {
    if let Ok(appdata) = std::env::var("APPDATA") {
        let path = PathBuf::from(appdata).join("GitHub CLI").join("hosts.yml");
        if path.exists() {
            return Some(path);
        }
    }
    let path = dirs::home_dir()?.join(".config").join("gh").join("hosts.yml");
    path.exists().then_some(path)
}

fn error(message: impl Into<String>) -> Reading {
    let mut reading = Reading::error(ProviderId::Copilot, State::Missing, message);
    reading.title = "GitHub Copilot".to_string();
    reading
}
