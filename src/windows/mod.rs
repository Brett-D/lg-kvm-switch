//! Windows backend: NVAPI I2C, Configuration Manager USB enumeration, and a
//! notification-area app.

pub mod app;
pub mod autostart;
pub mod flyout;
pub mod icon;
pub mod nvapi;
pub mod usb;

use crate::config::APP_NAME;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

/// NUL-terminated UTF-16 for Win32 calls.
pub(crate) fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

/// `%LOCALAPPDATA%\lg-kvm-switch`, for logs.
pub fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_NAME)
}

pub fn log_path() -> PathBuf {
    data_dir().join("lg-kvm-switch.log")
}
