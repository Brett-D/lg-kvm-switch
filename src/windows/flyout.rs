//! A Windows 11-style flyout opened from the tray icon: input tiles,
//! Picture-by-Picture, automatic switching, and picture and audio sliders.
//!
//! Drawn with Direct2D and DirectWrite. DWM supplies the rounded corners,
//! border, and shadow. All layout is in DIPs and scaled by the monitor's DPI.

use super::wide;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND as D2dHwnd;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE,
};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromPoint, MonitorFromWindow, ScreenToClient,
    ValidateRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{
    RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE,
    TRACKMOUSEEVENT, VK_ESCAPE, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetClientRect, GetCursorPos, GetForegroundWindow, GetWindowRect, KillTimer,
    LoadCursorW, RegisterClassW, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow,
    CS_DROPSHADOW, HWND_TOPMOST, IDC_ARROW, SPI_SETWORKAREA, SWP_NOACTIVATE, SWP_NOSIZE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, WA_INACTIVE, WM_ACTIVATE, WM_CAPTURECHANGED,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW,
    WNDPROC, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const WM_MOUSELEAVE: u32 = 0x02A3;

const WIDTH: f32 = 380.0;
const HEIGHT: f32 = 312.0;
const PAD: f32 = 20.0;
const TILE_TOP: f32 = 20.0;
const TILE_COUNT: usize = 5;
const TILE_GAP: f32 = 8.0;
const TILE_W: f32 = (WIDTH - 2.0 * PAD - (TILE_COUNT as f32 - 1.0) * TILE_GAP) / TILE_COUNT as f32;
const TILE_H: f32 = 48.0;
const SLIDER_TOP: f32 = 98.0;
const ROW_H: f32 = 52.0;
/// Track position within a slider row; the caption sits above it.
const TRACK_OFFSET: f32 = 34.0;
const ICON_LEFT: f32 = 12.0;
const ICON_SIZE: f32 = 40.0;
const TRACK_LEFT: f32 = 64.0;
const TRACK_RIGHT: f32 = WIDTH - PAD - 40.0;
const FOOTER_H: f32 = 48.0;
/// Distance the flyout slides in from when it opens.
const SLIDE: f32 = 16.0;
const SLIDE_TIME: Duration = Duration::from_millis(160);
/// Minimum time between DDC writes while dragging a slider.
const SEND_INTERVAL: Duration = Duration::from_millis(120);
/// A tray click that arrives this soon after the flyout closed is the click
/// that closed it, so it shouldn't reopen it.
const REOPEN_GUARD: Duration = Duration::from_millis(400);
/// Width of the dropdown part of a tile that has options.
const MENU_ZONE: f32 = 20.0;

const FLUSH_TIMER: usize = 1;
const SLIDE_TIMER: usize = 2;
/// Closes the flyout on clicks elsewhere when it couldn't take focus.
const OUTSIDE_TIMER: usize = 3;

pub const GLYPH_THIS_PC: char = '\u{E7F4}';
pub const GLYPH_OTHER_PC: char = '\u{E977}';
pub const GLYPH_SIDE_BY_SIDE: char = '\u{E89A}';
pub const GLYPH_SWAP: char = '\u{E8AB}';
pub const GLYPH_AUTO: char = '\u{E895}';
pub const GLYPH_BRIGHTNESS: char = '\u{E706}';
pub const GLYPH_CONTRAST: char = '\u{E793}';
pub const GLYPH_VOLUME: char = '\u{E767}';
pub const GLYPH_MUTED: char = '\u{E74F}';
const GLYPH_SETTINGS: char = '\u{E713}';
const GLYPH_CHEVRON: char = '\u{E70D}';

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tile {
    pub glyph: char,
    pub label: String,
    pub on: bool,
    pub enabled: bool,
    /// Right-clicking opens a menu of options; shown with a small chevron.
    pub has_menu: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Slider {
    pub glyph: char,
    pub label: String,
    /// 0–100, or `None` when the monitor didn't report it.
    pub value: Option<u16>,
    /// Whether clicking the icon does something (mute).
    pub icon_button: bool,
}

/// Everything the flyout shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Model {
    pub tiles: Vec<Tile>,
    pub sliders: Vec<Slider>,
    pub status: String,
}

/// What the user asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Tile(usize),
    /// Right-click on a tile that has options.
    TileMenu(usize),
    SetLevel(usize, u16),
    Icon(usize),
    /// The settings button.
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    None,
    Tile(usize),
    /// The dropdown part on the right of a tile that has options.
    TileMenu(usize),
    Track(usize),
    Icon(usize),
    Settings,
}

/// Colors for the current Windows theme.
#[derive(Clone, Copy, Debug)]
struct Theme {
    dark: bool,
    accent: u32,
    background: u32,
    footer: u32,
    divider: u32,
    border: u32,
    tile: u32,
    tile_hover: u32,
    tile_border: u32,
    text: u32,
    secondary: u32,
    on_accent: u32,
    track: u32,
    thumb: u32,
    thumb_border: u32,
    hover: u32,
}

impl Theme {
    fn current() -> Self {
        let dark = read_dword(
            r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
            "SystemUsesLightTheme",
        ) != Some(1);
        // AccentPalette holds 8 RGBA colors from lightest to darkest. Windows 11
        // uses Light2 on dark backgrounds and Dark1 on light ones.
        let palette = read_binary(
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent",
            "AccentPalette",
        );
        let index = if dark { 1 } else { 4 };
        let accent = palette
            .filter(|p| p.len() >= 32)
            .map(|p| {
                let c = &p[index * 4..index * 4 + 3];
                u32::from(c[0]) << 16 | u32::from(c[1]) << 8 | u32::from(c[2])
            })
            .unwrap_or(if dark { 0x60CDFF } else { 0x005FB8 });
        if dark {
            Self {
                dark,
                accent,
                background: 0x2C2C2C,
                footer: 0x202020,
                divider: 0x1C1C1C,
                border: 0x3D3D3D,
                tile: 0x373737,
                tile_hover: 0x3E3E3E,
                tile_border: 0x434343,
                text: 0xFFFFFF,
                secondary: 0xCFCFCF,
                on_accent: 0x000000,
                track: 0x9E9E9E,
                thumb: 0x454545,
                thumb_border: 0x525252,
                hover: 0x3A3A3A,
            }
        } else {
            Self {
                dark,
                accent,
                background: 0xF3F3F3,
                footer: 0xEBEBEB,
                divider: 0xE0E0E0,
                border: 0xD9D9D9,
                tile: 0xFBFBFB,
                tile_hover: 0xF6F6F6,
                tile_border: 0xE3E3E3,
                text: 0x1B1B1B,
                secondary: 0x5F5F5F,
                on_accent: 0xFFFFFF,
                track: 0x8A8A8A,
                thumb: 0xFFFFFF,
                thumb_border: 0xDDDDDD,
                hover: 0xE6E6E6,
            }
        }
    }
}

fn read_dword(key: &str, value: &str) -> Option<u32> {
    let mut data = 0u32;
    let mut size = 4u32;
    // SAFETY: `data` is a 4-byte buffer and `size` says so.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(key).as_ptr(),
            wide(value).as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    (status == 0).then_some(data)
}

fn read_binary(key: &str, value: &str) -> Option<Vec<u8>> {
    let mut data = vec![0u8; 64];
    let mut size = data.len() as u32;
    // SAFETY: `size` is the buffer's size in bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(key).as_ptr(),
            wide(value).as_ptr(),
            RRF_RT_REG_BINARY,
            null_mut(),
            data.as_mut_ptr().cast(),
            &mut size,
        )
    };
    (status == 0).then(|| {
        data.truncate(size as usize);
        data
    })
}

fn color(rgb: u32, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 0xFF) as f32 / 255.0,
        g: ((rgb >> 8) & 0xFF) as f32 / 255.0,
        b: (rgb & 0xFF) as f32 / 255.0,
        a: alpha,
    }
}

/// COLORREF (0x00BBGGRR) for DWM.
fn colorref(rgb: u32) -> u32 {
    (rgb & 0xFF) << 16 | (rgb & 0xFF00) | (rgb >> 16) & 0xFF
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

fn contains(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn tile_rect(i: usize) -> D2D_RECT_F {
    rect(
        PAD + i as f32 * (TILE_W + TILE_GAP),
        TILE_TOP,
        TILE_W,
        TILE_H,
    )
}

fn row_top(i: usize) -> f32 {
    SLIDER_TOP + i as f32 * ROW_H
}

/// Vertical center of a slider's track, icon, and value.
fn track_y(i: usize) -> f32 {
    row_top(i) + TRACK_OFFSET
}

fn icon_rect(i: usize) -> D2D_RECT_F {
    rect(
        ICON_LEFT,
        track_y(i) - ICON_SIZE / 2.0,
        ICON_SIZE,
        ICON_SIZE,
    )
}

fn settings_rect() -> D2D_RECT_F {
    rect(
        WIDTH - 12.0 - ICON_SIZE,
        HEIGHT - FOOTER_H / 2.0 - ICON_SIZE / 2.0,
        ICON_SIZE,
        ICON_SIZE,
    )
}

fn slider_value(x: f32) -> u16 {
    let fraction = ((x - TRACK_LEFT) / (TRACK_RIGHT - TRACK_LEFT)).clamp(0.0, 1.0);
    (fraction * 100.0).round() as u16
}

/// Signed client coordinates from a mouse message's LPARAM.
fn point_from(lparam: LPARAM) -> (i32, i32) {
    (
        i32::from((lparam & 0xFFFF) as u16 as i16),
        i32::from(((lparam >> 16) & 0xFFFF) as u16 as i16),
    )
}

struct Fonts {
    label: IDWriteTextFormat,
    value: IDWriteTextFormat,
    status: IDWriteTextFormat,
    caption: IDWriteTextFormat,
    icon: IDWriteTextFormat,
    small_icon: IDWriteTextFormat,
}

impl Fonts {
    fn new() -> windows::core::Result<Self> {
        // SAFETY: plain COM factory creation.
        let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let icons = if Path::new(r"C:\Windows\Fonts\SegoeIcons.ttf").exists() {
            w!("Segoe Fluent Icons")
        } else {
            w!("Segoe MDL2 Assets")
        };
        let text = w!("Segoe UI Variable Text");
        let make = |family: PCWSTR,
                    size: f32,
                    weight: DWRITE_FONT_WEIGHT,
                    align: DWRITE_TEXT_ALIGNMENT|
         -> windows::core::Result<IDWriteTextFormat> {
            // SAFETY: valid factory and NUL-terminated names.
            unsafe {
                let format = factory.CreateTextFormat(
                    family,
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("en-us"),
                )?;
                format.SetTextAlignment(align)?;
                format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                Ok(format)
            }
        };
        Ok(Self {
            label: make(
                text,
                12.0,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_TEXT_ALIGNMENT_CENTER,
            )?,
            value: make(
                text,
                12.0,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_TEXT_ALIGNMENT_TRAILING,
            )?,
            status: make(
                text,
                12.0,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            caption: make(
                text,
                11.0,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            icon: make(
                icons,
                16.0,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_TEXT_ALIGNMENT_CENTER,
            )?,
            small_icon: make(
                icons,
                8.0,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_TEXT_ALIGNMENT_CENTER,
            )?,
        })
    }
}

/// Drawing helpers bound to one frame.
struct Painter<'a> {
    target: &'a ID2D1HwndRenderTarget,
    brush: &'a ID2D1SolidColorBrush,
}

impl Painter<'_> {
    fn fill(&self, r: D2D_RECT_F, radius: f32, fill: D2D1_COLOR_F) {
        let rounded = D2D1_ROUNDED_RECT {
            rect: r,
            radiusX: radius,
            radiusY: radius,
        };
        // SAFETY: drawing between BeginDraw and EndDraw.
        unsafe {
            self.brush.SetColor(&fill);
            self.target.FillRoundedRectangle(&rounded, self.brush);
        }
    }

    fn outline(&self, r: D2D_RECT_F, radius: f32, stroke: D2D1_COLOR_F) {
        let inset = D2D1_ROUNDED_RECT {
            rect: rect(
                r.left + 0.5,
                r.top + 0.5,
                r.right - r.left - 1.0,
                r.bottom - r.top - 1.0,
            ),
            radiusX: radius,
            radiusY: radius,
        };
        // SAFETY: as above.
        unsafe {
            self.brush.SetColor(&stroke);
            self.target
                .DrawRoundedRectangle(&inset, self.brush, 1.0, None);
        }
    }

    fn circle(&self, cx: f32, cy: f32, radius: f32, fill: D2D1_COLOR_F) {
        self.fill(
            rect(cx - radius, cy - radius, radius * 2.0, radius * 2.0),
            radius,
            fill,
        );
    }

    fn text(&self, text: &str, format: &IDWriteTextFormat, r: D2D_RECT_F, fill: D2D1_COLOR_F) {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: as above.
        unsafe {
            self.brush.SetColor(&fill);
            self.target.DrawText(
                &utf16,
                format,
                &r,
                self.brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
}

struct Slide {
    start: Instant,
    x: i32,
    from: i32,
    to: i32,
}

pub struct Flyout {
    hwnd: HWND,
    factory: ID2D1Factory,
    fonts: Fonts,
    target: Option<ID2D1HwndRenderTarget>,
    model: Model,
    theme: Theme,
    dpi: f32,
    visible: bool,
    hidden_at: Option<Instant>,
    hover: Hit,
    pressed: Hit,
    tracking_leave: bool,
    dragging: Option<usize>,
    pending: Option<(usize, u16)>,
    last_sent: Option<Instant>,
    slide: Option<Slide>,
}

impl Flyout {
    /// Creates the (hidden) flyout window. Its messages must be passed to
    /// [`Flyout::handle`] by `window_proc`.
    pub fn create(window_proc: WNDPROC) -> Result<Self, String> {
        // SAFETY: COM factories and a standard popup window; strings outlive
        // the calls.
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                .map_err(|e| format!("Direct2D unavailable: {e}"))?;
            let fonts = Fonts::new().map_err(|e| format!("DirectWrite unavailable: {e}"))?;
            let instance = GetModuleHandleW(null());
            let class_name = wide("LgKvmSwitchFlyout");
            let class = WNDCLASSW {
                style: CS_DROPSHADOW,
                lpfnWndProc: window_proc,
                hInstance: instance,
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                lpszClassName: class_name.as_ptr(),
                ..zeroed()
            };
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                class_name.as_ptr(),
                wide("LG KVM Switch").as_ptr(),
                WS_POPUP,
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
                return Err("could not create the flyout window".into());
            }
            let round: u32 = 2; // DWMWCP_ROUND
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE as _,
                (&round as *const u32).cast::<c_void>(),
                4,
            );
            Ok(Self {
                hwnd,
                factory,
                fonts,
                target: None,
                model: Model::default(),
                theme: Theme::current(),
                dpi: 96.0,
                visible: false,
                hidden_at: None,
                hover: Hit::None,
                pressed: Hit::None,
                tracking_leave: false,
                dragging: None,
                pending: None,
                last_sent: None,
                slide: None,
            })
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Whether the flyout just closed, e.g. because the tray icon was clicked.
    pub fn just_closed(&self) -> bool {
        self.hidden_at.is_some_and(|at| at.elapsed() < REOPEN_GUARD)
    }

    /// Opens the flyout above the taskbar on the monitor under the cursor.
    pub fn show(&mut self, model: Model) {
        self.model = model;
        self.theme = Theme::current();
        self.hover = Hit::None;
        self.pressed = Hit::None;
        // SAFETY: plain Win32 calls on our own window.
        unsafe {
            let dark = u32::from(self.theme.dark);
            DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE as _,
                (&dark as *const u32).cast::<c_void>(),
                4,
            );
            let border = colorref(self.theme.border);
            DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_BORDER_COLOR as _,
                (&border as *const u32).cast::<c_void>(),
                4,
            );
        }
        self.place(true);
        // SAFETY: plain Win32 calls on our own window.
        unsafe {
            SetForegroundWindow(self.hwnd);
            SetFocus(self.hwnd);
            SetTimer(self.hwnd, OUTSIDE_TIMER, 100, None);
        }
        self.visible = true;
        self.invalidate();
    }

    /// Sizes and positions the flyout next to the taskbar, like Windows' own
    /// flyouts: on the monitor under the cursor when opening, and on its
    /// current monitor when the display changes while it's open.
    fn place(&mut self, opening: bool) {
        // SAFETY: plain Win32 calls on our own window.
        unsafe {
            let monitor = if opening {
                let mut cursor = POINT { x: 0, y: 0 };
                GetCursorPos(&mut cursor);
                MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST)
            } else {
                MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST)
            };
            let mut info: MONITORINFO = zeroed();
            info.cbSize = size_of::<MONITORINFO>() as u32;
            GetMonitorInfoW(monitor, &mut info);
            let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
            GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
            self.dpi = dpi_x as f32;
            let scale = self.dpi / 96.0;
            let px = |dips: f32| (dips * scale).round() as i32;
            let (work, screen) = (info.rcWork, info.rcMonitor);
            let (w, h, margin) = (px(WIDTH), px(HEIGHT), px(12.0));

            let taskbar_top = work.top > screen.top;
            let taskbar_left = work.left > screen.left;
            let x = if taskbar_left {
                work.left + margin
            } else {
                work.right - w - margin
            };
            let y = if taskbar_top {
                work.top + margin
            } else {
                work.bottom - h - margin
            };
            if opening {
                // Slide in from the taskbar side.
                let from = if taskbar_top {
                    y - px(SLIDE)
                } else {
                    y + px(SLIDE)
                };
                SetWindowPos(self.hwnd, HWND_TOPMOST, x, from, w, h, SWP_SHOWWINDOW);
                self.slide = Some(Slide {
                    start: Instant::now(),
                    x,
                    from,
                    to: y,
                });
                SetTimer(self.hwnd, SLIDE_TIMER, 10, None);
            } else {
                self.slide = None;
                KillTimer(self.hwnd, SLIDE_TIMER);
                SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
                self.invalidate();
            }
        }
    }

    /// Closes the flyout, returning any slider change not yet sent.
    pub fn hide(&mut self) -> Option<Action> {
        if !self.visible {
            return None;
        }
        let action = self.flush();
        self.visible = false;
        self.dragging = None;
        self.hidden_at = Some(Instant::now());
        // SAFETY: our own window and timer.
        unsafe {
            KillTimer(self.hwnd, OUTSIDE_TIMER);
            ShowWindow(self.hwnd, SW_HIDE);
        }
        action
    }

    /// Closes the flyout when a mouse button goes down outside it. Only
    /// needed when it couldn't take focus; otherwise deactivation closes it.
    fn close_on_outside_click(&mut self) -> Option<Action> {
        // SAFETY: plain queries about the cursor and our own window.
        unsafe {
            if GetForegroundWindow() == self.hwnd {
                return None;
            }
            let down = [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON]
                .iter()
                .any(|&key| GetAsyncKeyState(i32::from(key)) as u16 & 0x8000 != 0);
            if !down {
                return None;
            }
            let mut cursor = POINT { x: 0, y: 0 };
            GetCursorPos(&mut cursor);
            let mut window: RECT = zeroed();
            GetWindowRect(self.hwnd, &mut window);
            let inside = cursor.x >= window.left
                && cursor.x < window.right
                && cursor.y >= window.top
                && cursor.y < window.bottom;
            if inside {
                None
            } else {
                self.hide()
            }
        }
    }

    /// Replaces what the flyout shows, keeping a slider that is being dragged.
    pub fn update(&mut self, mut model: Model) {
        if let Some(i) = self.dragging {
            if let (Some(new), Some(current)) =
                (model.sliders.get_mut(i), self.model.sliders.get(i))
            {
                new.value = current.value;
            }
        }
        if model != self.model {
            self.model = model;
            self.invalidate();
        }
    }

    fn invalidate(&self) {
        // SAFETY: our own window.
        unsafe { InvalidateRect(self.hwnd, null(), 0) };
    }

    fn to_dips(&self, x: i32, y: i32) -> (f32, f32) {
        let scale = self.dpi / 96.0;
        (x as f32 / scale, y as f32 / scale)
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        if let Some(i) = (0..self.model.tiles.len()).find(|&i| contains(&tile_rect(i), x, y)) {
            if self.model.tiles[i].has_menu && x >= tile_rect(i).right - MENU_ZONE {
                return Hit::TileMenu(i);
            }
            return Hit::Tile(i);
        }
        for i in 0..self.model.sliders.len() {
            let top = row_top(i);
            if (top..top + ROW_H).contains(&y) {
                if contains(&icon_rect(i), x, y) {
                    return Hit::Icon(i);
                }
                if (TRACK_LEFT - 12.0..=TRACK_RIGHT + 12.0).contains(&x) {
                    return Hit::Track(i);
                }
            }
        }
        if contains(&settings_rect(), x, y) {
            return Hit::Settings;
        }
        Hit::None
    }

    /// Records a slider value and returns an action if it's time to send it.
    fn set_slider(&mut self, i: usize, value: u16) -> Option<Action> {
        let slider = self.model.sliders.get_mut(i)?;
        if slider.value == Some(value) {
            return None;
        }
        slider.value = Some(value);
        self.pending = Some((i, value));
        self.invalidate();
        if self
            .last_sent
            .is_none_or(|at| at.elapsed() >= SEND_INTERVAL)
        {
            self.flush()
        } else {
            // SAFETY: timer on our own window.
            unsafe {
                SetTimer(
                    self.hwnd,
                    FLUSH_TIMER,
                    SEND_INTERVAL.as_millis() as u32,
                    None,
                )
            };
            None
        }
    }

    fn flush(&mut self) -> Option<Action> {
        let (i, value) = self.pending.take()?;
        self.last_sent = Some(Instant::now());
        Some(Action::SetLevel(i, value))
    }

    fn step_slide(&mut self) {
        let Some(slide) = &self.slide else {
            // SAFETY: our own timer.
            unsafe { KillTimer(self.hwnd, SLIDE_TIMER) };
            return;
        };
        let t = (slide.start.elapsed().as_secs_f32() / SLIDE_TIME.as_secs_f32()).min(1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        let y = slide.from + ((slide.to - slide.from) as f32 * eased).round() as i32;
        // SAFETY: our own window.
        unsafe {
            SetWindowPos(
                self.hwnd,
                null_mut(),
                slide.x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        if t >= 1.0 {
            self.slide = None;
            // SAFETY: our own timer.
            unsafe { KillTimer(self.hwnd, SLIDE_TIMER) };
        }
    }

    /// Handles a flyout window message. Returns the result if handled, and
    /// any action the user took.
    pub fn handle(
        &mut self,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> (Option<LRESULT>, Option<Action>) {
        match msg {
            WM_PAINT => {
                self.paint();
                // SAFETY: our own window.
                unsafe { ValidateRect(self.hwnd, null()) };
                (Some(0), None)
            }
            WM_ERASEBKGND => (Some(1), None),
            WM_DPICHANGED => (Some(0), None),
            WM_ACTIVATE if (wparam & 0xFFFF) as u32 == WA_INACTIVE => (Some(0), self.hide()),
            WM_KEYDOWN if wparam == VK_ESCAPE as usize => (Some(0), self.hide()),
            WM_TIMER if wparam == SLIDE_TIMER => {
                self.step_slide();
                (Some(0), None)
            }
            WM_TIMER if wparam == OUTSIDE_TIMER => (Some(0), self.close_on_outside_click()),
            WM_TIMER if wparam == FLUSH_TIMER => {
                // SAFETY: our own timer.
                unsafe { KillTimer(self.hwnd, FLUSH_TIMER) };
                (Some(0), self.flush())
            }
            WM_MOUSEMOVE => {
                if !self.tracking_leave {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.hwnd,
                        dwHoverTime: 0,
                    };
                    // SAFETY: valid struct for our own window.
                    self.tracking_leave = unsafe { TrackMouseEvent(&mut track) } != 0;
                }
                let (x, y) = point_from(lparam);
                let (x, y) = self.to_dips(x, y);
                if let Some(i) = self.dragging {
                    return (Some(0), self.set_slider(i, slider_value(x)));
                }
                let hover = self.hit(x, y);
                if hover != self.hover {
                    self.hover = hover;
                    self.invalidate();
                }
                (Some(0), None)
            }
            WM_MOUSELEAVE => {
                self.tracking_leave = false;
                if self.hover != Hit::None {
                    self.hover = Hit::None;
                    self.invalidate();
                }
                (Some(0), None)
            }
            WM_LBUTTONDOWN => {
                let (x, y) = point_from(lparam);
                let (x, y) = self.to_dips(x, y);
                self.pressed = self.hit(x, y);
                self.invalidate();
                if let Hit::Track(i) = self.pressed {
                    if self.model.sliders.get(i).is_some_and(|s| s.value.is_some()) {
                        self.dragging = Some(i);
                        // SAFETY: our own window.
                        unsafe { SetCapture(self.hwnd) };
                        return (Some(0), self.set_slider(i, slider_value(x)));
                    }
                }
                (Some(0), None)
            }
            WM_LBUTTONUP => {
                let pressed = std::mem::replace(&mut self.pressed, Hit::None);
                self.invalidate();
                if self.dragging.take().is_some() {
                    // SAFETY: we hold the capture.
                    unsafe { ReleaseCapture() };
                    return (Some(0), self.flush());
                }
                let (x, y) = point_from(lparam);
                let (x, y) = self.to_dips(x, y);
                if self.hit(x, y) != pressed {
                    return (Some(0), None);
                }
                let action = match pressed {
                    Hit::Tile(i) if self.model.tiles[i].enabled => Some(Action::Tile(i)),
                    Hit::TileMenu(i) => Some(Action::TileMenu(i)),
                    Hit::Icon(i) if self.model.sliders[i].icon_button => Some(Action::Icon(i)),
                    Hit::Settings => Some(Action::Settings),
                    _ => None,
                };
                (Some(0), action)
            }
            WM_RBUTTONUP => {
                let (x, y) = point_from(lparam);
                let (x, y) = self.to_dips(x, y);
                let action = match self.hit(x, y) {
                    Hit::Tile(i) | Hit::TileMenu(i) if self.model.tiles[i].has_menu => {
                        Some(Action::TileMenu(i))
                    }
                    _ => None,
                };
                (Some(0), action)
            }
            WM_DISPLAYCHANGE => {
                // Split screen changes this computer's resolution and scaling.
                if self.visible {
                    self.place(false);
                }
                (Some(0), None)
            }
            WM_SETTINGCHANGE if wparam == SPI_SETWORKAREA as usize => {
                if self.visible {
                    self.place(false);
                }
                (Some(0), None)
            }
            WM_CAPTURECHANGED => {
                self.dragging = None;
                (Some(0), self.flush())
            }
            WM_MOUSEWHEEL => {
                let delta = i32::from((wparam >> 16) as u16 as i16) / 120;
                let (x, y) = point_from(lparam);
                let mut point = POINT { x, y };
                // SAFETY: our own window.
                unsafe { ScreenToClient(self.hwnd, &mut point) };
                let (x, y) = self.to_dips(point.x, point.y);
                let row = match self.hit(x, y) {
                    Hit::Track(i) | Hit::Icon(i) => Some(i),
                    _ => None,
                };
                let action = row.and_then(|i| {
                    let value = self.model.sliders[i].value?;
                    let next = (i32::from(value) + delta * 2).clamp(0, 100) as u16;
                    self.set_slider(i, next)
                });
                (Some(0), action)
            }
            _ => (None, None),
        }
    }

    fn paint(&mut self) {
        // SAFETY: Direct2D drawing on our own window, between BeginDraw and
        // EndDraw.
        unsafe {
            let mut client: RECT = zeroed();
            GetClientRect(self.hwnd, &mut client);
            let size = D2D_SIZE_U {
                width: (client.right - client.left).max(1) as u32,
                height: (client.bottom - client.top).max(1) as u32,
            };
            if self.target.is_none() {
                let properties = D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_IGNORE,
                    },
                    dpiX: 0.0,
                    dpiY: 0.0,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                };
                let window = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                    hwnd: D2dHwnd(self.hwnd),
                    pixelSize: size,
                    presentOptions: D2D1_PRESENT_OPTIONS_NONE,
                };
                self.target = self
                    .factory
                    .CreateHwndRenderTarget(&properties, &window)
                    .ok();
            }
            let Some(target) = self.target.clone() else {
                return;
            };
            let _ = target.Resize(&size);
            target.SetDpi(self.dpi, self.dpi);
            let Ok(brush) = target.CreateSolidColorBrush(&color(0, 1.0), None) else {
                return;
            };
            target.BeginDraw();
            target.Clear(Some(&color(self.theme.background, 1.0)));
            self.draw(&Painter {
                target: &target,
                brush: &brush,
            });
            if target.EndDraw(None, None).is_err() {
                // The device was lost; recreate the target next time.
                self.target = None;
            }
        }
    }

    fn draw(&self, p: &Painter) {
        let t = &self.theme;
        let fonts = &self.fonts;

        for (i, tile) in self.model.tiles.iter().enumerate() {
            let r = tile_rect(i);
            let hovered = self.hover == Hit::Tile(i) && tile.enabled;
            let pressed = self.pressed == Hit::Tile(i);
            let alpha = if tile.enabled { 1.0 } else { 0.4 };
            if tile.on {
                let shade = if pressed {
                    0.8
                } else if hovered {
                    0.9
                } else {
                    1.0
                };
                p.fill(r, 4.0, color(t.accent, alpha * shade));
            } else {
                let fill = if hovered { t.tile_hover } else { t.tile };
                p.fill(r, 4.0, color(fill, alpha));
                p.outline(r, 4.0, color(t.tile_border, alpha));
            }
            let glyph_color = if tile.on { t.on_accent } else { t.text };
            let main = if tile.has_menu {
                rect(r.left, r.top, TILE_W - MENU_ZONE, TILE_H)
            } else {
                r
            };
            p.text(
                &tile.glyph.to_string(),
                &fonts.icon,
                main,
                color(glyph_color, alpha),
            );
            p.text(
                &tile.label,
                &fonts.label,
                rect(r.left - 6.0, r.bottom + 4.0, TILE_W + 12.0, 18.0),
                color(t.text, alpha),
            );
            if tile.has_menu {
                // A split button: the right part opens the options menu.
                let zone = rect(r.right - MENU_ZONE, r.top, MENU_ZONE, TILE_H);
                if self.hover == Hit::TileMenu(i) {
                    let inset = rect(zone.left, zone.top + 1.0, MENU_ZONE - 1.0, TILE_H - 2.0);
                    let fill = if tile.on {
                        color(t.on_accent, 0.12)
                    } else {
                        color(t.tile_hover, 1.0)
                    };
                    p.fill(inset, 3.0, fill);
                }
                p.fill(
                    rect(zone.left, r.top + 12.0, 1.0, TILE_H - 24.0),
                    0.0,
                    color(glyph_color, 0.25),
                );
                p.text(
                    &GLYPH_CHEVRON.to_string(),
                    &fonts.small_icon,
                    zone,
                    color(glyph_color, 0.8),
                );
            }
        }

        for (i, slider) in self.model.sliders.iter().enumerate() {
            let cy = track_y(i);
            let icon = icon_rect(i);
            p.text(
                &slider.label,
                &fonts.caption,
                rect(
                    TRACK_LEFT - 2.0,
                    row_top(i) + 4.0,
                    TRACK_RIGHT - TRACK_LEFT,
                    16.0,
                ),
                color(t.secondary, 1.0),
            );
            if slider.icon_button && self.hover == Hit::Icon(i) {
                p.fill(icon, 4.0, color(t.hover, 1.0));
            }
            p.text(
                &slider.glyph.to_string(),
                &fonts.icon,
                icon,
                color(t.text, 1.0),
            );
            let Some(value) = slider.value else {
                p.fill(
                    rect(TRACK_LEFT, cy - 2.0, TRACK_RIGHT - TRACK_LEFT, 4.0),
                    2.0,
                    color(t.track, 0.4),
                );
                p.text(
                    "–",
                    &fonts.value,
                    rect(
                        TRACK_RIGHT + 8.0,
                        cy - 10.0,
                        WIDTH - PAD - TRACK_RIGHT - 8.0,
                        20.0,
                    ),
                    color(t.secondary, 1.0),
                );
                continue;
            };
            let thumb_x = TRACK_LEFT + (TRACK_RIGHT - TRACK_LEFT) * f32::from(value) / 100.0;
            p.fill(
                rect(TRACK_LEFT, cy - 2.0, TRACK_RIGHT - TRACK_LEFT, 4.0),
                2.0,
                color(t.track, 1.0),
            );
            p.fill(
                rect(TRACK_LEFT, cy - 2.0, thumb_x - TRACK_LEFT, 4.0),
                2.0,
                color(t.accent, 1.0),
            );
            p.circle(thumb_x, cy, 10.0, color(t.thumb_border, 1.0));
            p.circle(thumb_x, cy, 9.0, color(t.thumb, 1.0));
            let inner = if self.dragging == Some(i) {
                5.0
            } else if self.hover == Hit::Track(i) {
                7.0
            } else {
                6.0
            };
            p.circle(thumb_x, cy, inner, color(t.accent, 1.0));
            p.text(
                &value.to_string(),
                &fonts.value,
                rect(
                    TRACK_RIGHT + 8.0,
                    cy - 10.0,
                    WIDTH - PAD - TRACK_RIGHT - 8.0,
                    20.0,
                ),
                color(t.text, 1.0),
            );
        }

        let footer_top = HEIGHT - FOOTER_H;
        p.fill(
            rect(0.0, footer_top, WIDTH, FOOTER_H),
            0.0,
            color(t.footer, 1.0),
        );
        p.fill(
            rect(0.0, footer_top, WIDTH, 1.0),
            0.0,
            color(t.divider, 1.0),
        );
        p.text(
            &self.model.status,
            &fonts.status,
            rect(PAD, footer_top, WIDTH - PAD - ICON_SIZE - 16.0, FOOTER_H),
            color(t.secondary, 1.0),
        );
        let settings = settings_rect();
        if self.hover == Hit::Settings {
            p.fill(settings, 4.0, color(t.hover, 1.0));
        }
        p.text(
            &GLYPH_SETTINGS.to_string(),
            &fonts.icon,
            settings,
            color(t.text, 1.0),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_track_positions_to_values() {
        assert_eq!(slider_value(TRACK_LEFT - 50.0), 0);
        assert_eq!(slider_value((TRACK_LEFT + TRACK_RIGHT) / 2.0), 50);
        assert_eq!(slider_value(TRACK_RIGHT + 50.0), 100);
    }

    #[test]
    fn converts_colors_for_dwm() {
        assert_eq!(colorref(0x123456), 0x563412);
    }
}
