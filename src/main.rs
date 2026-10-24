#![windows_subsystem = "windows"]

mod autostart;
mod config;
mod credentials;
mod diagnose;
mod format;
mod http;
mod i18n;
mod models;
mod native_interop;
mod providers;
mod theme;
mod tray;
mod updater;
mod window;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if let Ok(exe) = std::env::current_exe() {
        let marker = exe.with_extension("new.update");
        if let Ok(content) = std::fs::read_to_string(&marker) {
            let mut lines = content.lines();
            if let (Some(target), Some(pid)) = (
                lines.next(),
                lines.next().and_then(|value| value.parse::<u32>().ok()),
            ) {
                updater::apply_update(target, pid);
                return;
            }
        }
    }

    if let Some(index) = args.iter().position(|arg| arg == "--apply-update") {
        if let (Some(target), Some(pid)) = (
            args.get(index + 1),
            args.get(index + 2).and_then(|value| value.parse::<u32>().ok()),
        ) {
            updater::apply_update(target, pid);
            return;
        }
    }

    let diagnose_enabled = args.iter().any(|arg| arg == "--diagnose");
    if diagnose_enabled {
        match diagnose::init() {
            Ok(path) => diagnose::log(format!("startup args={args:?} log_path={}", path.display())),
            Err(_) => {}
        }
    }

    if let Some(index) = args.iter().position(|arg| arg == "--config") {
        if let Some(path) = args.get(index + 1) {
            std::env::set_var("ALM_CONFIG", path);
        }
    }

    if diagnose_enabled {
        diagnose::log("entering window::run");
    }
    window::run();
}
