//! Switch an LG monitor's input when a USB KVM moves your keyboard and mouse
//! between computers.
//!
//! LG monitors ignore the standard DDC/CI input-select command. They only act
//! on LG's own command (VCP `0xF4`) sent from I2C source address `0x50`, which
//! the Windows monitor API cannot send. This crate writes that packet straight
//! to the monitor's I2C bus: through NVAPI on Windows and `/dev/i2c-*` on Linux.

pub mod config;
pub mod controller;
pub mod edid;
pub mod lg;
pub mod log;
pub mod monitor;
pub mod usb;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;
