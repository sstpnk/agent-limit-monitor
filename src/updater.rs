use serde::Deserialize;
use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::http;
use crate::native_interop::wide_str;

const RELEASES_URL: &str =
    "https://api.github.com/repos/sstpnk/Claude-Code-Usage-Monitor/releases/latest";

#[derive(Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub html_url: String,
    pub name: Option<String>,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

pub fn install(release: &Release) -> Result<(), String> {
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name.eq_ignore_ascii_case("agent-limit-monitor.exe"))
        .or_else(|| {
            release
                .assets
                .iter()
                .find(|asset| asset.name.to_ascii_lowercase().ends_with(".exe"))
        })
        .ok_or_else(|| "В релизе нет exe-файла".to_string())?;

    let bytes = http::get_bytes(&asset.browser_download_url)
        .map_err(|_| "Не удалось скачать обновление".to_string())?;
    if bytes.len() < 64 * 1024 || bytes.get(0..2) != Some(b"MZ") {
        return Err("Файл обновления повреждён".to_string());
    }

    let current = std::env::current_exe().map_err(|_| "Не удалось найти текущий exe".to_string())?;
    let temp = std::env::temp_dir().join(format!(
        "agent-limit-monitor-{}-update.exe",
        std::process::id()
    ));
    std::fs::write(&temp, bytes).map_err(|_| "Не удалось сохранить обновление".to_string())?;

    let script = std::env::temp_dir().join(format!(
        "agent-limit-monitor-{}-update.cmd",
        std::process::id()
    ));
    let script_text = format!(
        "@echo off\r\n\
         ping 127.0.0.1 -n 3 >nul\r\n\
         copy /y \"{}\" \"{}\" >nul\r\n\
         start \"\" \"{}\"\r\n\
         del \"%~f0\"\r\n",
        temp.display(),
        current.display(),
        current.display()
    );
    std::fs::write(&script, script_text)
        .map_err(|_| "Не удалось создать скрипт обновления".to_string())?;

    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("cmd")
        .args(["/C", &script.to_string_lossy()])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|_| "Не удалось запустить обновление".to_string())?;
    Ok(())
}

pub fn check() -> Result<Option<Release>, String> {
    let headers = [
        ("User-Agent", "agent-limit-monitor"),
        ("Accept", "application/vnd.github+json"),
    ];
    let release: Release = http::get_json(RELEASES_URL, &headers)
        .map_err(|_| "Не удалось проверить обновления (нет релизов или сети)".to_string())?;
    if is_newer(env!("CARGO_PKG_VERSION"), &release.tag_name) {
        Ok(Some(release))
    } else {
        Ok(None)
    }
}

pub fn open_release(release: &Release) {
    let title = wide_str("open");
    let url = wide_str(&release.html_url);
    unsafe {
        let _ = ShellExecuteW(
            HWND::default(),
            PCWSTR::from_raw(title.as_ptr()),
            PCWSTR::from_raw(url.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn is_newer(current: &str, latest: &str) -> bool {
    let current = parse_version(current);
    let latest = parse_version(latest);
    latest > current
}

fn parse_version(value: &str) -> (u64, u64, u64) {
    let value = value.trim().trim_start_matches('v');
    let mut parts = value.split('.').map(|part| part.parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}
