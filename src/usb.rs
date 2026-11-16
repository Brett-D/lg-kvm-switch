//! USB device identities and enumeration.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

/// A USB vendor/product ID pair, written `VID:PID` in hex (e.g. `046D:C07D`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UsbId {
    pub vid: u16,
    pub pid: u16,
}

impl fmt::Display for UsbId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04X}:{:04X}", self.vid, self.pid)
    }
}

impl FromStr for UsbId {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let invalid = || format!("'{text}' is not a USB ID like 046D:C07D");
        let (vid, pid) = text.trim().split_once(':').ok_or_else(invalid)?;
        let hex = |part: &str| {
            let part = part.trim();
            let part = part.strip_prefix("0x").unwrap_or(part);
            u16::from_str_radix(part, 16).map_err(|_| invalid())
        };
        Ok(Self {
            vid: hex(vid)?,
            pid: hex(pid)?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct UsbDevice {
    pub id: UsbId,
    pub name: Option<String>,
    /// Platform-specific location (Windows instance ID, Linux sysfs name).
    pub location: String,
}

/// Lists connected USB devices. Looking up names is slower, so it is optional.
pub fn list_devices(with_names: bool) -> Result<Vec<UsbDevice>, String> {
    #[cfg(windows)]
    return crate::windows::usb::list_devices(with_names);
    #[cfg(target_os = "linux")]
    return crate::linux::usb::list_devices(with_names);
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = with_names;
        Err("USB device listing is not supported on this platform".into())
    }
}

/// IDs of all connected USB devices.
pub fn present_ids() -> Result<BTreeSet<UsbId>, String> {
    Ok(list_devices(false)?.into_iter().map(|d| d.id).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats_usb_ids() {
        let id: UsbId = "fc32:1287".parse().unwrap();
        assert_eq!(
            id,
            UsbId {
                vid: 0xFC32,
                pid: 0x1287
            }
        );
        assert_eq!(id.to_string(), "FC32:1287");
        assert_eq!("0x046D:0xC07D".parse::<UsbId>().unwrap().pid, 0xC07D);
        assert!("046DC07D".parse::<UsbId>().is_err());
        assert!("046D:XYZ".parse::<UsbId>().is_err());
    }
}
