//! Start-at-logon through the per-user `Run` registry key.

use super::wide;
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "lg-kvm-switch";

/// The command Windows runs at logon, if start-at-logon is on.
pub fn command() -> Option<String> {
    let (key, name) = (wide(RUN_KEY), wide(VALUE_NAME));
    let mut size = 0u32;
    // SAFETY: size query with valid NUL-terminated names.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let mut buffer = vec![0u16; size as usize / 2 + 1];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: `size` is the buffer's size in bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let text = String::from_utf16_lossy(&buffer[..size as usize / 2]);
    Some(text.trim_end_matches('\0').to_string())
}

pub fn is_enabled() -> bool {
    command().is_some()
}

pub fn enable(tray_exe: &Path, config: Option<&Path>) -> Result<(), String> {
    let mut command = format!("\"{}\"", tray_exe.display());
    if let Some(config) = config {
        command.push_str(&format!(" --config \"{}\"", config.display()));
    }
    let data = wide(&command);
    // SAFETY: `data` is NUL-terminated UTF-16 of the given byte length.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            wide(VALUE_NAME).as_ptr(),
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    match status {
        0 => Ok(()),
        _ => Err(format!(
            "could not write the Run registry key (error {status})"
        )),
    }
}

pub fn disable() -> Result<(), String> {
    // SAFETY: valid NUL-terminated names.
    let status = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            wide(VALUE_NAME).as_ptr(),
        )
    };
    match status {
        0 | ERROR_FILE_NOT_FOUND => Ok(()),
        _ => Err(format!(
            "could not remove the Run registry key (error {status})"
        )),
    }
}
