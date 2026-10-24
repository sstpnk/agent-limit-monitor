use std::time::SystemTime;

use serde::Deserialize;
use serde_json::Value;

use crate::config::Config;
use crate::format::{format_percent, format_reset_detail, parse_iso8601};
use crate::http;
use crate::models::{DetailLine, PollError, ProviderId, Reading, State, Value as ReadingValue, WindowInfo};

const CREDENTIAL_TARGET: &str = "gemini:antigravity";
const USER_AGENT: &str = "antigravity/2.24.0";
const ENDPOINTS: [&str; 3] = [
    "https://daily-cloudcode-pa.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
    "https://daily-cloudcode-pa.sandbox.googleapis.com",
];

#[derive(Deserialize)]
struct AuthFile {
    token: TokenData,
}

#[derive(Deserialize)]
struct TokenData {
    access_token: Option<String>,
    expiry: Option<String>,
}

pub fn poll(_config: &Config) -> Reading {
    let Some(content) = crate::credentials::read_generic(CREDENTIAL_TARGET) else {
        return error("Нет входа Antigravity", State::Missing);
    };
    let auth: AuthFile = match serde_json::from_str(&content) {
        Ok(auth) => auth,
        Err(_) => return error("Не удалось разобрать вход Antigravity", State::Error),
    };
    let Some(token) = auth.token.access_token.filter(|token| !token.trim().is_empty()) else {
        return error("Нет токена Antigravity", State::Missing);
    };
    if auth
        .token
        .expiry
        .as_deref()
        .and_then(parse_iso8601)
        .is_some_and(|expiry| expiry <= SystemTime::now())
    {
        return error("Токен Antigravity истёк — откройте Antigravity", State::Auth);
    }

    let mut last_error = "Antigravity не вернул лимиты";
    for base in ENDPOINTS {
        match fetch_from(base, &token) {
            Ok(reading) => return reading,
            Err(PollError::Auth) => {
                return error("Требуется вход Antigravity", State::Auth);
            }
            Err(_) => last_error = "Не удалось запросить лимиты Antigravity",
        }
    }
    error(last_error, State::Error)
}

fn fetch_from(base: &str, token: &str) -> Result<Reading, PollError> {
    let headers = [
        ("Authorization", format!("Bearer {token}")),
        ("User-Agent", USER_AGENT.to_string()),
        ("Accept", "application/json".to_string()),
    ];
    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();

    let load_body = serde_json::json!({
        "metadata": { "ideType": "ANTIGRAVITY" }
    });
    let load: Value = http::post_json(
        &format!("{base}/v1internal:loadCodeAssist"),
        &header_refs,
        &load_body,
    )?;
    let project = load
        .get("cloudaicompanionProject")
        .and_then(Value::as_str)
        .filter(|project| !project.is_empty());

    if let Some(project) = project {
        let body = serde_json::json!({ "project": project });
        if let Ok(summary) = http::post_json::<Value, Value>(
            &format!("{base}/v1internal:retrieveUserQuotaSummary"),
            &header_refs,
            &body,
        ) {
            if let Some(reading) = reading_from_summary(&summary) {
                return Ok(reading);
            }
        }
    }

    let body = project
        .map(|project| serde_json::json!({ "project": project }))
        .unwrap_or_else(|| serde_json::json!({}));
    let models: Value = http::post_json(
        &format!("{base}/v1internal:fetchAvailableModels"),
        &header_refs,
        &body,
    )?;
    reading_from_models(&models).ok_or(PollError::Parse)
}

fn reading_from_summary(response: &Value) -> Option<Reading> {
    let groups = response.get("groups")?.as_array()?;
    let mut windows = Vec::new();
    for group in groups {
        let group_name = group
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or("Antigravity");
        let buckets = group.get("buckets").and_then(Value::as_array);
        if let Some(buckets) = buckets {
            for bucket in buckets {
                let remaining = bucket
                    .get("remainingFraction")
                    .and_then(Value::as_f64)
                    .map(|value| value.clamp(0.0, 1.0) * 100.0)?;
                let label = bucket
                    .get("displayName")
                    .and_then(Value::as_str)
                    .or_else(|| bucket.get("window").and_then(Value::as_str))
                    .unwrap_or(group_name);
                windows.push(WindowInfo {
                    label: label.to_string(),
                    remaining,
                    reset_at: bucket
                        .get("resetTime")
                        .and_then(Value::as_str)
                        .and_then(parse_iso8601),
                    limit_reached: remaining <= 0.0,
                });
            }
        }
    }
    reading_from_windows(windows)
}

fn reading_from_models(response: &Value) -> Option<Reading> {
    let models = response.get("models")?.as_object()?;
    let mut windows = Vec::new();
    for (name, info) in models {
        if !name.starts_with("gemini") {
            continue;
        }
        let Some(quota) = info.get("quotaInfo") else {
            continue;
        };
        let Some(remaining) = quota
            .get("remainingFraction")
            .and_then(Value::as_f64)
            .map(|value| value.clamp(0.0, 1.0) * 100.0)
        else {
            continue;
        };
        windows.push(WindowInfo {
            label: name.clone(),
            remaining,
            reset_at: quota
                .get("resetTime")
                .and_then(Value::as_str)
                .and_then(parse_iso8601),
            limit_reached: remaining <= 0.0,
        });
    }
    reading_from_windows(windows)
}

fn reading_from_windows(windows: Vec<WindowInfo>) -> Option<Reading> {
    if windows.is_empty() {
        return None;
    }
    let selected = windows
        .iter()
        .min_by(|left, right| {
            left.remaining
                .total_cmp(&right.remaining)
                .then_with(|| left.reset_at.cmp(&right.reset_at))
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
                "0% · лимит".to_string()
            } else {
                format!("осталось {}", format_percent(window.remaining))
            },
            reset: format_reset_detail(window.reset_at),
        });
    }
    Some(Reading {
        provider: ProviderId::Antigravity,
        value: ReadingValue::Percent(selected.remaining),
        reset_at: selected.reset_at,
        state,
        title: "Antigravity".to_string(),
        windows,
        detail,
    })
}

fn error(message: impl Into<String>, state: State) -> Reading {
    let mut reading = Reading::error(ProviderId::Antigravity, state, message);
    reading.title = "Antigravity".to_string();
    reading
}
