//! Tray icons drawn at runtime: a small monitor whose screen color shows the
//! state.

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIcon, GetSystemMetrics, HICON, SM_CXSMICON,
};

const OUTLINE: u32 = 0x20_20_20;

/// Creates a monitor icon whose screen is `rgb` (0xRRGGBB).
pub fn monitor_icon(rgb: u32) -> HICON {
    // SAFETY: plain metric query.
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as usize;
    let at = |fraction: f32| (fraction * size as f32).round() as usize;
    let border = (size / 16).max(1);
    let (left, right, top, bottom) = (at(0.04), at(0.96), at(0.10), at(0.70));

    // 32-bit BGRA color plane plus a 1-bit transparency mask whose rows are
    // padded to 16 bits. Mask bit 1 = transparent.
    let mut color = vec![0u8; size * size * 4];
    let mask_stride = size.div_ceil(16) * 2;
    let mut mask = vec![0xFFu8; mask_stride * size];

    for y in 0..size {
        for x in 0..size {
            let screen = (left..right).contains(&x) && (top..bottom).contains(&y);
            let edge = screen
                && (x < left + border
                    || x >= right - border
                    || y < top + border
                    || y >= bottom - border);
            let neck = (at(0.42)..at(0.58)).contains(&x) && (bottom..at(0.82)).contains(&y);
            let base = (at(0.24)..at(0.76)).contains(&x) && (at(0.82)..at(0.92)).contains(&y);
            let pixel = if edge || neck || base {
                OUTLINE
            } else if screen {
                rgb
            } else {
                continue;
            };
            let i = (y * size + x) * 4;
            color[i..i + 4].copy_from_slice(&[
                pixel as u8,
                (pixel >> 8) as u8,
                (pixel >> 16) as u8,
                0xFF,
            ]);
            mask[y * mask_stride + x / 8] &= !(0x80 >> (x % 8));
        }
    }
    // SAFETY: both planes are sized for a `size` x `size` 32-bit icon.
    unsafe {
        CreateIcon(
            GetModuleHandleW(std::ptr::null()),
            size as i32,
            size as i32,
            1,
            32,
            mask.as_ptr(),
            color.as_ptr(),
        )
    }
}
