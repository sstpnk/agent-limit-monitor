use serde::Deserialize;

use crate::config::Config;
use crate::format::format_money;
use crate::http;
use crate::models::{DetailLine, PollError, ProviderId, Reading, State, Value};

const BALANCE_URL: &str = "https://api.deepseek.com/user/balance";
const PLATFORM_URL: &str = "https://platform.deepseek.com/api/v0/users/get_user_summary";

#[derive(Deserialize)]
struct BalanceResponse {
    is_available: Option<bool>,
    #[serde(default)]
    balance_infos: Vec<BalanceInfo>,
}

#[derive(Deserialize, Clone)]
struct BalanceInfo {
    currency: Option<String>,
    total_balance: Option<String>,
    granted_balance: Option<String>,
    topped_up_balance: Option<String>,
}

#[derive(Deserialize)]
struct PlatformResponse {
    data: Option<PlatformEnvelope>,
}

#[derive(Deserialize)]
struct PlatformEnvelope {
    biz_data: Option<PlatformBiz>,
}

#[derive(Deserialize)]
struct PlatformBiz {
    #[serde(default)]
    normal_wallets: Vec<PlatformWallet>,
    #[serde(default)]
    bonus_wallets: Vec<PlatformWallet>,
}

#[derive(Deserialize)]
struct PlatformWallet {
    currency: Option<String>,
    balance: Option<String>,
}

pub fn poll(config: &Config) -> Reading {
    if config.providers.deepseek.use_platform_token {
        if let Some(token) = platform_token() {
            match fetch_platform(&token) {
                Ok(reading) => return reading,
                Err(error) => crate::diagnose::log(format!("DeepSeek platform failed: {error:?}")),
            }
        }
    }

    let Some(key) = config.deepseek_key() else {
        return error("Ключ не задан: DEEPSEEK_API_KEY или вход в DeepSeek Harness");
    };
    fetch_official(config, &key)
}

fn platform_token() -> Option<String> {
    let mut paths = Vec::new();
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".dsh").join(".credentials.yaml"));
    }
    if let Ok(home) = std::env::var("DSH_HOME") {
        if !home.trim().is_empty() {
            paths.push(std::path::PathBuf::from(home).join(".credentials.yaml"));
        }
    }

    for path in paths {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(start) = content.find("deepseek-account-platform/default:") else {
            continue;
        };
        let rest = &content[start..];
        for line in rest.lines() {
            let line = line.trim();
            if line.starts_with("kind:") && !line.contains("grant") {
                break;
            }
            if let Some(token) = line.strip_prefix("token:") {
                let token = token.trim();
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
            if line.starts_with("deepseek-account-platform/") && !line.contains("default:") {
                break;
            }
        }
    }
    None
}

fn fetch_platform(token: &str) -> Result<Reading, PollError> {
    let headers = [
        ("x-dsh-auth-token", token.to_string()),
        ("x-client-platform", "web".to_string()),
        ("x-client-version", "1.0.0".to_string()),
        ("Accept", "application/json".to_string()),
    ];
    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    let response: PlatformResponse = http::get_json(PLATFORM_URL, &header_refs)?;
    let biz = response
        .data
        .and_then(|data| data.biz_data)
        .ok_or(PollError::Parse)?;

    let currency = biz
        .normal_wallets
        .iter()
        .chain(biz.bonus_wallets.iter())
        .find_map(|wallet| wallet.currency.clone())
        .unwrap_or_else(|| "USD".to_string())
        .trim()
        .to_ascii_uppercase();
    let normal = wallet_total(&biz.normal_wallets, &currency);
    let bonus = wallet_total(&biz.bonus_wallets, &currency);
    let cents = normal + bonus;

    Ok(money_reading(
        cents,
        normal,
        bonus,
        &currency,
        &format!("DeepSeek · {currency}"),
    ))
}

fn wallet_total(wallets: &[PlatformWallet], currency: &str) -> i64 {
    wallets
        .iter()
        .filter(|wallet| {
            wallet
                .currency
                .as_deref()
                .map(|value| value.eq_ignore_ascii_case(currency))
                .unwrap_or(true)
        })
        .filter_map(|wallet| wallet.balance.as_deref().and_then(parse_cents))
        .sum()
}

fn fetch_official(config: &Config, key: &str) -> Reading {
    let headers = [
        ("Authorization", format!("Bearer {key}")),
        ("Accept", "application/json".to_string()),
    ];
    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();

    let response: BalanceResponse = match http::get_json(BALANCE_URL, &header_refs) {
        Ok(response) => response,
        Err(PollError::Auth) => return error("Ключ DeepSeek не принят"),
        Err(_) => return error("Не удалось запросить баланс DeepSeek"),
    };

    let preferred = config.providers.deepseek.currency.trim().to_ascii_uppercase();
    let info = response
        .balance_infos
        .iter()
        .find(|info| {
            info.currency
                .as_deref()
                .map(|currency| currency.eq_ignore_ascii_case(&preferred))
                .unwrap_or(false)
        })
        .or_else(|| {
            response.balance_infos.iter().find(|info| {
                info.currency
                    .as_deref()
                    .map(|currency| currency.eq_ignore_ascii_case("USD"))
                    .unwrap_or(false)
            })
        })
        .or_else(|| response.balance_infos.first());

    let Some(info) = info else {
        return error("DeepSeek не вернул баланс");
    };
    let currency = info
        .currency
        .as_deref()
        .unwrap_or("USD")
        .trim()
        .to_ascii_uppercase();

    let topped = info.topped_up_balance.as_deref().and_then(parse_cents);
    let granted = info.granted_balance.as_deref().and_then(parse_cents);
    let total = info.total_balance.as_deref().and_then(parse_cents);
    let cents = match (topped, granted) {
        (Some(topped), Some(granted)) => topped + granted,
        _ => match total {
            Some(total) => total,
            None => return error("Не удалось разобрать баланс DeepSeek"),
        },
    };

    let available = response.is_available.unwrap_or(true);
    let state = state_for(cents, available);
    let detail = balance_detail(topped, granted, cents, &currency, available);

    Reading {
        provider: ProviderId::DeepSeek,
        value: Value::Money {
            cents,
            currency: currency.clone(),
        },
        reset_at: None,
        state,
        title: format!("DeepSeek · {currency}"),
        windows: Vec::new(),
        detail,
    }
}

fn balance_detail(
    normal: Option<i64>,
    bonus: Option<i64>,
    cents: i64,
    currency: &str,
    available: bool,
) -> Vec<DetailLine> {
    let normal = normal.filter(|value| *value > 0);
    let bonus = bonus.filter(|value| *value > 0);
    let mut detail = Vec::new();
    if let Some(normal) = normal {
        detail.push(DetailLine {
            label: crate::i18n::s().topped_up.to_string(),
            value: format_money(normal, currency),
            reset: None,
        });
    }
    if let Some(bonus) = bonus {
        detail.push(DetailLine {
            label: crate::i18n::s().bonus.to_string(),
            value: format_money(bonus, currency),
            reset: None,
        });
    }
    if normal.is_some() && bonus.is_some() {
        detail.push(DetailLine {
            label: crate::i18n::s().total.to_string(),
            value: format_money(cents, currency),
            reset: None,
        });
    } else if normal.is_none() && bonus.is_none() {
        detail.push(DetailLine {
            label: crate::i18n::s().balance.to_string(),
            value: format_money(cents, currency),
            reset: None,
        });
    }
    if !available {
        detail.push(DetailLine {
            label: crate::i18n::s().status.to_string(),
            value: crate::i18n::s().insufficient.to_string(),
            reset: None,
        });
    }
    detail
}

fn money_reading(
    cents: i64,
    normal: i64,
    bonus: i64,
    currency: &str,
    title: &str,
) -> Reading {
    let detail = balance_detail(Some(normal), Some(bonus), cents, currency, true);
    Reading {
        provider: ProviderId::DeepSeek,
        value: Value::Money {
            cents,
            currency: currency.to_string(),
        },
        reset_at: None,
        state: state_for(cents, true),
        title: title.to_string(),
        windows: Vec::new(),
        detail,
    }
}

fn state_for(cents: i64, available: bool) -> State {
    if !available || cents <= 0 {
        State::Critical
    } else if cents <= 100 {
        State::Warning
    } else {
        State::Ok
    }
}

fn parse_cents(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let negative = value.starts_with('-');
    let value = value.trim_start_matches(['-', '+']);
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let whole: i64 = whole.parse().ok()?;
    let mut fraction = fraction.chars().take(2).collect::<String>();
    while fraction.len() < 2 {
        fraction.push('0');
    }
    let fraction: i64 = fraction.parse().ok()?;
    let cents = whole * 100 + fraction;
    Some(if negative { -cents } else { cents })
}

fn error(message: impl Into<String>) -> Reading {
    let mut reading = Reading::error(ProviderId::DeepSeek, State::Missing, message);
    reading.title = "DeepSeek".to_string();
    reading
}
