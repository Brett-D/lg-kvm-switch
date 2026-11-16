//! Platform-independent access to the monitor.

use crate::config::Settings;
use crate::edid::EdidInfo;

pub trait Monitor {
    /// Switches to an LG input code (see [`crate::lg::INPUTS`]).
    ///
    /// The monitor's own input read-back is unreliable on LG models, so there
    /// is deliberately no way to ask which input is showing.
    fn set_input(&mut self, input: u16) -> Result<(), String>;
}

/// A display found on the system, for diagnostics.
#[derive(Clone, Debug)]
pub struct Display {
    pub location: String,
    pub edid: Option<EdidInfo>,
    /// Whether this display matches the configured monitor filter.
    pub selected: bool,
}

pub fn open(settings: &Settings) -> Box<dyn Monitor> {
    #[cfg(windows)]
    return Box::new(crate::windows::nvapi::NvMonitor::new(settings));
    #[cfg(target_os = "linux")]
    return Box::new(crate::linux::i2c::I2cMonitor::new(settings));
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = settings;
        Box::new(Unsupported)
    }
}

pub fn list_displays(settings: &Settings) -> Result<Vec<Display>, String> {
    #[cfg(windows)]
    return crate::windows::nvapi::list_displays(settings);
    #[cfg(target_os = "linux")]
    return crate::linux::i2c::list_displays(settings);
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = settings;
        Err(UNSUPPORTED.into())
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
const UNSUPPORTED: &str = "monitor control is only supported on Windows and Linux";

#[cfg(not(any(windows, target_os = "linux")))]
struct Unsupported;

#[cfg(not(any(windows, target_os = "linux")))]
impl Monitor for Unsupported {
    fn set_input(&mut self, _input: u16) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
}
