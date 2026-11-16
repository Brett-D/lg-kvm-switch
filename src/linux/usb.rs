//! USB enumeration from `/sys/bus/usb/devices`.

use crate::usb::{UsbDevice, UsbId};
use std::fs;
use std::path::Path;

pub fn list_devices(with_names: bool) -> Result<Vec<UsbDevice>, String> {
    let root = Path::new("/sys/bus/usb/devices");
    let entries = fs::read_dir(root).map_err(|e| format!("cannot read {}: {e}", root.display()))?;
    let mut devices = Vec::new();
    for entry in entries.flatten() {
        let location = entry.file_name().to_string_lossy().into_owned();
        // Entries such as "1-2:1.0" are interfaces, not devices.
        if location.contains(':') {
            continue;
        }
        let dir = entry.path();
        let read = |file: &str| {
            fs::read_to_string(dir.join(file))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        let hex = |file: &str| read(file).and_then(|v| u16::from_str_radix(&v, 16).ok());
        let (Some(vid), Some(pid)) = (hex("idVendor"), hex("idProduct")) else {
            continue;
        };
        let name = if with_names {
            match (read("manufacturer"), read("product")) {
                (Some(m), Some(p)) => Some(format!("{m} {p}")),
                (m, p) => p.or(m),
            }
        } else {
            None
        };
        devices.push(UsbDevice {
            id: UsbId { vid, pid },
            name,
            location,
        });
    }
    Ok(devices)
}
