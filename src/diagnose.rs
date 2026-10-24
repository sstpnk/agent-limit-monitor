use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

struct DiagnoseState {
    file: Mutex<File>,
}

static DIAGNOSE_STATE: OnceLock<DiagnoseState> = OnceLock::new();

pub fn init() -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.to_path_buf());
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local).join("AgentLimitMonitor"));
    }
    candidates.push(std::env::temp_dir().join("AgentLimitMonitor"));

    let mut last_error = String::new();
    for dir in candidates {
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("agent-limit-monitor.log");
        match OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
        {
            Ok(file) => {
                let _ = DIAGNOSE_STATE.set(DiagnoseState {
                    file: Mutex::new(file),
                });
                log(format!("diagnostic logging enabled at {}", path.display()));
                return Ok(path);
            }
            Err(error) => last_error = format!("{}: {error}", path.display()),
        }
    }
    Err(last_error)
}

pub fn log(message: impl AsRef<str>) {
    let Some(state) = DIAGNOSE_STATE.get() else {
        return;
    };

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);

    if let Ok(mut file) = state.file.lock() {
        let _ = writeln!(file, "[{timestamp}] {}", message.as_ref());
        let _ = file.flush();
    }
}

pub fn log_error(context: &str, error: impl std::fmt::Display) {
    log(format!("{context}: {error}"));
}
