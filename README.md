# omm-gui

A native Linux GUI for editing the **onboard memory** of Logitech HID++ 2.0 mice:
DPI, button mappings and profiles. Settings live on the mouse, so they still apply
on other machines and without any software running.

It's written in Rust with GTK4 and libadwaita, and talks to the mouse directly over
`hidraw`. It doesn't need G Hub, Solaar or a daemon.

> **Status: early and tested on a single mouse.** Every write is backed up and
> verified, but this tool writes to your mouse's flash memory. Read
> [Safety](#safety) first.

## Features

- **Read** device info, the profile directory, and every profile: DPI table,
  report rate, button and G-Shift tables, and a raw hex dump.
- **Switch the active profile.** This takes effect immediately and doesn't write flash.
- **Edit DPI slots** with sliders. Turn slots on or off, 100–25600 DPI in steps of 50.
- **Remap buttons:** mouse buttons, keyboard keys with modifiers (including
  F13–F24), media keys, and built-in functions such as DPI cycle and G-Shift, on
  both the normal and the G-Shift layer.
- **Copy one profile over another**, optionally enabling the target.
- **Enable or disable profiles.**
- **Dry-run mode** (`--dry-run`) checks every change against the real mouse and
  shows exactly what would be sent, but sends nothing.

## Supported devices

| Device | Connection | Status |
|---|---|---|
| G Pro X Superlight (1st gen, PID `0x4093`) | Lightspeed receiver (`0xC547`) | Tested |

Other mice that support HID++ feature `0x8100` (onboard profiles) with
**profile format 1–4** will probably work. The app reads the format from the
device and only allows editing for formats it can decode. Formats 5 and up
(newer mice) can only be viewed for now.

A few things are specific to the Superlight and may differ on other mice: the
DPI range, the button names (left, right, middle, back, forward), and the
meaning of the directory's extra byte. Reports from other mice are welcome.

## Requirements

- Linux with `hidraw` (any modern kernel)
- GTK **4.14+** and libadwaita **1.5+**
- Rust (edition 2024, so Rust 1.85 or newer)

Build dependencies:

```sh
# Arch
sudo pacman -S rust gtk4 libadwaita

# Debian 13 / Ubuntu 24.04+
sudo apt install cargo libgtk-4-dev libadwaita-1-dev build-essential pkg-config

# Fedora
sudo dnf install cargo gtk4-devel libadwaita-devel
```

## Building and running

```sh
git clone <this repo>
cd omm-gui
cargo run --release                # normal mode
cargo run --release -- --dry-run   # validate changes, send nothing
```

The binary ends up at `target/release/omm-gui`.

### Permissions

On most systemd desktops the logged-in user already gets access to Logitech
`hidraw` nodes (`crw-rw----+` with an ACL), so **no root is needed**. If the app
says "Permission denied", add a udev rule:

```sh
echo 'KERNEL=="hidraw*", ATTRS{idVendor}=="046d", TAG+="uaccess"' |
  sudo tee /etc/udev/rules.d/70-logitech-hidraw.rules
sudo udevadm control --reload && sudo udevadm trigger
```

Then unplug and replug the receiver.

## Usage

- **Sidebar:** a *Device* page and one page per profile. Narrow windows, such as
  tiled ones, collapse to a single column.
- **Make Active** switches the mouse to that profile.
- **DPI** and **Buttons** changes stay in the window until you click **Apply**
  and confirm. **Revert** discards them.
- **Copy From…** replaces the whole profile with another one.
- **Enable / Disable** changes whether the mouse can cycle to that profile.
- Use the refresh button in the sidebar header to read the mouse again.

If the mouse is asleep, move it and press **Retry**.

## Safety

Flash writes follow a fixed set of rules:

1. **Read, modify, write whole sectors.** The sector is re-read from the mouse
   just before each write, and the CRC is recomputed.
2. **Refuse to write** a profile whose current CRC is bad.
3. **Back up every sector before writing it**, and verify it by reading it back.
4. **Never write ROM sectors** (`0x100` and up).
5. **Directory:** at least one profile stays enabled, the active profile can't be
   disabled, and only profiles with valid data can be enabled. The directory is
   only rewritten if the app can reproduce the current one byte for byte.
6. **Buttons:** at least one button must keep left click.
7. **One request at a time:** no device traffic overlaps a write.

Flash has limited write cycles, which is why nothing is written while you drag a
slider.

### Backups

Before each write, the old sector is saved to:

```
~/.local/state/omm-gui/backups/profile_sectorNN_backup_YYYYmmdd-HHMMSS.bin
```

`$XDG_STATE_HOME` is respected. `NN` is the sector number in hex (`00` is the
profile directory). Restoring a backup from the GUI is not implemented yet.

## Testing

```sh
cargo test                                  # unit tests, no device needed
cargo test -- --ignored --nocapture         # read the real mouse + dry-run every write path
```

The `--ignored` tests talk to the connected mouse. They only read from it, and
every write path runs in dry-run mode, so nothing is changed.

## Project layout

| File | Purpose |
|---|---|
| `src/hidpp.rs` | `hidraw` discovery and HID++ 2.0 request/reply framing |
| `src/onboard.rs` | Feature `0x8100`: reading, and every write path with its safety checks |
| `src/profile.rs` | Pure decoding and encoding: CRC, directory, profile layout, button actions |
| `src/ui.rs` | Main window, profile pages, DPI and button editors |
| `src/button_editor.rs` | The "change button action" dialog |

## Known limitations

- Only profile formats 1–4 can be edited.
- Macros can't be created or edited. Existing macro bindings are kept as they are.
- Only the mouse-button mappings are confirmed on hardware. Keyboard keys, media
  keys and built-in functions follow Logitech's own software but are untested on
  the mouse.
- Key names follow US key positions. The mouse sends key positions, so on other
  keyboard layouts the label may not match the character typed.
- Not editable yet: the default and shift DPI slot, report rate, profile name,
  and lighting.

## How it works

The protocol was worked out from Logitech Onboard Memory Manager 2.7 for
Windows, then tested on real hardware. In short:

- HID++ 2.0 short (`0x10`) and long (`0x11`) reports are sent on the receiver's
  or the mouse's vendor `hidraw` interface.
- Feature `0x8100` provides GetInfo, Get/SetActiveProfile, Read, and
  StartWrite/WriteData/EndWrite.
- Each sector ends with a big-endian CRC-16/CCITT-FALSE.

## Disclaimer

This is an unofficial project. It is not affiliated with or endorsed by
Logitech. "Logitech", "G Pro" and "Lightspeed" are trademarks of Logitech. Use
at your own risk.
