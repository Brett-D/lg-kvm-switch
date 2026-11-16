//! DDC/CI through the kernel's i2c-dev interface. Here the whole packet is
//! ours to write, so LG's 0x50 source address works on any GPU.

use crate::config::Settings;
use crate::edid::EdidInfo;
use crate::lg;
use crate::monitor::{Display, Monitor};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

const I2C_SLAVE: u64 = 0x0703;
const I2C_SLAVE_FORCE: u64 = 0x0706;
const EDID_ADDR: u8 = 0x50;

fn open_device(path: &Path, address: u8) -> io::Result<File> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    // A kernel driver (e.g. ddcci) may own the address; DDC still works.
    for request in [I2C_SLAVE, I2C_SLAVE_FORCE] {
        // SAFETY: I2C_SLAVE takes the 7-bit address as its argument.
        if unsafe { libc::ioctl(file.as_raw_fd(), request as _, address as libc::c_ulong) } == 0 {
            return Ok(file);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EBUSY) {
            return Err(error);
        }
    }
    Err(io::Error::last_os_error())
}

fn read_edid(path: &Path) -> io::Result<Vec<u8>> {
    let mut device = open_device(path, EDID_ADDR)?;
    device.write_all(&[0])?;
    let mut edid = vec![0u8; 128];
    device.read_exact(&mut edid)?;
    Ok(edid)
}

/// I2C adapters as (`/dev/i2c-N`, adapter name), in bus order.
fn adapters() -> Vec<(PathBuf, String)> {
    let mut found: Vec<(u32, PathBuf, String)> = fs::read_dir("/sys/class/i2c-dev")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let number = name.strip_prefix("i2c-")?.parse().ok()?;
            let adapter = fs::read_to_string(entry.path().join("name")).unwrap_or_default();
            Some((
                number,
                Path::new("/dev").join(&name),
                adapter.trim().to_string(),
            ))
        })
        .collect();
    found.sort();
    found
        .into_iter()
        .map(|(_, path, name)| (path, name))
        .collect()
}

struct Bus {
    path: PathBuf,
    adapter: String,
    edid: Option<EdidInfo>,
    selected: bool,
}

/// Finds monitors by reading EDIDs. The NVIDIA proprietary driver creates no
/// sysfs `ddc` links, so every non-SMBus adapter is probed.
fn scan(settings: &Settings) -> Result<Vec<Bus>, String> {
    let adapters = adapters();
    if adapters.is_empty() {
        return Err("no I2C buses found; load the i2c-dev module: sudo modprobe i2c-dev".into());
    }
    let mut buses = Vec::new();
    let mut denied = None;
    for (path, adapter) in adapters {
        let explicit = settings.i2c_bus.as_deref() == Some(path.as_path());
        if adapter.to_ascii_lowercase().contains("smbus") && !explicit {
            continue;
        }
        let edid = match read_edid(&path) {
            Ok(data) => EdidInfo::parse(&data),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                denied.get_or_insert(path.clone());
                None
            }
            Err(_) => None,
        };
        let selected = match &settings.i2c_bus {
            Some(bus) => bus == &path,
            None => edid.as_ref().is_some_and(|e| {
                e.matches(settings.monitor_vendor.as_deref(), settings.monitor_product)
            }),
        };
        if edid.is_some() || selected {
            buses.push(Bus {
                path,
                adapter,
                edid,
                selected,
            });
        }
    }
    if buses.is_empty() {
        if let Some(path) = denied {
            return Err(format!(
                "permission denied on {}; add your user to the i2c group (see README)",
                path.display()
            ));
        }
    }
    Ok(buses)
}

pub fn list_displays(settings: &Settings) -> Result<Vec<Display>, String> {
    Ok(scan(settings)?
        .into_iter()
        .map(|bus| Display {
            location: format!("{} ({})", bus.path.display(), bus.adapter),
            edid: bus.edid,
            selected: bus.selected,
        })
        .collect())
}

/// The configured LG monitor on one or more I2C buses.
pub struct I2cMonitor {
    settings: Settings,
    targets: Vec<PathBuf>,
}

impl I2cMonitor {
    pub fn new(settings: &Settings) -> Self {
        Self {
            settings: settings.clone(),
            targets: Vec::new(),
        }
    }

    fn send(&mut self, packet: &[u8]) -> Result<(), String> {
        if self.targets.is_empty() {
            self.targets = scan(&self.settings)?
                .into_iter()
                .filter(|bus| bus.selected)
                .map(|bus| bus.path)
                .collect();
        }
        if self.targets.is_empty() {
            return Err("no matching monitor on any I2C bus (see `lg-kvm-switch monitors`)".into());
        }
        let mut error = None;
        let mut sent = false;
        for path in &self.targets {
            match open_device(path, lg::DDC_ADDR).and_then(|mut d| d.write_all(packet)) {
                Ok(()) => sent = true,
                Err(e) => error = Some(format!("{}: {e}", path.display())),
            }
        }
        match (sent, error) {
            (true, _) => Ok(()),
            (false, error) => Err(error.unwrap_or_default()),
        }
    }
}

impl Monitor for I2cMonitor {
    fn set_input(&mut self, input: u16) -> Result<(), String> {
        let packet = lg::input_packet(input);
        let mut last_error = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                // Bus numbers can change when the GPU driver reloads.
                self.targets.clear();
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
