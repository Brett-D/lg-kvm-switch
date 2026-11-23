//! The Windows watcher: a hidden window that receives USB and power
//! notifications, plus an optional notification-area icon with a flyout
//! (left click) and a menu (right click).

use super::flyout::{self, Action, Flyout, Model, Slider, Tile};
use super::{autostart, icon, log_path, wide};
use crate::config::{self, Config};
use crate::controller::{Controller, Levels, Outcome, Step, SETTLE};
use crate::lg::{self, input_name};
use crate::log;
use crate::{monitor, usb};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::mem::{size_of, transmute, zeroed};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU32, Ordering};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, MessageBoxW, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterDeviceNotificationW, RegisterWindowMessageW,
    SetForegroundWindow, SetTimer, TrackPopupMenu, TranslateMessage, DBT_DEVTYP_DEVICEINTERFACE,
    DEVICE_NOTIFY_WINDOW_HANDLE, DEV_BROADCAST_DEVICEINTERFACE_W, HICON, HMENU, MB_ICONERROR,
    MB_OK, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PBT_APMRESUMEAUTOMATIC,
    SW_SHOWNORMAL, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_CLOSE, WM_DESTROY,
    WM_DEVICECHANGE, WM_LBUTTONUP, WM_NULL, WM_POWERBROADCAST, WM_RBUTTONUP, WM_SETTINGCHANGE,
    WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};

const WM_TRAY: u32 = WM_APP + 1;
const WM_SHOW_MENU: u32 = WM_APP + 2;
const SCAN_TIMER: usize = 1;
/// Runs the next step of a multi-step monitor change.
const ACTION_TIMER: usize = 2;
const REFRESH_TIMER: usize = 3;
const RETRY_MS: u32 = 5_000;
const RESUME_MS: u32 = 2_000;
const STARTUP_REFRESH_MS: u32 = 1_500;
const OPEN_REFRESH_MS: u32 = 250;
/// Delay before the first step, so the flyout repaints first.
const FIRST_STEP_MS: u32 = 30;
/// How long to put off a KVM check while a monitor change is running.
const BUSY_RETRY_MS: u32 = 1_000;
const USB_DEVICE_INTERFACE: GUID = GUID::from_u128(0xA5DCBF10_6530_11D2_901F_00C04FB951ED);
/// Monitor controls behind the flyout's sliders, top to bottom.
const SLIDER_VCPS: [u8; 3] = [lg::VCP_BRIGHTNESS, lg::VCP_CONTRAST, lg::VCP_VOLUME];

const CMD_LOCAL: usize = 10;
const CMD_REMOTE: usize = 11;
const CMD_AUTO: usize = 12;
const CMD_STARTUP: usize = 13;
const CMD_EDIT: usize = 14;
const CMD_RELOAD: usize = 15;
const CMD_LOG: usize = 16;
const CMD_EXIT: usize = 17;
const CMD_PBP: usize = 18;
const CMD_SWAP: usize = 19;

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

/// A multi-step monitor change, run one step per timer tick so the UI stays
/// responsive while the monitor settles.
struct Job {
    steps: VecDeque<Step>,
    action: String,
}

struct App {
    hwnd: HWND,
    tray: bool,
    config_path: PathBuf,
    custom_config: bool,
    controller: Option<Controller>,
    problem: Option<String>,
    icons: [HICON; 4],
    flyout: Option<Flyout>,
    /// Last known monitor settings; read in the background so the flyout
    /// opens instantly.
    levels: Levels,
    job: Option<Job>,
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
        let auto = self.controller.as_ref().is_none_or(|c| c.auto);
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
        if self.job.is_some() {
            return Some(BUSY_RETRY_MS);
        }
        let controller = self.controller.as_mut()?;
        let retry = match usb::present_ids() {
            Ok(present) => {
                let here = controller.is_here(&present);
                match controller.update(here) {
                    Outcome::Failed(_) => true,
                    // Check again in case split screen is turned off from
                    // the monitor's menu.
                    Outcome::PictureByPicture => {
                        self.levels.pbp = Some(true);
                        true
                    }
                    Outcome::Switched(_) | Outcome::AlreadyShown(_) => {
                        self.levels.pbp = Some(false);
                        false
                    }
                    _ => false,
                }
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
        if self.levels.pbp == Some(true) {
            let state = if c.kvm_here == Some(false) {
                State::Away
            } else {
                State::Here
            };
            let left = c.main_input().unwrap_or(s.pbp_left);
            let right = if left == s.pbp_left {
                s.pbp_right
            } else {
                s.pbp_left
            };
            let layout = self.levels.layout.unwrap_or(lg::PBP_SIDE_BY_SIDE);
            return (
                state,
                format!(
                    "{}: {} | {}",
                    lg::layout_name(layout),
                    input_name(left),
                    input_name(right)
                ),
            );
        }
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

    /// Updates the tray icon and, if open, the flyout.
    fn refresh(&mut self) {
        if self.tray {
            // SAFETY: as above.
            unsafe { Shell_NotifyIconW(NIM_MODIFY, &self.notify_data()) };
        }
        if self.flyout.as_ref().is_some_and(Flyout::is_visible) {
            let model = self.flyout_model();
            if let Some(flyout) = self.flyout.as_mut() {
                flyout.update(model);
            }
        }
    }

    fn remove_tray_icon(&self) {
        if self.tray {
            // SAFETY: as above.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &self.notify_data()) };
        }
    }

    fn refresh_levels(&mut self) {
        if let Some(c) = self.controller.as_mut() {
            self.levels = c.levels();
        }
        self.refresh();
    }

    fn flyout_model(&self) -> Model {
        let pbp = self.levels.pbp == Some(true);
        let (ready, auto, main, local, remote) = match &self.controller {
            Some(c) => (
                true,
                c.auto,
                c.main_input(),
                c.settings().local_input,
                c.settings().remote_input,
            ),
            None => (false, false, None, 0, 0),
        };
        let tile = |glyph, label: &str, on| Tile {
            glyph,
            label: label.into(),
            on,
            enabled: ready,
        };
        let muted = self.levels.muted == Some(true);
        let slider = |glyph, label: &str, value, icon_button| Slider {
            glyph,
            label: label.into(),
            value,
            icon_button,
        };
        // PIP can only be turned on from the monitor's menu; show it if it is.
        let split_label = if pbp && self.levels.layout == Some(lg::PBP_PIP) {
            "Pic in pic"
        } else {
            "Side by side"
        };
        Model {
            tiles: vec![
                tile(
                    flyout::GLYPH_THIS_PC,
                    "This PC",
                    ready && !pbp && main == Some(local),
                ),
                tile(
                    flyout::GLYPH_OTHER_PC,
                    "Other PC",
                    ready && !pbp && main == Some(remote),
                ),
                tile(flyout::GLYPH_SIDE_BY_SIDE, split_label, pbp),
                Tile {
                    enabled: ready && pbp,
                    ..tile(flyout::GLYPH_SWAP, "Swap sides", false)
                },
                tile(flyout::GLYPH_AUTO, "Auto switch", auto),
            ],
            sliders: vec![
                slider(
                    flyout::GLYPH_BRIGHTNESS,
                    "Brightness",
                    self.levels.brightness,
                    false,
                ),
                slider(
                    flyout::GLYPH_CONTRAST,
                    "Contrast",
                    self.levels.contrast,
                    false,
                ),
                slider(
                    if muted {
                        flyout::GLYPH_MUTED
                    } else {
                        flyout::GLYPH_VOLUME
                    },
                    if muted { "Volume (muted)" } else { "Volume" },
                    self.levels.volume,
                    self.levels.muted.is_some(),
                ),
            ],
            status: self.status().1,
        }
    }

    /// Opens or closes the flyout. `None` if there is no flyout; otherwise
    /// whether it opened.
    fn toggle_flyout(&mut self) -> Option<bool> {
        let model = self.flyout_model();
        let flyout = self.flyout.as_mut()?;
        if flyout.is_visible() {
            if let Some(action) = flyout.hide() {
                self.flyout_action(action);
            }
            return Some(false);
        }
        if flyout.just_closed() {
            return Some(false);
        }
        flyout.show(model);
        Some(true)
    }

    fn flyout_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let (result, action) = self.flyout.as_mut()?.handle(msg, wparam, lparam);
        if let Some(action) = action {
            self.flyout_action(action);
        }
        result
    }

    fn flyout_action(&mut self, action: Action) {
        match action {
            Action::Tile(0) => self.show(true),
            Action::Tile(1) => self.show(false),
            Action::Tile(2) => self.toggle_pbp(),
            Action::Tile(3) => self.swap_sides(),
            Action::Tile(4) => {
                self.toggle_auto();
                schedule_scan(self.hwnd, 10);
            }
            Action::Tile(_) => {}
            Action::SetLevel(i, value) => self.set_level(i, value),
            Action::Icon(_) => self.toggle_mute(),
            Action::Settings => {
                if let Some(Action::SetLevel(i, value)) =
                    self.flyout.as_mut().and_then(Flyout::hide)
                {
                    self.set_level(i, value);
                }
                // SAFETY: posting to our own window; the menu opens outside
                // this borrow of the app state.
                unsafe { PostMessageW(self.hwnd, WM_SHOW_MENU, 0, 0) };
            }
        }
        self.refresh();
    }

    /// Shows this or the other computer full screen.
    fn show(&mut self, local: bool) {
        if self.busy() {
            return;
        }
        let Some(c) = self.controller.as_mut() else {
            return;
        };
        let s = c.settings();
        let input = if local { s.local_input } else { s.remote_input };
        let steps = c.show_steps(input);
        self.start_job(steps, format!("switch to {} (manual)", input_name(input)));
    }

    fn toggle_pbp(&mut self) {
        if self.busy() {
            return;
        }
        let Some(c) = self.controller.as_mut() else {
            return;
        };
        let on = self.levels.pbp != Some(true);
        let (steps, action) = (c.pbp_steps(on), c.pbp_description(on));
        // Show the new state right away; the change takes a few seconds.
        self.levels.pbp = Some(on);
        self.start_job(steps, action);
    }

    fn swap_sides(&mut self) {
        if self.levels.pbp != Some(true) || self.busy() {
            return;
        }
        let Some(c) = self.controller.as_mut() else {
            return;
        };
        let (steps, left) = c.swap_pbp();
        self.start_job(
            steps,
            format!("swap sides ({} now on the left)", input_name(left)),
        );
        self.save_split();
    }

    /// Saves the split-screen sides so they're used next time.
    fn save_split(&self) {
        let Some(c) = &self.controller else {
            return;
        };
        let s = c.settings();
        let saved = config::load(&self.config_path).and_then(|mut config| {
            config.pbp_left = Some(lg::input_key(s.pbp_left));
            config.pbp_right = Some(lg::input_key(s.pbp_right));
            config::save(&self.config_path, &config)
        });
        if let Err(e) = saved {
            log!("Could not save the split-screen settings: {e}");
        }
    }

    /// Whether a multi-step change is still running. Other monitor changes
    /// are refused until it finishes, so they can't interleave with it.
    fn busy(&self) -> bool {
        if self.job.is_some() {
            log!("Still applying the last change; try again in a few seconds");
        }
        self.job.is_some()
    }

    fn start_job(&mut self, steps: Vec<Step>, action: String) {
        if self.job.is_some() || steps.is_empty() {
            return;
        }
        self.job = Some(Job {
            steps: steps.into(),
            action,
        });
        set_timer(self.hwnd, ACTION_TIMER, FIRST_STEP_MS);
    }

    /// Runs the next step of the current job. Returns the delay before the
    /// following step, if any.
    fn run_job_step(&mut self) -> Option<u32> {
        let Some(c) = self.controller.as_mut() else {
            // The config was reloaded with an error; drop the change.
            self.job = None;
            return None;
        };
        let job = self.job.as_mut()?;
        let step = job.steps.pop_front()?;
        let result = c.run_step(step);
        if result.is_ok() && !job.steps.is_empty() {
            return Some(SETTLE.as_millis() as u32);
        }
        c.record(&result, &job.action);
        self.levels.pbp = c.pbp();
        self.job = None;
        if result.is_err() {
            // The controller retries the KVM position if the job left it
            // unhandled, e.g. split screen went off but the input didn't change.
            schedule_scan(self.hwnd, RETRY_MS);
        }
        self.refresh();
        None
    }

    fn set_level(&mut self, slider: usize, value: u16) {
        let (Some(&vcp), Some(c)) = (SLIDER_VCPS.get(slider), self.controller.as_mut()) else {
            return;
        };
        if c.set_level(vcp, value).is_ok() {
            let level = match slider {
                0 => &mut self.levels.brightness,
                1 => &mut self.levels.contrast,
                _ => &mut self.levels.volume,
            };
            *level = Some(value);
        }
    }

    fn toggle_mute(&mut self) {
        let Some(c) = self.controller.as_mut() else {
            return;
        };
        let mute = self.levels.muted != Some(true);
        let value = if mute { lg::MUTE_ON } else { lg::MUTE_OFF };
        if c.set_level(lg::VCP_MUTE, value).is_ok() {
            self.levels.muted = Some(mute);
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
        let pbp = self.levels.pbp == Some(true);
        vec![
            item(0, self.status().1, false, false),
            MenuItem::Separator,
            item(CMD_LOCAL, format!("Show {local}"), false, ready),
            item(CMD_REMOTE, format!("Show {remote}"), false, ready),
            item(CMD_PBP, "Split screen (side by side)".into(), pbp, ready),
            item(CMD_SWAP, "Swap sides".into(), false, ready && pbp),
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
/// notification-area icon and flyout and logs to a file; otherwise logs to
/// stdout.
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
        // SAFETY: called before any window exists. Sharp text on every
        // monitor; the flyout scales itself.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        follow_dark_mode_in_menus();
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
    let flyout = if tray {
        Flyout::create(Some(flyout_proc))
            .map_err(|e| log!("Flyout unavailable, using the menu instead: {e}"))
            .ok()
    } else {
        None
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
        flyout,
        levels: Levels::default(),
        job: None,
    };
    log!("Started (config: {})", app.config_path.display());
    app.add_tray_icon();
    app.load_config();
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    schedule_scan(hwnd, 10);
    if tray {
        set_timer(hwnd, REFRESH_TIMER, STARTUP_REFRESH_MS);
    }

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

fn set_timer(hwnd: HWND, id: usize, delay_ms: u32) {
    // SAFETY: timer on our own window.
    unsafe { SetTimer(hwnd, id, delay_ms.max(10), None) };
}

/// uxtheme's `SetPreferredAppMode` (ordinal 135) and `FlushMenuThemes`
/// (ordinal 136). Undocumented, but used by Windows' own apps since Windows
/// 10 1903; without them, Win32 menus are always light.
fn uxtheme_function(ordinal: usize) -> Option<unsafe extern "system" fn() -> isize> {
    // SAFETY: uxtheme is a system DLL; ordinals are passed as the low word
    // of the name pointer, as GetProcAddress documents.
    unsafe {
        let library = LoadLibraryW(wide("uxtheme.dll").as_ptr());
        if library.is_null() {
            return None;
        }
        GetProcAddress(library, ordinal as *const u8)
    }
}

/// Makes the app's menus follow the system light/dark setting.
fn follow_dark_mode_in_menus() {
    const ALLOW_DARK: i32 = 1;
    if let Some(function) = uxtheme_function(135) {
        // SAFETY: SetPreferredAppMode takes one enum argument.
        unsafe {
            let set_preferred_app_mode: unsafe extern "system" fn(i32) -> i32 = transmute(function);
            set_preferred_app_mode(ALLOW_DARK);
        }
    }
    refresh_menu_theme();
}

fn refresh_menu_theme() {
    if let Some(flush_menu_themes) = uxtheme_function(136) {
        // SAFETY: FlushMenuThemes takes no arguments.
        unsafe { flush_menu_themes() };
    }
}

/// (Re)starts the scan timer, so bursts of USB events collapse into one scan.
fn schedule_scan(hwnd: HWND, delay_ms: u32) {
    set_timer(hwnd, SCAN_TIMER, delay_ms);
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
        WM_TIMER => {
            KillTimer(hwnd, wparam);
            match wparam {
                SCAN_TIMER => {
                    if let Some(Some(retry)) = with_app(|app| app.scan()) {
                        schedule_scan(hwnd, retry);
                    }
                }
                ACTION_TIMER => {
                    if let Some(Some(delay)) = with_app(|app| app.run_job_step()) {
                        set_timer(hwnd, ACTION_TIMER, delay);
                    }
                }
                REFRESH_TIMER => {
                    with_app(|app| app.refresh_levels());
                }
                _ => {}
            }
            0
        }
        WM_TRAY => {
            match (lparam as u32) & 0xFFFF {
                WM_LBUTTONUP => match with_app(|app| app.toggle_flyout()) {
                    Some(Some(true)) => set_timer(hwnd, REFRESH_TIMER, OPEN_REFRESH_MS),
                    Some(Some(false)) => {}
                    _ => show_menu(hwnd),
                },
                WM_RBUTTONUP => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_SHOW_MENU => {
            show_menu(hwnd);
            0
        }
        WM_SETTINGCHANGE => {
            // The light/dark setting may have changed.
            refresh_menu_theme();
            DefWindowProcW(hwnd, msg, wparam, lparam)
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

unsafe extern "system" fn flyout_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match with_app(|app| app.flyout_message(msg, wparam, lparam)) {
        Some(Some(result)) => result,
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn build_menu(items: Vec<MenuItem>) -> HMENU {
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
    menu
}

/// Shows a menu at the cursor and returns the chosen command, if any.
unsafe fn track_menu(owner: HWND, items: Vec<MenuItem>) -> Option<usize> {
    let menu = build_menu(items);
    let mut cursor = POINT { x: 0, y: 0 };
    GetCursorPos(&mut cursor);
    // Required so the menu closes when clicking elsewhere.
    SetForegroundWindow(owner);
    let command = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
        cursor.x,
        cursor.y,
        0,
        owner,
        null(),
    );
    PostMessageW(owner, WM_NULL, 0, 0);
    DestroyMenu(menu);
    usize::try_from(command).ok().filter(|&id| id > 0)
}

unsafe fn show_menu(hwnd: HWND) {
    let Some(items) = with_app(|app| app.menu()) else {
        return;
    };
    if let Some(command) = track_menu(hwnd, items) {
        run_command(hwnd, command);
    }
}

fn run_command(hwnd: HWND, command: usize) {
    match command {
        CMD_LOCAL | CMD_REMOTE => {
            with_app(|app| {
                app.show(command == CMD_LOCAL);
                app.refresh();
            });
        }
        CMD_PBP => {
            with_app(|app| {
                app.toggle_pbp();
                app.refresh();
            });
        }
        CMD_SWAP => {
            with_app(|app| {
                app.swap_sides();
                app.refresh();
            });
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
            set_timer(hwnd, REFRESH_TIMER, OPEN_REFRESH_MS);
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
