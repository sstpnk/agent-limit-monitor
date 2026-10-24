use std::path::PathBuf;
use std::time::SystemTime;

use serde::Deserialize;

use crate::config::Config;
use crate::format::{format_percent, format_reset_detail, parse_iso8601};
use crate::http;
use crate::models::{DetailLine, PollError, ProviderId, Reading, State, Value, WindowInfo};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

#[derive(Deserialize)]
struct AuthFile {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OAuth>,
}

#[derive(Deserialize)]
struct OAuth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>,
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
}

#[derive(Deserialize)]
struct UsageResponse {
    five_hour: Option<Bucket>,
    seven_day: Option<Bucket>,
}

#[derive(Deserialize)]
struct Bucket {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

pub fn poll(config: &Config) -> Reading {
    let ui = crate::i18n::s();
    let path = match credential_path(config) {
        Some(path) => path,
        None => return error(ui.credentials_missing, State::Missing),
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(_) => return error(ui.credentials_missing, State::Missing),
    };
    let auth: AuthFile = match serde_json::from_str(&content) {
        Ok(auth) => auth,
        Err(_) => return error(ui.parse_failed, State::Error),
    };
    let Some(oauth) = auth.oauth else {
        return error(ui.credentials_missing, State::Missing);
    };
    let Some(token) = oauth.access_token.filter(|token| !token.trim().is_empty()) else {
        return error(ui.credentials_missing, State::Missing);
    };
    if oauth
        .expires_at
        .is_some_and(|expires| expires <= crate::format::now_unix() * 1000)
    {
        return error(format!("{} — claude", ui.token_expired), State::Auth);
    }

    let headers = [
        ("Authorization", format!("Bearer {token}")),
        ("anthropic-beta", "oauth-2025-04-20".to_string()),
        ("Accept", "application/json".to_string()),
    ];
    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    let response: UsageResponse = match http::get_json(USAGE_URL, &header_refs) {
        Ok(response) => response,
        Err(PollError::Auth) => {
            return error(ui.login_required, State::Auth);
        }
        Err(_) => return error(ui.request_failed, State::Error),
    };

    let mut windows = Vec::new();
    if let Some(bucket) = response.five_hour {
        push_window(
            &mut windows,
            &format!("5{}", crate::i18n::s().unit_hour),
            bucket,
        );
    }
    if let Some(bucket) = response.seven_day {
        push_window(
            &mut windows,
            &format!("7{}", crate::i18n::s().unit_day),
            bucket,
        );
    }
    if windows.is_empty() {
        return error(ui.no_limits, State::Error);
    }

    let now = SystemTime::now();
    let selected = windows
        .iter()
        .find(|window| {
            window.limit_reached
                || window
                    .reset_at
                    .map(|reset| reset <= now)
                    .unwrap_or(false)
        })
        .or_else(|| {
            windows.iter().min_by(|left, right| {
                left.remaining
                    .total_cmp(&right.remaining)
                    .then_with(|| left.reset_at.cmp(&right.reset_at))
            })
        })
        .cloned()
        .unwrap_or_else(|| windows[0].clone());

    let state = if selected.limit_reached || selected.remaining <= 0.0 {
        State::Critical
    } else if selected.remaining <= 25.0 {
        State::Warning
    } else {
        State::Ok
    };

    let mut detail = Vec::new();
    for window in &windows {
        detail.push(DetailLine {
            label: window.label.clone(),
            value: if window.limit_reached {
                format!("0% · {}", crate::i18n::s().limit)
            } else {
                format!("{} {}", crate::i18n::s().remaining, format_percent(window.remaining))
            },
            reset: format_reset_detail(window.reset_at),
        });
    }

    let plan = oauth
        .subscription_type
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or("Claude");
    Reading {
        provider: ProviderId::Claude,
        value: Value::Percent(selected.remaining),
        reset_at: selected.reset_at,
        state,
        title: format!("Claude Code · {plan}"),
        windows,
        detail,
    }
}

fn push_window(windows: &mut Vec<WindowInfo>, label: &str, bucket: Bucket) {
    let used = bucket.utilization.unwrap_or(0.0).clamp(0.0, 100.0);
    windows.push(WindowInfo {
        label: label.to_string(),
        remaining: (100.0 - used).clamp(0.0, 100.0),
        reset_at: bucket.resets_at.as_deref().and_then(parse_iso8601),
        limit_reached: used >= 100.0,
    });
}

fn credential_path(config: &Config) -> Option<PathBuf> {
    let custom = config.providers.claude.auth_path.trim();
    if !custom.is_empty() {
        return Some(PathBuf::from(custom));
    }
    if let Ok(directory) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !directory.trim().is_empty() {
            return Some(PathBuf::from(directory).join(".credentials.json"));
        }
    }
    Some(dirs::home_dir()?.join(".claude").join(".credentials.json"))
}

fn error(message: impl Into<String>, state: State) -> Reading {
    let mut reading = Reading::error(ProviderId::Claude, state, message);
    reading.title = "Claude Code".to_string();
    reading
}
