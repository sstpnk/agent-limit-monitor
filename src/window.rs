use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::autostart;
use crate::config::{self, Config};
use crate::diagnose;
use crate::format::{row_reset, row_value};
use crate::i18n::{self, Lang};
use crate::models::{ProviderId, Reading, State};
use crate::native_interop::{
    self, Color, TIMER_COUNTDOWN, TIMER_POLL, TIMER_RESET_POLL, WM_APP_TRAY,
    WM_APP_USAGE_UPDATED,
};
use crate::providers;
use crate::theme;
use crate::tray;
use crate::updater;

const WIDGET_HEIGHT: i32 = 42;
const COMPACT_HEIGHT: i32 = 40;
const WIDGET_MIN_WIDTH: i32 = 140;
const TIP_PAD: i32 = 10;
const TIP_MAX_HEIGHT: i32 = 560;
const TIP_DELAY_MS: u32 = 350;

const TIMER_REPOSITION: usize = 10;
const TIMER_HOVER: usize = 11;
const TIMER_THEME: usize = 12;
const TIMER_TOPMOST: usize = 13;
const WM_MOUSELEAVE_MSG: u32 = 0x02A3;

const IDM_REFRESH: u16 = 1;
const IDM_EXIT: u16 = 2;
const IDM_FREQ_1MIN: u16 = 10;
const IDM_FREQ_2MIN: u16 = 14;
const IDM_FREQ_5MIN: u16 = 11;
const IDM_FREQ_15MIN: u16 = 12;
const IDM_FREQ_1HOUR: u16 = 13;
const IDM_VERSION: u16 = 30;
const IDM_UPDATE_CHECK: u16 = 31;
const IDM_RESET_POSITION: u16 = 32;
const IDM_AUTOSTART: u16 = 33;
const IDM_LANG_SYSTEM: u16 = 40;
const IDM_LANG_EN: u16 = 41;
const IDM_LANG_ZH: u16 = 42;
const IDM_LANG_ES: u16 = 43;
const IDM_LANG_HI: u16 = 44;
const IDM_LANG_FR: u16 = 45;
const IDM_LANG_RU: u16 = 46;
const IDM_MODEL_CHATGPT: u16 = 60;
const IDM_MODEL_ZAI: u16 = 61;
const IDM_MODEL_DEEPSEEK: u16 = 62;
const IDM_MODEL_CLAUDE: u16 = 63;
const IDM_MODEL_ANTIGRAVITY: u16 = 64;
const IDM_MODEL_COPILOT: u16 = 65;

static CURRENT_DPI: AtomicU32 = AtomicU32::new(96);
static STATE: Mutex<Option<AppState>> = Mutex::new(None);

#[derive(Clone, Copy)]
struct SendHwnd(isize);

unsafe impl Send for SendHwnd {}

impl SendHwnd {
    fn from_hwnd(hwnd: HWND) -> Self {
        Self(hwnd.0 as isize)
    }

    fn to_hwnd(self) -> HWND {
        HWND(self.0 as *mut _)
    }
}

struct AppState {
    hwnd: SendHwnd,
    tip_hwnd: SendHwnd,
    taskbar_hwnd: Option<HWND>,
    embedded: bool,
    is_dark: bool,
    config: Config,
    config_path: PathBuf,
    readings: Vec<Reading>,
    poll_interval_ms: u32,
    polling: bool,
    poll_again: bool,
    taskbar_index: usize,
    tray_offset: i32,
    widget_width: i32,
    widget_height: i32,
    dragging: bool,
    drag_start_mouse_x: i32,
    drag_start_offset: i32,
    widget_visible: bool,
    tooltip_visible: bool,
    hover_armed: bool,
    hover_tracking: bool,
}

unsafe impl Send for AppState {}

fn lock_state() -> MutexGuard<'static, Option<AppState>> {
    STATE.lock().unwrap_or_else(|error| error.into_inner())
}

fn sc(px: i32) -> i32 {
    let dpi = CURRENT_DPI.load(Ordering::Relaxed);
    (px as f64 * dpi as f64 / 96.0).round() as i32
}

fn refresh_dpi() {
    let hwnd = lock_state()
        .as_ref()
        .map(|state| state.hwnd.to_hwnd());
    if let Some(hwnd) = hwnd {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        if dpi > 0 {
            CURRENT_DPI.store(dpi, Ordering::Relaxed);
        }
    }
}

pub fn run() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CURRENT_DPI.store(GetDpiForSystem(), Ordering::Relaxed);
    }
    diagnose::log("window::run started");

    let mutex_name = native_interop::wide_str("Global\\AgentLimitMonitor");
    let _mutex = unsafe {
        match CreateMutexW(None, true, PCWSTR::from_raw(mutex_name.as_ptr())) {
            Ok(handle) => {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    diagnose::log("another Agent Limit Monitor instance is already running");
                    return;
                }
                handle
            }
            Err(error) => {
                diagnose::log_error("unable to create single-instance mutex", error);
                return;
            }
        }
    };

    unsafe {
        let hinstance = GetModuleHandleW(PCWSTR::null()).unwrap();
        let (large_icon, small_icon) = load_icons();

        let widget_class = native_interop::wide_str("AgentLimitMonitor");
        let widget = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: HINSTANCE(hinstance.0),
            hIcon: large_icon,
            hIconSm: small_icon,
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszClassName: PCWSTR::from_raw(widget_class.as_ptr()),
            ..Default::default()
        };
        let atom = RegisterClassExW(&widget);
        if atom == 0 {
            diagnose::log_error(
                "RegisterClassExW(widget) failed",
                std::io::Error::last_os_error(),
            );
        } else {
            diagnose::log(format!("widget class registered atom={atom}"));
        }

        let tip_class = native_interop::wide_str("AgentLimitMonitor.Tooltip");
        let tooltip = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(tip_wnd_proc),
            hInstance: HINSTANCE(hinstance.0),
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszClassName: PCWSTR::from_raw(tip_class.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassExW(&tooltip);

        let (config, mut config_path) = config::load();
        i18n::set(i18n::for_config(&config.language));
        config_path = config::save(&config, &config_path);
        let is_dark = theme::is_dark_mode();

        let title = native_interop::wide_str("Agent Limit Monitor");
        let hwnd = match CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            PCWSTR::from_raw(widget_class.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            sc(WIDGET_MIN_WIDTH),
            sc(widget_height(&config)),
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        ) {
            Ok(hwnd) => hwnd,
            Err(error) => {
                diagnose::log_error("unable to create widget window", error);
                diagnose::log_error(
                    "CreateWindowExW(widget) last error",
                    std::io::Error::last_os_error(),
                );
                return;
            }
        };

        set_window_icon(hwnd, large_icon, small_icon);

        let tip_hwnd = match CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOPMOST,
            PCWSTR::from_raw(tip_class.as_ptr()),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        ) {
            Ok(hwnd) => hwnd,
            Err(error) => {
                diagnose::log_error("unable to create tooltip window", error);
                let _ = DestroyWindow(hwnd);
                return;
            }
        };

        let mut readings: Vec<Reading> = ProviderId::ALL
            .iter()
            .copied()
            .filter(|provider| providers::is_enabled(&config, *provider))
            .map(|provider| Reading::placeholder(provider, State::Loading))
            .collect();
        if readings.is_empty() {
            readings.push(Reading::placeholder(ProviderId::ChatGpt, State::Loading));
        }

        {
            let mut state = lock_state();
            *state = Some(AppState {
                hwnd: SendHwnd::from_hwnd(hwnd),
                tip_hwnd: SendHwnd::from_hwnd(tip_hwnd),
                taskbar_hwnd: None,
                embedded: false,
                is_dark,
                config: config.clone(),
                config_path,
                readings,
                poll_interval_ms: (config.poll_interval_secs * 1000) as u32,
                polling: false,
                poll_again: false,
                taskbar_index: config.position.taskbar_index,
                tray_offset: config.position.offset,
                widget_width: sc(WIDGET_MIN_WIDTH),
                widget_height: sc(WIDGET_HEIGHT),
                dragging: false,
                drag_start_mouse_x: 0,
                drag_start_offset: 0,
                widget_visible: true,
                tooltip_visible: false,
                hover_armed: false,
                hover_tracking: false,
            });
        }

        if config.display.dock == "taskbar" {
            attach_to_taskbar(hwnd, config.position.taskbar_index);
        }
        position_window();
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        render_widget();
        sync_tray(hwnd);

        SetTimer(
            hwnd,
            TIMER_POLL,
            (config.poll_interval_secs * 1000).min(u32::MAX as u64) as u32,
            None,
        );
        SetTimer(hwnd, TIMER_COUNTDOWN, 30_000, None);
        SetTimer(hwnd, TIMER_REPOSITION, 2_000, None);
        SetTimer(hwnd, TIMER_THEME, 5_000, None);

        spawn_poll(hwnd);

        diagnose::log("entering message loop");
        let mut message = MSG::default();
        while GetMessageW(&mut message, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        diagnose::log("message loop exited");
    }
}

fn load_icons() -> (HICON, HICON) {
    unsafe {
        let mut path = [0u16; 260];
        let len = GetModuleFileNameW(None, &mut path) as usize;
        if len == 0 {
            return (HICON::default(), HICON::default());
        }
        let mut large = HICON::default();
        let mut small = HICON::default();
        let count = ExtractIconExW(
            PCWSTR::from_raw(path.as_ptr()),
            0,
            Some(&mut large),
            Some(&mut small),
            1,
        );
        if count == 0 {
            (HICON::default(), HICON::default())
        } else {
            (large, small)
        }
    }
}

unsafe fn set_window_icon(hwnd: HWND, large: HICON, small: HICON) {
    if !large.is_invalid() {
        let _ = SendMessageW(
            hwnd,
            WM_SETICON,
            WPARAM(ICON_BIG as usize),
            LPARAM(large.0 as isize),
        );
    }
    if !small.is_invalid() {
        let _ = SendMessageW(
            hwnd,
            WM_SETICON,
            WPARAM(ICON_SMALL as usize),
            LPARAM(small.0 as isize),
        );
    }
}

fn widget_height(config: &Config) -> i32 {
    if config.display.compact {
        COMPACT_HEIGHT
    } else {
        WIDGET_HEIGHT
    }
}

#[derive(Clone, Copy)]
struct WidgetLayout {
    width: i32,
    label_w: i32,
    value_w: i32,
    reset_w: i32,
    height: i32,
    rows: usize,
    column_width: i32,
    col_gap: i32,
}

fn widget_font_height(config: &Config) -> i32 {
    if config.display.compact { -10 } else { -11 }
}

fn measure_text(hdc: HDC, font: HFONT, text: &str) -> i32 {
    unsafe {
        let old = SelectObject(hdc, font);
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        let mut rect = RECT::default();
        let _ = DrawTextW(
            hdc,
            &mut wide,
            &mut rect,
            DT_CALCRECT | DT_SINGLELINE,
        );
        SelectObject(hdc, old);
        rect.right
    }
}

fn layout_widget(hdc: HDC, readings: &[Reading], config: &Config) -> WidgetLayout {
    let font = create_font(widget_font_height(config), FW_MEDIUM.0 as i32);
    let mut label_w = 0;
    let mut value_w = 0;
    let mut reset_w = 0;
    for reading in readings {
        let label = if config.display.labels == "short" {
            reading.provider.short_label()
        } else {
            reading.provider.label()
        };
        label_w = label_w.max(measure_text(hdc, font, label));
        value_w = value_w.max(measure_text(hdc, font, &row_value(reading)));
        let reset = row_reset(reading);
        if !reset.is_empty() {
            reset_w = reset_w.max(measure_text(hdc, font, &reset));
        }
    }
    unsafe {
        let _ = DeleteObject(font);
    }

    let pad_left = sc(10);
    let pad_right = sc(8);
    let label_gap = sc(9);
    let reset_gap = sc(6);
    let mut column_width = pad_left + label_w + label_gap + value_w + pad_right;
    if reset_w > 0 {
        column_width += reset_gap + reset_w;
    }
    let count = readings.len().max(1);
    let cols = if count > 3 { 2 } else { 1 };
    let rows = count.div_ceil(cols);
    let col_gap = sc(14);
    let width = cols as i32 * column_width + (cols.saturating_sub(1)) as i32 * col_gap;
    WidgetLayout {
        width: (width + sc(4)).max(sc(WIDGET_MIN_WIDTH)),
        label_w,
        value_w,
        reset_w,
        height: sc(widget_height(config)),
        rows,
        column_width,
        col_gap,
    }
}

fn attach_to_taskbar(hwnd: HWND, requested_index: usize) -> bool {
    let taskbars = native_interop::find_taskbars();
    if taskbars.is_empty() {
        diagnose::log("taskbar not found; using floating window");
        if let Some(state) = lock_state().as_mut() {
            state.embedded = false;
            state.taskbar_hwnd = None;
        }
        return false;
    }
    let index = requested_index.min(taskbars.len() - 1);
    let taskbar = taskbars[index];
    diagnose::log(format!(
        "attach taskbar hwnd={:?} rect=({},{},{},{})",
        taskbar.hwnd, taskbar.rect.left, taskbar.rect.top, taskbar.rect.right, taskbar.rect.bottom
    ));
    let embedded = native_interop::embed_in_taskbar(hwnd, taskbar.hwnd);
    if !embedded {
        diagnose::log("SetParent(taskbar) was denied; using taskbar overlay");
        if let Some(state) = lock_state().as_mut() {
            state.embedded = false;
            state.taskbar_hwnd = Some(taskbar.hwnd);
            state.taskbar_index = index;
        }
        return false;
    }
    if let Some(state) = lock_state().as_mut() {
        state.taskbar_hwnd = Some(taskbar.hwnd);
        state.embedded = true;
        state.taskbar_index = index;
    }
    diagnose::log(format!("embedded into taskbar index={index}"));
    true
}

fn position_window() {
    refresh_dpi();
    let (hwnd, embedded, offset, taskbar_hwnd, config, width, height) = {
        let state = lock_state();
        let Some(state) = state.as_ref() else {
            return;
        };
        (
            state.hwnd.to_hwnd(),
            state.embedded,
            state.tray_offset,
            state.taskbar_hwnd,
            state.config.clone(),
            state.widget_width,
            state.widget_height,
        )
    };


    if !embedded {
        if config.display.dock == "taskbar" {
            if let Some(taskbar_hwnd) = taskbar_hwnd {
                position_taskbar_overlay(hwnd, taskbar_hwnd, width, height, offset);
                return;
            }
        }
        position_floating(hwnd, width, height, offset);
        return;
    }
    let taskbar_hwnd = taskbar_hwnd.unwrap();
    let taskbar_rect = match native_interop::get_taskbar_rect(taskbar_hwnd)
        .or_else(|| native_interop::get_window_rect_safe(taskbar_hwnd))
    {
        Some(rect) => rect,
        None => {
            diagnose::log("position_window: unable to query taskbar rect");
            return;
        }
    };
    let mut tray_left = taskbar_rect.right;
    if let Some(tray_hwnd) = native_interop::find_child_window(taskbar_hwnd, "TrayNotifyWnd") {
        if let Some(tray_rect) = native_interop::get_window_rect_safe(tray_hwnd) {
            tray_left = tray_rect.left;
        }
    }
    let max_offset = (tray_left - taskbar_rect.left - width).max(0);
    let offset = offset.clamp(0, max_offset);
    let changed = {
        let mut state = lock_state();
        match state.as_mut() {
            Some(state) => {
                if state.tray_offset != offset {
                    state.tray_offset = offset;
                    state.config.position.offset = offset;
                    true
                } else {
                    false
                }
            }
            None => false,
        }
    };
    if changed {
        save_config();
    }

    let y = taskbar_rect.top
        + ((taskbar_rect.bottom - taskbar_rect.top - height) / 2).max(0);
    let x = tray_left - taskbar_rect.left - width - offset;
    native_interop::move_window(hwnd, x, y - taskbar_rect.top, width, height);
}

/// Fallback when Windows refuses SetParent: overlay the taskbar exactly where
/// the child widget would sit, immediately left of the notification area.
fn position_taskbar_overlay(
    hwnd: HWND,
    taskbar_hwnd: HWND,
    width: i32,
    height: i32,
    offset: i32,
) {
    let Some(taskbar_rect) = native_interop::get_taskbar_rect(taskbar_hwnd)
        .or_else(|| native_interop::get_window_rect_safe(taskbar_hwnd))
    else {
        return;
    };
    let mut tray_left = taskbar_rect.right;
    if let Some(tray) = native_interop::find_child_window(taskbar_hwnd, "TrayNotifyWnd") {
        if let Some(rect) = native_interop::get_window_rect_safe(tray) {
            tray_left = rect.left;
        }
    }
    let max_offset = (tray_left - taskbar_rect.left - width).max(0);
    let offset = offset.clamp(0, max_offset);
    let changed = if let Some(state) = lock_state().as_mut() {
        if state.tray_offset != offset {
            state.tray_offset = offset;
            state.config.position.offset = offset;
            true
        } else {
            false
        }
    } else {
        false
    };
    if changed {
        save_config();
    }
    let x = tray_left - width - offset;
    let y = taskbar_rect.top
        + ((taskbar_rect.bottom - taskbar_rect.top - height) / 2).max(0);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

fn position_floating(hwnd: HWND, width: i32, height: i32, offset: i32) {
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let work = if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            RECT {
                left: 0,
                top: 0,
                right: GetSystemMetrics(SM_CXSCREEN),
                bottom: GetSystemMetrics(SM_CYSCREEN),
            }
        };
        let x = work.right - width - sc(24) - offset;
        let y = work.bottom - height - sc(24);
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE);
    }
}

fn save_config() {
    let (config, path) = {
        let state = lock_state();
        let Some(state) = state.as_ref() else {
            return;
        };
        (state.config.clone(), state.config_path.clone())
    };
    let saved = config::save(&config, &path);
    if saved != path {
        if let Some(state) = lock_state().as_mut() {
            state.config_path = saved;
        }
    }
}

fn spawn_poll(hwnd: HWND) {
    {
        let mut state = lock_state();
        let Some(state) = state.as_mut() else {
            return;
        };
        if state.polling {
            state.poll_again = true;
            return;
        }
        state.polling = true;
    }
    let send = SendHwnd::from_hwnd(hwnd);
    std::thread::spawn(move || do_poll(send));
}

fn do_poll(send: SendHwnd) {
    let hwnd = send.to_hwnd();
    let config = {
        let state = lock_state();
        let Some(state) = state.as_ref() else {
            return;
        };
        state.config.clone()
    };

    let readings = providers::poll_all(&config);
    diagnose::log(format!(
        "poll finished: {}",
        readings
            .iter()
            .map(|reading| format!(
                "{}={:?}/{:?}",
                reading.provider.label(),
                reading.state,
                reading.value
            ))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    {
        let mut guard = lock_state();
        let Some(state) = guard.as_mut() else {
            return;
        };
        let mut merged = readings;
        for reading in &mut merged {
            let previous = state
                .readings
                .iter()
                .find(|previous| previous.provider == reading.provider)
                .cloned();
            if reading.state.is_error()
                && !reading.state.is_missing()
                && previous.as_ref().is_some_and(|previous| previous.has_value())
            {
                let mut stale = previous.unwrap();
                stale.state = State::Stale;
                stale.title = reading.title.clone();
                stale.detail = reading.detail.clone();
                *reading = stale;
            }
        }
        state.readings = merged;
        state.polling = false;
        let again = std::mem::take(&mut state.poll_again);
        if again {
            // The configuration changed while this poll was running.
            // Drop the state lock before starting the replacement poll.
            drop(guard);
            spawn_poll(hwnd);
            return;
        }
    }
    unsafe {
        let _ = PostMessageW(hwnd, WM_APP_USAGE_UPDATED, WPARAM(0), LPARAM(0));
    }
}

fn sync_tray(hwnd: HWND) {
    let tooltip = tray_summary();
    tray::sync(hwnd, &tooltip);
}

fn tray_summary() -> String {
    let state = lock_state();
    let Some(state) = state.as_ref() else {
        return "Agent Limit Monitor".to_string();
    };
    let mut parts = Vec::new();
    for reading in &state.readings {
        let value = row_value(reading);
        let reset = row_reset(reading);
        if reset.is_empty() {
            parts.push(format!("{} {value}", reading.provider.label()));
        } else {
            parts.push(format!("{} {value} {reset}", reading.provider.label()));
        }
    }
    parts.join(" · ")
}

fn render_widget() {
    refresh_dpi();
    let (hwnd, is_dark, readings, config, embedded) = {
        let state = lock_state();
        let Some(state) = state.as_ref() else {
            return;
        };
        (
            state.hwnd.to_hwnd(),
            state.is_dark,
            state.readings.clone(),
            state.config.clone(),
            state.embedded,
        )
    };

    let layout = unsafe {
        let screen = GetDC(hwnd);
        let layout = layout_widget(screen, &readings, &config);
        ReleaseDC(hwnd, screen);
        layout
    };
    let width = layout.width;
    let height = layout.height;
    let resized = {
        let mut state = lock_state();
        match state.as_mut() {
            Some(state) if state.widget_width != width || state.widget_height != height => {
                state.widget_width = width;
                state.widget_height = height;
                true
            }
            _ => false,
        }
    };
    if resized {
        position_window();
    }
    let bg = if is_dark {
        Color::from_hex("#1C1C1C")
    } else {
        Color::from_hex("#F3F3F3")
    };

    unsafe {
        let screen = GetDC(hwnd);
        let mem = CreateCompatibleDC(screen);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib =
            CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
        if dib.is_invalid() || bits.is_null() {
            let _ = DeleteDC(mem);
            ReleaseDC(hwnd, screen);
            return;
        }
        let old = SelectObject(mem, dib);
        paint_widget(mem, width, height, is_dark, &readings, &config, &bg, &layout);

        let bg_ref = bg.to_colorref();
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u32, (width * height) as usize);
        for pixel in pixels.iter_mut() {
            let rgb = *pixel & 0x00FF_FFFF;
            *pixel = if embedded && rgb == bg_ref {
                0x0100_0000
            } else {
                rgb | 0xFF00_0000
            };
        }

        let size = SIZE { cx: width, cy: height };
        let source = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: 0,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: 1,
        };
        let _ = UpdateLayeredWindow(
            hwnd,
            screen,
            None,
            Some(&size),
            mem,
            Some(&source),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );

        SelectObject(mem, old);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem);
        ReleaseDC(hwnd, screen);
    }

    if is_tooltip_visible() {
        render_tooltip();
    }
}

fn paint_widget(
    hdc: HDC,
    width: i32,
    height: i32,
    is_dark: bool,
    readings: &[Reading],
    config: &Config,
    bg: &Color,
    layout: &WidgetLayout,
) {
    unsafe {
        let rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        let brush = CreateSolidBrush(COLORREF(bg.to_colorref()));
        FillRect(hdc, &rect, brush);
        let _ = DeleteObject(brush);

        let grip = CreateSolidBrush(COLORREF(native_interop::colorref(
            if is_dark { 90 } else { 180 },
            if is_dark { 90 } else { 180 },
            if is_dark { 90 } else { 180 },
        )));
        let grip_rect = RECT {
            left: 0,
            top: sc(8),
            right: sc(2),
            bottom: height - sc(8),
        };
        FillRect(hdc, &grip_rect, grip);
        let _ = DeleteObject(grip);

        let fg = if is_dark {
            Color::from_hex("#F2F2F2")
        } else {
            Color::from_hex("#1A1A1A")
        };
        let muted = if is_dark {
            Color::from_hex("#9A9A9A")
        } else {
            Color::from_hex("#6B6B6B")
        };
        let warning = if is_dark {
            Color::from_hex("#E5B567")
        } else {
            Color::from_hex("#9A5B00")
        };
        let critical = if is_dark {
            Color::from_hex("#F07167")
        } else {
            Color::from_hex("#B42318")
        };

        let _ = SetBkMode(hdc, TRANSPARENT);
        let font = create_font(widget_font_height(config), FW_MEDIUM.0 as i32);
        let old_font = SelectObject(hdc, font);

        let row_height = if config.display.compact {
            sc(12)
        } else {
            sc(13)
        };
        let top = (height - row_height * layout.rows as i32) / 2;

        for (index, reading) in readings.iter().enumerate() {
            let column = index / layout.rows;
            let row = index % layout.rows;
            let base_x = sc(4) + column as i32 * (layout.column_width + layout.col_gap);
            let label_left = base_x + sc(10);
            let label_right = label_left + layout.label_w;
            let value_left = label_right + sc(9);
            let value_right = value_left + layout.value_w;
            let reset_left = value_right + sc(6);
            let reset_right = reset_left + layout.reset_w;
            let row_top = top + row as i32 * row_height;
            let row_bottom = row_top + row_height;
            let label = if config.display.labels == "short" {
                reading.provider.short_label()
            } else {
                reading.provider.label()
            };
            let mut label_rect = RECT {
                left: label_left,
                top: row_top,
                right: label_right,
                bottom: row_bottom,
            };
            draw_text(
                hdc,
                label,
                &mut label_rect,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                muted,
            );

            let value = row_value(reading);
            let color = if reading.state.is_missing()
                || matches!(reading.state, State::Stale | State::Loading)
            {
                muted
            } else {
                match reading.state {
                    State::Critical => critical,
                    State::Warning => warning,
                    _ => fg,
                }
            };
            let mut value_rect = RECT {
                left: value_left,
                top: row_top,
                right: value_right,
                bottom: row_bottom,
            };
            draw_text(
                hdc,
                &value,
                &mut value_rect,
                DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
                color,
            );

            let reset = row_reset(reading);
            if !reset.is_empty() {
                let mut reset_rect = RECT {
                    left: reset_left,
                    top: row_top,
                    right: reset_right,
                    bottom: row_bottom,
                };
                draw_text(
                    hdc,
                    &reset,
                    &mut reset_rect,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                    muted,
                );
            }
        }

        SelectObject(hdc, old_font);
        let _ = DeleteObject(font);
    }
}

fn create_font(height: i32, weight: i32) -> HFONT {
    let name = native_interop::wide_str(i18n::font_name());
    unsafe {
        CreateFontW(
            sc(height),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_TT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            PCWSTR::from_raw(name.as_ptr()),
        )
    }
}

fn draw_text(hdc: HDC, text: &str, rect: &mut RECT, flags: DRAW_TEXT_FORMAT, color: Color) {
    unsafe {
        let _ = SetTextColor(hdc, COLORREF(color.to_colorref()));
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        let _ = DrawTextW(hdc, &mut wide, rect, flags);
    }
}

fn is_tooltip_visible() -> bool {
    lock_state()
        .as_ref()
        .map(|state| state.tooltip_visible)
        .unwrap_or(false)
}

enum TipRow {
    Title { text: String, color: Color },
    Detail {
        label: String,
        value: String,
        reset: Option<String>,
        color: Color,
    },
    Gap,
}

fn tip_lines(readings: &[Reading], is_dark: bool) -> Vec<TipRow> {
    let fg = if is_dark {
        Color::from_hex("#F2F2F2")
    } else {
        Color::from_hex("#1A1A1A")
    };
    let muted = if is_dark {
        Color::from_hex("#9A9A9A")
    } else {
        Color::from_hex("#6B6B6B")
    };
    let mut rows = Vec::new();
    for (index, reading) in readings.iter().enumerate() {
        if index > 0 {
            rows.push(TipRow::Gap);
        }
        rows.push(TipRow::Title {
            text: reading.title.clone(),
            color: fg,
        });
        if reading.detail.is_empty() {
            rows.push(TipRow::Detail {
                label: String::new(),
                value: row_value(reading),
                reset: None,
                color: muted,
            });
        } else {
            for line in &reading.detail {
                rows.push(TipRow::Detail {
                    label: line.label.clone(),
                    value: line.value.clone(),
                    reset: line.reset.clone(),
                    color: if reading.state.is_missing() {
                        muted
                    } else {
                        fg
                    },
                });
            }
        }
        if reading.state == State::Stale {
            rows.push(TipRow::Detail {
                label: String::new(),
                value: i18n::s().stale.to_string(),
                reset: None,
                color: muted,
            });
        }
    }

    rows
}

struct TipLayout {
    width: i32,
    height: i32,
    label_w: i32,
}

fn tip_layout(hdc: HDC, rows: &[TipRow]) -> TipLayout {
    let body = create_font(-11, FW_NORMAL.0 as i32);
    let title = create_font(-12, FW_SEMIBOLD.0 as i32);
    let mut label_w = 0;
    let mut content_w = 0;
    for row in rows {
        match row {
            TipRow::Title { text, .. } => content_w = content_w.max(measure_text(hdc, title, text)),
            TipRow::Detail { label, .. } if !label.is_empty() => {
                label_w = label_w.max(measure_text(hdc, body, label));
            }
            TipRow::Detail { value, .. } => {
                content_w = content_w.max(measure_text(hdc, body, value));
            }
            TipRow::Gap => {}
        }
    }
    label_w += sc(8);

    let mut row_width = 0;
    for row in rows {
        if let TipRow::Detail {
            label,
            value,
            reset,
            ..
        } = row
        {
            if label.is_empty() {
                continue;
            }
            let mut width = label_w + sc(8) + measure_text(hdc, body, value);
            if let Some(reset) = reset {
                width += sc(10) + measure_text(hdc, body, reset);
            }
            row_width = row_width.max(width);
        }
    }
    let width = (TIP_PAD * 2 + row_width)
        .max(content_w + TIP_PAD * 2)
        .max(sc(250))
        .min(sc(580));

    let mut height = TIP_PAD * 2;
    for row in rows {
        height += match row {
            TipRow::Title { .. } => sc(18),
            TipRow::Detail { .. } => sc(16),
            TipRow::Gap => sc(6),
        };
    }
    unsafe {
        let _ = DeleteObject(body);
        let _ = DeleteObject(title);
    }
    TipLayout {
        width,
        height: height.clamp(sc(72), sc(TIP_MAX_HEIGHT)),
        label_w,
    }
}

fn render_tooltip() {
    let (hwnd, readings, is_dark, widget) = {
        let state = lock_state();
        let Some(state) = state.as_ref() else {
            return;
        };
        (
            state.tip_hwnd.to_hwnd(),
            state.readings.clone(),
            state.is_dark,
            state.hwnd.to_hwnd(),
        )
    };
    if !is_tooltip_visible() {
        return;
    }

    let rows = tip_lines(&readings, is_dark);
    let layout = unsafe {
        let screen = GetDC(hwnd);
        let layout = tip_layout(screen, &rows);
        ReleaseDC(hwnd, screen);
        layout
    };
    let width = layout.width;
    let height = layout.height;
    let bg = if is_dark {
        Color::from_hex("#2B2B2B")
    } else {
        Color::from_hex("#FFFFFF")
    };
    let border = if is_dark {
        Color::from_hex("#3C3C3C")
    } else {
        Color::from_hex("#D8D8D8")
    };

    unsafe {
        let screen = GetDC(hwnd);
        let mem = CreateCompatibleDC(screen);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib =
            CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
        if dib.is_invalid() || bits.is_null() {
            let _ = DeleteDC(mem);
            ReleaseDC(hwnd, screen);
            return;
        }
        let old = SelectObject(mem, dib);

        let full = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        let brush = CreateSolidBrush(COLORREF(bg.to_colorref()));
        FillRect(mem, &full, brush);
        let _ = DeleteObject(brush);
        let border_brush = CreateSolidBrush(COLORREF(border.to_colorref()));
        let _ = FrameRect(mem, &full, border_brush);
        let _ = DeleteObject(border_brush);

        let fg = if is_dark {
            Color::from_hex("#F2F2F2")
        } else {
            Color::from_hex("#1A1A1A")
        };
        let muted = if is_dark {
            Color::from_hex("#9A9A9A")
        } else {
            Color::from_hex("#6B6B6B")
        };
        let _ = SetBkMode(mem, TRANSPARENT);
        let body = create_font(-11, FW_NORMAL.0 as i32);
        let title_font = create_font(-12, FW_SEMIBOLD.0 as i32);
        let old_font = SelectObject(mem, body);

        let title_line = sc(18);
        let detail_line = sc(16);
        let gap_line = sc(6);
        let label_left = TIP_PAD;
        let label_right = label_left + layout.label_w;
        let value_left = label_right + sc(8);

        let mut y = TIP_PAD;
        for row in &rows {
            match row {
                TipRow::Title { text, color } => {
                    SelectObject(mem, title_font);
                    let mut rect = RECT {
                        left: TIP_PAD,
                        top: y,
                        right: width - TIP_PAD,
                        bottom: y + title_line,
                    };
                    draw_text(
                        mem,
                        text,
                        &mut rect,
                        DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                        *color,
                    );
                    SelectObject(mem, body);
                    y += title_line;
                }
                TipRow::Detail {
                    label,
                    value,
                    reset,
                    color,
                } => {
                    if label.is_empty() {
                        let mut rect = RECT {
                            left: TIP_PAD,
                            top: y,
                            right: width - TIP_PAD,
                            bottom: y + detail_line,
                        };
                        draw_text(
                            mem,
                            value,
                            &mut rect,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                            *color,
                        );
                    } else {
                        let mut label_rect = RECT {
                            left: label_left,
                            top: y,
                            right: label_right,
                            bottom: y + detail_line,
                        };
                        draw_text(
                            mem,
                            label,
                            &mut label_rect,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                            muted,
                        );
                        let value_width = measure_text(mem, body, value);
                        let reset_left = value_left + value_width + sc(10);
                        let right = if reset.is_some() {
                            reset_left - sc(6)
                        } else {
                            width - TIP_PAD
                        };
                        let mut value_rect = RECT {
                            left: value_left,
                            top: y,
                            right: right,
                            bottom: y + detail_line,
                        };
                        draw_text(
                            mem,
                            value,
                            &mut value_rect,
                            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                            *color,
                        );
                        if let Some(reset) = reset {
                            let reset_width = measure_text(mem, body, reset) + sc(4);
                            let mut reset_rect = RECT {
                                left: reset_left,
                                top: y,
                                right: reset_left + reset_width,
                                bottom: y + detail_line,
                            };
                            draw_text(
                                mem,
                                reset,
                                &mut reset_rect,
                                DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                                muted,
                            );
                        }
                    }
                    y += detail_line;
                }
                TipRow::Gap => {
                    y += gap_line;
                }
            }
        }

        SelectObject(mem, old_font);
        let _ = DeleteObject(body);
        let _ = DeleteObject(title_font);
        let _ = fg;

        let pixels = std::slice::from_raw_parts_mut(bits as *mut u32, (width * height) as usize);
        for pixel in pixels.iter_mut() {
            *pixel = (*pixel & 0x00FF_FFFF) | 0xD900_0000;
        }

        let (x, y_pos) = tooltip_position(widget, width, height);
        let size = SIZE {
            cx: width,
            cy: height,
        };
        let source = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: 0,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: 1,
        };
        let _ = UpdateLayeredWindow(
            hwnd,
            screen,
            Some(&POINT { x, y: y_pos }),
            Some(&size),
            mem,
            Some(&source),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            x,
            y_pos,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );

        SelectObject(mem, old);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem);
        ReleaseDC(hwnd, screen);
    }
}

fn tooltip_position(widget: HWND, width: i32, height: i32) -> (i32, i32) {
    unsafe {
        let mut rect = RECT::default();
        let _ = GetWindowRect(widget, &mut rect);
        let monitor = MonitorFromWindow(widget, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let work = if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            RECT {
                left: 0,
                top: 0,
                right: GetSystemMetrics(SM_CXSCREEN),
                bottom: GetSystemMetrics(SM_CYSCREEN),
            }
        };
        let mut x = rect.left;
        if x + width > work.right {
            x = work.right - width;
        }
        x = x.max(work.left);
        let mut y = rect.bottom + sc(6);
        if y + height > work.bottom {
            y = rect.top - height - sc(20);
        }
        y = y.max(work.top);
        (x, y)
    }
}

fn show_tooltip() {
    let tip = {
        let mut state = lock_state();
        let Some(state) = state.as_mut() else {
            return;
        };
        state.tooltip_visible = true;
        state.tip_hwnd.to_hwnd()
    };
    render_tooltip();
    unsafe {
        let _ = SetWindowPos(
            tip,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        );
        let _ = SetTimer(
            lock_state()
                .as_ref()
                .map(|state| state.hwnd.to_hwnd())
                .unwrap_or_default(),
            TIMER_TOPMOST,
            200,
            None,
        );
    }
}

fn hide_tooltip() {
    let tip = {
        let mut state = lock_state();
        let Some(state) = state.as_mut() else {
            return;
        };
        if !state.tooltip_visible {
            return;
        }
        state.tooltip_visible = false;
        state.hover_armed = false;
        state.tip_hwnd.to_hwnd()
    };
    unsafe {
        let _ = ShowWindow(tip, SW_HIDE);
        if let Some(state) = lock_state().as_ref() {
            let _ = KillTimer(state.hwnd.to_hwnd(), TIMER_TOPMOST);
        }
    }
}

fn cursor_inside(hwnd: HWND) -> bool {
    unsafe {
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return false;
        }
        point.x >= rect.left
            && point.x < rect.right
            && point.y >= rect.top
            && point.y < rect.bottom
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut paint);
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            refresh_dpi();
            position_window();
            render_widget();
            LRESULT(0)
        }
        WM_DPICHANGED => {
            CURRENT_DPI.store((wparam.0 & 0xFFFF) as u32, Ordering::Relaxed);
            position_window();
            render_widget();
            LRESULT(0)
        }
        WM_TIMER => {
            match wparam.0 {
                TIMER_POLL => spawn_poll(hwnd),
                TIMER_COUNTDOWN => render_widget(),
                TIMER_RESET_POLL => spawn_poll(hwnd),
                TIMER_REPOSITION => {
                    let taskbar = lock_state()
                        .as_ref()
                        .and_then(|state| state.taskbar_hwnd);
                    if let Some(taskbar) = taskbar {
                        if !native_interop::find_taskbars()
                            .iter()
                            .any(|candidate| candidate.hwnd == taskbar)
                        {
                            attach_to_taskbar(hwnd, 0);
                        }
                        position_window();
                    }
                }
                TIMER_THEME => {
                    let changed = {
                        let mut state = lock_state();
                        let Some(state) = state.as_mut() else {
                            return LRESULT(0);
                        };
                        let dark = theme::is_dark_mode();
                        if state.is_dark != dark {
                            state.is_dark = dark;
                            true
                        } else {
                            false
                        }
                    };
                    if changed {
                        render_widget();
                    }
                }
                TIMER_TOPMOST => {
                    let tip = lock_state()
                        .as_ref()
                        .map(|state| state.tip_hwnd.to_hwnd())
                        .unwrap_or_default();
                    if is_tooltip_visible() {
                        let _ = SetWindowPos(
                            tip,
                            HWND_TOPMOST,
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
                        );
                    }
                }
                TIMER_HOVER => {
                    let visible = is_tooltip_visible();
                    if !visible && cursor_inside(hwnd) {
                        show_tooltip();
                    }
                    if let Some(state) = lock_state().as_mut() {
                        state.hover_armed = false;
                    }
                    let _ = KillTimer(hwnd, TIMER_HOVER);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_USAGE_UPDATED => {
            render_widget();
            sync_tray(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let dragging = {
                let state = lock_state();
                state
                    .as_ref()
                    .map(|state| state.dragging)
                    .unwrap_or(false)
            };
            if dragging {
                drag_widget(hwnd);
                return LRESULT(0);
            }

            let (armed, visible, tracking) = {
                let state = lock_state();
                state
                    .as_ref()
                    .map(|state| {
                        (
                            state.hover_armed,
                            state.tooltip_visible,
                            state.hover_tracking,
                        )
                    })
                    .unwrap_or((false, false, false))
            };
            if !tracking {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut track);
                if let Some(state) = lock_state().as_mut() {
                    state.hover_tracking = true;
                }
            }
            if !visible && !armed {
                let _ = SetTimer(hwnd, TIMER_HOVER, TIP_DELAY_MS, None);
                if let Some(state) = lock_state().as_mut() {
                    state.hover_armed = true;
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE_MSG => {
            let _ = KillTimer(hwnd, TIMER_HOVER);
            if let Some(state) = lock_state().as_mut() {
                state.hover_armed = false;
                state.hover_tracking = false;
            }
            hide_tooltip();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            hide_tooltip();
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            if x > sc(10) {
                return LRESULT(0);
            }
            let mut point = POINT::default();
            let _ = GetCursorPos(&mut point);
            if let Some(state) = lock_state().as_mut() {
                state.dragging = true;
                state.drag_start_mouse_x = point.x;
                state.drag_start_offset = state.tray_offset;
            }
            let _ = SetCapture(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let was_dragging = {
                let mut state = lock_state();
                match state.as_mut() {
                    Some(state) if state.dragging => {
                        state.dragging = false;
                        true
                    }
                    _ => false,
                }
            };
            if was_dragging {
                let _ = ReleaseCapture();
                save_config();
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            hide_tooltip();
            show_context_menu(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            handle_menu_command(hwnd, wparam.0 as u16);
            LRESULT(0)
        }
        _ if msg == WM_APP_TRAY => {
            match tray::handle_message(lparam) {
                tray::TrayAction::ToggleWidget => toggle_widget(hwnd),
                tray::TrayAction::ShowContextMenu => show_context_menu(hwnd),
                tray::TrayAction::None => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            diagnose::log("WM_DESTROY received");
            tray::remove(hwnd);
            let _ = KillTimer(hwnd, TIMER_POLL);
            let _ = KillTimer(hwnd, TIMER_COUNTDOWN);
            let _ = KillTimer(hwnd, TIMER_RESET_POLL);
            let _ = KillTimer(hwnd, TIMER_REPOSITION);
            let _ = KillTimer(hwnd, TIMER_THEME);
            let _ = KillTimer(hwnd, TIMER_HOVER);
            let _ = KillTimer(hwnd, TIMER_TOPMOST);
            if let Some(tip) = lock_state()
                .as_ref()
                .map(|state| state.tip_hwnd.to_hwnd())
            {
                let _ = DestroyWindow(tip);
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn drag_widget(hwnd: HWND) {
    let mut point = POINT::default();
    let _ = unsafe { GetCursorPos(&mut point) };
    let (width, taskbar_rect, embedded) = {
        let state = lock_state();
        let Some(state) = state.as_ref() else {
            return;
        };
        let width = state.widget_width;
        let taskbar_rect = state
            .taskbar_hwnd
            .and_then(|taskbar| {
                native_interop::get_taskbar_rect(taskbar)
                    .or_else(|| native_interop::get_window_rect_safe(taskbar))
            });
        (width, taskbar_rect, state.embedded)
    };
    let Some(taskbar_rect) = taskbar_rect else {
        return;
    };
    let mut tray_left = taskbar_rect.right;
    if let Some(taskbar) = lock_state()
        .as_ref()
        .and_then(|state| state.taskbar_hwnd)
    {
        if let Some(tray) = native_interop::find_child_window(taskbar, "TrayNotifyWnd") {
            if let Some(rect) = native_interop::get_window_rect_safe(tray) {
                tray_left = rect.left;
            }
        }
    }
    let max_offset = (tray_left - taskbar_rect.left - width).max(0);
    let (new_offset, height) = {
        let mut state = lock_state();
        let Some(state) = state.as_mut() else {
            return;
        };
        let delta = state.drag_start_mouse_x - point.x;
        let offset = (state.drag_start_offset + delta).clamp(0, max_offset);
        state.tray_offset = offset;
        state.config.position.offset = offset;
        (offset, state.widget_height)
    };
    let y = taskbar_rect.top
        + ((taskbar_rect.bottom - taskbar_rect.top - height) / 2).max(0);
    if embedded {
        let x = tray_left - taskbar_rect.left - width - new_offset;
        native_interop::move_window(hwnd, x, y - taskbar_rect.top, width, height);
    } else {
        let x = tray_left - width - new_offset;
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        };
    }
}

fn toggle_widget(hwnd: HWND) {
    let visible = {
        let mut state = lock_state();
        let Some(state) = state.as_mut() else {
            return;
        };
        state.widget_visible = !state.widget_visible;
        if !state.widget_visible {
            state.tooltip_visible = false;
        }
        state.widget_visible
    };
    if visible {
        position_window();
        let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        render_widget();
    } else {
        hide_tooltip();
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
    }
}

fn reload_config() {
    let (new_config, path) = config::load();
    i18n::set(i18n::for_config(&new_config.language));
    let mut state = lock_state();
    let Some(state) = state.as_mut() else {
        return;
    };
    state.config = new_config.clone();
    state.config_path = path;
    state.poll_interval_ms = (new_config.poll_interval_secs * 1000) as u32;
    state.tray_offset = new_config.position.offset;
    unsafe {
        let _ = SetTimer(
            state.hwnd.to_hwnd(),
            TIMER_POLL,
            state.poll_interval_ms,
            None,
        );
    }
}

fn show_context_menu(hwnd: HWND) {
    let (interval, config) = {
        let state = lock_state();
        match state.as_ref() {
            Some(state) => (state.config.poll_interval_secs, state.config.clone()),
            None => (120, Config::default()),
        }
    };
    let ui = i18n::s();
    let autostart_enabled = autostart::is_enabled();

    unsafe {
        let menu = CreatePopupMenu().unwrap();
        append_menu(menu, MENU_ITEM_FLAGS(0), IDM_REFRESH, ui.refresh);

        let freq_menu = CreatePopupMenu().unwrap();
        for (id, secs, label) in [
            (IDM_FREQ_1MIN, 60u64, ui.min1),
            (IDM_FREQ_2MIN, 120, ui.min2),
            (IDM_FREQ_5MIN, 300, ui.min5),
            (IDM_FREQ_15MIN, 900, ui.min15),
            (IDM_FREQ_1HOUR, 3600, ui.hour1),
        ] {
            let flags = if interval == secs { MF_CHECKED } else { MENU_ITEM_FLAGS(0) };
            append_menu(freq_menu, flags, id, label);
        }
        append_submenu(menu, freq_menu, ui.interval);

        let models_menu = CreatePopupMenu().unwrap();
        for (id, provider, label) in [
            (IDM_MODEL_CHATGPT, ProviderId::ChatGpt, "ChatGPT"),
            (IDM_MODEL_ZAI, ProviderId::Zai, "Z.AI AutoClaw"),
            (IDM_MODEL_DEEPSEEK, ProviderId::DeepSeek, "DeepSeek"),
            (IDM_MODEL_CLAUDE, ProviderId::Claude, "Claude Code"),
            (IDM_MODEL_ANTIGRAVITY, ProviderId::Antigravity, "Antigravity"),
            (IDM_MODEL_COPILOT, ProviderId::Copilot, "GitHub Copilot"),
        ] {
            let flags = if providers::is_enabled(&config, provider) {
                MF_CHECKED
            } else {
                MENU_ITEM_FLAGS(0)
            };
            append_menu(models_menu, flags, id, label);
        }
        append_submenu(menu, models_menu, ui.models);

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        append_menu(menu, MENU_ITEM_FLAGS(0), IDM_UPDATE_CHECK, ui.update_app);
        append_menu(
            menu,
            MENU_ITEM_FLAGS(0),
            IDM_VERSION,
            &format!("{} {}", ui.version, env!("CARGO_PKG_VERSION")),
        );

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let auto_flags = if autostart_enabled {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        append_menu(menu, auto_flags, IDM_AUTOSTART, ui.autostart);

        let language_menu = CreatePopupMenu().unwrap();
        let system_flags = if config.language.trim().is_empty()
            || config.language.eq_ignore_ascii_case("system")
        {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        append_menu(
            language_menu,
            system_flags,
            IDM_LANG_SYSTEM,
            ui.system_default,
        );
        for (id, lang) in [
            (IDM_LANG_EN, Lang::En),
            (IDM_LANG_ZH, Lang::Zh),
            (IDM_LANG_ES, Lang::Es),
            (IDM_LANG_HI, Lang::Hi),
            (IDM_LANG_FR, Lang::Fr),
            (IDM_LANG_RU, Lang::Ru),
        ] {
            let flags = if Lang::from_code(&config.language) == Some(lang) {
                MF_CHECKED
            } else {
                MENU_ITEM_FLAGS(0)
            };
            append_menu(language_menu, flags, id, lang.native_label());
        }
        append_submenu(menu, language_menu, ui.language);

        append_menu(menu, MENU_ITEM_FLAGS(0), IDM_RESET_POSITION, ui.reset_position);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        append_menu(menu, MENU_ITEM_FLAGS(0), IDM_EXIT, ui.exit);

        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD,
            point.x,
            point.y,
            0,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        handle_menu_command(hwnd, command.0 as u16);
    }
}

unsafe fn append_menu(menu: HMENU, flags: MENU_ITEM_FLAGS, id: u16, label: &str) {
    let wide = native_interop::wide_str(label);
    let _ = AppendMenuW(menu, flags, id as usize, PCWSTR::from_raw(wide.as_ptr()));
}

unsafe fn append_submenu(menu: HMENU, submenu: HMENU, label: &str) {
    let wide = native_interop::wide_str(label);
    let _ = AppendMenuW(
        menu,
        MF_POPUP,
        submenu.0 as usize,
        PCWSTR::from_raw(wide.as_ptr()),
    );
}

fn handle_menu_command(hwnd: HWND, id: u16) {
    diagnose::log(format!("menu command id={id}"));
    match id {
        IDM_REFRESH => {
            reload_config();
            spawn_poll(hwnd);
        }
        IDM_EXIT => {
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        IDM_FREQ_1MIN => set_poll_interval(hwnd, 60),
        IDM_FREQ_2MIN => set_poll_interval(hwnd, 120),
        IDM_FREQ_5MIN => set_poll_interval(hwnd, 300),
        IDM_FREQ_15MIN => set_poll_interval(hwnd, 900),
        IDM_FREQ_1HOUR => set_poll_interval(hwnd, 3600),
        IDM_MODEL_CHATGPT => toggle_provider(hwnd, ProviderId::ChatGpt),
        IDM_MODEL_ZAI => toggle_provider(hwnd, ProviderId::Zai),
        IDM_MODEL_DEEPSEEK => toggle_provider(hwnd, ProviderId::DeepSeek),
        IDM_MODEL_CLAUDE => toggle_provider(hwnd, ProviderId::Claude),
        IDM_MODEL_ANTIGRAVITY => toggle_provider(hwnd, ProviderId::Antigravity),
        IDM_MODEL_COPILOT => toggle_provider(hwnd, ProviderId::Copilot),
        IDM_UPDATE_CHECK | IDM_VERSION => check_updates(hwnd),
        IDM_AUTOSTART => toggle_autostart(hwnd),
        IDM_LANG_SYSTEM => set_language(hwnd, "system"),
        IDM_LANG_EN => set_language(hwnd, "en"),
        IDM_LANG_ZH => set_language(hwnd, "zh"),
        IDM_LANG_ES => set_language(hwnd, "es"),
        IDM_LANG_HI => set_language(hwnd, "hi"),
        IDM_LANG_FR => set_language(hwnd, "fr"),
        IDM_LANG_RU => set_language(hwnd, "ru"),
        IDM_RESET_POSITION => reset_position(hwnd),
        _ => {}
    }
}

fn toggle_autostart(hwnd: HWND) {
    let enabled = autostart::is_enabled();
    if let Err(_) = autostart::set_enabled(!enabled) {
        show_message(
            hwnd,
            i18n::s().autostart,
            i18n::s().autostart_failed,
            MB_OK | MB_ICONWARNING,
        );
    }
}

fn set_language(hwnd: HWND, code: &str) {
    {
        let mut state = lock_state();
        let Some(state) = state.as_mut() else {
            return;
        };
        state.config.language = code.to_string();
    }
    i18n::set(i18n::for_config(code));
    save_config();
    render_widget();
    sync_tray(hwnd);
    if is_tooltip_visible() {
        render_tooltip();
    }
}

fn set_poll_interval(hwnd: HWND, secs: u64) {
    {
        let mut state = lock_state();
        let Some(state) = state.as_mut() else {
            return;
        };
        state.config.poll_interval_secs = secs;
        state.poll_interval_ms = (secs * 1000) as u32;
        unsafe {
            let _ = SetTimer(hwnd, TIMER_POLL, state.poll_interval_ms, None);
        }
    }
    save_config();
    spawn_poll(hwnd);
}

fn toggle_provider(hwnd: HWND, provider: ProviderId) {
    let (currently, active) = {
        let state = lock_state();
        let Some(state) = state.as_ref() else {
            return;
        };
        (
            providers::is_enabled(&state.config, provider),
            ProviderId::ALL
                .iter()
                .filter(|candidate| providers::is_enabled(&state.config, **candidate))
                .count(),
        )
    };
    if currently && active <= 1 {
        return;
    }
    if let Some(state) = lock_state().as_mut() {
        let enabled = !currently;
        match provider {
            ProviderId::ChatGpt => state.config.providers.chatgpt.enabled = enabled,
            ProviderId::Zai => state.config.providers.zai.enabled = enabled,
            ProviderId::DeepSeek => state.config.providers.deepseek.enabled = enabled,
            ProviderId::Claude => state.config.providers.claude.enabled = enabled,
            ProviderId::Antigravity => state.config.providers.antigravity.enabled = enabled,
            ProviderId::Copilot => state.config.providers.copilot.enabled = enabled,
        }
    }
    save_config();
    spawn_poll(hwnd);
}

fn reset_position(hwnd: HWND) {
    {
        let mut state = lock_state();
        let Some(state) = state.as_mut() else {
            return;
        };
        state.tray_offset = 0;
        state.config.position.offset = 0;
    }
    save_config();
    position_window();
    let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
    render_widget();
}

fn check_updates(hwnd: HWND) {
    let ui = i18n::s();
    match updater::check() {
        Ok(Some(release)) => {
            let version = release
                .name
                .clone()
                .unwrap_or_else(|| release.tag_name.clone());
            let text = format!(
                "{}: {version}\n{} {}.\n\n{}",
                ui.update_available,
                ui.version,
                env!("CARGO_PKG_VERSION"),
                ui.update_confirm
            );
            let result = show_message(hwnd, ui.update_title, &text, MB_YESNO | MB_ICONINFORMATION);
            if result == IDYES {
                match updater::install(&release) {
                    Ok(()) => {
                        show_message(
                            hwnd,
                            ui.update_title,
                            ui.update_downloaded,
                            MB_OK | MB_ICONINFORMATION,
                        );
                        let _ = unsafe { DestroyWindow(hwnd) };
                    }
                    Err(error) => {
                        show_message(hwnd, ui.update_title, &error, MB_OK | MB_ICONWARNING);
                        updater::open_release(&release);
                    }
                }
            }
        }
        Ok(None) => {
            show_message(
                hwnd,
                ui.update_title,
                ui.last_version,
                MB_OK | MB_ICONINFORMATION,
            );
        }
        Err(error) => {
            show_message(
                hwnd,
                ui.update_title,
                &format!("{}: {error}", ui.update_failed),
                MB_OK | MB_ICONWARNING,
            );
        }
    }
}

fn show_message(hwnd: HWND, title: &str, text: &str, flags: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    let title = native_interop::wide_str(title);
    let text = native_interop::wide_str(text);
    unsafe {
        MessageBoxW(
            hwnd,
            PCWSTR::from_raw(text.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            flags,
        )
    }
}

unsafe extern "system" fn tip_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut paint);
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
