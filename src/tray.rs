use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::UI::Shell::{
    ExtractIconExW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::native_interop::WM_APP_TRAY;

const TRAY_ID: u32 = 1;
static ADDED: AtomicBool = AtomicBool::new(false);

pub enum TrayAction {
    None,
    ToggleWidget,
    ShowContextMenu,
}

pub fn sync(hwnd: HWND, tooltip: &str) {
    let icon = embedded_icon();
    unsafe {
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = TRAY_ID;
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = WM_APP_TRAY;
        data.hIcon = icon;
        copy_tip(tooltip, &mut data.szTip);

        if ADDED.load(Ordering::Relaxed) {
            data.uFlags = NIF_ICON | NIF_TIP;
            let modified = Shell_NotifyIconW(NIM_MODIFY, &data).as_bool();
            if !modified {
                let _ = Shell_NotifyIconW(NIM_ADD, &data);
            }
        } else {
            let added = Shell_NotifyIconW(NIM_ADD, &data).as_bool();
            let registered = if added {
                true
            } else {
                Shell_NotifyIconW(NIM_MODIFY, &data).as_bool()
            };
            crate::diagnose::log(format!(
                "tray icon add={added} modify_fallback={registered} icon_valid={}",
                !icon.is_invalid()
            ));
            if registered {
                ADDED.store(true, Ordering::Relaxed);
            }
        }

        if !icon.is_invalid() {
            let _ = DestroyIcon(icon);
        }
    }
}

pub fn remove(hwnd: HWND) {
    if !ADDED.swap(false, Ordering::Relaxed) {
        return;
    }
    unsafe {
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = TRAY_ID;
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

pub fn handle_message(lparam: LPARAM) -> TrayAction {
    match lparam.0 as u32 {
        WM_LBUTTONUP => TrayAction::ToggleWidget,
        WM_RBUTTONUP => TrayAction::ShowContextMenu,
        _ => TrayAction::None,
    }
}

fn embedded_icon() -> HICON {
    unsafe {
        let mut path = [0u16; 260];
        let len = GetModuleFileNameW(None, &mut path) as usize;
        if len == 0 {
            return HICON::default();
        }
        let mut small = HICON::default();
        let mut large = HICON::default();
        let count = ExtractIconExW(
            PCWSTR::from_raw(path.as_ptr()),
            0,
            Some(&mut large),
            Some(&mut small),
            1,
        );
        if count == 0 {
            return LoadIconW(HINSTANCE::default(), IDI_APPLICATION).unwrap_or_default();
        }
        if !small.is_invalid() {
            small
        } else {
            large
        }
    }
}

fn copy_tip(text: &str, buffer: &mut [u16; 128]) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut len = wide.len().min(127);
    if len > 0 && (0xD800..=0xDBFF).contains(&wide[len - 1]) {
        len -= 1;
    }
    buffer[..len].copy_from_slice(&wide[..len]);
    buffer[len] = 0;
}
