//! The TOML configuration file.

use crate::lg;
use crate::usb::UsbId;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const APP_NAME: &str = "lg-kvm-switch";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// USB devices (`VID:PID`) that move with the KVM.
    pub watch_devices: Vec<String>,
    pub local_input: String,
    pub remote_input: String,
    /// Input on the left half in Picture-by-Picture; defaults to `remote_input`.
    pub pbp_left: Option<String>,
    pub debounce_ms: u64,
    pub monitor_vendor: String,
    pub monitor_product: Option<String>,
    pub i2c_bus: Option<String>,
    pub poll_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            watch_devices: Vec::new(),
            local_input: "dp".into(),
            remote_input: "usbc".into(),
            pbp_left: None,
            debounce_ms: 750,
            monitor_vendor: "GSM".into(),
            monitor_product: None,
            i2c_bus: None,
            poll_ms: 500,
        }
    }
}

/// A validated [`Config`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub watch: Vec<UsbId>,
    pub local_input: u16,
    pub remote_input: u16,
    /// Input on the left half in Picture-by-Picture: `local_input` or
    /// `remote_input`. The other one goes on the right.
    pub pbp_left: u16,
    pub debounce: Duration,
    pub monitor_vendor: Option<String>,
    pub monitor_product: Option<u16>,
    pub i2c_bus: Option<PathBuf>,
    pub poll: Duration,
}

impl Settings {
    /// Input on the right half in Picture-by-Picture.
    pub fn pbp_right(&self) -> u16 {
        if self.pbp_left == self.local_input {
            self.remote_input
        } else {
            self.local_input
        }
    }
}

impl Config {
    pub fn settings(&self) -> Result<Settings, String> {
        let watch = self
            .watch_devices
            .iter()
            .map(|id| id.parse())
            .collect::<Result<Vec<UsbId>, _>>()
            .map_err(|e| format!("watch_devices: {e}"))?;
        let local_input =
            lg::parse_input(&self.local_input).map_err(|e| format!("local_input: {e}"))?;
        let remote_input =
            lg::parse_input(&self.remote_input).map_err(|e| format!("remote_input: {e}"))?;
        if local_input == remote_input {
            return Err("local_input and remote_input must be different".into());
        }
        let pbp_left = match &self.pbp_left {
            Some(text) => lg::parse_input(text).map_err(|e| format!("pbp_left: {e}"))?,
            None => remote_input,
        };
        if pbp_left != local_input && pbp_left != remote_input {
            return Err("pbp_left must be the same input as local_input or remote_input".into());
        }
        let monitor_product = self
            .monitor_product
            .as_deref()
            .map(|p| {
                let hex = p.trim().trim_start_matches("0x");
                u16::from_str_radix(hex, 16)
                    .map_err(|_| format!("monitor_product '{p}' is not a hex code like 9E9D"))
            })
            .transpose()?;
        let vendor = self.monitor_vendor.trim();
        Ok(Settings {
            watch,
            local_input,
            remote_input,
            pbp_left,
            debounce: Duration::from_millis(self.debounce_ms),
            monitor_vendor: (!vendor.is_empty()).then(|| vendor.to_ascii_uppercase()),
            monitor_product,
            i2c_bus: self.i2c_bus.as_deref().map(PathBuf::from),
            poll: Duration::from_millis(self.poll_ms.max(100)),
        })
    }

    /// Renders the config as commented TOML.
    pub fn to_toml(&self) -> String {
        let watch = self
            .watch_devices
            .iter()
            .map(|id| quote(id))
            .collect::<Vec<_>>()
            .join(", ");
        let product = match &self.monitor_product {
            Some(p) => format!("monitor_product = {}", quote(p)),
            None => "# monitor_product = \"9E9D\"".into(),
        };
        let bus = match &self.i2c_bus {
            Some(b) => format!("i2c_bus = {}", quote(b)),
            None => "# i2c_bus = \"/dev/i2c-5\"".into(),
        };
        let pbp_left = match &self.pbp_left {
            Some(input) => format!("pbp_left = {}", quote(input)),
            None => format!("# pbp_left = {}", quote(&self.remote_input)),
        };
        format!(
            r#"# lg-kvm-switch configuration

# USB devices (VID:PID) that move with your KVM. When any of them is
# connected, the KVM is on this computer. Run `lg-kvm-switch learn` to detect
# them, or pick them from `lg-kvm-switch devices`.
watch_devices = [{watch}]

# Monitor input to show when the KVM is on this computer, and when it is not:
# dp, usbc, hdmi1, hdmi2, or a raw LG input code such as 0xD0.
local_input = {local}
remote_input = {remote}

# Picture-by-Picture: the input on the left half (local_input or
# remote_input); the other goes on the right. Defaults to remote_input.
{pbp_left}

# Wait for USB changes to settle this long before switching.
debounce_ms = {debounce}

# Only control monitors whose EDID vendor matches (GSM = LG). Use "" for any.
monitor_vendor = {vendor}
# Optionally also require this EDID product code (hex). Some LG monitors
# report a different code on each input, so leave it unset unless needed.
{product}

# Linux only: the monitor's I2C bus. Found automatically when unset.
{bus}
# Linux only: how often to check USB devices.
poll_ms = {poll}
"#,
            local = quote(&self.local_input),
            remote = quote(&self.remote_input),
            debounce = self.debounce_ms,
            vendor = quote(&self.monitor_vendor),
            poll = self.poll_ms,
        )
    }
}

fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `%APPDATA%\lg-kvm-switch\config.toml` on Windows,
/// `$XDG_CONFIG_HOME/lg-kvm-switch/config.toml` elsewhere.
pub fn default_path() -> PathBuf {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let base = if cfg!(windows) {
        var("APPDATA").map(PathBuf::from)
    } else {
        var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| var("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.unwrap_or_else(|| PathBuf::from("."))
        .join(APP_NAME)
        .join("config.toml")
}

pub fn load(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "no config at {} (run `lg-kvm-switch learn` to create one)",
                path.display()
            )
        } else {
            format!("cannot read {}: {e}", path.display())
        }
    })?;
    toml::from_str(&text).map_err(|e| format!("invalid config {}: {e}", path.display()))
}

/// Loads the config, or returns the defaults when the file does not exist.
pub fn load_or_default(path: &Path) -> Result<Config, String> {
    if path.exists() {
        load(path)
    } else {
        Ok(Config::default())
    }
}

pub fn save(path: &Path, config: &Config) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, config.to_toml())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_config_round_trips() {
        let config = Config {
            watch_devices: vec!["FC32:1287".into(), "046D:C07D".into()],
            pbp_left: Some("dp".into()),
            monitor_product: Some("9E9D".into()),
            i2c_bus: Some(r"/dev/i2c-5".into()),
            ..Config::default()
        };
        let parsed: Config = toml::from_str(&config.to_toml()).unwrap();
        assert_eq!(parsed, config);
        assert_eq!(
            toml::from_str::<Config>(&Config::default().to_toml()).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn validates_settings() {
        let config = Config {
            watch_devices: vec!["fc32:1287".into()],
            monitor_product: Some("0x9e9d".into()),
            ..Config::default()
        };
        let settings = config.settings().unwrap();
        assert_eq!(settings.watch[0].to_string(), "FC32:1287");
        assert_eq!((settings.local_input, settings.remote_input), (0xD0, 0xD1));
        assert_eq!(settings.monitor_vendor.as_deref(), Some("GSM"));
        assert_eq!(settings.monitor_product, Some(0x9E9D));

        let same = Config {
            remote_input: "displayport".into(),
            ..Config::default()
        };
        assert!(same.settings().is_err());
        let any_vendor = Config {
            monitor_vendor: " ".into(),
            ..Config::default()
        };
        assert_eq!(any_vendor.settings().unwrap().monitor_vendor, None);
    }

    #[test]
    fn pbp_layout_defaults_to_remote_on_the_left() {
        let settings = Config::default().settings().unwrap();
        assert_eq!((settings.pbp_left, settings.pbp_right()), (0xD1, 0xD0));
        let swapped = Config {
            pbp_left: Some("dp".into()),
            ..Config::default()
        };
        let settings = swapped.settings().unwrap();
        assert_eq!((settings.pbp_left, settings.pbp_right()), (0xD0, 0xD1));
        let unrelated = Config {
            pbp_left: Some("hdmi1".into()),
            ..Config::default()
        };
        assert!(unrelated.settings().is_err());
    }

    #[test]
    fn rejects_unknown_keys_and_bad_ids() {
        assert!(toml::from_str::<Config>("watch = []").is_err());
        let bad = Config {
            watch_devices: vec!["keyboard".into()],
            ..Config::default()
        };
        assert!(bad.settings().is_err());
    }
}
