//! Platform-independent access to the monitor.

use crate::config::Settings;
use crate::edid::EdidInfo;

pub trait Monitor {
    /// Switches to an LG input code (see [`crate::lg::INPUTS`]).
    ///
    /// LG monitors report the wrong input when asked (VCP 0x60 stays on
    /// DisplayPort while USB-C is shown), so there is deliberately no way to
    /// read the input back.
    fn set_input(&mut self, input: u16) -> Result<(), String>;

    /// Writes a VCP feature from the standard host address (0x51).
    fn set_vcp(&mut self, vcp: u8, value: u16) -> Result<(), String>;

    /// Reads a VCP feature's current value.
    fn get_vcp(&mut self, vcp: u8) -> Result<u16, String>;
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

    fn set_vcp(&mut self, _vcp: u8, _value: u16) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }

    fn get_vcp(&mut self, _vcp: u8) -> Result<u16, String> {
        Err(UNSUPPORTED.into())
    }
}
