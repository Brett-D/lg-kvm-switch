# lg-kvm-switch

Automatically switch an LG monitor's input when a USB KVM moves your keyboard
and mouse between computers. No LG OnScreen Control required.

When the keyboard and mouse appear on this computer, the monitor switches to
this computer's input. When they disappear because the KVM moved them to
another computer, it switches to that computer's input. Only one computer
needs to run it.

It also replaces the parts of OnScreen Control you'd miss: Picture-by-Picture
(both computers side by side), brightness, contrast, and volume. On Windows
they're in a Windows 11-style flyout on the tray icon.

## Why LG monitors need this

LG monitors accept the standard DDC/CI input-select command (VCP `0x60`) and
then ignore it. They only switch when they get LG's own command, VCP `0xF4`,
sent from I2C source address `0x50` instead of the standard `0x51`.

```
0x50 0x84 0x03 0xF4 <value hi> <value lo> <checksum>
```

The Windows monitor API (`dxva2`) always sends from `0x51`, so tools built on
it can't switch these monitors. lg-kvm-switch writes the raw packet to the
monitor's I2C bus instead:

| Platform | How | GPUs |
| --- | --- | --- |
| Windows | NVIDIA NVAPI (`NvAPI_I2CWrite`) | NVIDIA |
| Linux | `/dev/i2c-*` (i2c-dev) | Any |

Input codes (LG 45GX950A-B, and most LG models):

| Name | Code |
| --- | --- |
| `dp` | `0xD0` |
| `usbc` | `0xD1` |
| `hdmi1` | `0x90` |
| `hdmi2` | `0x91` |

Some LG models use other codes. You can put any raw code in the config, such
as `0xE0`. The [monmux model catalog][monmux] and the
[ddcutil LG page][ddcutil-lg] list codes per model.

Other controls use the standard host address `0x51`:

| Control | VCP code | Values |
| --- | --- | --- |
| Picture-by-Picture (LG) | `0xD7` | `0x01` off, `0x05` side by side 50/50, `0x00` picture-in-picture (read only) |
| Shown input (LG) | `0xF5` | `1` DisplayPort, `2` USB-C (the left picture in a split) |
| Brightness, contrast, volume | `0x10`, `0x12`, `0x62` | 0–100 |
| Mute | `0x8D` | `0x01` muted, `0x02` unmuted |

With Picture-by-Picture on, selecting an input moves it to the left half and
the previous input to the right. The monitor ignores commands for a few
seconds after a layout change, so multi-step changes are spaced 3 s apart.

The standard input code (VCP `0x60`) always reports DisplayPort, so the shown
input is read from LG's `0xF5` instead. Only the DisplayPort and USB-C values
are verified.

Picture-in-picture can only be turned on from the monitor's own menu: the
monitor rejects `0xD7 = 0x00` over DDC/CI. (OnScreen Control's PIP option
doesn't use the monitor's PIP either; it arranges windows on your desktop.)
lg-kvm-switch still detects PIP and treats it like split screen.

## Install

Build with Rust 1.82 or newer. On Windows, use the MSVC toolchain
(`rustup toolchain install stable-x86_64-pc-windows-msvc`).

```sh
cargo build --release
```

This builds `lg-kvm-switch` (the command-line tool) and, on Windows,
`lg-kvm-switch-tray` (the notification-area app). Prebuilt binaries are
attached to each CI run.

## Set up

1. Find the USB devices that move with your KVM. The easiest way is to let the
   tool watch while you switch the KVM away and back:

   ```sh
   lg-kvm-switch learn
   ```

   Or list devices with `lg-kvm-switch devices` and add their IDs yourself.

2. Check the config. It is created at `%APPDATA%\lg-kvm-switch\config.toml` on
   Windows, or `~/.config/lg-kvm-switch/config.toml` on Linux:

   ```toml
   watch_devices = ["FC32:1287", "046D:C07D"]  # keyboard, mouse
   local_input = "dp"     # this computer
   remote_input = "usbc"  # the other computer
   pbp_left = "usbc"      # which KVM input is on the left in split screen (optional)
   pbp_right = "dp"       # and on the right (optional; Swap sides saves both)
   ```

3. Check that the monitor is reachable, then switch it once to confirm:

   ```sh
   lg-kvm-switch monitors
   lg-kvm-switch switch usbc
   lg-kvm-switch switch dp
   ```

### Windows

Run `lg-kvm-switch-tray.exe`. The icon's screen color shows the state:

- Blue: the KVM is on this PC.
- Orange: the KVM is on the other PC.
- Gray: automatic switching is paused.
- Red: there is a problem; hover over the icon for details.

Left-click the icon for the flyout. It follows your light or dark theme and
accent color:

- **This PC** / **Other PC**: show that computer full screen.
- **Side by side**: Picture-by-Picture with both KVM computers
  (`local_input` and `remote_input`), `pbp_left` on the left. Automatic
  switching waits while it's on, since both computers are visible.
- **Swap sides**: swap the halves, and remember the order for next time.
- **Auto switch**: pause or resume following the KVM.
- **Brightness**, **Contrast**, and **Volume** sliders; click the speaker to
  mute.

Keyboard: Tab or the arrow keys move between controls, Enter or Space
activates one, and the arrows, Page Up/Down, Home, and End adjust a slider.
Pressing Enter on the tray icon (Win+B) opens the menu.

Right-click the icon (or click the gear in the flyout) for the same actions
plus **Start with Windows**, editing or reloading the config, and the log
(`%LOCALAPPDATA%\lg-kvm-switch\lg-kvm-switch.log`).
`lg-kvm-switch autostart on` also enables start-at-logon.

You can uninstall LG OnScreen Control; lg-kvm-switch doesn't use it.

### Linux

1. Load i2c-dev and give your user access to the I2C buses:

   ```sh
   echo i2c-dev | sudo tee /etc/modules-load.d/i2c-dev.conf
   sudo modprobe i2c-dev
   sudo groupadd --system i2c 2>/dev/null
   echo 'KERNEL=="i2c-[0-9]*", GROUP="i2c", MODE="0660"' | sudo tee /etc/udev/rules.d/45-i2c.rules
   sudo usermod -aG i2c "$USER"   # then log out and back in
   ```

2. Install the binary and the systemd user service:

   ```sh
   install -Dm755 target/release/lg-kvm-switch ~/.local/bin/lg-kvm-switch
   install -Dm644 packaging/linux/lg-kvm-switch.service ~/.config/systemd/user/lg-kvm-switch.service
   systemctl --user enable --now lg-kvm-switch
   journalctl --user -u lg-kvm-switch -f
   ```

The monitor's bus is found by reading EDIDs. To pin it, set
`i2c_bus = "/dev/i2c-N"`. With the NVIDIA proprietary driver, some GPUs need
`options nvidia NVreg_RegistryDwords=RMUseSwI2c=0x01;RMI2cSpeed=100` in
`/etc/modprobe.d/` for reliable DDC.

## Behavior

- USB changes are debounced (`debounce_ms`, default 750 ms), so the burst of
  events from a KVM switch causes one input change.
- At startup the monitor is only switched if it reports showing the wrong
  input; monitors that can't report it are left alone. Re-selecting the input
  already shown makes the screen flicker, so the shown input is checked before
  every switch.
- While split screen (side by side, or PIP turned on from the monitor's menu)
  is on, KVM moves don't switch inputs. Turning it off shows the computer that
  has the KVM.
- After the PC wakes from sleep, the KVM position is checked again.
- A failed switch is retried every 5 seconds.

## Commands

```
lg-kvm-switch run                 Watch the KVM and switch inputs (foreground)
lg-kvm-switch switch <input>      Switch now: dp, usbc, hdmi1, hdmi2, or a code
lg-kvm-switch pbp [on|off]        Show or set Picture-by-Picture
lg-kvm-switch brightness [0-100]  Show or set brightness
lg-kvm-switch contrast [0-100]    Show or set contrast
lg-kvm-switch volume [0-100]      Show or set volume
lg-kvm-switch mute [on|off]       Show or set mute
lg-kvm-switch devices             List USB devices (* = watched)
lg-kvm-switch learn               Detect the KVM's devices
lg-kvm-switch monitors            List reachable monitors (* = controlled)
lg-kvm-switch status              Show config, KVM position, and the input shown
lg-kvm-switch autostart on|off    Start the tray app at Windows logon
```

All commands accept `--config <path>`.

## Troubleshooting

- **`nvapi64.dll not found`**: Windows support requires an NVIDIA GPU, with the
  monitor plugged into it. AMD (ADL) and Intel (IGCL) backends are possible
  future work; their APIs also allow raw I2C.
- **No monitor marked `*` in `monitors`**: check `monitor_vendor` (LG is
  `GSM`), and make sure DDC/CI is enabled in the monitor's OSD.
- **Switch reports success but nothing happens**: your model may use different
  input codes; try them with `lg-kvm-switch switch 0x..`.

## Credits

The LG source-address quirk is documented by the [ddcutil][ddcutil-lg] wiki,
[meer-cha/lg-input-switch][lg-input-switch], and the [monmux][monmux] catalog.
The Picture-by-Picture code comes from meer-cha/lg-input-switch.

## License

MIT

[ddcutil-lg]: https://github.com/rockowitz/ddcutil/wiki/Switching-input-source-on-LG-monitors
[lg-input-switch]: https://github.com/meer-cha/lg-input-switch
[monmux]: https://github.com/leinardi/monmux
