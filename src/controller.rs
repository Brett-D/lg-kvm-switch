//! Decides when to switch the monitor based on where the KVM is, and drives
//! Picture-by-Picture and the picture and audio controls.

use crate::config::Settings;
use crate::lg::{self, input_name};
use crate::log;
use crate::monitor::Monitor;
use crate::usb::UsbId;
use std::collections::BTreeSet;
use std::thread::sleep;
use std::time::Duration;

/// Time the monitor needs after an input or layout change before it takes
/// the next one. Selecting an input 1 s after turning PBP on was ignored;
/// 4 s worked.
pub const SETTLE: Duration = Duration::from_millis(3000);

/// One monitor command in a multi-step change such as Picture-by-Picture.
/// Run steps [`SETTLE`] apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Input(u16),
    Layout(u16),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The KVM has not moved since the last action.
    Unchanged,
    /// Automatic switching is paused.
    Paused,
    /// Picture-by-Picture shows both computers, so inputs are left alone.
    PictureByPicture,
    /// The monitor already shows the input for the KVM's position.
    AlreadyShown(u16),
    /// First reading after startup, with the monitor already on (or, if it
    /// can't report its input, assumed to be on) the right input.
    Started,
    Switched(u16),
    Failed(String),
}

/// Current picture and audio settings; `None` where the monitor didn't answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Levels {
    pub brightness: Option<u16>,
    pub contrast: Option<u16>,
    pub volume: Option<u16>,
    pub muted: Option<bool>,
    pub pbp: Option<bool>,
    /// Raw split-screen layout code from [`lg::LG_PBP_VCP`].
    pub layout: Option<u16>,
}

pub struct Controller {
    settings: Settings,
    monitor: Box<dyn Monitor>,
    /// KVM position the monitor was last set (or left) for.
    acted_on: Option<bool>,
    started: bool,
    /// Input last selected; in Picture-by-Picture, the one on the left.
    main_input: Option<u16>,
    /// Whether a switch is waiting for split screen to end (logged once).
    deferred: bool,
    settle: Duration,
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
            main_input: None,
            deferred: false,
            settle: SETTLE,
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

    fn target(&self, here: bool) -> u16 {
        if here {
            self.settings.local_input
        } else {
            self.settings.remote_input
        }
    }

    /// Records the KVM position and assumes the monitor shows its input, as
    /// at startup, without logging or switching.
    pub fn assume_position(&mut self, here: bool) {
        self.kvm_here = Some(here);
        self.acted_on = Some(here);
        self.started = true;
        self.main_input = Some(self.target(here));
    }

    /// Acts on the current KVM position. Call once USB changes have settled,
    /// and again every few seconds while it returns
    /// [`Outcome::PictureByPicture`] or [`Outcome::Failed`].
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
            self.deferred = false;
        }
        self.kvm_here = Some(here);
        if self.acted_on == Some(here) {
            return Outcome::Unchanged;
        }
        if !self.auto {
            return Outcome::Paused;
        }
        let target = self.target(here);
        let current = self.sync_main_input();
        if !self.started && current.is_none_or(|input| input == target) {
            self.started = true;
            self.acted_on = Some(here);
            self.main_input = Some(target);
            log!(
                "Startup: the monitor {} {}",
                if current.is_some() {
                    "shows"
                } else {
                    "is assumed to show"
                },
                input_name(target)
            );
            return Outcome::Started;
        }
        self.started = true;
        if self.pbp() == Some(true) {
            if !self.deferred {
                self.deferred = true;
                log!("Split screen is on; leaving inputs alone");
            }
            return Outcome::PictureByPicture;
        }
        self.deferred = false;
        if current == Some(target) {
            self.acted_on = Some(here);
            self.last_error = None;
            log!("The monitor already shows {}", input_name(target));
            return Outcome::AlreadyShown(target);
        }
        match self.monitor.set_input(target) {
            Ok(()) => {
                self.acted_on = Some(here);
                self.main_input = Some(target);
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

    /// Steps that show `input` full screen, turning split screen off first if
    /// it is on. Empty if the monitor already shows it.
    pub fn show_steps(&mut self, input: u16) -> Vec<Step> {
        let current = self.sync_main_input();
        if self.pbp() != Some(true) {
            return if current == Some(input) {
                Vec::new()
            } else {
                vec![Step::Input(input)]
            };
        }
        let mut steps = vec![Step::Layout(lg::PBP_OFF)];
        if self.main_input != Some(input) {
            steps.push(Step::Input(input));
        }
        steps
    }

    /// Switches to `input` full screen on request, regardless of the KVM
    /// position, waiting between steps.
    pub fn show(&mut self, input: u16) -> Result<(), String> {
        let steps = self.show_steps(input);
        let result = self.run_steps(&steps);
        self.record(
            &result,
            &format!("switch to {} (manual)", input_name(input)),
        );
        result
    }

    /// The input shown; in split screen, the left (or main) picture.
    pub fn main_input(&self) -> Option<u16> {
        self.main_input
    }

    /// Reads which input the monitor shows, when it can say, and remembers
    /// it.
    pub fn sync_main_input(&mut self) -> Option<u16> {
        let current = self
            .monitor
            .get_vcp(lg::LG_ACTIVE_INPUT_VCP)
            .ok()
            .and_then(lg::active_input);
        if current.is_some() {
            self.main_input = current;
        }
        current
    }

    /// Whether split screen is on, if the monitor answers.
    pub fn pbp(&mut self) -> Option<bool> {
        self.monitor
            .get_vcp(lg::LG_PBP_VCP)
            .ok()
            .map(|layout| layout != lg::PBP_OFF)
    }

    /// Steps that put `left` and `right` on the two pictures of an active
    /// split. Selecting an input moves it to the left (main) picture and the
    /// previous one to the right, so select the right-hand input first.
    fn side_steps(&self, left: u16, right: u16) -> Vec<Step> {
        let mut steps = Vec::new();
        if self.main_input != Some(right) {
            steps.push(Step::Input(right));
        }
        steps.push(Step::Input(left));
        steps
    }

    /// Steps that turn side-by-side split screen on with the configured
    /// sides, or off, showing the KVM's position full screen.
    pub fn pbp_steps(&mut self, on: bool) -> Vec<Step> {
        self.sync_main_input();
        let mut steps = Vec::new();
        if on {
            let s = &self.settings;
            if self.main_input != Some(s.pbp_right) {
                steps.push(Step::Input(s.pbp_right));
            }
            steps.push(Step::Layout(lg::PBP_SIDE_BY_SIDE));
            steps.push(Step::Input(s.pbp_left));
        } else {
            steps.push(Step::Layout(lg::PBP_OFF));
            if let Some(here) = self.kvm_here {
                let target = self.target(here);
                if self.main_input != Some(target) {
                    steps.push(Step::Input(target));
                }
            }
        }
        steps
    }

    /// Swaps the two pictures by selecting the right-hand input, and keeps
    /// the new order for next time. Returns the step and the new left input.
    pub fn swap_pbp(&mut self) -> (Step, u16) {
        self.sync_main_input();
        let s = &mut self.settings;
        let left = self.main_input.unwrap_or(s.pbp_left);
        let right = if left == s.pbp_left {
            s.pbp_right
        } else {
            s.pbp_left
        };
        (s.pbp_left, s.pbp_right) = (right, left);
        (Step::Input(right), right)
    }

    /// Changes the split-screen sides. Returns the steps that apply the
    /// change if split screen is on.
    pub fn set_split(&mut self, left: u16, right: u16) -> Vec<Step> {
        let old = (self.settings.pbp_left, self.settings.pbp_right);
        self.settings.pbp_left = left;
        self.settings.pbp_right = right;
        if (left, right) == old || self.pbp() != Some(true) {
            return Vec::new();
        }
        self.sync_main_input();
        self.side_steps(left, right)
    }

    /// Runs one step and updates what the controller knows about the monitor.
    pub fn run_step(&mut self, step: Step) -> Result<(), String> {
        let target = self.kvm_here.map(|here| (here, self.target(here)));
        match step {
            Step::Input(input) => {
                self.monitor.set_input(input)?;
                self.main_input = Some(input);
                // Completes turning split screen off (see below).
                if let Some((here, target)) = target {
                    if self.acted_on.is_none() && input == target {
                        self.acted_on = Some(here);
                    }
                }
            }
            Step::Layout(layout) => {
                self.monitor.set_vcp(lg::LG_PBP_VCP, layout)?;
                if layout == lg::PBP_OFF {
                    // Full screen again. The KVM position is handled once its
                    // input shows; until then, `update` retries it.
                    if let Some((here, target)) = target {
                        self.started = true;
                        self.acted_on = (self.main_input == Some(target)).then_some(here);
                    }
                }
            }
        }
        Ok(())
    }

    /// Runs steps [`SETTLE`] apart, blocking.
    fn run_steps(&mut self, steps: &[Step]) -> Result<(), String> {
        for (i, &step) in steps.iter().enumerate() {
            if i > 0 {
                sleep(self.settle);
            }
            self.run_step(step)?;
        }
        Ok(())
    }

    /// Turns side-by-side Picture-by-Picture on or off, blocking until done.
    pub fn set_pbp(&mut self, on: bool) -> Result<(), String> {
        let steps = self.pbp_steps(on);
        let result = self.run_steps(&steps);
        self.record(&result, &self.pbp_description(on));
        result
    }

    /// Describes a split-screen change, for logs.
    pub fn pbp_description(&self, on: bool) -> String {
        let s = &self.settings;
        if on {
            format!(
                "turn on side by side ({} left, {} right)",
                input_name(s.pbp_left),
                input_name(s.pbp_right)
            )
        } else {
            "turn off split screen".to_string()
        }
    }

    /// Reads brightness, contrast, volume, mute, split screen, and the
    /// shown input.
    pub fn levels(&mut self) -> Levels {
        self.sync_main_input();
        let mut read = |vcp| self.monitor.get_vcp(vcp).ok();
        let layout = read(lg::LG_PBP_VCP);
        Levels {
            brightness: read(lg::VCP_BRIGHTNESS),
            contrast: read(lg::VCP_CONTRAST),
            volume: read(lg::VCP_VOLUME),
            muted: read(lg::VCP_MUTE).map(|v| v == lg::MUTE_ON),
            pbp: layout.map(|v| v != lg::PBP_OFF),
            layout,
        }
    }

    /// Sets a standard VCP control such as [`lg::VCP_BRIGHTNESS`]. Only
    /// failures are logged, since sliders send many values.
    pub fn set_level(&mut self, vcp: u8, value: u16) -> Result<(), String> {
        let result = self.monitor.set_vcp(vcp, value);
        match &result {
            Ok(()) => self.last_error = None,
            Err(e) => {
                log!("Failed to set {} to {value}: {e}", control_name(vcp));
                self.last_error = Some(e.clone());
            }
        }
        result
    }

    /// Logs the outcome of a user action and tracks the last error.
    pub fn record(&mut self, result: &Result<(), String>, action: &str) {
        match result {
            Ok(()) => {
                self.last_error = None;
                log!("Did {action}");
            }
            Err(e) => {
                log!("Failed to {action}: {e}");
                self.last_error = Some(e.clone());
            }
        }
    }
}

/// Name of a VCP control, for logs.
pub fn control_name(vcp: u8) -> String {
    match vcp {
        lg::VCP_BRIGHTNESS => "brightness".into(),
        lg::VCP_CONTRAST => "contrast".into(),
        lg::VCP_VOLUME => "volume".into(),
        lg::VCP_MUTE => "mute".into(),
        _ => format!("VCP 0x{vcp:02X}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    type Shared<T> = Rc<RefCell<T>>;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Op {
        Input(u16),
        Vcp(u8, u16),
    }

    /// A monitor that records what it is sent and remembers VCP values.
    #[derive(Default)]
    struct Fake {
        ops: Shared<Vec<Op>>,
        values: Shared<HashMap<u8, u16>>,
        fail: Shared<bool>,
    }

    impl Monitor for Fake {
        fn set_input(&mut self, input: u16) -> Result<(), String> {
            if *self.fail.borrow() {
                return Err("bus error".into());
            }
            self.ops.borrow_mut().push(Op::Input(input));
            // A monitor that reports its input (VCP 0xF5) now reports this one.
            let mut values = self.values.borrow_mut();
            if let Some(active) = values.get_mut(&lg::LG_ACTIVE_INPUT_VCP) {
                *active = if input == 0xD0 { 1 } else { 2 };
            }
            Ok(())
        }

        fn set_vcp(&mut self, vcp: u8, value: u16) -> Result<(), String> {
            if *self.fail.borrow() {
                return Err("bus error".into());
            }
            self.ops.borrow_mut().push(Op::Vcp(vcp, value));
            self.values.borrow_mut().insert(vcp, value);
            Ok(())
        }

        fn get_vcp(&mut self, vcp: u8) -> Result<u16, String> {
            self.values
                .borrow()
                .get(&vcp)
                .copied()
                .ok_or_else(|| "no reply".to_string())
        }
    }

    struct Rig {
        controller: Controller,
        ops: Shared<Vec<Op>>,
        values: Shared<HashMap<u8, u16>>,
        fail: Shared<bool>,
    }

    impl Rig {
        fn inputs(&self) -> Vec<u16> {
            self.ops
                .borrow()
                .iter()
                .filter_map(|op| match op {
                    Op::Input(input) => Some(*input),
                    Op::Vcp(..) => None,
                })
                .collect()
        }
    }

    fn rig() -> Rig {
        let config = Config {
            watch_devices: vec!["FC32:1287".into()],
            ..Config::default()
        };
        let fake = Fake::default();
        let (ops, values, fail) = (fake.ops.clone(), fake.values.clone(), fake.fail.clone());
        let mut controller = Controller::new(config.settings().unwrap(), Box::new(fake));
        controller.settle = Duration::ZERO;
        Rig {
            controller,
            ops,
            values,
            fail,
        }
    }

    #[test]
    fn leaves_monitor_alone_at_startup_then_follows_the_kvm() {
        let mut r = rig();
        let c = &mut r.controller;
        assert_eq!(c.update(true), Outcome::Started);
        assert_eq!(c.update(true), Outcome::Unchanged);
        assert_eq!(c.update(false), Outcome::Switched(0xD1));
        assert_eq!(c.update(false), Outcome::Unchanged);
        assert_eq!(c.update(true), Outcome::Switched(0xD0));
        assert_eq!(r.inputs(), vec![0xD1, 0xD0]);
    }

    #[test]
    fn retries_after_a_failed_switch() {
        let mut r = rig();
        r.controller.update(true);
        *r.fail.borrow_mut() = true;
        assert!(matches!(r.controller.update(false), Outcome::Failed(_)));
        assert!(r.controller.last_error.is_some());
        *r.fail.borrow_mut() = false;
        assert_eq!(r.controller.update(false), Outcome::Switched(0xD1));
        assert_eq!(r.controller.last_error, None);
        assert_eq!(r.inputs(), vec![0xD1]);
    }

    #[test]
    fn pausing_defers_the_switch_until_resumed() {
        let mut r = rig();
        r.controller.update(true);
        r.controller.auto = false;
        assert_eq!(r.controller.update(false), Outcome::Paused);
        r.controller.auto = true;
        assert_eq!(r.controller.update(false), Outcome::Switched(0xD1));
        assert_eq!(r.inputs(), vec![0xD1]);
    }

    #[test]
    fn pbp_puts_the_remote_input_left_and_leaves_inputs_alone() {
        let mut r = rig();
        r.controller.update(true);
        r.controller.set_pbp(true).unwrap();
        // DisplayPort is already the main input, so PBP turns on and USB-C is
        // selected, which moves it left and DisplayPort right.
        assert_eq!(
            *r.ops.borrow(),
            vec![
                Op::Vcp(lg::LG_PBP_VCP, lg::PBP_SIDE_BY_SIDE),
                Op::Input(0xD1)
            ]
        );
        assert_eq!(r.controller.pbp(), Some(true));
        assert_eq!(r.controller.update(false), Outcome::PictureByPicture);
        assert_eq!(r.controller.update(true), Outcome::Unchanged);
        assert_eq!(r.inputs(), vec![0xD1]);
    }

    #[test]
    fn pbp_starts_from_the_right_input_when_the_left_one_is_shown() {
        let mut r = rig();
        r.controller.update(false);
        r.controller.set_pbp(true).unwrap();
        assert_eq!(r.inputs(), vec![0xD0, 0xD1]);
    }

    #[test]
    fn turning_pbp_off_shows_the_kvm_position_full_screen() {
        let mut r = rig();
        r.controller.update(true);
        r.controller.set_pbp(true).unwrap();
        // The KVM moves while PBP is on; nothing switches until PBP is off.
        assert_eq!(r.controller.update(false), Outcome::PictureByPicture);
        r.ops.borrow_mut().clear();
        r.controller.set_pbp(false).unwrap();
        assert_eq!(*r.ops.borrow(), vec![Op::Vcp(lg::LG_PBP_VCP, lg::PBP_OFF)]);
        assert_eq!(r.controller.update(false), Outcome::Unchanged);

        r.controller.update(true);
        assert_eq!(r.inputs(), vec![0xD0]);
    }

    #[test]
    fn turning_pbp_off_switches_back_when_the_kvm_is_here() {
        let mut r = rig();
        r.controller.update(true);
        r.controller.set_pbp(true).unwrap();
        r.ops.borrow_mut().clear();
        r.controller.set_pbp(false).unwrap();
        assert_eq!(
            *r.ops.borrow(),
            vec![Op::Vcp(lg::LG_PBP_VCP, lg::PBP_OFF), Op::Input(0xD0)]
        );
    }

    #[test]
    fn reads_the_shown_input_to_fix_it_at_startup_and_skip_repeats() {
        let mut r = rig();
        // The monitor shows USB-C although the KVM is here: fix it at startup.
        r.values.borrow_mut().insert(lg::LG_ACTIVE_INPUT_VCP, 2);
        assert_eq!(r.controller.update(true), Outcome::Switched(0xD0));
        assert_eq!(r.controller.update(false), Outcome::Switched(0xD1));
        // Someone picks DisplayPort on the monitor before the KVM comes back.
        r.values.borrow_mut().insert(lg::LG_ACTIVE_INPUT_VCP, 1);
        assert_eq!(r.controller.update(true), Outcome::AlreadyShown(0xD0));
        assert_eq!(r.inputs(), vec![0xD0, 0xD1]);
        assert!(r.controller.show_steps(0xD0).is_empty());
    }

    #[test]
    fn changes_split_sides_while_on() {
        let mut r = rig();
        r.controller.update(true);
        r.controller.set_pbp(true).unwrap();
        r.ops.borrow_mut().clear();
        // USB-C is on the left; put DisplayPort there.
        let steps = r.controller.set_split(0xD0, 0xD1);
        assert_eq!(steps, vec![Step::Input(0xD0)]);
        assert!(r.controller.set_split(0xD0, 0xD1).is_empty());

        r.controller.set_pbp(false).unwrap();
        assert!(r.controller.set_split(0xD1, 0xD0).is_empty());
        // Off shows DisplayPort (the KVM is here), which is already the right side.
        assert_eq!(
            r.controller.pbp_steps(true),
            vec![Step::Layout(lg::PBP_SIDE_BY_SIDE), Step::Input(0xD1)]
        );
    }

    #[test]
    fn treats_pip_from_the_monitor_menu_as_split_screen() {
        let mut r = rig();
        r.controller.update(true);
        r.values.borrow_mut().insert(lg::LG_PBP_VCP, lg::PBP_PIP);
        assert_eq!(r.controller.update(false), Outcome::PictureByPicture);
        assert_eq!(r.controller.levels().layout, Some(lg::PBP_PIP));
        assert!(r.inputs().is_empty());
    }

    #[test]
    fn applies_a_deferred_switch_once_split_screen_ends() {
        let mut r = rig();
        r.controller.update(true);
        r.values
            .borrow_mut()
            .insert(lg::LG_PBP_VCP, lg::PBP_SIDE_BY_SIDE);
        assert_eq!(r.controller.update(false), Outcome::PictureByPicture);
        assert_eq!(r.controller.update(false), Outcome::PictureByPicture);
        // Split screen is turned off from the monitor's menu.
        r.values.borrow_mut().insert(lg::LG_PBP_VCP, lg::PBP_OFF);
        assert_eq!(r.controller.update(false), Outcome::Switched(0xD1));
    }

    #[test]
    fn retries_when_turning_pbp_off_fails_to_select_the_input() {
        let mut r = rig();
        r.controller.update(true);
        r.controller.set_pbp(true).unwrap();
        // USB-C is on the left, so turning PBP off must then select DisplayPort.
        let steps = r.controller.pbp_steps(false);
        assert_eq!(steps, vec![Step::Layout(lg::PBP_OFF), Step::Input(0xD0)]);
        r.controller.run_step(steps[0]).unwrap();
        *r.fail.borrow_mut() = true;
        assert!(r.controller.run_step(steps[1]).is_err());
        *r.fail.borrow_mut() = false;
        assert_eq!(r.controller.update(true), Outcome::Switched(0xD0));
    }

    #[test]
    fn clears_the_error_when_the_input_turns_out_to_be_shown() {
        let mut r = rig();
        r.values.borrow_mut().insert(lg::LG_ACTIVE_INPUT_VCP, 1);
        r.controller.update(true);
        *r.fail.borrow_mut() = true;
        assert!(matches!(r.controller.update(false), Outcome::Failed(_)));
        *r.fail.borrow_mut() = false;
        // The switch landed anyway, or someone picked USB-C on the monitor.
        r.values.borrow_mut().insert(lg::LG_ACTIVE_INPUT_VCP, 2);
        assert_eq!(r.controller.update(false), Outcome::AlreadyShown(0xD1));
        assert_eq!(r.controller.last_error, None);
    }

    #[test]
    fn swapping_selects_the_right_input_and_remembers_it() {
        let mut r = rig();
        r.controller.update(true);
        r.controller.set_pbp(true).unwrap();
        assert_eq!(r.controller.swap_pbp(), (Step::Input(0xD0), 0xD0));
        r.controller.run_step(Step::Input(0xD0)).unwrap();
        assert_eq!(r.controller.settings().pbp_left, 0xD0);
        assert_eq!(r.controller.swap_pbp(), (Step::Input(0xD1), 0xD1));
    }

    #[test]
    fn showing_an_input_turns_pbp_off_first() {
        let mut r = rig();
        r.controller.update(true);
        r.controller.set_pbp(true).unwrap();
        r.ops.borrow_mut().clear();
        // USB-C is already on the left, so turning PBP off shows it.
        r.controller.show(0xD1).unwrap();
        assert_eq!(*r.ops.borrow(), vec![Op::Vcp(lg::LG_PBP_VCP, lg::PBP_OFF)]);

        r.controller.set_pbp(true).unwrap();
        r.ops.borrow_mut().clear();
        r.controller.show(0xD0).unwrap();
        assert_eq!(
            *r.ops.borrow(),
            vec![Op::Vcp(lg::LG_PBP_VCP, lg::PBP_OFF), Op::Input(0xD0)]
        );
        assert_eq!(r.controller.main_input(), Some(0xD0));
    }

    #[test]
    fn reads_levels_and_reports_missing_ones() {
        let mut r = rig();
        r.values.borrow_mut().extend([
            (lg::VCP_BRIGHTNESS, 58),
            (lg::VCP_MUTE, lg::MUTE_ON),
            (lg::LG_PBP_VCP, lg::PBP_OFF),
        ]);
        let levels = r.controller.levels();
        assert_eq!(levels.brightness, Some(58));
        assert_eq!(levels.contrast, None);
        assert_eq!(levels.muted, Some(true));
        assert_eq!(levels.pbp, Some(false));
    }

    #[test]
    fn detects_watched_devices() {
        let r = rig();
        let keyboard = UsbId {
            vid: 0xFC32,
            pid: 0x1287,
        };
        let hub = UsbId {
            vid: 0x05E3,
            pid: 0x0610,
        };
        assert!(r.controller.is_here(&BTreeSet::from([hub, keyboard])));
        assert!(!r.controller.is_here(&BTreeSet::from([hub])));
    }
}
