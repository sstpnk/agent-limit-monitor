#![windows_subsystem = "windows"]

mod config;
mod credentials;
mod diagnose;
mod format;
mod http;
mod models;
mod native_interop;
mod providers;
mod theme;
mod tray;
mod updater;
mod window;

fn main() {
    let args: Vec<String> = std::env::args().collect();
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
