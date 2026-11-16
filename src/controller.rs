//! Decides when to switch the monitor based on where the KVM is.

use crate::config::Settings;
use crate::lg::input_name;
use crate::log;
use crate::monitor::Monitor;
use crate::usb::UsbId;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The KVM has not moved since the last action.
    Unchanged,
    /// Automatic switching is paused.
    Paused,
    /// First reading after startup: the monitor is left alone, since it
    /// cannot report its input and re-selecting the shown input flickers.
    Started,
    Switched(u16),
    Failed(String),
}

pub struct Controller {
    settings: Settings,
    monitor: Box<dyn Monitor>,
    /// KVM position the monitor was last set (or left) for.
    acted_on: Option<bool>,
    started: bool,
    pub auto: bool,
    pub kvm_here: Option<bool>,
    pub last_error: Option<String>,
}

impl Controller {
    pub fn new(settings: Settings, monitor: Box<dyn Monitor>) -> Self {
        Self {
            settings,
            monitor,
            acted_on: None,
            started: false,
            auto: true,
            kvm_here: None,
            last_error: None,
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Whether any watched USB device is connected.
    pub fn is_here(&self, present: &BTreeSet<UsbId>) -> bool {
        self.settings.watch.iter().any(|id| present.contains(id))
    }

    /// Acts on the current KVM position. Call once USB changes have settled.
    pub fn update(&mut self, here: bool) -> Outcome {
        if self.kvm_here != Some(here) {
            log!(
                "KVM is on {}",
                if here {
                    "this computer"
                } else {
                    "another computer"
                }
            );
        }
        self.kvm_here = Some(here);
        if self.acted_on == Some(here) {
            return Outcome::Unchanged;
        }
        if !self.auto {
            return Outcome::Paused;
        }
        let target = if here {
            self.settings.local_input
        } else {
            self.settings.remote_input
        };
        if !self.started {
            self.started = true;
            self.acted_on = Some(here);
            log!(
                "Startup: assuming the monitor already shows {}",
                input_name(target)
            );
            return Outcome::Started;
        }
        match self.monitor.set_input(target) {
            Ok(()) => {
                self.acted_on = Some(here);
                self.last_error = None;
                log!("Switched monitor to {}", input_name(target));
                Outcome::Switched(target)
            }
            Err(e) => {
                self.acted_on = None;
                log!("Failed to switch to {}: {e}", input_name(target));
                self.last_error = Some(e.clone());
                Outcome::Failed(e)
            }
        }
    }

    /// Switches to `input` on request, regardless of the KVM position.
    pub fn show(&mut self, input: u16) -> Result<(), String> {
        let result = self.monitor.set_input(input);
        match &result {
            Ok(()) => {
                self.last_error = None;
                log!("Switched monitor to {} (manual)", input_name(input));
            }
            Err(e) => {
                log!("Failed to switch to {}: {e}", input_name(input));
                self.last_error = Some(e.clone());
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::cell::RefCell;
    use std::rc::Rc;

    type Shared<T> = Rc<RefCell<T>>;

    #[derive(Default)]
    struct Fake {
        sent: Shared<Vec<u16>>,
        fail: Shared<bool>,
    }

    impl Monitor for Fake {
        fn set_input(&mut self, input: u16) -> Result<(), String> {
            if *self.fail.borrow() {
                return Err("bus error".into());
            }
            self.sent.borrow_mut().push(input);
            Ok(())
        }
    }

    fn controller() -> (Controller, Shared<Vec<u16>>, Shared<bool>) {
        let config = Config {
            watch_devices: vec!["FC32:1287".into()],
            ..Config::default()
        };
        let fake = Fake::default();
        let (sent, fail) = (fake.sent.clone(), fake.fail.clone());
        let controller = Controller::new(config.settings().unwrap(), Box::new(fake));
        (controller, sent, fail)
    }

    #[test]
    fn leaves_monitor_alone_at_startup_then_follows_the_kvm() {
        let (mut c, sent, _) = controller();
        assert_eq!(c.update(true), Outcome::Started);
        assert_eq!(c.update(true), Outcome::Unchanged);
        assert_eq!(c.update(false), Outcome::Switched(0xD1));
        assert_eq!(c.update(false), Outcome::Unchanged);
        assert_eq!(c.update(true), Outcome::Switched(0xD0));
        assert_eq!(*sent.borrow(), vec![0xD1, 0xD0]);
    }

    #[test]
    fn retries_after_a_failed_switch() {
        let (mut c, sent, fail) = controller();
        c.update(true);
        *fail.borrow_mut() = true;
        assert!(matches!(c.update(false), Outcome::Failed(_)));
        assert!(c.last_error.is_some());
        *fail.borrow_mut() = false;
        assert_eq!(c.update(false), Outcome::Switched(0xD1));
        assert_eq!(c.last_error, None);
        assert_eq!(*sent.borrow(), vec![0xD1]);
    }

    #[test]
    fn pausing_defers_the_switch_until_resumed() {
        let (mut c, sent, _) = controller();
        c.update(true);
        c.auto = false;
        assert_eq!(c.update(false), Outcome::Paused);
        c.auto = true;
        assert_eq!(c.update(false), Outcome::Switched(0xD1));
        assert_eq!(*sent.borrow(), vec![0xD1]);
    }

    #[test]
    fn detects_watched_devices() {
        let (c, _, _) = controller();
        let keyboard = UsbId {
            vid: 0xFC32,
            pid: 0x1287,
        };
        let hub = UsbId {
            vid: 0x05E3,
            pid: 0x0610,
        };
        assert!(c.is_here(&BTreeSet::from([hub, keyboard])));
        assert!(!c.is_here(&BTreeSet::from([hub])));
    }
}
