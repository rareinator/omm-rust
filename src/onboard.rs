//! Read-only access to feature 0x8100 (onboard profiles), HIDPP_PROTOCOL.md section 3.
//!
//! Only GetInfo, GetMode, GetActiveProfile and Read are sent from here. No function
//! that changes device state (SetActive, StartWrite, WriteData, EndWrite) is used.

use crate::hidpp::{self, Hidpp, Node};
use crate::profile::{self, DirEntry, Profile};

pub const FEATURE_ONBOARD_PROFILES: u16 = 0x8100;

const F_GET_INFO: u8 = 0;
const F_GET_MODE: u8 = 2;
const F_GET_ACTIVE: u8 = 4;
const F_READ: u8 = 5;

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

/// Find the mouse, then read info, mode, active profile, the directory and every
/// profile listed in it.
pub fn read_snapshot() -> Result<Snapshot, String> {
    let (mut h, node, protocol) = Hidpp::probe(&hidpp::find_nodes())?;
    let err = |what: &str| {
        let what = what.to_string();
        move |e: hidpp::Error| format!("{what}: {e}")
    };

    let idx = h.feature_index(FEATURE_ONBOARD_PROFILES).map_err(err("Feature lookup"))?;
    if idx == 0 {
        return Err("This device does not support onboard profiles (feature 0x8100)".into());
    }
    let info = read_info(&mut h, idx).map_err(err("GetInfo"))?;
    if info.sector_size < 16 {
        return Err(format!("Unexpected sector size {}", info.sector_size));
    }
    let mode = h.request(idx, F_GET_MODE, &[]).map_err(err("GetMode"))?[0];
    let active = h.request(idx, F_GET_ACTIVE, &[]).map_err(err("GetActiveProfile"))?;
    let active_sector = u16::from_be_bytes([active[0], active[1]]);

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
}
