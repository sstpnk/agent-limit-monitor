use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::config::Config;
use crate::format::{format_credits, format_reset_detail, now_unix};
use crate::http;
use crate::models::{DetailLine, PollError, ProviderId, Reading, State, Value};

const APP_ID: &str = "100003";
const APP_KEY: &str = "38d2391985e2369a5fb8227d8e6cd5e5";
const API_VERSION: &str = "1.9.1";
const WALLET_URL: &str =
    "https://autoglm-api.autoglm.ai/agent-assetmgr/api/v2/wallets?biz_app_id=autoclaw";
const LEDGER_URL: &str =
    "https://autoglm-api.autoglm.ai/agent-assetmgr/api/v1/ledgers_std?asset_type=point&wallet_type=all";
const REFRESH_URL: &str = "https://autoglm-api.autoglm.ai/userapi/v1/refresh";

#[repr(C)]
struct DataBlob {
    cb_data: u32,
    pb_data: *mut u8,
}

#[link(name = "Crypt32")]
extern "system" {
    fn CryptProtectData(
        data_in: *const DataBlob,
        description: *const u16,
        entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;
    fn CryptUnprotectData(
        data_in: *const DataBlob,
        description: *mut *mut u16,
        entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(handle: *mut c_void) -> *mut c_void;
}

#[derive(Clone, Debug)]
struct TokenData {
    access_token: String,
    refresh_token: Option<String>,
    device_id: Option<String>,
    source_id: String,
    expires_at: i64,
}

#[derive(Deserialize)]
struct CredentialsFile {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct CachedToken {
    access_token: String,
    refresh_token: Option<String>,
    device_id: Option<String>,
    source_id: String,
    expires_at: i64,
}

#[derive(Deserialize)]
struct WalletResponse {
    code: i64,
    data: Option<WalletData>,
}

#[derive(Deserialize)]
struct WalletData {
    total_balance: i64,
    #[serde(default)]
    wallets: Vec<Wallet>,
}

#[derive(Deserialize)]
struct Wallet {
    display_name: Option<String>,
    balance: i64,
    public_wallet_type: Option<String>,
}

#[derive(Deserialize)]
struct LedgerResponse {
    data: Option<LedgerData>,
}

#[derive(Deserialize)]
struct LedgerData {
    #[serde(default)]
    entries: Vec<LedgerEntry>,
}

#[derive(Deserialize)]
struct LedgerEntry {
    wallet_expires_at: Option<i64>,
}

#[derive(Serialize)]
struct RefreshBody<'a> {
    source_id: &'a str,
    device_id: &'a str,
    refresh_token: &'a str,
}

#[derive(Deserialize)]
struct RefreshResponse {
    code: i64,
    data: Option<RefreshData>,
}

#[derive(Deserialize)]
struct RefreshData {
    access_token: String,
    refresh_token: Option<String>,
}

static LEDGER_CACHE: Mutex<Option<(Instant, Option<SystemTime>)>> = Mutex::new(None);
static TRACE_COUNTER: AtomicU64 = AtomicU64::new(1);
const LEDGER_CACHE_TTL: Duration = Duration::from_secs(3600);

pub fn poll(config: &Config) -> Reading {
    let Some(root) = auto_claw_root() else {
        return error(
            "AutoClaw не найден — установите и войдите в приложение",
            "Z.AI AutoClaw",
        );
    };

    let token = match read_token(&root, config) {
        Ok(token) => token,
        Err(message) => return error(message, "Z.AI AutoClaw"),
    };

    let headers = signed_headers(Some(&token.access_token));
    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();

    let response: WalletResponse = match http::get_json(WALLET_URL, &header_refs) {
        Ok(response) => response,
        Err(PollError::Auth) => {
            return error("Токен не принят — откройте AutoClaw", "Z.AI AutoClaw");
        }
        Err(_) => return error("Не удалось запросить кредиты Z.AI", "Z.AI AutoClaw"),
    };
    if response.code != 0 {
        return error("Z.AI вернул ошибку кошелька", "Z.AI AutoClaw");
    }
    let Some(data) = response.data else {
        return error("Z.AI не вернул кошелёк", "Z.AI AutoClaw");
    };

    let total = data.total_balance.max(0) as u64;
    let expiry = if config.providers.zai.show_expiry {
        ledger_expiry(&header_refs)
    } else {
        None
    };

    let mut detail = Vec::new();
    let mut components = 0;
    for wallet in data.wallets.iter().filter(|wallet| wallet.balance > 0) {
        let label = wallet
            .display_name
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .or(wallet.public_wallet_type.as_deref())
            .unwrap_or("Wallet")
            .to_string();
        detail.push(DetailLine {
            label,
            value: format_credits(wallet.balance.max(0) as u64),
            reset: None,
        });
        components += 1;
    }
    if detail.is_empty() {
        detail.push(DetailLine {
            label: "Кредиты".to_string(),
            value: format_credits(total),
            reset: None,
        });
        components = 1;
    }
    let expiry_text = expiry.and_then(|time| format_reset_detail(Some(time)));
    if components > 1 {
        detail.push(DetailLine {
            label: "Итого".to_string(),
            value: format_credits(total),
            reset: expiry_text,
        });
    } else if let Some(line) = detail.last_mut() {
        line.reset = expiry_text;
    }

    let state = if total == 0 {
        State::Critical
    } else if total < 500 {
        State::Warning
    } else {
        State::Ok
    };

    Reading {
        provider: ProviderId::Zai,
        value: Value::Credits(total),
        reset_at: expiry,
        state,
        title: "Z.AI AutoClaw · Reward Points".to_string(),
        windows: Vec::new(),
        detail,
    }
}

fn read_token(root: &Path, config: &Config) -> Result<TokenData, String> {
    let account_dir = account_dir(root, &config.providers.zai.account)
        .ok_or_else(|| "Не найден профиль AutoClaw".to_string())?;
    let key = local_state_key(root).ok_or_else(|| "Не удалось прочитать ключ AutoClaw".to_string())?;
    let credentials_path = account_dir.join("account-credentials.enc");
    let blob = std::fs::read(&credentials_path)
        .map_err(|_| "Не удалось прочитать credentials AutoClaw".to_string())?;
    let plaintext = decrypt_credentials(&key, &blob)
        .ok_or_else(|| "Не удалось расшифровать credentials AutoClaw".to_string())?;
    let credentials: CredentialsFile = serde_json::from_slice(&plaintext).map_err(|error| {
        crate::diagnose::log(format!(
            "AutoClaw credentials parse failed: {error}; plaintext_len={}",
            plaintext.len()
        ));
        "Неизвестный формат credentials AutoClaw".to_string()
    })?;

    let access = strip_bearer(&credentials.access_token);
    let claims = jwt_claims(&access);
    let expires_at = claim_i64(&claims, "exp").unwrap_or_else(|| now_unix() + 86_400);

    let mut token = TokenData {
        access_token: access,
        refresh_token: credentials.refresh_token,
        device_id: claim_str(&claims, "device_id"),
        source_id: claim_str(&claims, "source_id").unwrap_or_else(|| "autoclaw".to_string()),
        expires_at,
    };

    if token.expires_at > now_unix() + 60 {
        return Ok(token);
    }

    let cached = load_cached_token();
    if let Some(cached) = cached.as_ref().filter(|cached| cached.expires_at > now_unix() + 60) {
        return Ok(cached.clone());
    }

    if !config.providers.zai.refresh_token {
        return Err("Токен истёк — откройте AutoClaw".to_string());
    }

    let refresh_token = token
        .refresh_token
        .clone()
        .or_else(|| cached.as_ref().and_then(|cached| cached.refresh_token.clone()))
        .ok_or_else(|| "Нет refresh_token AutoClaw".to_string())?;
    let device_id = token
        .device_id
        .clone()
        .or_else(|| cached.as_ref().and_then(|cached| cached.device_id.clone()))
        .ok_or_else(|| "Нет device_id AutoClaw".to_string())?;

    let refreshed = refresh_access_token(&token.source_id, &device_id, &refresh_token)
        .map_err(|_| "Не удалось обновить токен AutoClaw".to_string())?;
    token.access_token = refreshed.access_token;
    token.refresh_token = refreshed
        .refresh_token
        .or(Some(refresh_token))
        .filter(|value| !value.is_empty());
    token.device_id = Some(device_id);
    token.expires_at = jwt_claims(&token.access_token)
        .and_then(|claims| claim_i64_value(&claims, "exp"))
        .unwrap_or_else(|| now_unix() + 86_400);
    save_cached_token(&token);
    Ok(token)
}

fn refresh_access_token(
    source_id: &str,
    device_id: &str,
    refresh_token: &str,
) -> Result<RefreshData, PollError> {
    let headers = signed_headers(None);
    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    let body = RefreshBody {
        source_id,
        device_id,
        refresh_token,
    };
    let response: RefreshResponse = http::post_json(REFRESH_URL, &header_refs, &body)?;
    if response.code != 0 {
        return Err(PollError::Auth);
    }
    response.data.ok_or(PollError::Parse)
}

fn account_dir(root: &Path, requested: &str) -> Option<PathBuf> {
    let accounts = root.join("accounts");
    if requested != "auto" && !requested.trim().is_empty() {
        let path = accounts.join(requested.trim());
        return path.is_dir().then_some(path);
    }
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&accounts).ok()?.flatten() {
        let path = entry.path();
        if !path.join("account-credentials.enc").is_file() {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        if best
            .as_ref()
            .map(|(current, _)| modified > *current)
            .unwrap_or(true)
        {
            best = Some((modified, path));
        }
    }
    best.map(|(_, path)| path)
}

fn auto_claw_root() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    for name in ["AutoClaw-oversea-official", "AutoClaw"] {
        let path = PathBuf::from(&appdata).join(name);
        if path.is_dir() {
            return Some(path);
        }
    }
    None
}

fn local_state_key(root: &Path) -> Option<Vec<u8>> {
    let text = std::fs::read_to_string(root.join("Local State")).ok()?;
    let state: JsonValue = serde_json::from_str(&text).ok()?;
    let encoded = state
        .get("os_crypt")?
        .get("encrypted_key")?
        .as_str()?;
    let blob = STANDARD.decode(encoded).ok()?;
    let blob = blob.strip_prefix(b"DPAPI")?;
    dpapi_unprotect(blob)
}

fn decrypt_credentials(key: &[u8], blob: &[u8]) -> Option<Vec<u8>> {
    if blob.len() < 3 + 12 + 16 {
        return None;
    }
    let prefix = &blob[..3];
    if prefix != b"v10" && prefix != b"v11" {
        return None;
    }
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let nonce = Nonce::from_slice(&blob[3..15]);
    cipher.decrypt(nonce, &blob[15..]).ok()
}

fn jwt_claims(token: &str) -> Option<JsonValue> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| STANDARD.decode(payload))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn claim_i64(claims: &Option<JsonValue>, key: &str) -> Option<i64> {
    claims.as_ref()?.get(key)?.as_i64()
}

fn claim_i64_value(claims: &JsonValue, key: &str) -> Option<i64> {
    claims.get(key)?.as_i64()
}

fn claim_str(claims: &Option<JsonValue>, key: &str) -> Option<String> {
    claims
        .as_ref()?
        .get(key)?
        .as_str()
        .map(|value| value.to_string())
}

fn signed_headers(token: Option<&str>) -> Vec<(&'static str, String)> {
    let timestamp = now_unix().to_string();
    let mut hasher = Md5::new();
    hasher.update(format!("{APP_ID}&{timestamp}&{APP_KEY}"));
    let sign = format!("{:x}", hasher.finalize());
    let mut headers = vec![
        ("X-Auth-Appid", APP_ID.to_string()),
        ("X-Auth-TimeStamp", timestamp),
        ("X-Auth-Sign", sign),
        ("X-Product", "autoclaw".to_string()),
        ("X-Version", API_VERSION.to_string()),
        ("X-Tm", "win".to_string()),
        ("X-Trace-Id", trace_id()),
        ("Content-Type", "application/json".to_string()),
    ];
    if let Some(token) = token {
        headers.push(("authorization", format!("Bearer {token}")));
    }
    headers
}

fn trace_id() -> String {
    let counter = TRACE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:016x}{:016x}", now_unix() as u64, counter)
}

fn ledger_expiry(headers: &[(&str, &str)]) -> Option<SystemTime> {
    {
        let cache = LEDGER_CACHE.lock().unwrap_or_else(|error| error.into_inner());
        if let Some((checked, value)) = cache.as_ref() {
            if checked.elapsed() < LEDGER_CACHE_TTL {
                return *value;
            }
        }
    }

    let response: Option<LedgerResponse> = http::get_json(LEDGER_URL, headers).ok();
    let now = now_unix();
    let mut expiry = None;
    if let Some(response) = response {
        if let Some(data) = response.data {
            for entry in &data.entries {
                if let Some(timestamp) = entry.wallet_expires_at.filter(|value| *value > now) {
                    expiry = Some(timestamp);
                    break;
                }
            }
            if expiry.is_none() {
                expiry = data
                    .entries
                    .iter()
                    .filter_map(|entry| entry.wallet_expires_at)
                    .filter(|value| *value > now)
                    .max();
            }
        }
    }
    let value = expiry.and_then(|timestamp| {
        (timestamp > 0).then(|| UNIX_EPOCH + Duration::from_secs(timestamp as u64))
    });

    let mut cache = LEDGER_CACHE.lock().unwrap_or_else(|error| error.into_inner());
    *cache = Some((Instant::now(), value));
    value
}

fn strip_bearer(token: &str) -> String {
    token
        .trim()
        .strip_prefix("Bearer ")
        .unwrap_or(token.trim())
        .to_string()
}

fn cache_path() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(
        PathBuf::from(appdata)
            .join("AgentLimitMonitor")
            .join("zai-token.bin"),
    )
}

fn load_cached_token() -> Option<TokenData> {
    let path = cache_path()?;
    let blob = std::fs::read(path).ok()?;
    let plaintext = dpapi_unprotect(&blob)?;
    let cached: CachedToken = serde_json::from_slice(&plaintext).ok()?;
    Some(TokenData {
        access_token: cached.access_token,
        refresh_token: cached.refresh_token,
        device_id: cached.device_id,
        source_id: cached.source_id,
        expires_at: cached.expires_at,
    })
}

fn save_cached_token(token: &TokenData) {
    let Some(path) = cache_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let cached = CachedToken {
        access_token: token.access_token.clone(),
        refresh_token: token.refresh_token.clone(),
        device_id: token.device_id.clone(),
        source_id: token.source_id.clone(),
        expires_at: token.expires_at,
    };
    let Ok(json) = serde_json::to_vec(&cached) else {
        return;
    };
    if let Some(protected) = dpapi_protect(&json) {
        let _ = std::fs::write(path, protected);
    }
}

fn dpapi_unprotect(data: &[u8]) -> Option<Vec<u8>> {
    let input = DataBlob {
        cb_data: data.len() as u32,
        pb_data: data.as_ptr() as *mut u8,
    };
    let mut output = DataBlob {
        cb_data: 0,
        pb_data: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            &mut output,
        )
    };
    if ok == 0 || output.pb_data.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(output.pb_data, output.cb_data as usize) };
    let result = bytes.to_vec();
    unsafe {
        LocalFree(output.pb_data as *mut c_void);
    }
    Some(result)
}

fn dpapi_protect(data: &[u8]) -> Option<Vec<u8>> {
    let input = DataBlob {
        cb_data: data.len() as u32,
        pb_data: data.as_ptr() as *mut u8,
    };
    let mut output = DataBlob {
        cb_data: 0,
        pb_data: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            &mut output,
        )
    };
    if ok == 0 || output.pb_data.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(output.pb_data, output.cb_data as usize) };
    let result = bytes.to_vec();
    unsafe {
        LocalFree(output.pb_data as *mut c_void);
    }
    Some(result)
}

fn error(message: impl Into<String>, title: &str) -> Reading {
    let mut reading = Reading::error(ProviderId::Zai, State::Missing, message);
    reading.title = title.to_string();
    reading
}
