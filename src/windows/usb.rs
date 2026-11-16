//! USB enumeration through the Configuration Manager API.

use super::wide;
use crate::usb::{UsbDevice, UsbId};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_Device_ID_ListW, CM_Get_Device_ID_List_SizeW,
    CM_Locate_DevNodeW, CM_GETIDLIST_FILTER_ENUMERATOR, CM_GETIDLIST_FILTER_PRESENT,
    CM_LOCATE_DEVNODE_NORMAL, CR_BUFFER_SMALL, CR_SUCCESS,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPKEY_Device_BusReportedDeviceDesc, DEVPKEY_Device_DeviceDesc, DEVPKEY_Device_FriendlyName,
    DEVPROP_TYPE_STRING,
};

pub fn list_devices(with_names: bool) -> Result<Vec<UsbDevice>, String> {
    let mut devices = Vec::new();
    for instance in device_ids()? {
        // Interfaces of composite devices (…&MI_xx) repeat their parent's ID.
        if instance.to_ascii_uppercase().contains("&MI_") {
            continue;
        }
        let Some(id) = parse_instance_id(&instance) else {
            continue;
        };
        let name = if with_names {
            device_name(&instance)
        } else {
            None
        };
        devices.push(UsbDevice {
            id,
            name,
            location: instance,
        });
    }
    Ok(devices)
}

/// Present device instance IDs under the USB enumerator.
fn device_ids() -> Result<Vec<String>, String> {
    let filter = wide("USB");
    let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
    loop {
        let mut len = 0u32;
        // SAFETY: valid out pointer and NUL-terminated filter.
        let status = unsafe { CM_Get_Device_ID_List_SizeW(&mut len, filter.as_ptr(), flags) };
        if status != CR_SUCCESS {
            return Err(format!("CM_Get_Device_ID_List_Size failed ({status})"));
        }
        let mut buffer = vec![0u16; len as usize];
        // SAFETY: the buffer holds `len` UTF-16 units.
        let status =
            unsafe { CM_Get_Device_ID_ListW(filter.as_ptr(), buffer.as_mut_ptr(), len, flags) };
        if status == CR_BUFFER_SMALL {
            continue; // a device arrived between the two calls
        }
        if status != CR_SUCCESS {
            return Err(format!("CM_Get_Device_ID_List failed ({status})"));
        }
        return Ok(buffer
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect());
    }
}

fn device_name(instance: &str) -> Option<String> {
    let mut devinst = 0u32;
    let id = wide(instance);
    // SAFETY: valid out pointer and NUL-terminated instance ID.
    if unsafe { CM_Locate_DevNodeW(&mut devinst, id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) }
        != CR_SUCCESS
    {
        return None;
    }
    for key in [
        &DEVPKEY_Device_BusReportedDeviceDesc,
        &DEVPKEY_Device_FriendlyName,
        &DEVPKEY_Device_DeviceDesc,
    ] {
        let mut kind = 0u32;
        let mut buffer = [0u16; 256];
        let mut size = std::mem::size_of_val(&buffer) as u32;
        // SAFETY: `size` is the buffer's size in bytes.
        let status = unsafe {
            CM_Get_DevNode_PropertyW(
                devinst,
                key,
                &mut kind,
                buffer.as_mut_ptr().cast(),
                &mut size,
                0,
            )
        };
        if status == CR_SUCCESS && kind == DEVPROP_TYPE_STRING {
            let len = (size as usize / 2).min(buffer.len());
            let name = String::from_utf16_lossy(&buffer[..len]);
            let name = name.trim_end_matches('\0').trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Reads `VID_xxxx&PID_xxxx` from an instance ID such as
/// `USB\VID_046D&PID_C07D\1663345A3831`.
pub fn parse_instance_id(instance: &str) -> Option<UsbId> {
    let upper = instance.to_ascii_uppercase();
    let hex_after = |tag: &str| {
        let start = upper.find(tag)? + tag.len();
        u16::from_str_radix(upper.get(start..start + 4)?, 16).ok()
    };
    Some(UsbId {
        vid: hex_after("VID_")?,
        pid: hex_after("PID_")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_instance_ids() {
        assert_eq!(
            parse_instance_id(r"USB\VID_FC32&PID_1287\7&3668FDEB&0&3"),
            Some(UsbId {
                vid: 0xFC32,
                pid: 0x1287
            })
        );
        assert_eq!(parse_instance_id(r"USB\ROOT_HUB30\4&1DE9F484&0&0"), None);
    }
}
