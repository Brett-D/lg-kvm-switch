//! The Windows watcher: a hidden window that receives USB and power
//! notifications, plus an optional notification-area icon and menu.

use super::{autostart, icon, log_path, wide};
use crate::config::{self, Config};
use crate::controller::{Controller, Outcome};
use crate::lg::input_name;
use crate::log;
use crate::{monitor, usb};
use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU32, Ordering};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, MessageBoxW, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterDeviceNotificationW, RegisterWindowMessageW,
    SetForegroundWindow, SetTimer, TrackPopupMenu, TranslateMessage, DBT_DEVTYP_DEVICEINTERFACE,
    DEVICE_NOTIFY_WINDOW_HANDLE, DEV_BROADCAST_DEVICEINTERFACE_W, HICON, MB_ICONERROR, MB_OK,
    MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PBT_APMRESUMEAUTOMATIC, SW_SHOWNORMAL,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_CLOSE, WM_DESTROY, WM_DEVICECHANGE,
    WM_LBUTTONUP, WM_NULL, WM_POWERBROADCAST, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_OVERLAPPED,
};

const WM_TRAY: u32 = WM_APP + 1;
const SCAN_TIMER: usize = 1;
const RETRY_MS: u32 = 5_000;
const RESUME_MS: u32 = 2_000;
const USB_DEVICE_INTERFACE: GUID = GUID::from_u128(0xA5DCBF10_6530_11D2_901F_00C04FB951ED);

const CMD_LOCAL: usize = 10;
const CMD_REMOTE: usize = 11;
const CMD_AUTO: usize = 12;
const CMD_STARTUP: usize = 13;
const CMD_EDIT: usize = 14;
const CMD_RELOAD: usize = 15;
const CMD_LOG: usize = 16;
const CMD_EXIT: usize = 17;

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app state. Returns `None` during re-entrant messages
/// (modal loops dispatch messages while the state is borrowed) or before the
/// state exists.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

#[derive(Clone, Copy)]
enum State {
    Here = 0,
    Away = 1,
    Idle = 2,
    Problem = 3,
}

struct App {
    hwnd: HWND,
    tray: bool,
    config_path: PathBuf,
    custom_config: bool,
    controller: Option<Controller>,
    problem: Option<String>,
    icons: [HICON; 4],
}

enum MenuItem {
    Separator,
    Item {
        id: usize,
        label: String,
        checked: bool,
        enabled: bool,
    },
}

impl App {
    fn load_config(&mut self) {
        let auto = self.controller.as_ref().map_or(true, |c| c.auto);
        let result = (|| {
            if !self.config_path.exists() {
                config::save(&self.config_path, &Config::default())?;
                log!("Created {}", self.config_path.display());
            }
            let settings = config::load(&self.config_path)?.settings()?;
            if settings.watch.is_empty() {
                return Err(
                    "no watch_devices configured; run `lg-kvm-switch learn` or edit the config"
                        .to_string(),
                );
            }
            Ok(Controller::new(settings.clone(), monitor::open(&settings)))
        })();
        match result {
            Ok(mut controller) => {
                controller.auto = auto;
                let s = controller.settings();
                let ids: Vec<String> = s.watch.iter().map(|id| id.to_string()).collect();
                log!(
                    "Watching {} -> {} here, {} away",
                    ids.join(", "),
                    input_name(s.local_input),
                    input_name(s.remote_input)
                );
                self.controller = Some(controller);
                self.problem = None;
            }
            Err(e) => {
                log!("Config problem: {e}");
                self.controller = None;
                self.problem = Some(e);
            }
        }
        self.refresh();
    }

    fn debounce_ms(&self) -> u32 {
        self.controller
            .as_ref()
            .map_or(750, |c| c.settings().debounce.as_millis() as u32)
    }

    /// Checks the KVM position. Returns a delay if a retry is needed.
    fn scan(&mut self) -> Option<u32> {
        let controller = self.controller.as_mut()?;
        let retry = match usb::present_ids() {
            Ok(present) => {
                let here = controller.is_here(&present);
                matches!(controller.update(here), Outcome::Failed(_))
            }
            Err(e) => {
                log!("Could not list USB devices: {e}");
                true
            }
        };
        self.refresh();
        retry.then_some(RETRY_MS)
    }

    fn status(&self) -> (State, String) {
        if let Some(problem) = &self.problem {
            return (State::Problem, format!("Config problem: {problem}"));
        }
        let Some(c) = &self.controller else {
            return (State::Problem, "Not configured".into());
        };
        if let Some(e) = &c.last_error {
            return (State::Problem, format!("Error: {e}"));
        }
        let s = c.settings();
        match (c.auto, c.kvm_here) {
            (false, _) => (State::Idle, "Automatic switching paused".into()),
            (true, None) => (State::Idle, "Checking USB devices…".into()),
            (true, Some(true)) => (
                State::Here,
                format!("KVM on this PC: {}", input_name(s.local_input)),
            ),
            (true, Some(false)) => (
                State::Away,
                format!("KVM on other PC: {}", input_name(s.remote_input)),
            ),
        }
    }

    fn notify_data(&self) -> NOTIFYICONDATAW {
        let (state, text) = self.status();
        // SAFETY: NOTIFYICONDATAW is plain data; zero is a valid initial state.
        let mut data: NOTIFYICONDATAW = unsafe { zeroed() };
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.hwnd;
        data.uID = 1;
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = self.icons[state as usize];
        let tip: Vec<u16> = format!("LG KVM Switch\n{text}")
            .encode_utf16()
            .take(data.szTip.len() - 1)
            .collect();
        data.szTip[..tip.len()].copy_from_slice(&tip);
        data
    }

    fn add_tray_icon(&self) {
        if self.tray {
            // SAFETY: fully initialized notify data for our window.
            unsafe { Shell_NotifyIconW(NIM_ADD, &self.notify_data()) };
        }
    }

    fn refresh(&self) {
        if self.tray {
            // SAFETY: as above.
            unsafe { Shell_NotifyIconW(NIM_MODIFY, &self.notify_data()) };
        }
    }

    fn remove_tray_icon(&self) {
        if self.tray {
            // SAFETY: as above.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &self.notify_data()) };
        }
    }

    fn menu(&self) -> Vec<MenuItem> {
        let item = |id, label: String, checked, enabled| MenuItem::Item {
            id,
            label,
            checked,
            enabled,
        };
        let ready = self.controller.is_some();
        let (local, remote) = self.controller.as_ref().map_or_else(
            || ("this PC".to_string(), "other PC".to_string()),
            |c| {
                (
                    format!("this PC ({})", input_name(c.settings().local_input)),
                    format!("other PC ({})", input_name(c.settings().remote_input)),
                )
            },
        );
        let auto = self.controller.as_ref().is_some_and(|c| c.auto);
        vec![
            item(0, self.status().1, false, false),
            MenuItem::Separator,
            item(CMD_LOCAL, format!("Show {local}"), false, ready),
            item(CMD_REMOTE, format!("Show {remote}"), false, ready),
            MenuItem::Separator,
            item(CMD_AUTO, "Switch automatically".into(), auto, ready),
            item(
                CMD_STARTUP,
                "Start with Windows".into(),
                autostart::is_enabled(),
                true,
            ),
            MenuItem::Separator,
            item(CMD_EDIT, "Edit config".into(), false, true),
            item(CMD_RELOAD, "Reload config".into(), false, true),
            item(CMD_LOG, "Open log".into(), false, true),
            MenuItem::Separator,
            item(CMD_EXIT, "Exit".into(), false, true),
        ]
    }

    fn show(&mut self, local: bool) {
        if let Some(c) = self.controller.as_mut() {
            let s = c.settings();
            let input = if local { s.local_input } else { s.remote_input };
            let _ = c.show(input);
        }
        self.refresh();
    }

    fn toggle_auto(&mut self) {
        if let Some(c) = self.controller.as_mut() {
            c.auto = !c.auto;
            log!(
                "Automatic switching {}",
                if c.auto { "resumed" } else { "paused" }
            );
        }
        self.refresh();
    }

    fn toggle_startup(&self) {
        let result = if autostart::is_enabled() {
            autostart::disable().map(|()| "off")
        } else {
            std::env::current_exe()
                .map_err(|e| e.to_string())
                .and_then(|exe| {
                    let config = self.custom_config.then_some(self.config_path.as_path());
                    autostart::enable(&exe, config)
                })
                .map(|()| "on")
        };
        match result {
            Ok(state) => log!("Start with Windows: {state}"),
            Err(e) => log!("Could not change start-with-Windows: {e}"),
        }
    }
}

/// Runs the watcher until the window closes. With `tray`, shows the
/// notification-area icon and logs to a file; otherwise logs to stdout.
pub fn run(config_path: Option<PathBuf>, tray: bool) -> Result<(), String> {
    let custom_config = config_path.is_some();
    let config_path = config_path.unwrap_or_else(config::default_path);
    if tray {
        log::set_stdout(false);
        let _ = log::to_file(&log_path());
        let name = wide("Local\\lg-kvm-switch-tray");
        // SAFETY: named mutex; the handle is kept for the process lifetime.
        let mutex = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
        if !mutex.is_null() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            log!("Already running; exiting");
            return Ok(());
        }
    }

    // SAFETY: standard Win32 window setup; all strings outlive the calls.
    let hwnd = unsafe {
        let instance = GetModuleHandleW(null());
        let class_name = wide("LgKvmSwitchWindow");
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..zeroed()
        };
        if RegisterClassW(&class) == 0 {
            return Err(format!("RegisterClassW failed ({})", GetLastError()));
        }
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            Ordering::Relaxed,
        );
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class_name.as_ptr(),
            wide("LG KVM Switch").as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return Err(format!("CreateWindowExW failed ({})", GetLastError()));
        }
        register_usb_notifications(hwnd);
        hwnd
    };

    let mut app = App {
        hwnd,
        tray,
        config_path,
        custom_config,
        controller: None,
        problem: None,
        icons: [
            icon::monitor_icon(0x2E8BFF),
            icon::monitor_icon(0xFF9F1C),
            icon::monitor_icon(0x9A9A9A),
            icon::monitor_icon(0xE5484D),
        ],
    };
    log!("Started (config: {})", app.config_path.display());
    app.add_tray_icon();
    app.load_config();
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    schedule_scan(hwnd, 10);

    // SAFETY: standard message loop.
    unsafe {
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    APP.with(|cell| cell.borrow_mut().take());
    log!("Stopped");
    Ok(())
}

pub fn error_box(message: &str) {
    // SAFETY: NUL-terminated strings that outlive the call.
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(message).as_ptr(),
            wide("LG KVM Switch").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

unsafe fn register_usb_notifications(hwnd: HWND) {
    let mut filter: DEV_BROADCAST_DEVICEINTERFACE_W = zeroed();
    filter.dbcc_size = size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32;
    filter.dbcc_devicetype = DBT_DEVTYP_DEVICEINTERFACE;
    filter.dbcc_classguid = USB_DEVICE_INTERFACE;
    let handle = RegisterDeviceNotificationW(
        hwnd,
        (&filter as *const DEV_BROADCAST_DEVICEINTERFACE_W).cast::<c_void>(),
        DEVICE_NOTIFY_WINDOW_HANDLE,
    );
    if handle.is_null() {
        log!(
            "USB notifications unavailable ({}); relying on device broadcasts",
            GetLastError()
        );
    }
}

/// (Re)starts the scan timer, so bursts of USB events collapse into one scan.
fn schedule_scan(hwnd: HWND, delay_ms: u32) {
    // SAFETY: timer on our own window.
    unsafe { SetTimer(hwnd, SCAN_TIMER, delay_ms.max(10), None) };
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_DEVICECHANGE => {
            let delay = with_app(|app| app.debounce_ms()).unwrap_or(750);
            schedule_scan(hwnd, delay);
            1
        }
        WM_POWERBROADCAST => {
            if wparam == PBT_APMRESUMEAUTOMATIC as usize {
                schedule_scan(hwnd, RESUME_MS);
            }
            1
        }
        WM_TIMER if wparam == SCAN_TIMER => {
            KillTimer(hwnd, SCAN_TIMER);
            if let Some(Some(retry)) = with_app(|app| app.scan()) {
                schedule_scan(hwnd, retry);
            }
            0
        }
        WM_TRAY => {
            let event = (lparam as u32) & 0xFFFF;
            if event == WM_LBUTTONUP || event == WM_RBUTTONUP {
                show_menu(hwnd);
            }
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            with_app(|app| app.remove_tray_icon());
            PostQuitMessage(0);
            0
        }
        _ if msg != 0 && msg == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            with_app(|app| app.add_tray_icon());
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let Some(items) = with_app(|app| app.menu()) else {
        return;
    };
    let menu = CreatePopupMenu();
    for item in items {
        match item {
            MenuItem::Separator => {
                AppendMenuW(menu, MF_SEPARATOR, 0, null());
            }
            MenuItem::Item {
                id,
                label,
                checked,
                enabled,
            } => {
                let mut flags = MF_STRING;
                if checked {
                    flags |= MF_CHECKED;
                }
                if !enabled {
                    flags |= MF_GRAYED;
                }
                AppendMenuW(menu, flags, id, wide(&label).as_ptr());
            }
        }
    }
    let mut cursor = POINT { x: 0, y: 0 };
    GetCursorPos(&mut cursor);
    // Required so the menu closes when clicking elsewhere.
    SetForegroundWindow(hwnd);
    let command = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
        cursor.x,
        cursor.y,
        0,
        hwnd,
        null(),
    );
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);
    if command > 0 {
        run_command(hwnd, command as usize);
    }
}

fn run_command(hwnd: HWND, command: usize) {
    match command {
        CMD_LOCAL | CMD_REMOTE => {
            with_app(|app| app.show(command == CMD_LOCAL));
        }
        CMD_AUTO => {
            with_app(|app| app.toggle_auto());
            schedule_scan(hwnd, 10);
        }
        CMD_STARTUP => {
            with_app(|app| app.toggle_startup());
        }
        CMD_EDIT => {
            if let Some(path) = with_app(|app| app.config_path.clone()) {
                if !path.exists() {
                    let _ = config::save(&path, &Config::default());
                }
                open_in_notepad(&path);
            }
        }
        CMD_RELOAD => {
            with_app(|app| app.load_config());
            schedule_scan(hwnd, 10);
        }
        CMD_LOG => open_in_notepad(&log_path()),
        CMD_EXIT => {
            // SAFETY: posting to our own window.
            unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
        }
        _ => {}
    }
}

fn open_in_notepad(path: &Path) {
    let args = wide(format!("\"{}\"", path.display()));
    // SAFETY: NUL-terminated strings that outlive the call.
    unsafe {
        ShellExecuteW(
            null_mut(),
            wide("open").as_ptr(),
            wide("notepad.exe").as_ptr(),
            args.as_ptr(),
            null(),
            SW_SHOWNORMAL,
        );
    }
}
