use lg_kvm_switch::config::{self, Config, Settings};
use lg_kvm_switch::controller::{control_name, Controller};
use lg_kvm_switch::lg::{self, input_name};
use lg_kvm_switch::monitor;
use lg_kvm_switch::usb::{self, UsbId};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

const HELP: &str = "\
lg-kvm-switch: switch an LG monitor's input when a USB KVM moves your keyboard and mouse

USAGE:
    lg-kvm-switch [--config <path>] <command>

COMMANDS:
    run                  Watch the KVM and switch inputs (foreground)
    switch <input>       Switch now: dp, usbc, hdmi1, hdmi2, or a code like 0xD0
    pbp [on|off]         Show or set Picture-by-Picture (side by side)
    brightness [0-100]   Show or set brightness
    contrast [0-100]     Show or set contrast
    volume [0-100]       Show or set volume
    mute [on|off]        Show or set mute
    devices              List connected USB devices (* = watched)
    learn                Detect the KVM's devices by switching the KVM away
    monitors             List monitors the program can reach
    status               Show config, KVM position, and target monitor
    autostart on|off     Start the tray app at Windows logon (Windows only)

OPTIONS:
    -c, --config <path>   Config file (default: per-user config directory)
    -h, --help            Show this help
    -V, --version         Show the version
";

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut config_arg = None;
    let mut words = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-c" | "--config" => {
                config_arg = Some(PathBuf::from(args.next().ok_or("--config needs a path")?));
            }
            "-h" | "--help" => words.insert(0, "help".to_string()),
            "-V" | "--version" => words.insert(0, "version".to_string()),
            _ => words.push(arg),
        }
    }
    let path = config_arg.clone().unwrap_or_else(config::default_path);
    let command = words.first().map(String::as_str).unwrap_or("help");
    let arg = words.get(1).map(String::as_str);
    match command {
        "help" => print!("{HELP}"),
        "version" => println!("lg-kvm-switch {}", env!("CARGO_PKG_VERSION")),
        "run" => run_watcher(config_arg, &path)?,
        "switch" => switch(&path, arg.ok_or("usage: lg-kvm-switch switch <input>")?)?,
        "pbp" => pbp(&path, arg)?,
        "brightness" => level(&path, lg::VCP_BRIGHTNESS, arg)?,
        "contrast" => level(&path, lg::VCP_CONTRAST, arg)?,
        "volume" => level(&path, lg::VCP_VOLUME, arg)?,
        "mute" => mute(&path, arg)?,
        "devices" => devices(&path)?,
        "learn" => learn(&path)?,
        "monitors" => monitors(&path)?,
        "status" => status(&path)?,
        "autostart" => autostart(config_arg.as_deref(), arg)?,
        other => return Err(format!("unknown command '{other}' (see --help)")),
    }
    Ok(())
}

/// Settings for commands that work without a complete config.
fn settings(path: &Path) -> Result<Settings, String> {
    config::load_or_default(path)?.settings()
}

#[cfg(windows)]
fn run_watcher(config_arg: Option<PathBuf>, _path: &Path) -> Result<(), String> {
    lg_kvm_switch::windows::app::run(config_arg, false)
}

#[cfg(target_os = "linux")]
fn run_watcher(_config_arg: Option<PathBuf>, path: &Path) -> Result<(), String> {
    lg_kvm_switch::linux::daemon::run(path)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn run_watcher(_config_arg: Option<PathBuf>, _path: &Path) -> Result<(), String> {
    Err("only Windows and Linux are supported".into())
}

fn switch(path: &Path, input: &str) -> Result<(), String> {
    let input = lg::parse_input(input)?;
    monitor::open(&settings(path)?).set_input(input)?;
    println!("Switched to {}", input_name(input));
    Ok(())
}

/// A controller that knows the KVM position, for commands that depend on it.
fn controller(path: &Path) -> Result<Controller, String> {
    let settings = settings(path)?;
    let mut controller = Controller::new(settings.clone(), monitor::open(&settings));
    if !settings.watch.is_empty() {
        let here = controller.is_here(&usb::present_ids()?);
        controller.assume_position(here);
    }
    Ok(controller)
}

fn parse_on_off(arg: &str) -> Result<bool, String> {
    match arg.to_ascii_lowercase().as_str() {
        "on" | "true" | "1" => Ok(true),
        "off" | "false" | "0" => Ok(false),
        _ => Err(format!("expected on or off, got '{arg}'")),
    }
}

fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

fn pbp(path: &Path, arg: Option<&str>) -> Result<(), String> {
    let mut controller = controller(path)?;
    match arg {
        None => {
            let on = controller
                .pbp()
                .ok_or("the monitor did not report its layout")?;
            println!("Split screen: {}", on_off(on));
        }
        Some(arg) => {
            let on = parse_on_off(arg)?;
            controller.set_pbp(on)?;
            let s = controller.settings();
            if on {
                println!(
                    "Side by side on: {} left, {} right",
                    input_name(s.pbp_left),
                    input_name(s.pbp_right)
                );
            } else {
                println!("Split screen off");
            }
        }
    }
    Ok(())
}

fn level(path: &Path, vcp: u8, arg: Option<&str>) -> Result<(), String> {
    let mut monitor = monitor::open(&settings(path)?);
    let name = control_name(vcp);
    match arg {
        None => println!("{name}: {}", monitor.get_vcp(vcp)?),
        Some(arg) => {
            let value: u16 = arg
                .parse()
                .ok()
                .filter(|v| *v <= 100)
                .ok_or_else(|| format!("{name} must be 0-100, got '{arg}'"))?;
            monitor.set_vcp(vcp, value)?;
            println!("{name}: {value}");
        }
    }
    Ok(())
}

fn mute(path: &Path, arg: Option<&str>) -> Result<(), String> {
    let mut monitor = monitor::open(&settings(path)?);
    match arg {
        None => {
            let muted = monitor.get_vcp(lg::VCP_MUTE)? == lg::MUTE_ON;
            println!("mute: {}", on_off(muted));
        }
        Some(arg) => {
            let on = parse_on_off(arg)?;
            let value = if on { lg::MUTE_ON } else { lg::MUTE_OFF };
            monitor.set_vcp(lg::VCP_MUTE, value)?;
            println!("mute: {}", on_off(on));
        }
    }
    Ok(())
}

fn names(devices: &[usb::UsbDevice]) -> BTreeMap<UsbId, String> {
    let mut names = BTreeMap::new();
    for device in devices {
        let name = device.name.clone().unwrap_or_default();
        names
            .entry(device.id)
            .and_modify(|existing: &mut String| {
                if existing.is_empty() {
                    existing.clone_from(&name);
                }
            })
            .or_insert(name);
    }
    names
}

fn devices(path: &Path) -> Result<(), String> {
    let watched: BTreeSet<UsbId> = settings(path)?.watch.into_iter().collect();
    println!("  VID:PID    Name");
    for (id, name) in names(&usb::list_devices(true)?) {
        let mark = if watched.contains(&id) { '*' } else { ' ' };
        println!("{mark} {id}  {name}");
    }
    println!("\n* = in watch_devices ({})", path.display());
    Ok(())
}

fn learn(path: &Path) -> Result<(), String> {
    let before = usb::list_devices(true)?;
    let before_ids: BTreeSet<UsbId> = before.iter().map(|d| d.id).collect();
    println!("Learning which USB devices move with your KVM.\n");
    println!("  1. Switch the KVM to another computer.");
    println!("  2. Wait a few seconds; this finishes once the USB devices settle.");
    println!("  3. Switch the KVM back.\n");
    println!("Waiting for USB changes (up to 2 minutes)...");
    let after_ids = wait_for_change(&before_ids, Duration::from_secs(120))?;

    let gone: Vec<UsbId> = before_ids.difference(&after_ids).copied().collect();
    let came: Vec<UsbId> = after_ids.difference(&before_ids).copied().collect();
    let kvm = if gone.is_empty() { came } else { gone };
    if kvm.is_empty() {
        return Err(
            "the USB devices came back before learning finished; try again and \
                    wait a few seconds before switching back"
                .into(),
        );
    }
    let mut known = names(&before);
    known.extend(
        names(&usb::list_devices(true)?)
            .into_iter()
            .filter(|(_, n)| !n.is_empty()),
    );

    let mut config = config::load_or_default(path)?;
    config.watch_devices = kvm.iter().map(UsbId::to_string).collect();
    config::save(path, &config)?;
    println!("\nThese devices move with the KVM:");
    for id in &kvm {
        println!("  {id}  {}", known.get(id).map_or("", String::as_str));
    }
    println!("\nSaved to {}", path.display());
    println!("Switch the KVM back, then start the watcher (or reload its config).");
    Ok(())
}

/// Waits until the set of USB devices changes, then until it stays the same
/// for two seconds.
fn wait_for_change(before: &BTreeSet<UsbId>, timeout: Duration) -> Result<BTreeSet<UsbId>, String> {
    let start = Instant::now();
    loop {
        if start.elapsed() > timeout {
            return Err(
                "no USB changes seen; are the keyboard and mouse connected through the KVM?".into(),
            );
        }
        sleep(Duration::from_millis(250));
        let mut current = usb::present_ids()?;
        if &current == before {
            continue;
        }
        let mut stable_since = Instant::now();
        while stable_since.elapsed() < Duration::from_secs(2) {
            sleep(Duration::from_millis(250));
            let next = usb::present_ids()?;
            if next != current {
                current = next;
                stable_since = Instant::now();
            }
        }
        return Ok(current);
    }
}

fn monitors(path: &Path) -> Result<(), String> {
    let displays = monitor::list_displays(&settings(path)?)?;
    if displays.is_empty() {
        println!("No monitors found.");
    }
    for display in displays {
        let mark = if display.selected { '*' } else { ' ' };
        let edid = display
            .edid
            .map_or_else(|| "no EDID".to_string(), |e| e.describe());
        println!("{mark} {}  {edid}", display.location);
    }
    println!("\n* = controlled by lg-kvm-switch");
    Ok(())
}

fn status(path: &Path) -> Result<(), String> {
    println!("Config:    {}", path.display());
    let config = if path.exists() {
        config::load(path)?
    } else {
        println!("           (not created yet; run `lg-kvm-switch learn`)");
        Config::default()
    };
    let settings = config.settings()?;
    let present = usb::present_ids()?;
    let watched: Vec<String> = settings
        .watch
        .iter()
        .map(|id| {
            let state = if present.contains(id) {
                "connected"
            } else {
                "absent"
            };
            format!("{id} ({state})")
        })
        .collect();
    println!(
        "Watching:  {}",
        if watched.is_empty() {
            "nothing".to_string()
        } else {
            watched.join(", ")
        }
    );
    if !settings.watch.is_empty() {
        let here = settings.watch.iter().any(|id| present.contains(id));
        let (place, input) = if here {
            ("this computer", settings.local_input)
        } else {
            ("another computer", settings.remote_input)
        };
        println!("KVM:       on {place} -> {}", input_name(input));
    }
    match monitor::list_displays(&settings) {
        Ok(displays) => {
            let selected: Vec<String> = displays
                .into_iter()
                .filter(|d| d.selected)
                .map(|d| match d.edid {
                    Some(edid) => format!("{} on {}", edid.describe(), d.location),
                    None => d.location,
                })
                .collect();
            println!(
                "Monitor:   {}",
                if selected.is_empty() {
                    "none found".to_string()
                } else {
                    selected.join("; ")
                }
            );
            if !selected.is_empty() {
                let mut monitor = monitor::open(&settings);
                let shown = monitor
                    .get_vcp(lg::LG_ACTIVE_INPUT_VCP)
                    .ok()
                    .and_then(lg::active_input)
                    .map_or_else(|| "unknown".to_string(), input_name);
                let split = match monitor.get_vcp(lg::LG_PBP_VCP) {
                    Ok(lg::PBP_OFF) => " (full screen)".to_string(),
                    Ok(layout) => format!(" ({}, main picture)", lg::layout_name(layout)),
                    Err(_) => String::new(),
                };
                println!("Showing:   {shown}{split}");
            }
        }
        Err(e) => println!("Monitor:   {e}"),
    }
    #[cfg(windows)]
    println!(
        "Autostart: {}",
        lg_kvm_switch::windows::autostart::command().unwrap_or_else(|| "off".into())
    );
    Ok(())
}

#[cfg(windows)]
fn autostart(config_arg: Option<&Path>, arg: Option<&str>) -> Result<(), String> {
    use lg_kvm_switch::windows::autostart;
    match arg {
        Some("on") => {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let tray = exe.with_file_name("lg-kvm-switch-tray.exe");
            if !tray.exists() {
                return Err(format!("{} not found", tray.display()));
            }
            let config = config_arg
                .map(std::path::absolute)
                .transpose()
                .map_err(|e| e.to_string())?;
            autostart::enable(&tray, config.as_deref())?;
            println!("The tray app will start at logon: {}", tray.display());
        }
        Some("off") => {
            autostart::disable()?;
            println!("The tray app will no longer start at logon.");
        }
        None | Some("status") => {
            println!("{}", autostart::command().unwrap_or_else(|| "off".into()));
        }
        Some(other) => return Err(format!("expected on or off, got '{other}'")),
    }
    Ok(())
}

#[cfg(not(windows))]
fn autostart(_config_arg: Option<&Path>, _arg: Option<&str>) -> Result<(), String> {
    Err("autostart is Windows-only; on Linux enable the systemd user service (see README)".into())
}
