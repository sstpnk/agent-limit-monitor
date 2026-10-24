use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows::Win32::UI::Shell::{SHAppBarMessage, ABM_GETTASKBARPOS, APPBARDATA};
use windows::Win32::UI::WindowsAndMessaging::*;

// Window style constants
pub const WS_POPUP_STYLE: u32 = 0x80000000;
pub const WS_CHILD_STYLE: u32 = 0x40000000;
pub const WS_CLIPSIBLINGS_STYLE: u32 = 0x04000000;

// Timer IDs
pub const TIMER_POLL: usize = 1;
pub const TIMER_COUNTDOWN: usize = 2;
pub const TIMER_RESET_POLL: usize = 3;

// Custom messages
pub const WM_APP: u32 = 0x8000;
pub const WM_APP_USAGE_UPDATED: u32 = WM_APP + 1;
pub const WM_APP_TRAY: u32 = WM_APP + 3;

#[derive(Clone, Copy, Debug)]
pub struct TaskbarWindow {
    pub hwnd: HWND,
    pub rect: RECT,
}

pub fn find_taskbars() -> Vec<TaskbarWindow> {
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let taskbars = &mut *(lparam.0 as *mut Vec<TaskbarWindow>);
        let mut class_name = [0u16; 64];
        let len = unsafe { GetClassNameW(hwnd, &mut class_name) };
        if len > 0 {
            let class_name = String::from_utf16_lossy(&class_name[..len as usize]);
            if class_name == "Shell_TrayWnd" || class_name == "Shell_SecondaryTrayWnd" {
                if let Some(rect) = get_taskbar_rect(hwnd).or_else(|| get_window_rect_safe(hwnd)) {
                    taskbars.push(TaskbarWindow { hwnd, rect });
                }
            }
        }
        BOOL(1)
    }

    let mut taskbars: Vec<TaskbarWindow> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut taskbars as *mut _ as isize));
    }
    taskbars.sort_by_key(|taskbar| {
        (
            taskbar.rect.top,
            taskbar.rect.left,
            taskbar.rect.bottom,
            taskbar.rect.right,
        )
    });
    taskbars
}

/// Find a child window by class name
pub fn find_child_window(parent: HWND, class_name: &str) -> Option<HWND> {
    unsafe {
        let class = wide_str(class_name);
        match FindWindowExW(
            parent,
            HWND::default(),
            PCWSTR::from_raw(class.as_ptr()),
            PCWSTR::null(),
        ) {
            Ok(h) if h != HWND::default() => Some(h),
            _ => None,
        }
    }
}

/// Get taskbar position via SHAppBarMessage
pub fn get_taskbar_rect(taskbar_hwnd: HWND) -> Option<RECT> {
    unsafe {
        let mut class_name = [0u16; 64];
        let len = GetClassNameW(taskbar_hwnd, &mut class_name);
        if len > 0 {
            let class_name = String::from_utf16_lossy(&class_name[..len as usize]);
            if class_name == "Shell_SecondaryTrayWnd" {
                return get_window_rect_safe(taskbar_hwnd);
            }
        }

        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: taskbar_hwnd,
            ..Default::default()
        };
        let result = SHAppBarMessage(ABM_GETTASKBARPOS, &mut abd);
        if result == 0 {
            return None;
        }
        Some(abd.rc)
    }
}

/// Get the bounding rectangle of a window
pub fn get_window_rect_safe(hwnd: HWND) -> Option<RECT> {
    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_ok() {
            Some(rect)
        } else {
            None
        }
    }
}

/// Embed our window as a child of the taskbar. Returns true when the parent
/// actually changed. The app manifest is required for layered child windows on
/// the Windows 11 raised desktop.
pub fn embed_in_taskbar(hwnd: HWND, taskbar_hwnd: HWND) -> bool {
    unsafe {
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE);
        let _ = SetWindowLongW(
            hwnd,
            GWL_EXSTYLE,
            (ex_style | WS_EX_TOOLWINDOW.0 as i32 | WS_EX_NOACTIVATE.0 as i32)
                & !(WS_EX_TOPMOST.0 as i32),
        );

        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let new_style = (style & !WS_POPUP_STYLE) | WS_CHILD_STYLE | WS_CLIPSIBLINGS_STYLE;
        let _ = SetWindowLongW(hwnd, GWL_STYLE, new_style as i32);

        let _ = SetParent(hwnd, taskbar_hwnd);
        if GetParent(hwnd).ok() == Some(taskbar_hwnd) {
            // Windows 11 can leave a reparented layered surface below the
            // taskbar's DirectComposition visual. Rebind it once.
            let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE);
            if ex_style & WS_EX_LAYERED.0 as i32 != 0 {
                let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, ex_style & !(WS_EX_LAYERED.0 as i32));
                let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, ex_style);
            }
            true
        } else {
            let _ = SetParent(hwnd, HWND::default());
            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            let restored = (style & !(WS_CHILD_STYLE | WS_CLIPSIBLINGS_STYLE)) | WS_POPUP_STYLE;
            let _ = SetWindowLongW(hwnd, GWL_STYLE, restored as i32);
            false
        }
    }
}

/// Move the window
pub fn move_window(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    unsafe {
        let _ = MoveWindow(hwnd, x, y, w, h, true);
    }
}

/// Convert a Rust string to a null-terminated wide string
pub fn wide_str(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// COLORREF wrapper (RGB packed into u32)
pub fn colorref(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

/// Color helper
#[derive(Clone, Copy, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    #[allow(dead_code)]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn from_hex(hex: &str) -> Self {
        let hex = hex.trim_start_matches('#');
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
        Self { r, g, b }
    }

    pub fn to_colorref(self) -> u32 {
        colorref(self.r, self.g, self.b)
    }
}
