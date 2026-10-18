//! Access to feature 0x8100 (onboard profiles), HIDPP_PROTOCOL.md section 3.
//!
//! Reads use GetInfo, GetMode, GetActiveProfile and Read. State changes:
//! - SetActiveProfile (`set_active_profile`): immediate, no flash write.
//! - Sector writes (`write_profile_dpi`): StartWrite / WriteData / EndWrite, following
//!   the rules in HIDPP_PROTOCOL.md section 8 (whole sector, valid CRC, backup first,
//!   read-back verify, never ROM sectors).

use std::fs;
use std::path::PathBuf;

use crate::hidpp::{self, Hidpp, Node};
use crate::profile::{self, DirEntry, Profile};

pub const FEATURE_ONBOARD_PROFILES: u16 = 0x8100;

const F_GET_INFO: u8 = 0;
const F_GET_MODE: u8 = 2;
const F_SET_ACTIVE: u8 = 3;
const F_GET_ACTIVE: u8 = 4;
const F_READ: u8 = 5;
const F_START_WRITE: u8 = 6;
const F_WRITE_DATA: u8 = 7;
const F_END_WRITE: u8 = 8;

/// Sectors at or above this are ROM and must never be written.
const ROM_SECTOR_START: u16 = 0x100;

/// GetInfo (function 0) reply.
#[derive(Debug, Clone)]
pub struct Info {
    pub raw: Vec<u8>,
    pub memory_model: u8,
    pub profile_format: u8,
    pub macro_format: u8,
    pub profile_count: u8,
    pub rom_profile_count: u8,
    pub button_count: u8,
    pub sector_count: u8,
    pub sector_size: u16,
}

#[derive(Debug, Clone)]
pub struct ProfileSector {
    pub entry: DirEntry,
    pub raw: Vec<u8>,
    pub crc_ok: bool,
    /// Decoded fields; None if the CRC is bad or the format is not 1-4.
    pub profile: Option<Profile>,
}

/// Everything the GUI shows, gathered in one pass over the device.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub node: Node,
    pub device_index: u8,
    pub protocol: (u8, u8),
    pub feature_index: u8,
    pub info: Info,
    pub mode: u8,
    pub active_sector: u16,
    pub directory_crc_ok: bool,
    pub profiles: Vec<ProfileSector>,
}

impl Snapshot {
    pub fn mode_name(&self) -> &'static str {
        match self.mode {
            1 => "Onboard",
            2 => "Host",
            _ => "Unknown",
        }
    }
}

/// Read a whole sector in 16-byte chunks; the last chunk is realigned to end at `size`.
pub fn read_sector(h: &mut Hidpp, idx: u8, sector: u16, size: u16) -> hidpp::Result<Vec<u8>> {
    let size = size as usize;
    let mut buf = vec![0u8; size];
    let mut off = 0usize;
    while off < size {
        if size - off < 16 {
            off = size.saturating_sub(16);
        }
        let mut params = [0u8; 5];
        params[..2].copy_from_slice(&sector.to_be_bytes());
        params[2..4].copy_from_slice(&(off as u16).to_be_bytes());
        params[4] = 0x10;
        let chunk = h.request(idx, F_READ, &params)?;
        let n = 16.min(size - off);
        buf[off..off + n].copy_from_slice(&chunk[..n]);
        off += 16;
    }
    Ok(buf)
}

fn read_info(h: &mut Hidpp, idx: u8) -> hidpp::Result<Info> {
    let r = h.request(idx, F_GET_INFO, &[])?;
    Ok(Info {
        memory_model: r[0],
        profile_format: r[1],
        macro_format: r[2],
        profile_count: r[3],
        rom_profile_count: r[4],
        button_count: r[5],
        sector_count: r[6],
        sector_size: u16::from_be_bytes([r[7], r[8]]),
        raw: r,
    })
}

fn err(what: &str) -> impl Fn(hidpp::Error) -> String {
    let what = what.to_string();
    move |e| format!("{what}: {e}")
}

fn get_active(h: &mut Hidpp, idx: u8) -> Result<u16, String> {
    let r = h.request(idx, F_GET_ACTIVE, &[]).map_err(err("GetActiveProfile"))?;
    Ok(u16::from_be_bytes([r[0], r[1]]))
}

/// Probe the device and look up the 0x8100 feature index.
fn open() -> Result<(Hidpp, Node, (u8, u8), u8), String> {
    let (mut h, node, protocol) = Hidpp::probe(&hidpp::find_nodes())?;
    let idx = h.feature_index(FEATURE_ONBOARD_PROFILES).map_err(err("Feature lookup"))?;
    if idx == 0 {
        return Err("This device does not support onboard profiles (feature 0x8100)".into());
    }
    Ok((h, node, protocol, idx))
}

#[derive(Debug, Clone)]
pub enum SwitchOutcome {
    Switched { from: u16, to: u16 },
    /// Nothing was sent; `report` is the SetActiveProfile report that would have been.
    DryRun { to: u16, report: Vec<u8> },
}

/// Make `sector` the active profile, after re-checking the device's current state:
/// the profile must be in the directory, enabled, and hold a valid CRC.
pub fn set_active_profile(sector: u16, dry_run: bool) -> Result<SwitchOutcome, String> {
    let (mut h, _, _, idx) = open()?;
    let info = read_info(&mut h, idx).map_err(err("GetInfo"))?;
    let directory = read_sector(&mut h, idx, 0x0000, info.sector_size).map_err(err("Read directory"))?;
    let entry = profile::parse_directory(&directory)
        .into_iter()
        .find(|e| e.sector == sector)
        .ok_or_else(|| format!("Profile {sector} is not in the profile directory"))?;
    if !entry.enabled() {
        return Err(format!("Profile {sector} is disabled; enable it first"));
    }
    let data = read_sector(&mut h, idx, sector, info.sector_size)
        .map_err(err(&format!("Read profile {sector}")))?;
    if !profile::crc_ok(&data) {
        return Err(format!("Profile {sector} has no valid data (CRC mismatch); refusing to switch"));
    }

    let from = get_active(&mut h, idx)?;
    let params = sector.to_be_bytes();
    if dry_run {
        return Ok(SwitchOutcome::DryRun { to: sector, report: h.encode(idx, F_SET_ACTIVE, &params) });
    }
    h.request(idx, F_SET_ACTIVE, &params).map_err(err("SetActiveProfile"))?;
    let now = get_active(&mut h, idx)?;
    if now != sector {
        return Err(format!("The mouse reports profile {now} as active, not {sector}"));
    }
    Ok(SwitchOutcome::Switched { from, to: now })
}

/// StartWrite(sector, 0, len) -> WriteData x N (16 bytes, last padded 0xFF) -> EndWrite.
fn write_sector(h: &mut Hidpp, idx: u8, sector: u16, data: &[u8]) -> Result<(), String> {
    assert!(sector < ROM_SECTOR_START, "refusing to write ROM sector");
    let mut params = [0u8; 6];
    params[..2].copy_from_slice(&sector.to_be_bytes());
    params[4..].copy_from_slice(&(data.len() as u16).to_be_bytes());
    h.request(idx, F_START_WRITE, &params).map_err(err("StartWrite"))?;
    for chunk in data.chunks(16) {
        let mut buf = [0xFFu8; 16];
        buf[..chunk.len()].copy_from_slice(chunk);
        h.request(idx, F_WRITE_DATA, &buf).map_err(err("WriteData"))?;
    }
    h.request(idx, F_END_WRITE, &[]).map_err(err("EndWrite"))?;
    Ok(())
}

/// Number of requests a sector write takes: StartWrite, WriteData per 16 bytes, EndWrite.
fn write_request_count(len: usize) -> usize {
    len.div_ceil(16) + 2
}

/// `$XDG_STATE_HOME/omm-gui/backups`, falling back to `~/.local/state/omm-gui/backups`.
pub fn backup_dir() -> PathBuf {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."));
    state.join("omm-gui/backups")
}

/// Local time as YYYYmmdd-HHMMSS, the same format hidpp_read_profile.py uses.
fn timestamp() -> String {
    // SAFETY: time() and localtime_r() only write to the provided tm struct.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec
        )
    }
}

fn save_backup(sector: u16, data: &[u8]) -> Result<PathBuf, String> {
    let dir = backup_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("Create {}: {e}", dir.display()))?;
    let path = dir.join(format!("profile_sector{sector:02x}_backup_{}.bin", timestamp()));
    fs::write(&path, data).map_err(|e| format!("Write backup {}: {e}", path.display()))?;
    Ok(path)
}

#[derive(Debug, Clone)]
pub struct DpiChange {
    pub slot: usize,
    pub old: u16,
    pub new: u16,
}

#[derive(Debug, Clone)]
pub enum WriteOutcome {
    NoChange,
    Written { sector: u16, changes: Vec<DpiChange>, backup: PathBuf, reactivated: bool },
    /// Nothing was sent or saved; `requests` is how many write requests it would take.
    DryRun { sector: u16, changes: Vec<DpiChange>, new_crc: u16, requests: usize },
}

/// Replace the DPI table of profile `sector`. The sector is re-read from the device,
/// so edits are applied to its current contents, not to what the GUI showed.
/// Slots that are unused on the device must stay unused.
pub fn write_profile_dpi(sector: u16, dpi: [u16; 5], dry_run: bool) -> Result<WriteOutcome, String> {
    if sector == 0 || sector >= ROM_SECTOR_START {
        return Err(format!("Sector {sector:#06x} is not a writable profile sector"));
    }
    let (mut h, _, _, idx) = open()?;
    let info = read_info(&mut h, idx).map_err(err("GetInfo"))?;
    if !profile::is_supported_format(info.profile_format) {
        return Err(format!("Editing only supports profile formats 1-4 (this device: {})", info.profile_format));
    }
    let directory = read_sector(&mut h, idx, 0x0000, info.sector_size).map_err(err("Read directory"))?;
    if !profile::parse_directory(&directory).iter().any(|e| e.sector == sector) {
        return Err(format!("Profile {sector} is not in the profile directory"));
    }
    let old = read_sector(&mut h, idx, sector, info.sector_size)
        .map_err(err(&format!("Read profile {sector}")))?;
    if !profile::crc_ok(&old) {
        return Err(format!("Refusing to write: profile {sector}'s current CRC doesn't match"));
    }

    let current = Profile::decode(&old, 0).ok_or("Sector is too short for the profile layout")?.dpi;
    let mut changes = Vec::new();
    for slot in 0..5 {
        let (was, new) = (current[slot], dpi[slot]);
        if was == new {
            continue;
        }
        if !profile::dpi_used(was) {
            return Err(format!("DPI slot {slot} is unused on the device; refusing to fill it"));
        }
        if !(profile::DPI_MIN..=profile::DPI_MAX).contains(&new) {
            return Err(format!("DPI must be between {} and {}", profile::DPI_MIN, profile::DPI_MAX));
        }
        changes.push(DpiChange { slot, old: was, new });
    }
    if changes.is_empty() {
        return Ok(WriteOutcome::NoChange);
    }
    let new = profile::with_dpi(&old, &dpi);

    if dry_run {
        return Ok(WriteOutcome::DryRun {
            sector,
            changes,
            new_crc: profile::stored_crc(&new),
            requests: write_request_count(new.len()),
        });
    }
    let backup = save_backup(sector, &old)?;
    write_sector(&mut h, idx, sector, &new)?;
    let readback = read_sector(&mut h, idx, sector, info.sector_size).map_err(err("Read-back"))?;
    if readback != new {
        return Err(format!(
            "Read-back of profile {sector} does not match what was written! Backup: {}",
            backup.display()
        ));
    }
    // Re-select the active profile so the mouse reloads it from memory
    let reactivated = get_active(&mut h, idx)? == sector;
    if reactivated {
        h.request(idx, F_SET_ACTIVE, &sector.to_be_bytes()).map_err(err("SetActiveProfile"))?;
    }
    Ok(WriteOutcome::Written { sector, changes, backup, reactivated })
}

/// Find the mouse, then read info, mode, active profile, the directory and every
/// profile listed in it.
pub fn read_snapshot() -> Result<Snapshot, String> {
    let (mut h, node, protocol, idx) = open()?;
    let info = read_info(&mut h, idx).map_err(err("GetInfo"))?;
    if info.sector_size < 16 {
        return Err(format!("Unexpected sector size {}", info.sector_size));
    }
    let mode = h.request(idx, F_GET_MODE, &[]).map_err(err("GetMode"))?[0];
    let active_sector = get_active(&mut h, idx)?;

    let directory = read_sector(&mut h, idx, 0x0000, info.sector_size).map_err(err("Read directory"))?;
    let directory_crc_ok = profile::crc_ok(&directory);

    let mut profiles = Vec::new();
    for entry in profile::parse_directory(&directory) {
        let raw = read_sector(&mut h, idx, entry.sector, info.sector_size)
            .map_err(err(&format!("Read profile {}", entry.sector)))?;
        let crc_ok = profile::crc_ok(&raw);
        let decoded = (crc_ok && profile::is_supported_format(info.profile_format))
            .then(|| Profile::decode(&raw, info.button_count as usize))
            .flatten();
        profiles.push(ProfileSector { entry, raw, crc_ok, profile: decoded });
    }

    Ok(Snapshot {
        node,
        device_index: h.dev,
        protocol,
        feature_index: idx,
        info,
        mode,
        active_sector,
        directory_crc_ok,
        profiles,
    })
}

#[cfg(test)]
mod tests {
    /// Reads the real mouse. Run with `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn read_device() {
        let s = super::read_snapshot().expect("read failed");
        println!("{} ({}) index {} HID++ {}.{}", s.node.name, s.node.path.display(),
                 s.device_index, s.protocol.0, s.protocol.1);
        println!("{:?}", s.info);
        println!("mode {} active {}", s.mode, s.active_sector);
        for p in &s.profiles {
            println!("{:?} crc_ok={} {:?}", p.entry, p.crc_ok, p.profile);
        }
    }

    /// Dry run only: validates against the real mouse but sends no SetActiveProfile.
    #[test]
    #[ignore]
    fn switch_dry_run() {
        for sector in [1, 2, 3, 99] {
            println!("{sector}: {:?}", super::set_active_profile(sector, true));
        }
    }

    /// Dry run only: validates DPI edits against the real mouse but writes nothing.
    #[test]
    #[ignore]
    fn dpi_dry_run() {
        let cases: [(u16, [u16; 5]); 5] = [
            (1, [3200, 0, 0, 0, 0]),       // change the one used slot
            (1, [6400, 0, 0, 0, 0]),       // no change
            (1, [6400, 800, 0, 0, 0]),     // unused slot -> refused
            (1, [30000, 0, 0, 0, 0]),      // out of range -> refused
            (3, [500, 800, 1600, 3200, 25600]),
        ];
        for (sector, dpi) in cases {
            println!("{sector} {dpi:?}: {:?}", super::write_profile_dpi(sector, dpi, true));
        }
    }
}
