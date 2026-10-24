use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::Deserialize;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::http;
use crate::native_interop::wide_str;

const RELEASES_URL: &str =
    "https://api.github.com/repos/sstpnk/agent-limit-monitor/releases/latest";

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
    let temp = current.with_extension("new.exe");
    let marker = current.with_extension("new.update");
    let pid = std::process::id();
    std::fs::write(
        &marker,
        format!("{}\n{pid}\n", current.display()),
    )
    .map_err(|_| "Не удалось сохранить параметры обновления".to_string())?;
    std::fs::write(&temp, bytes).map_err(|_| "Не удалось сохранить обновление".to_string())?;

    let operation = wide_str("open");
    let file = wide_str(&temp.to_string_lossy());
    let result = unsafe {
        ShellExecuteW(
            HWND::default(),
            PCWSTR::from_raw(operation.as_ptr()),
            PCWSTR::from_raw(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        return Err("Не удалось запустить обновление".to_string());
    }
    Ok(())
}

/// Runs in the freshly downloaded exe: wait for the old process, replace the
/// installed file and start it again. This avoids keeping a cmd/PowerShell
/// helper alive after the parent exits.
pub fn apply_update(target: &str, pid: u32) {
    let target = PathBuf::from(target);
    for _ in 0..120 {
        if !process_alive(pid) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    std::thread::sleep(std::time::Duration::from_millis(400));

    let Ok(self_exe) = std::env::current_exe() else {
        return;
    };
    let mut copied = false;
    for _ in 0..20 {
        if std::fs::copy(&self_exe, &target).is_ok() {
            copied = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    if copied {
        let _ = std::fs::remove_file(target.with_extension("new.update"));
        let _ = Command::new(&target).spawn();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let _ = std::fs::remove_file(self_exe);
    }
}

fn process_alive(pid: u32) -> bool {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
        if let Ok(handle) = handle {
            let _ = CloseHandle(handle);
            true
        } else {
            false
        }
    }
}

pub fn check() -> Result<Option<Release>, String> {
    let headers = [
        ("User-Agent", "agent-limit-monitor"),
        ("Accept", "application/vnd.github+json"),
    ];
    let release: Release = match http::get_json(RELEASES_URL, &headers) {
        Ok(release) => release,
        Err(error) => {
            crate::diagnose::log(format!("GitHub API failed ({error:?}); trying Node"));
            node_latest_release()?
        }
    };
    if is_newer(env!("CARGO_PKG_VERSION"), &release.tag_name) {
        Ok(Some(release))
    } else {
        Ok(None)
    }
}

fn node_latest_release() -> Result<Release, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = r#"
const https=require('https');
const req=https.get('https://api.github.com/repos/sstpnk/agent-limit-monitor/releases/latest',{headers:{'User-Agent':'agent-limit-monitor','Accept':'application/vnd.github+json'}},res=>{
 let body=''; res.on('data',c=>body+=c); res.on('end',()=>process.stdout.write(body));
});
req.on('error',()=>process.exit(2));
req.setTimeout(10000,()=>{req.destroy();process.exit(3);});
"#;
    let output = Command::new(node_path())
        .arg("-e")
        .arg(SCRIPT)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| "Не удалось проверить обновления".to_string())?;
    if !output.status.success() {
        return Err("Не удалось проверить обновления (нет релизов или сети)".to_string());
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| "Не удалось разобрать ответ GitHub".to_string())
}

fn node_path() -> PathBuf {
    for candidate in [
        r"C:\Program Files\nodejs\node.exe",
        r"C:\Program Files (x86)\nodejs\node.exe",
    ] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from("node")
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
