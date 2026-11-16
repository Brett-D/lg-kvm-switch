//! Linux backend: `/dev/i2c-*` DDC/CI, sysfs USB enumeration, and a polling
//! daemon for systemd.

pub mod daemon;
pub mod i2c;
pub mod usb;
