//! HID++ 2.0 transport over Linux hidraw (HIDPP_PROTOCOL.md sections 2.1-2.4).

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const LOGITECH_VID: u16 = 0x046D;
/// Arbitrary non-zero software id; replies echo it, notifications don't.
const SW_ID: u8 = 0x0A;
const REPORT_SHORT: u8 = 0x10;
const REPORT_LONG: u8 = 0x11;
const SHORT_LEN: usize = 7;
const LONG_LEN: usize = 20;

/// The Logitech library uses 3000 ms per request.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);
/// Device indices to probe: receiver slots 1-6, then 0xFF for a direct device.
const PROBE_INDICES: [u8; 7] = [1, 2, 3, 4, 5, 6, 0xFF];

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Timeout { feature: u8, function: u8 },
    /// HID++ 2.0 error reply: [id, dev, 0xFF, feat_idx, fn_sw, code]
    Hidpp20 { feature: u8, function: u8, code: u8 },
    /// HID++ 1.0 error reply: [0x10, dev, 0x8F, sub_id, addr, code], usually "not reachable"
    Hidpp10 { device: u8, code: u8 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Timeout { feature, function } => {
                write!(f, "no reply to feature {feature:#04x} function {function}")
            }
            Error::Hidpp20 { feature, function, code } => {
                write!(f, "HID++ 2.0 error {code:#04x} (feature {feature:#04x} function {function})")
            }
            Error::Hidpp10 { device, code } => {
                write!(f, "HID++ 1.0 error {code:#04x} (device {device} not reachable?)")
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A Logitech hidraw interface that exposes HID++ long reports.
#[derive(Debug, Clone)]
pub struct Node {
    pub path: PathBuf,
    pub name: String,
}

/// Find hidraw nodes whose descriptor declares report 0x11 on a vendor usage page.
pub fn find_nodes() -> Vec<Node> {
    let mut nodes = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/hidraw") else {
        return nodes;
    };
    let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    for dir in dirs {
        let (Ok(uevent), Ok(desc)) = (
            fs::read_to_string(dir.join("device/uevent")),
            fs::read(dir.join("device/report_descriptor")),
        ) else {
            continue;
        };
        if !uevent.contains(&format!(":0000{LOGITECH_VID:04X}:")) {
            continue;
        }
        // 85 11 = Report ID 0x11 (HID++ long), 06 00 ff = usage page 0xFF00
        if contains(&desc, &[0x85, 0x11]) && contains(&desc, &[0x06, 0x00, 0xFF]) {
            let name = uevent
                .lines()
                .find_map(|l| l.strip_prefix("HID_NAME="))
                .unwrap_or("?")
                .to_string();
            let file_name = dir.file_name().unwrap_or_default();
            nodes.push(Node { path: PathBuf::from("/dev").join(file_name), name });
        }
    }
    nodes
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

pub struct Hidpp {
    file: File,
    pub dev: u8,
}

impl Hidpp {
    pub fn open(node: &Node, dev: u8) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(&node.path)?;
        Ok(Hidpp { file, dev })
    }

    /// Open the first node/index pair that answers with HID++ >= 2.0.
    /// Returns the connection, the node used and the protocol version.
    pub fn probe(nodes: &[Node]) -> std::result::Result<(Self, Node, (u8, u8)), String> {
        if nodes.is_empty() {
            return Err("No Logitech HID++ hidraw interface found".into());
        }
        let mut denied = Vec::new();
        for node in nodes {
            for dev in PROBE_INDICES {
                let mut h = match Hidpp::open(node, dev) {
                    Ok(h) => h,
                    Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                        denied.push(node.path.display().to_string());
                        break;
                    }
                    Err(_) => break,
                };
                if let Ok(version @ (major, _)) = h.protocol_version() {
                    if major >= 2 {
                        return Ok((h, node.clone(), version));
                    }
                }
            }
        }
        if !denied.is_empty() {
            return Err(format!("Permission denied on {}", denied.join(", ")));
        }
        Err("No HID++ 2.0 device answered. Move the mouse to wake it and retry.".into())
    }

    pub fn request(&mut self, feature: u8, function: u8, params: &[u8]) -> Result<Vec<u8>> {
        self.request_timeout(feature, function, params, DEFAULT_TIMEOUT)
    }

    pub fn request_timeout(
        &mut self,
        feature: u8,
        function: u8,
        params: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let (rid, size) = if params.len() <= SHORT_LEN - 4 {
            (REPORT_SHORT, SHORT_LEN)
        } else {
            (REPORT_LONG, LONG_LEN)
        };
        assert!(params.len() <= size - 4, "params too long");
        let fn_sw = (function << 4) | SW_ID;
        let mut msg = vec![0u8; size];
        msg[..4].copy_from_slice(&[rid, self.dev, feature, fn_sw]);
        msg[4..4 + params.len()].copy_from_slice(params);
        self.file.write_all(&msg)?;

        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 64];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || !self.wait_readable(remaining)? {
                return Err(Error::Timeout { feature, function });
            }
            let n = self.file.read(&mut buf)?;
            let rep = &buf[..n];
            if n < 6 || !matches!(rep[0], REPORT_SHORT | REPORT_LONG) || rep[1] != self.dev {
                continue;
            }
            if rep[2] == 0xFF && rep[3] == feature && rep[4] == fn_sw {
                return Err(Error::Hidpp20 { feature, function, code: rep[5] });
            }
            if rep[2] == 0x8F && rep[3] == feature {
                return Err(Error::Hidpp10 { device: self.dev, code: rep[5] });
            }
            if rep[2] == feature && rep[3] == fn_sw {
                return Ok(rep[4..].to_vec());
            }
        }
    }

    fn wait_readable(&self, timeout: Duration) -> io::Result<bool> {
        let mut pfd = libc::pollfd { fd: self.file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        loop {
            // SAFETY: pfd is a valid pollfd for the duration of the call.
            let r = unsafe { libc::poll(&mut pfd, 1, ms.max(1)) };
            if r < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            return Ok(r > 0);
        }
    }

    /// Root feature, function 0. Returns 0 if the feature is not supported.
    pub fn feature_index(&mut self, feature_id: u16) -> Result<u8> {
        Ok(self.request(0x00, 0, &feature_id.to_be_bytes())?[0])
    }

    /// Root feature, function 1.
    pub fn protocol_version(&mut self) -> Result<(u8, u8)> {
        let rep = self.request_timeout(0x00, 1, &[0x00, 0x00, 0x5A], Duration::from_secs(1))?;
        Ok((rep[0], rep[1]))
    }
}
