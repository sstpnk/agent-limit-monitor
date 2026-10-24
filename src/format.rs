use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};

use crate::models::{Reading, Value};

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub fn unix_to_system_time(seconds: Option<i64>) -> Option<SystemTime> {
    let seconds = seconds?;
    if seconds <= 0 {
        return None;
    }
    Some(UNIX_EPOCH + std::time::Duration::from_secs(seconds as u64))
}

pub fn format_countdown(reset_at: Option<SystemTime>) -> Option<String> {
    let reset = reset_at?;
    let now = SystemTime::now();
    let remaining = match reset.duration_since(now) {
        Ok(duration) => duration.as_secs(),
        Err(_) => return Some("сейчас".to_string()),
    };
    Some(format_secs(remaining))
}

fn format_secs(total_secs: u64) -> String {
    let days = total_secs / 86400;
    if days > 0 {
        return format!("{days}д");
    }
    let hours = total_secs / 3600;
    if hours > 0 {
        return format!("{hours}ч");
    }
    let minutes = total_secs / 60;
    if minutes > 0 {
        return format!("{minutes}м");
    }
    format!("{total_secs}с")
}

pub fn format_percent(percent: f64) -> String {
    let percent = percent.clamp(0.0, 100.0);
    format!("{:.0}%", percent)
}

pub fn format_credits(credits: u64) -> String {
    let digits = credits.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

pub fn format_money(cents: i64, currency: &str) -> String {
    let code = currency.trim().to_ascii_uppercase();
    let symbol = match code.as_str() {
        "USD" => "$",
        "CNY" | "RMB" => "¥",
        "EUR" => "€",
        _ => "",
    };
    let sign = if cents < 0 { "-" } else { "" };
    let absolute = cents.unsigned_abs();
    let whole = absolute / 100;
    let fraction = absolute % 100;
    if symbol.is_empty() {
        format!("{sign}{code} {whole}.{fraction:02}")
    } else {
        format!("{sign}{symbol}{whole}.{fraction:02}")
    }
}

pub fn row_value(reading: &Reading) -> String {
    if reading.state.is_error() {
        return "—".to_string();
    }
    if matches!(reading.state, crate::models::State::Loading) {
        return "…".to_string();
    }
    match &reading.value {
        Value::Percent(percent) => format_percent(*percent),
        Value::Credits(credits) => format_credits(*credits),
        Value::Money { cents, currency } => format_money(*cents, currency),
    }
}

pub fn row_reset(reading: &Reading) -> String {
    if reading.state.is_error() || matches!(reading.state, crate::models::State::Loading) {
        return String::new();
    }
    match format_countdown(reading.reset_at) {
        Some(value) => format!("· {value}"),
        None => String::new(),
    }
}

pub fn format_reset_detail(reset_at: Option<SystemTime>) -> Option<String> {
    let reset = reset_at?;
    let remaining = reset.duration_since(SystemTime::now()).ok();
    if remaining.is_none() {
        return Some("сейчас".to_string());
    }
    let remaining = remaining?;
    let countdown = format_secs(remaining.as_secs());
    if remaining.as_secs() < 86400 {
        Some(format!("{} ({countdown})", local_time(reset)))
    } else {
        Some(format!("{} ({countdown})", local_date(reset)))
    }
}

fn timezone_bias_minutes() -> i64 {
    let mut info = TIME_ZONE_INFORMATION::default();
    let _ = unsafe { GetTimeZoneInformation(&mut info) };
    info.Bias as i64
}

fn local_parts(time: SystemTime) -> (i64, u32, u32, u32, u32) {
    let unix = time
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let local = unix - timezone_bias_minutes() * 60;
    let days = local.div_euclid(86400);
    let seconds_of_day = local.rem_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    let hour = (seconds_of_day / 3600) as u32;
    let minute = ((seconds_of_day % 3600) / 60) as u32;
    (year, month, day, hour, minute)
}

fn local_time(time: SystemTime) -> String {
    let (_, _, _, hour, minute) = local_parts(time);
    format!("{hour:02}:{minute:02}")
}

fn local_date(time: SystemTime) -> String {
    let (_, month, day, _, _) = local_parts(time);
    format!("{day:02}.{month:02}")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = (year - era * 400) as u64;
    let mp = if month > 2 { month - 3 } else { month + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + u64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// Parse the UTC timestamps returned by the provider APIs (`RFC 3339`).
pub fn parse_iso8601(value: &str) -> Option<SystemTime> {
    let value = value.trim();
    if value.len() < 10 {
        return None;
    }
    let date = &value[..10];
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut seconds = days_from_civil(year, month, day) * 86_400;

    if value.len() > 10 {
        let rest = value[10..].trim_start_matches(['T', ' ']);
        if rest.len() < 8 {
            return None;
        }
        let hour: i64 = rest[0..2].parse().ok()?;
        let minute: i64 = rest[3..5].parse().ok()?;
        let second: i64 = rest[6..8].parse().ok()?;
        seconds += hour * 3600 + minute * 60 + second;

        let mut offset = 0i64;
        let after = &rest[8..];
        if let Some(zone) = after.strip_prefix('+').or_else(|| after.strip_prefix('-')) {
            let sign = if after.starts_with('-') { -1 } else { 1 };
            let mut parts = zone.split(':');
            let hours: i64 = parts.next()?.parse().ok()?;
            let minutes: i64 = parts.next().unwrap_or("0").parse().ok()?;
            offset = sign * (hours * 3600 + minutes * 60);
        }
        seconds -= offset;
    }

    if seconds >= 0 {
        UNIX_EPOCH.checked_add(std::time::Duration::from_secs(seconds as u64))
    } else {
        UNIX_EPOCH.checked_sub(std::time::Duration::from_secs((-seconds) as u64))
    }
}
