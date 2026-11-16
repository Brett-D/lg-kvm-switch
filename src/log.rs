//! Timestamped logging to stdout and, optionally, a file.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static FILE: Mutex<Option<File>> = Mutex::new(None);
static STDOUT: AtomicBool = AtomicBool::new(true);

/// Also append log lines to `path`, rotating it once it passes 1 MB.
pub fn to_file(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > 1_000_000) {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    *FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(file);
    Ok(())
}

pub fn set_stdout(enabled: bool) {
    STDOUT.store(enabled, Ordering::Relaxed);
}

pub fn write(message: &str) {
    let line = format!("{} {message}", timestamp());
    if STDOUT.load(Ordering::Relaxed) {
        println!("{line}");
    }
    if let Some(file) = FILE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = writeln!(file, "{line}");
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::write(&format!($($arg)*)) };
}

#[cfg(windows)]
fn timestamp() -> String {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: GetLocalTime only writes the SYSTEMTIME it is given.
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

#[cfg(target_os = "linux")]
fn timestamp() -> String {
    // SAFETY: localtime_r writes only the tm it is given.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(not(any(windows, target_os = "linux")))]
fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format!("[{secs}]")
}
