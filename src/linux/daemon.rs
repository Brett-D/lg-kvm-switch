//! Foreground watcher for Linux, meant to run as a systemd user service.

use super::i2c::I2cMonitor;
use crate::config;
use crate::controller::{Controller, Outcome};
use crate::lg::input_name;
use crate::log;
use crate::usb;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

const RETRY: Duration = Duration::from_secs(5);

pub fn run(config_path: &Path) -> Result<(), String> {
    let settings = config::load(config_path)?.settings()?;
    if settings.watch.is_empty() {
        return Err("no watch_devices configured; run `lg-kvm-switch learn` first".into());
    }
    let ids: Vec<String> = settings.watch.iter().map(|id| id.to_string()).collect();
    log!(
        "Watching {} -> {} here, {} away",
        ids.join(", "),
        input_name(settings.local_input),
        input_name(settings.remote_input)
    );
    let (poll, debounce) = (settings.poll, settings.debounce);
    let mut controller = Controller::new(settings.clone(), Box::new(I2cMonitor::new(&settings)));

    // Act only once the position has been stable for the debounce period.
    let mut seen: Option<bool> = None;
    let mut changed_at = Instant::now();
    let mut retry_at: Option<Instant> = None;
    loop {
        match usb::present_ids() {
            Ok(present) => {
                let here = controller.is_here(&present);
                if seen != Some(here) {
                    seen = Some(here);
                    changed_at = Instant::now();
                }
                let settled = changed_at.elapsed() >= debounce;
                if settled && retry_at.map_or(true, |at| Instant::now() >= at) {
                    retry_at = match controller.update(here) {
                        Outcome::Failed(_) => Some(Instant::now() + RETRY),
                        _ => None,
                    };
                }
            }
            Err(e) => log!("Could not list USB devices: {e}"),
        }
        sleep(poll);
    }
}
