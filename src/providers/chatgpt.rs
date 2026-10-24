use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::SystemTime;

use serde::Deserialize;
use std::os::windows::process::CommandExt;

use crate::config::Config;
use crate::format::{format_percent, format_reset_detail, unix_to_system_time};
use crate::http;
use crate::models::{DetailLine, PollError, ProviderId, Reading, State, Value, WindowInfo};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

#[derive(Deserialize)]
struct AuthFile {
    tokens: Option<Tokens>,
    #[serde(rename = "OPENAI_API_KEY")]
    openai_api_key: Option<String>,
}

#[derive(Deserialize)]
struct Tokens {
    access_token: Option<String>,
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct WhamResponse {
    plan_type: Option<String>,
    rate_limit: Option<RateLimit>,
}

#[derive(Deserialize)]
struct RateLimit {
    limit_reached: Option<bool>,
    primary_window: Option<RateWindow>,
    secondary_window: Option<RateWindow>,
}

#[derive(Deserialize)]
struct RateWindow {
    used_percent: Option<f64>,
    limit_window_seconds: Option<i64>,
    reset_at: Option<i64>,
}

pub fn poll(config: &Config) -> Reading {
    let Some(path) = auth_path(config) else {
        return error("Нет пути к auth.json", State::Missing);
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => return error(format!("Нет входа Codex: {}", path.display()), State::Missing),
    };
    let auth: AuthFile = match serde_json::from_str(&text) {
        Ok(auth) => auth,
        Err(_) => return error("Не удалось прочитать auth.json", State::Error),
    };

    let Some(tokens) = auth.tokens else {
        if auth
            .openai_api_key
            .as_deref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
        {
            return error("API-ключ: остаток OpenAI не отдаётся", State::Missing);
        }
        return error("Нет входа Codex — запустите codex login", State::Auth);
    };
    let Some(access_token) = tokens
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return error("Нет входа Codex — запустите codex login", State::Auth);
    };

    let account_id = tokens
        .account_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let mut headers = vec![
        ("Authorization", format!("Bearer {access_token}")),
        ("Accept", "application/json".to_string()),
        ("User-Agent", "node".to_string()),
    ];
    if let Some(account_id) = account_id.as_deref() {
        headers.push(("ChatGPT-Account-Id", account_id.to_string()));
    }
    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();

    let response: WhamResponse = match http::get_json(USAGE_URL, &header_refs) {
        Ok(response) => response,
        Err(PollError::Auth) => match node_usage(access_token, account_id.as_deref()) {
            Ok(response) => response,
            Err(()) => {
                return error(
                    "Cloudflare отклонил запрос ChatGPT; откройте Codex или повторите позже",
                    State::Error,
                );
            }
        },
        Err(_) => return error("Не удалось запросить лимиты ChatGPT", State::Error),
    };

    let plan = response
        .plan_type
        .as_deref()
        .unwrap_or("unknown")
        .to_string();
    let plan_label = if plan.eq_ignore_ascii_case("free") {
        crate::i18n::s().free
    } else {
        plan.as_str()
    };
    let rate = match response.rate_limit {
        Some(rate) => rate,
        None => return error("ChatGPT не вернул лимиты", State::Error),
    };
    let limit_reached = rate.limit_reached.unwrap_or(false);

    let mut windows = Vec::new();
    for window in [rate.primary_window, rate.secondary_window].into_iter().flatten() {
        let used = window.used_percent.unwrap_or(0.0).clamp(0.0, 100.0);
        let remaining = (100.0 - used).clamp(0.0, 100.0);
        let reset_at = unix_to_system_time(window.reset_at);
        windows.push(WindowInfo {
            label: window_label(window.limit_window_seconds),
            remaining,
            reset_at,
            limit_reached: limit_reached || remaining <= 0.0,
        });
    }

    if windows.is_empty() {
        return error("ChatGPT не вернул ни одного окна лимита", State::Error);
    }

    let selected = select_window(&windows, &config.providers.chatgpt.window)
        .cloned()
        .unwrap_or_else(|| windows[0].clone());

    let mut detail = Vec::new();
    for window in &windows {
        let reset = format_reset_detail(window.reset_at);
        let value = if window.limit_reached {
            format!("0% · {}", crate::i18n::s().limit)
        } else {
            format!("{} {}", crate::i18n::s().remaining, format_percent(window.remaining))
        };
        detail.push(DetailLine {
            label: window.label.clone(),
            value,
            reset,
        });
    }

    let state = if selected.limit_reached || selected.remaining <= 0.0 {
        State::Critical
    } else if selected.remaining <= 25.0 {
        State::Warning
    } else {
        State::Ok
    };

    Reading {
        provider: ProviderId::ChatGpt,
        value: Value::Percent(selected.remaining),
        reset_at: selected.reset_at,
        state,
        title: format!("ChatGPT (Codex) · {plan_label}"),
        windows,
        detail,
    }
}

fn select_window<'a>(windows: &'a [WindowInfo], mode: &str) -> Option<&'a WindowInfo> {
    match mode {
        "primary" => windows.first(),
        "secondary" => windows.get(1).or_else(|| windows.first()),
        "nearest_reset" => windows.iter().min_by_key(|window| window.reset_at),
        _ => {
            let now = SystemTime::now();
            let expired = windows.iter().find(|window| {
                window.limit_reached
                    || window
                        .reset_at
                        .map(|reset| reset <= now)
                        .unwrap_or(false)
            });
            if let Some(window) = expired {
                return Some(window);
            }
            windows.iter().min_by(|left, right| {
                left.remaining
                    .total_cmp(&right.remaining)
                    .then_with(|| left.reset_at.cmp(&right.reset_at))
            })
        }
    }
}

fn window_label(seconds: Option<i64>) -> String {
    let Some(seconds) = seconds.filter(|seconds| *seconds > 0) else {
        return crate::i18n::s().limit.to_string();
    };
    if seconds <= 6 * 3600 {
        format!("5{}", crate::i18n::s().unit_hour)
    } else if seconds <= 2 * 86400 {
        format!("1{}", crate::i18n::s().unit_day)
    } else if seconds <= 8 * 86400 {
        format!("7{}", crate::i18n::s().unit_day)
    } else if seconds <= 31 * 86400 {
        format!("30{}", crate::i18n::s().unit_day)
    } else {
        format!("{}{}", seconds / 86400, crate::i18n::s().unit_day)
    }
}

fn auth_path(config: &Config) -> Option<PathBuf> {
    let configured = config.providers.chatgpt.auth_path.trim();
    if !configured.is_empty() {
        return Some(expand_home(configured));
    }
    if let Ok(codex_home) = std::env::var("CODEX_HOME") {
        let codex_home = codex_home.trim();
        if !codex_home.is_empty() {
            return Some(PathBuf::from(codex_home).join("auth.json"));
        }
    }
    dirs::home_dir().map(|home| home.join(".codex").join("auth.json"))
}

fn expand_home(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

fn error(message: impl Into<String>, state: State) -> Reading {
    let mut reading = Reading::error(ProviderId::ChatGpt, state, message);
    reading.title = "ChatGPT (Codex)".to_string();
    reading
}

const NODE_SCRIPT: &str = r#"
const https = require('https');
const token = process.env.ALM_TOK;
const account = process.env.ALM_ACC || '';
const req = https.get({
  hostname: 'chatgpt.com',
  path: '/backend-api/wham/usage',
  headers: {
    'Authorization': 'Bearer ' + token,
    'ChatGPT-Account-Id': account,
    'User-Agent': 'node',
    'Accept': 'application/json'
  }
}, (res) => {
  let data = '';
  res.on('data', (chunk) => data += chunk);
  res.on('end', () => {
    if (res.statusCode !== 200) { process.exit(2); }
    process.stdout.write(data);
  });
});
req.setTimeout(10000, () => { req.destroy(); process.exit(3); });
req.on('error', () => process.exit(4));
"#;

fn node_usage(access_token: &str, account_id: Option<&str>) -> Result<WhamResponse, ()> {
    let node = node_path();
    let mut command = Command::new(node);
    command
        .arg("-e")
        .arg(NODE_SCRIPT)
        .env("ALM_TOK", access_token)
        .env("ALM_ACC", account_id.unwrap_or(""))
        .creation_flags(0x0800_0000)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let output = command.output().map_err(|_| ())?;
    if !output.status.success() {
        return Err(());
    }
    serde_json::from_slice(&output.stdout).map_err(|_| ())
}

fn node_path() -> PathBuf {
    let candidates = [
        r"C:\Program Files\nodejs\node.exe",
        r"C:\Program Files (x86)\nodejs\node.exe",
    ];
    for candidate in candidates {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from("node")
}
