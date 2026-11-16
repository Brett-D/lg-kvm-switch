//! Raw DDC/CI over NVIDIA's I2C API.
//!
//! Windows' monitor API always sends from host address 0x51, which LG ignores
//! for input changes. NVAPI lets us write the whole packet, so we can send
//! from 0x50. Function IDs are resolved through `nvapi_QueryInterface`.

use super::wide;
use crate::config::Settings;
use crate::edid::EdidInfo;
use crate::lg;
use crate::monitor::{Display, Monitor};
use std::ffi::c_void;
use std::mem::{size_of, transmute};
use std::ptr::null_mut;
use std::thread::sleep;
use std::time::Duration;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

type GpuHandle = *mut c_void;
type QueryInterfaceFn = unsafe extern "C" fn(u32) -> *mut c_void;
type InitializeFn = unsafe extern "C" fn() -> i32;
type EnumGpusFn = unsafe extern "C" fn(*mut GpuHandle, *mut u32) -> i32;
type ConnectedOutputsFn = unsafe extern "C" fn(GpuHandle, *mut u32) -> i32;
type GetEdidFn = unsafe extern "C" fn(GpuHandle, u32, *mut NvEdid) -> i32;
type I2cFn = unsafe extern "C" fn(GpuHandle, *mut NvI2cInfo) -> i32;

const ID_INITIALIZE: u32 = 0x0150_E828;
const ID_ENUM_PHYSICAL_GPUS: u32 = 0xE5AC_921F;
const ID_GET_CONNECTED_OUTPUTS: u32 = 0x1730_BFC9;
const ID_GET_EDID: u32 = 0x37D3_2E69;
const ID_I2C_WRITE: u32 = 0xE812_EB07;
const MAX_GPUS: usize = 64;

#[cfg(target_pointer_width = "64")]
const DLL: &str = "nvapi64.dll";
#[cfg(not(target_pointer_width = "64"))]
const DLL: &str = "nvapi.dll";

/// `NV_I2C_INFO_V3` from the NVAPI SDK.
#[repr(C)]
struct NvI2cInfo {
    version: u32,
    display_mask: u32,
    is_ddc_port: u8,
    i2c_dev_address: u8,
    reg_address: *mut u8,
    reg_address_size: u32,
    data: *mut u8,
    size: u32,
    speed: u32,
    speed_khz: u32,
    port_id: u8,
    is_port_id_set: u32,
}

/// `NV_EDID_V3` from the NVAPI SDK.
#[repr(C)]
struct NvEdid {
    version: u32,
    data: [u8; 256],
    size: u32,
    edid_id: u32,
    offset: u32,
}

const fn struct_version<T>(version: u32) -> u32 {
    size_of::<T>() as u32 | (version << 16)
}

fn check(status: i32, what: &str) -> Result<(), String> {
    match status {
        0 => Ok(()),
        _ => Err(format!("{what} failed (NVAPI status {status})")),
    }
}

/// An output on an NVIDIA GPU.
#[derive(Clone, Debug)]
pub struct NvOutput {
    gpu: usize,
    mask: u32,
    pub edid: Option<EdidInfo>,
}

impl NvOutput {
    pub fn location(&self) -> String {
        format!("NVIDIA GPU {} output 0x{:08X}", self.gpu, self.mask)
    }
}

pub struct NvApi {
    gpus: Vec<GpuHandle>,
    connected_outputs: ConnectedOutputsFn,
    get_edid: GetEdidFn,
    i2c_write: I2cFn,
}

impl NvApi {
    pub fn load() -> Result<Self, String> {
        // SAFETY: the function pointers come from nvapi_QueryInterface for the
        // documented NVAPI function IDs, and are called with their real
        // signatures. The DLL stays loaded for the life of the process.
        unsafe {
            let lib = LoadLibraryW(wide(DLL).as_ptr());
            if lib.is_null() {
                return Err(format!(
                    "{DLL} not found; an NVIDIA GPU and driver are required on Windows"
                ));
            }
            let query = GetProcAddress(lib, c"nvapi_QueryInterface".as_ptr().cast())
                .ok_or("nvapi_QueryInterface not found in NVAPI")?;
            let query: QueryInterfaceFn = transmute(query);
            let get = |id: u32| {
                let ptr = query(id);
                if ptr.is_null() {
                    Err(format!("NVAPI function 0x{id:08X} is unavailable"))
                } else {
                    Ok(ptr)
                }
            };
            let initialize: InitializeFn = transmute(get(ID_INITIALIZE)?);
            check(initialize(), "NvAPI_Initialize")?;
            let enum_gpus: EnumGpusFn = transmute(get(ID_ENUM_PHYSICAL_GPUS)?);
            let mut handles = [null_mut(); MAX_GPUS];
            let mut count = 0u32;
            check(
                enum_gpus(handles.as_mut_ptr(), &mut count),
                "NvAPI_EnumPhysicalGPUs",
            )?;
            Ok(Self {
                gpus: handles[..(count as usize).min(MAX_GPUS)].to_vec(),
                connected_outputs: transmute::<*mut c_void, ConnectedOutputsFn>(get(
                    ID_GET_CONNECTED_OUTPUTS,
                )?),
                get_edid: transmute::<*mut c_void, GetEdidFn>(get(ID_GET_EDID)?),
                i2c_write: transmute::<*mut c_void, I2cFn>(get(ID_I2C_WRITE)?),
            })
        }
    }

    /// Connected outputs on every NVIDIA GPU, with their EDIDs.
    pub fn outputs(&self) -> Vec<NvOutput> {
        let mut found = Vec::new();
        for (gpu, &handle) in self.gpus.iter().enumerate() {
            let mut connected = 0u32;
            // SAFETY: valid GPU handle and out pointer.
            if unsafe { (self.connected_outputs)(handle, &mut connected) } != 0 {
                continue;
            }
            for bit in 0..32 {
                let mask = 1u32 << bit;
                if connected & mask != 0 {
                    found.push(NvOutput {
                        gpu,
                        mask,
                        edid: self.edid(handle, mask),
                    });
                }
            }
        }
        found
    }

    fn edid(&self, gpu: GpuHandle, mask: u32) -> Option<EdidInfo> {
        let mut edid = NvEdid {
            version: struct_version::<NvEdid>(3),
            data: [0; 256],
            size: 0,
            edid_id: 0,
            offset: 0,
        };
        // SAFETY: the struct is versioned and sized as NVAPI expects.
        if unsafe { (self.get_edid)(gpu, mask, &mut edid) } != 0 {
            return None;
        }
        EdidInfo::parse(&edid.data)
    }

    /// Writes a DDC/CI packet (starting with the source address) to the
    /// monitor on `output`.
    pub fn write(&self, output: &NvOutput, packet: &[u8]) -> Result<(), String> {
        let mut data = packet.to_vec();
        let mut info = NvI2cInfo {
            version: struct_version::<NvI2cInfo>(3),
            display_mask: output.mask,
            is_ddc_port: 1,
            i2c_dev_address: lg::DDC_ADDR << 1,
            reg_address: null_mut(),
            reg_address_size: 0,
            data: data.as_mut_ptr(),
            size: data.len() as u32,
            speed: 0xFFFF, // NVAPI_I2C_SPEED_DEPRECATED: use speed_khz
            speed_khz: 0,  // NVAPI_I2C_SPEED_DEFAULT
            port_id: 0,
            is_port_id_set: 0,
        };
        let gpu = *self.gpus.get(output.gpu).ok_or("GPU disappeared")?;
        // SAFETY: `info` points at `data`, which outlives the call.
        check(
            unsafe { (self.i2c_write)(gpu, &mut info) },
            "NvAPI_I2CWrite",
        )
    }
}

fn select(outputs: Vec<NvOutput>, settings: &Settings) -> Vec<NvOutput> {
    let (vendor, product) = (settings.monitor_vendor.as_deref(), settings.monitor_product);
    outputs
        .into_iter()
        .filter(|o| match &o.edid {
            Some(edid) => edid.matches(vendor, product),
            None => vendor.is_none() && product.is_none(),
        })
        .collect()
}

pub fn list_displays(settings: &Settings) -> Result<Vec<Display>, String> {
    let api = NvApi::load()?;
    let outputs = api.outputs();
    let selected: Vec<u32> = select(outputs.clone(), settings)
        .iter()
        .map(|o| o.mask)
        .collect();
    Ok(outputs
        .into_iter()
        .map(|o| Display {
            location: o.location(),
            selected: selected.contains(&o.mask),
            edid: o.edid,
        })
        .collect())
}

/// The configured LG monitor, reached through NVAPI.
pub struct NvMonitor {
    settings: Settings,
    api: Option<NvApi>,
    /// Last outputs that matched, used if the monitor briefly stops
    /// reporting as connected after switching away.
    last_targets: Vec<NvOutput>,
}

impl NvMonitor {
    pub fn new(settings: &Settings) -> Self {
        Self {
            settings: settings.clone(),
            api: None,
            last_targets: Vec::new(),
        }
    }

    fn send(&mut self, packet: &[u8]) -> Result<(), String> {
        if self.api.is_none() {
            self.api = Some(NvApi::load()?);
        }
        let api = self.api.as_ref().expect("NVAPI was just loaded");
        let mut targets = select(api.outputs(), &self.settings);
        if targets.is_empty() {
            targets = self.last_targets.clone();
        }
        if targets.is_empty() {
            return Err(
                "no matching monitor on any NVIDIA output (see `lg-kvm-switch monitors`)".into(),
            );
        }
        let mut error = None;
        let mut sent = false;
        for target in &targets {
            match api.write(target, packet) {
                Ok(()) => sent = true,
                Err(e) => error = Some(format!("{}: {e}", target.location())),
            }
        }
        self.last_targets = targets;
        match (sent, error) {
            (true, _) => Ok(()),
            (false, error) => Err(error.unwrap_or_default()),
        }
    }
}

impl Monitor for NvMonitor {
    fn set_input(&mut self, input: u16) -> Result<(), String> {
        let packet = lg::input_packet(input);
        let mut last_error = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                // The driver may have reset (sleep, driver update); reload it.
                self.api = None;
                sleep(Duration::from_millis(300));
            }
            match self.send(&packet) {
                Ok(()) => return Ok(()),
                Err(e) => last_error = e,
            }
        }
        Err(last_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structs_match_the_nvapi_sdk_layout() {
        #[cfg(target_pointer_width = "64")]
        assert_eq!(size_of::<NvI2cInfo>(), 64);
        assert_eq!(size_of::<NvEdid>(), 272);
    }
}
