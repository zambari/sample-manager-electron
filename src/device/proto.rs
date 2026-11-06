//! Elektron's SysEx file API, as reverse-engineered by elk-herd.
//!
//! Wire format of one API message:
//!
//! ```text
//! F0 00 20 3C 10 00  <7-bit encoded payload>  F7
//! payload = msg_id:u16be  resp_id:u16be  command:u8  args...
//! ```
//!
//! A response carries `command | 0x80` and `resp_id` set to the request's
//! `msg_id`. Integers are big-endian u32, strings are NUL-terminated
//! Windows-1252, booleans are a 0/1 byte.

pub const HEADER: [u8; 6] = [0xF0, 0x00, 0x20, 0x3C, 0x10, 0x00];

pub const DEVICE: u8 = 0x01;
pub const DIR_LIST: u8 = 0x10;
pub const DIR_CREATE: u8 = 0x11;
pub const DIR_DELETE: u8 = 0x12;
pub const FILE_DELETE: u8 = 0x20;
pub const ITEM_RENAME: u8 = 0x21;
pub const READ_OPEN: u8 = 0x30;
pub const READ_CLOSE: u8 = 0x31;
pub const READ: u8 = 0x32;
pub const WRITE_OPEN: u8 = 0x40;
pub const WRITE_CLOSE: u8 = 0x41;
pub const WRITE: u8 = 0x42;
/// `hash, length` → path of that file on the +Drive.
pub const SAMPLE_INFO: u8 = 0x23;
/// Data API (saved projects, sounds): list a folder.
pub const DATA_LIST: u8 = 0x53;
/// Data API writes, moves, copies, clears and swaps. These change saved
/// projects on the +Drive; never sent (see project_discovery.md §0).
pub const FORBIDDEN: [u8; 7] = [0x57, 0x58, 0x59, 0x5A, 0x5B, 0x5C, 0x5D];

/// Bytes per read/write chunk, as elk-herd uses.
pub const CHUNK: usize = 0x2000;

/// Each group of 7 bytes becomes 8: a byte holding their high bits (first
/// byte's in bit 6), then the 7 bytes with the high bit cleared. A short last
/// group just has fewer bytes.
pub fn encode7(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len().div_ceil(7));
    for group in data.chunks(7) {
        let mut hi = 0u8;
        for (i, b) in group.iter().enumerate() {
            hi |= (b >> 7) << (6 - i);
        }
        out.push(hi);
        out.extend(group.iter().map(|b| b & 0x7F));
    }
    out
}

pub fn decode7(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for group in data.chunks(8) {
        let hi = group[0];
        for (i, b) in group[1..].iter().enumerate() {
            out.push(b | (((hi >> (6 - i)) & 1) << 7));
        }
    }
    out
}

/// A whole SysEx message for `command` with already-built `args`.
pub fn frame(msg_id: u16, command: u8, args: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(5 + args.len());
    payload.extend_from_slice(&msg_id.to_be_bytes());
    payload.extend_from_slice(&0u16.to_be_bytes());
    payload.push(command);
    payload.extend_from_slice(args);
    let mut out = HEADER.to_vec();
    out.extend(encode7(&payload));
    out.push(0xF7);
    out
}

/// If `sysex` is an API response to `msg_id`, its command byte and args.
pub fn unframe(sysex: &[u8], msg_id: u16) -> Option<(u8, Vec<u8>)> {
    let body = sysex.strip_prefix(&HEADER)?;
    let body = body.strip_suffix(&[0xF7]).unwrap_or(body);
    let p = decode7(body);
    if p.len() < 5 {
        return None;
    }
    let resp_id = u16::from_be_bytes([p[2], p[3]]);
    (resp_id == msg_id).then(|| (p[4], p[5..].to_vec()))
}

// ---- argument building ------------------------------------------------------

#[derive(Default)]
pub struct Args(pub Vec<u8>);

impl Args {
    pub fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn str(mut self, s: &str) -> Self {
        // Latin-1 is Windows-1252 apart from 0x80-0x9F; close enough for
        // file names. Anything wider becomes '?'.
        self.0.extend(
            s.chars()
                .map(|c| if (c as u32) < 0x100 { c as u8 } else { b'?' }),
        );
        self.0.push(0);
        self
    }

    pub fn bytes(mut self, b: &[u8]) -> Self {
        self.0.extend_from_slice(b);
        self
    }
}

// ---- argument parsing -------------------------------------------------------

pub struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self { b }
    }

    pub fn is_empty(&self) -> bool {
        self.b.is_empty()
    }

    pub fn u8(&mut self) -> Result<u8, String> {
        let (&v, rest) = self.b.split_first().ok_or("short response")?;
        self.b = rest;
        Ok(v)
    }

    pub fn bool(&mut self) -> Result<bool, String> {
        Ok(self.u8()? != 0)
    }

    pub fn u32(&mut self) -> Result<u32, String> {
        if self.b.len() < 4 {
            return Err("short response".into());
        }
        let v = u32::from_be_bytes([self.b[0], self.b[1], self.b[2], self.b[3]]);
        self.b = &self.b[4..];
        Ok(v)
    }

    pub fn str(&mut self) -> Result<String, String> {
        let end = self.b.iter().position(|&c| c == 0).unwrap_or(self.b.len());
        let s = self.b[..end].iter().map(|&c| c as char).collect();
        self.b = &self.b[(end + 1).min(self.b.len())..];
        Ok(s)
    }

    pub fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.b)
    }
}

/// One row of a directory listing.
#[derive(Clone, Debug)]
pub struct DirEntry {
    pub size: u32,
    pub is_dir: bool,
    pub name: String,
}

pub fn parse_listing(args: &[u8]) -> Result<Vec<DirEntry>, String> {
    let mut r = Reader::new(args);
    let mut out = Vec::new();
    while !r.is_empty() {
        let _hash = r.u32()?;
        let size = r.u32()?;
        let _locked = r.bool()?;
        let kind = r.u8()?;
        let name = r.str()?;
        if !name.is_empty() {
            out.push(DirEntry {
                size,
                is_dir: kind == b'D',
                name,
            });
        }
    }
    Ok(out)
}
