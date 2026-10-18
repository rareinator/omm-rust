//! Pure decoding of onboard-memory sectors (HIDPP_PROTOCOL.md sections 3.4-5).

/// CRC-16/CCITT-FALSE, as in FUN_1800e7d10.
pub fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

pub fn stored_crc(sector: &[u8]) -> u16 {
    u16::from_be_bytes([sector[sector.len() - 2], sector[sector.len() - 1]])
}

pub fn crc_ok(sector: &[u8]) -> bool {
    sector.len() >= 2 && crc16_ccitt(&sector[..sector.len() - 2]) == stored_crc(sector)
}

/// One profile directory entry (sector 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirEntry {
    pub sector: u16,
    pub flags: u8,
    pub extra: u8,
}

impl DirEntry {
    pub fn enabled(&self) -> bool {
        self.flags & 1 != 0
    }
}

/// Directory sector: 4-byte entries [sector BE16, flags, extra] until 0xFFFF (FUN_1800da3e0).
pub fn parse_directory(d: &[u8]) -> Vec<DirEntry> {
    d[..d.len().saturating_sub(2)]
        .chunks_exact(4)
        .map(|c| DirEntry { sector: u16::from_be_bytes([c[0], c[1]]), flags: c[2], extra: c[3] })
        .take_while(|e| e.sector != 0xFFFF)
        .collect()
}

/// Rebuild the directory the way FUN_1800da3e0 does: 0xFF fill, entries, 0xFFFF, CRC.
pub fn build_directory(entries: &[DirEntry], size: usize) -> Result<Vec<u8>, String> {
    if entries.len() * 4 + 2 > size.saturating_sub(2) {
        return Err("Directory exceeds the sector size".into());
    }
    if !entries.iter().any(DirEntry::enabled) {
        return Err("The directory must have at least one enabled profile".into());
    }
    let mut buf = vec![0xFFu8; size];
    for (i, e) in entries.iter().enumerate() {
        buf[i * 4..i * 4 + 2].copy_from_slice(&e.sector.to_be_bytes());
        buf[i * 4 + 2] = e.flags;
        buf[i * 4 + 3] = e.extra;
    }
    let n = entries.len() * 4;
    buf[n..n + 2].copy_from_slice(&[0xFF, 0xFF]);
    let crc = crc16_ccitt(&buf[..size - 2]);
    buf[size - 2..].copy_from_slice(&crc.to_be_bytes());
    Ok(buf)
}

/// `entry` with the enabled bit set or cleared. On the Superlight enabled entries carry
/// extra 0x00 and disabled ones 0xFF, so that byte is flipped along with the bit.
pub fn with_enabled(entry: DirEntry, enabled: bool) -> DirEntry {
    match (enabled, entry.extra) {
        (true, 0xFF) => DirEntry { flags: entry.flags | 1, extra: 0x00, ..entry },
        (true, extra) => DirEntry { flags: entry.flags | 1, extra, ..entry },
        (false, 0x00) => DirEntry { flags: entry.flags & !1, extra: 0xFF, ..entry },
        (false, extra) => DirEntry { flags: entry.flags & !1, extra, ..entry },
    }
}

pub fn is_supported_format(format: u8) -> bool {
    (1..=4).contains(&format)
}

/// Fields of profile formats 1-4 (FUN_1800ead80 / FUN_1800eaf80).
#[derive(Debug, Clone)]
pub struct Profile {
    pub report_rate: u8,
    pub default_dpi_slot: u8,
    pub shift_dpi_slot: u8,
    pub dpi: [u16; 5],
    pub led_rgb: [u8; 3],
    pub power_mode: u8,
    pub angle_snapping: u8,
    pub name: Option<String>,
    pub buttons: Vec<[u8; 4]>,
    pub gshift_buttons: Vec<[u8; 4]>,
}

const BUTTON_TABLE: usize = 0x20;
const GSHIFT_TABLE: usize = 0x60;
const NAME_OFFSET: usize = 0xA0;
const NAME_LEN: usize = 0x30;
const MAX_BUTTONS: usize = 16;

impl Profile {
    /// Decode a format 1-4 sector. Returns None if the sector is too short for the layout.
    pub fn decode(s: &[u8], button_count: usize) -> Option<Self> {
        if s.len() < NAME_OFFSET + NAME_LEN + 2 {
            return None;
        }
        let count = button_count.min(MAX_BUTTONS);
        let table = |base: usize| -> Vec<[u8; 4]> {
            (0..count)
                .map(|i| s[base + i * 4..base + i * 4 + 4].try_into().unwrap())
                .collect()
        };
        Some(Profile {
            report_rate: s[0],
            default_dpi_slot: s[1],
            shift_dpi_slot: s[2],
            dpi: std::array::from_fn(|i| u16::from_le_bytes([s[3 + i * 2], s[4 + i * 2]])),
            led_rgb: [s[0x0D], s[0x0E], s[0x0F]],
            power_mode: s[0x10],
            angle_snapping: s[0x11],
            name: decode_name(&s[NAME_OFFSET..NAME_OFFSET + NAME_LEN]),
            buttons: table(BUTTON_TABLE),
            gshift_buttons: table(GSHIFT_TABLE),
        })
    }

    /// Approximate polling rate, assuming the byte is 1000/Hz (unverified beyond 0x01).
    pub fn report_rate_hz(&self) -> Option<u32> {
        (1..=8).contains(&self.report_rate).then(|| 1000 / self.report_rate as u32)
    }
}

/// Profile name, assumed UTF-16LE; all 0xFF means unnamed.
fn decode_name(raw: &[u8]) -> Option<String> {
    let units = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0);
    let text: String = char::decode_utf16(units).collect::<Result<_, _>>().ok()?;
    let text = text.trim_matches('\u{FFFF}');
    (!text.is_empty()).then(|| text.to_string())
}

pub fn dpi_used(v: u16) -> bool {
    v != 0 && v != 0xFFFF
}

/// G Pro X Superlight (HERO 25K) sensor range, as used by hidpp_read_profile.py.
pub const DPI_MIN: u16 = 100;
pub const DPI_MAX: u16 = 25600;
pub const DPI_STEP: u16 = 50;

/// Value written for an unused slot (what the factory profiles use).
pub const DPI_UNUSED: u16 = 0;

/// Whether `slot` can be switched off: it must not be the default or shift slot.
pub fn dpi_slot_locked(slot: usize, default_slot: u8, shift_slot: u8) -> bool {
    slot == default_slot as usize || slot == shift_slot as usize
}

/// Rules for a DPI table we write: used values in range, used slots contiguous from
/// slot 0 (so DPI cycling never hits a gap), and the default/shift slots used.
pub fn validate_dpi_table(dpi: &[u16; 5], default_slot: u8, shift_slot: u8) -> Result<(), String> {
    for (slot, &v) in dpi.iter().enumerate() {
        if dpi_used(v) && !(DPI_MIN..=DPI_MAX).contains(&v) {
            return Err(format!("DPI slot {slot}: must be between {DPI_MIN} and {DPI_MAX}"));
        }
    }
    let used = dpi.iter().take_while(|&&v| dpi_used(v)).count();
    if dpi[used..].iter().any(|&v| dpi_used(v)) {
        return Err("Used DPI slots must be contiguous, starting at slot 0".into());
    }
    for (what, slot) in [("default", default_slot), ("shift", shift_slot)] {
        if (slot as usize) < dpi.len() && !dpi_used(dpi[slot as usize]) {
            return Err(format!("Slot {slot} is the {what} DPI slot and can't be unused"));
        }
    }
    Ok(())
}

pub fn dpi_text(v: u16) -> String {
    if dpi_used(v) { v.to_string() } else { "unused".into() }
}

/// Copy of a format 1-4 sector with the DPI table replaced and a fresh CRC.
pub fn with_dpi(s: &[u8], dpi: &[u16; 5]) -> Vec<u8> {
    let mut buf = s.to_vec();
    for (slot, v) in dpi.iter().enumerate() {
        buf[3 + slot * 2..5 + slot * 2].copy_from_slice(&v.to_le_bytes());
    }
    let n = buf.len();
    let crc = crc16_ccitt(&buf[..n - 2]);
    buf[n - 2..].copy_from_slice(&crc.to_be_bytes());
    buf
}

// Function-execution ids (0x90 xx), from the switch in FUN_18006c010
const BUTTON_FUNCTIONS: [&str; 18] = [
    "No action", "Tilt left", "Tilt right", "Select next DPI", "Select previous DPI",
    "Cycle DPIs", "Default DPI", "DPI shift", "Select next profile", "Select previous profile",
    "Cycle profiles", "G-Shift", "Show battery life indicator", "Enable profile at index X",
    "Toggle performance / office mode", "Host button", "Scroll down", "Scroll up",
];
const MODIFIERS: [&str; 8] = ["LCtrl", "LShift", "LAlt", "LGUI", "RCtrl", "RShift", "RAlt", "RGUI"];
const MOUSE_BUTTONS: [&str; 5] = ["Left", "Right", "Middle", "Back", "Forward"];

/// Slot names for the Superlight default layout (inferred, not stored in the profile).
pub fn slot_name(slot: usize) -> String {
    MOUSE_BUTTONS.get(slot).map(|s| s.to_string()).unwrap_or_else(|| format!("Button {}", slot + 1))
}

/// Name for a HID keyboard usage (page 0x07).
pub fn hid_key_name(code: u8) -> String {
    match code {
        0x04..=0x1D => ((b'A' + code - 0x04) as char).to_string(),
        0x1E..=0x26 => (code - 0x1D).to_string(),
        0x3A..=0x45 => format!("F{}", code - 0x39),
        0x68..=0x73 => format!("F{}", code - 0x68 + 13),
        0x27 => "0".into(),
        0x28 => "Enter".into(),
        0x29 => "Esc".into(),
        0x2A => "Backspace".into(),
        0x2B => "Tab".into(),
        0x2C => "Space".into(),
        0x2D => "-".into(),
        0x2E => "=".into(),
        0x2F => "[".into(),
        0x30 => "]".into(),
        0x31 => "\\".into(),
        0x33 => ";".into(),
        0x34 => "'".into(),
        0x35 => "`".into(),
        0x36 => ",".into(),
        0x37 => ".".into(),
        0x38 => "/".into(),
        0x39 => "CapsLock".into(),
        0x46 => "PrintScreen".into(),
        0x47 => "ScrollLock".into(),
        0x48 => "Pause".into(),
        0x49 => "Insert".into(),
        0x4A => "Home".into(),
        0x4B => "PageUp".into(),
        0x4C => "Delete".into(),
        0x4D => "End".into(),
        0x4E => "PageDown".into(),
        0x4F => "Right".into(),
        0x50 => "Left".into(),
        0x51 => "Down".into(),
        0x52 => "Up".into(),
        _ => format!("key {code:#04x}"),
    }
}

/// HID consumer usages offered for buttons (same set as hidpp_read_profile.py).
pub const CONSUMER_KEYS: [(u16, &str); 7] = [
    (0x00E9, "Volume up"),
    (0x00EA, "Volume down"),
    (0x00E2, "Mute"),
    (0x00CD, "Play/Pause"),
    (0x00B5, "Next track"),
    (0x00B6, "Previous track"),
    (0x00B7, "Stop"),
];

/// Keyboard usages that have a name, in HID order.
pub fn named_keys() -> Vec<(u8, String)> {
    (0x04..=0x73).map(|c| (c, hid_key_name(c))).filter(|(_, n)| !n.starts_with("key ")).collect()
}

/// Built-in functions (0x90 xx) offered for buttons. 0x0D (switch to profile index)
/// is left out: what the index refers to is not verified.
pub fn button_functions() -> Vec<(u8, &'static str)> {
    BUTTON_FUNCTIONS.iter().enumerate().filter(|(i, _)| *i != 0x0D).map(|(i, n)| (i as u8, *n)).collect()
}

pub const MODIFIER_NAMES: [&str; 4] = ["Ctrl", "Shift", "Alt", "Super"];

/// A button action the editor can build. Anything else (macros, combined mouse
/// buttons, right-hand modifiers, ...) is `Other` and kept byte for byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Default,
    Mouse(usize),
    /// Left-hand modifier bits (Ctrl 1, Shift 2, Alt 4, Super 8) and a HID usage.
    Key { mods: u8, key: u8 },
    Consumer(u16),
    Function(u8),
    Other([u8; 4]),
}

impl Action {
    pub fn decode(e: &[u8; 4]) -> Action {
        if e[0] == 0xFF || (e[0] == 0 && e[1] == 0) {
            return Action::Default;
        }
        match (e[0], e[1]) {
            (0x80, 1) => {
                let mask = u16::from_be_bytes([e[2], e[3]]);
                match (mask.count_ones(), mask.trailing_zeros() as usize) {
                    (1, bit) if bit < MOUSE_BUTTONS.len() => Action::Mouse(bit),
                    _ => Action::Other(*e),
                }
            }
            (0x80, 2) if e[2] & 0xF0 == 0 && e[3] != 0 => Action::Key { mods: e[2], key: e[3] },
            (0x80, 3) => Action::Consumer(u16::from_be_bytes([e[2], e[3]])),
            (0x90, id) if id != 0x0D && (id as usize) < BUTTON_FUNCTIONS.len() => Action::Function(id),
            _ => Action::Other(*e),
        }
    }

    /// The 4-byte entry, encoded like hidpp_read_profile.py's parse_action.
    pub fn encode(&self) -> [u8; 4] {
        match *self {
            Action::Default => [0xFF; 4],
            Action::Mouse(bit) => {
                let m = (1u16 << bit).to_be_bytes();
                [0x80, 0x01, m[0], m[1]]
            }
            Action::Key { mods, key } => [0x80, 0x02, mods, key],
            Action::Consumer(code) => {
                let c = code.to_be_bytes();
                [0x80, 0x03, c[0], c[1]]
            }
            Action::Function(id) => [0x90, id, 0x00, 0x00],
            Action::Other(e) => e,
        }
    }
}

/// Whether the main button table still has a left click: an explicit left-button
/// mapping on any button, or slot 0 (left on the Superlight) left at its default.
pub fn keeps_left_click(buttons: &[[u8; 4]]) -> bool {
    buttons.iter().enumerate().any(|(slot, e)| match Action::decode(e) {
        Action::Default => slot == 0,
        Action::Mouse(0) => true,
        Action::Other([0x80, 0x01, hi, lo]) => u16::from_be_bytes([hi, lo]) & 1 != 0,
        _ => false,
    })
}

/// One line per changed entry between two (buttons, G-Shift) table pairs.
pub fn button_changes(before: [&[[u8; 4]]; 2], after: [&[[u8; 4]]; 2]) -> Vec<String> {
    let mut changes = Vec::new();
    for (layer, name) in ["Button", "G-Shift"].iter().enumerate() {
        for (slot, (b, a)) in before[layer].iter().zip(after[layer]).enumerate() {
            if b != a {
                changes.push(format!("{name} {slot} ({}): {} → {}", slot_name(slot), decode_button(b), decode_button(a)));
            }
        }
    }
    changes
}

/// Copy of a format 1-4 sector with both button tables replaced and a fresh CRC.
pub fn with_buttons(s: &[u8], buttons: &[[u8; 4]], gshift: &[[u8; 4]]) -> Vec<u8> {
    let mut buf = s.to_vec();
    for (base, table) in [(BUTTON_TABLE, buttons), (GSHIFT_TABLE, gshift)] {
        for (slot, e) in table.iter().enumerate().take(MAX_BUTTONS) {
            buf[base + slot * 4..base + slot * 4 + 4].copy_from_slice(e);
        }
    }
    let n = buf.len();
    let crc = crc16_ccitt(&buf[..n - 2]);
    buf[n - 2..].copy_from_slice(&crc.to_be_bytes());
    buf
}

/// Decode one 4-byte button entry, as parsed by FUN_18006c740.
pub fn decode_button(e: &[u8; 4]) -> String {
    if e[0] == 0xFF || (e[0] == 0 && e[1] == 0) {
        return "Default (not remapped)".into();
    }
    match e[0] & 0xF0 {
        kind @ (0x00 | 0x10) => {
            let sector = u16::from_be_bytes([e[0], e[1]]) & 0x0FFF;
            let addr = u16::from_be_bytes([e[2], e[3]]);
            let verb = if kind == 0x00 { "Execute macro" } else { "Stop macro" };
            format!("{verb} at sector {sector:#05x} offset {addr:#06x}")
        }
        0x20 => "Stop all macros".into(),
        0x80 => match e[1] {
            1 => {
                let mask = u16::from_be_bytes([e[2], e[3]]);
                let names: Vec<String> =
                    (0..16).filter(|i| mask & (1 << i) != 0).map(|i| slot_name(i).replace(' ', "")).collect();
                let names = if names.is_empty() { "none".into() } else { names.join("+") };
                format!("Mouse button: {names}")
            }
            2 => {
                let mut parts: Vec<String> =
                    (0..8).filter(|i| e[2] & (1 << i) != 0).map(|i| MODIFIERS[i].to_string()).collect();
                if e[3] != 0 {
                    parts.push(hid_key_name(e[3]));
                }
                format!("Key: {}", parts.join("+"))
            }
            3 => {
                let code = u16::from_be_bytes([e[2], e[3]]);
                match CONSUMER_KEYS.iter().find(|(c, _)| *c == code) {
                    Some((_, name)) => format!("Media key: {name}"),
                    None => format!("Consumer key {code:#06x}"),
                }
            }
            sub => format!("Remap (not sent, opcode {sub})"),
        },
        0x90 => match e[1] {
            0x0D => format!("Enable profile at index {}", e[2]),
            id => BUTTON_FUNCTIONS
                .get(id as usize)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("Function {id:#04x}")),
        },
        _ => format!("Unknown ({})", hex(e)),
    }
}

pub fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_check_value() {
        assert_eq!(crc16_ccitt(b"123456789"), 0x29B1);
    }

    /// Superlight profile 1 as shipped (HIDPP_PROTOCOL.md section 4.1).
    fn superlight_profile() -> Vec<u8> {
        let mut s = vec![0xFFu8; 255];
        let head: [u8; 0x40] = [
            0x01, 0x00, 0x00, 0x00, 0x19, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff,
            0xff, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0x80, 0x01, 0x00, 0x01, 0x80, 0x01, 0x00, 0x02, 0x80, 0x01, 0x00, 0x04, 0x80, 0x01, 0x00, 0x08,
            0x80, 0x01, 0x00, 0x10, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ];
        s[..0x40].copy_from_slice(&head);
        let crc = crc16_ccitt(&s[..253]);
        s[253..].copy_from_slice(&crc.to_be_bytes());
        s
    }

    #[test]
    fn decodes_superlight_profile() {
        let s = superlight_profile();
        assert!(crc_ok(&s));
        let p = Profile::decode(&s, 5).unwrap();
        assert_eq!(p.report_rate_hz(), Some(1000));
        assert_eq!(p.dpi, [6400, 0, 0, 0, 0]);
        assert_eq!(p.name, None);
        assert_eq!(decode_button(&p.buttons[0]), "Mouse button: Left");
        assert_eq!(decode_button(&p.buttons[4]), "Mouse button: Forward");
        assert_eq!(decode_button(&p.gshift_buttons[0]), "Default (not remapped)");
    }

    #[test]
    fn with_dpi_only_touches_table_and_crc() {
        let s = superlight_profile();
        let new = with_dpi(&s, &[3200, 0, 0, 0, 0]);
        assert!(crc_ok(&new));
        assert_eq!(&new[3..5], &3200u16.to_le_bytes());
        let changed: Vec<usize> = (0..s.len()).filter(|&i| s[i] != new[i]).collect();
        assert!(changed.iter().all(|&i| (3..13).contains(&i) || i >= s.len() - 2), "{changed:?}");
        assert_eq!(with_dpi(&s, &[6400, 0, 0, 0, 0]), s);
    }

    #[test]
    fn actions_round_trip() {
        for e in [[0x80, 0x01, 0x00, 0x08], [0x80, 0x02, 0x05, 0x06], [0x80, 0x03, 0x00, 0xE9], [0x90, 0x05, 0, 0]] {
            assert!(!matches!(Action::decode(&e), Action::Other(_)), "{e:?}");
            assert_eq!(Action::decode(&e).encode(), e);
        }
        assert_eq!(Action::decode(&[0xFF; 4]), Action::Default);
        // Combined buttons, right-hand modifiers and macros are kept as they are
        for e in [[0x80, 0x01, 0x00, 0x03], [0x80, 0x02, 0x10, 0x06], [0x01, 0x02, 0x00, 0x10]] {
            assert_eq!(Action::decode(&e), Action::Other(e));
        }
        assert_eq!(Action::Key { mods: 1, key: 0x06 }.encode(), [0x80, 0x02, 0x01, 0x06]);
        assert!(named_keys().iter().any(|(c, n)| *c == 0x68 && n == "F13"));
    }

    #[test]
    fn button_edits() {
        let s = superlight_profile();
        let p = Profile::decode(&s, 5).unwrap();
        assert!(keeps_left_click(&p.buttons));
        let mut b = p.buttons.clone();
        b[0] = Action::Function(0x03).encode();
        assert!(!keeps_left_click(&b));
        b[3] = Action::Mouse(0).encode();
        assert!(keeps_left_click(&b));

        let mut g = p.gshift_buttons.clone();
        g[4] = Action::Consumer(0xE9).encode();
        let new = with_buttons(&s, &b, &g);
        assert!(crc_ok(&new));
        let q = Profile::decode(&new, 5).unwrap();
        assert_eq!((q.buttons, q.gshift_buttons, q.dpi), (b, g, p.dpi));
        assert_eq!(with_buttons(&s, &p.buttons, &p.gshift_buttons), s);
    }

    #[test]
    fn dpi_table_rules() {
        assert!(validate_dpi_table(&[6400, 0, 0, 0, 0], 0, 0).is_ok());
        assert!(validate_dpi_table(&[400, 800, 1600, 0, 0], 1, 0).is_ok());
        assert!(validate_dpi_table(&[400, 800, 0xFFFF, 0, 0], 1, 0).is_ok());
        assert!(validate_dpi_table(&[400, 0, 1600, 0, 0], 0, 0).is_err()); // gap
        assert!(validate_dpi_table(&[400, 0, 0, 0, 0], 1, 0).is_err()); // default unused
        assert!(validate_dpi_table(&[0, 0, 0, 0, 0], 0, 0).is_err());
        assert!(validate_dpi_table(&[30000, 0, 0, 0, 0], 0, 0).is_err());
    }

    #[test]
    fn parses_shipped_directory() {
        let mut d = vec![0xFFu8; 255];
        d[..20].copy_from_slice(&[
            0, 1, 1, 0, 0, 2, 0, 0xff, 0, 3, 0, 0xff, 0, 4, 0, 0xff, 0, 5, 0, 0xff,
        ]);
        let e = parse_directory(&d);
        assert_eq!(e.len(), 5);
        assert!(e[0].enabled() && !e[1].enabled());
        assert_eq!(e[4].sector, 5);

        // Rebuilding reproduces the shipped directory (section 3.6), CRC included
        let crc = crc16_ccitt(&d[..253]);
        d[253..].copy_from_slice(&crc.to_be_bytes());
        assert_eq!(build_directory(&e, 255).unwrap(), d);

        let mut enabled = e.clone();
        enabled[2] = with_enabled(enabled[2], true);
        assert_eq!(enabled[2], DirEntry { sector: 3, flags: 1, extra: 0 });
        assert!(crc_ok(&build_directory(&enabled, 255).unwrap()));
        // Disabling it again gives back the shipped directory
        enabled[2] = with_enabled(enabled[2], false);
        assert_eq!(build_directory(&enabled, 255).unwrap(), d);
        let none: Vec<DirEntry> = e.iter().map(|x| DirEntry { flags: 0, ..*x }).collect();
        assert!(build_directory(&none, 255).is_err());
    }

    #[test]
    fn decodes_button_kinds() {
        assert_eq!(decode_button(&[0x80, 0x02, 0x01, 0x06]), "Key: LCtrl+C");
        assert_eq!(decode_button(&[0x80, 0x03, 0x00, 0xE9]), "Media key: Volume up");
        assert_eq!(decode_button(&[0x80, 0x03, 0x01, 0x23]), "Consumer key 0x0123");
        assert_eq!(decode_button(&[0x90, 0x0B, 0, 0]), "G-Shift");
        assert_eq!(decode_button(&[0x90, 0x0D, 2, 0]), "Enable profile at index 2");
    }
}
