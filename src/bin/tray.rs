#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    use lg_kvm_switch::windows::app;
    let mut config = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--config" || arg == "-c" {
            config = args.next().map(std::path::PathBuf::from);
        }
    }
    if let Err(e) = app::run(config, true) {
        lg_kvm_switch::log!("Fatal: {e}");
        app::error_box(&e);
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("lg-kvm-switch-tray is Windows-only; on Linux run `lg-kvm-switch run`.");
    std::process::exit(1);
}
