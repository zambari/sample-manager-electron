//! WAV cue points: where each slice of a kit starts. Stored as a standard
//! `cue ` chunk after the audio, so other tools can see them and players
//! that don't care skip them.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Cue positions (in frames) from a WAV file, sorted. Empty for anything
/// that isn't a WAV or has no cues.
pub fn read(path: &Path) -> Vec<usize> {
    read_inner(path).unwrap_or_default()
}

fn read_inner(path: &Path) -> Option<Vec<usize>> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return None;
    }
    let mut ch = [0u8; 8];
    while f.read_exact(&mut ch).is_ok() {
        let size = u32::from_le_bytes(ch[4..8].try_into().ok()?) as u64;
        if &ch[0..4] == b"cue " {
            let mut buf = vec![0u8; size.min(1 << 20) as usize];
            f.read_exact(&mut buf).ok()?;
            let n = u32::from_le_bytes(buf.get(0..4)?.try_into().ok()?) as usize;
            let mut out: Vec<usize> = (0..n)
                .filter_map(|i| {
                    let p = buf.get(4 + i * 24..4 + (i + 1) * 24)?;
                    Some(u32::from_le_bytes(p[20..24].try_into().ok()?) as usize)
                })
                .collect();
            out.sort_unstable();
            out.dedup();
            return Some(out);
        }
        f.seek(SeekFrom::Current((size + (size & 1)) as i64)).ok()?;
    }
    None
}

/// Append a `cue ` chunk with `frames` to a finished WAV and fix up the
/// RIFF size.
pub fn append(path: &Path, frames: &[usize]) -> Result<(), String> {
    if frames.is_empty() {
        return Ok(());
    }
    let mut c = Vec::with_capacity(4 + frames.len() * 24);
    c.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    for (i, &fr) in frames.iter().enumerate() {
        let fr = fr.min(u32::MAX as usize) as u32;
        c.extend_from_slice(&(i as u32 + 1).to_le_bytes()); // id
        c.extend_from_slice(&fr.to_le_bytes()); // play order position
        c.extend_from_slice(b"data");
        c.extend_from_slice(&0u32.to_le_bytes()); // chunk start
        c.extend_from_slice(&0u32.to_le_bytes()); // block start
        c.extend_from_slice(&fr.to_le_bytes()); // sample offset
    }
    append_chunk(path, b"cue ", &c).map_err(|e| format!("writing cue points: {e}"))
}

/// Our own chunk: the file each kit slice came from, one UTF-8 line per
/// slice (the first is the slice before the first cue). Players skip it.
const NAMES_ID: &[u8; 4] = b"slnm";

/// Append the slice names chunk; nothing is written when none are known.
pub fn append_names(path: &Path, names: &[String]) -> Result<(), String> {
    if names.iter().all(String::is_empty) {
        return Ok(());
    }
    let text = names
        .iter()
        .map(|n| n.replace(['\n', '\r'], " "))
        .collect::<Vec<_>>()
        .join("\n");
    append_chunk(path, NAMES_ID, text.as_bytes())
        .map_err(|e| format!("writing slice names: {e}"))
}

/// Slice names saved by [`append_names`], or empty.
pub fn read_names(path: &Path) -> Vec<String> {
    read_names_inner(path).unwrap_or_default()
}

fn read_names_inner(path: &Path) -> Option<Vec<String>> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return None;
    }
    let mut ch = [0u8; 8];
    while f.read_exact(&mut ch).is_ok() {
        let size = u32::from_le_bytes(ch[4..8].try_into().ok()?) as u64;
        if &ch[0..4] == NAMES_ID {
            let mut buf = vec![0u8; size.min(1 << 20) as usize];
            f.read_exact(&mut buf).ok()?;
            let text = String::from_utf8_lossy(&buf);
            return Some(text.split('\n').map(str::to_owned).collect());
        }
        f.seek(SeekFrom::Current((size + (size & 1)) as i64)).ok()?;
    }
    None
}

/// Append one chunk to a finished WAV (padding to an even offset first)
/// and fix up the RIFF size.
fn append_chunk(path: &Path, id: &[u8; 4], data: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let mut len = f.seek(SeekFrom::End(0))?;
    if len & 1 == 1 {
        f.write_all(&[0])?;
        len += 1;
    }
    let mut c = Vec::with_capacity(8 + data.len() + 1);
    c.extend_from_slice(id);
    c.extend_from_slice(&(data.len() as u32).to_le_bytes());
    c.extend_from_slice(data);
    if data.len() & 1 == 1 {
        c.push(0);
    }
    f.write_all(&c)?;
    let riff = (len + c.len() as u64 - 8).min(u32::MAX as u64) as u32;
    f.seek(SeekFrom::Start(4))?;
    f.write_all(&riff.to_le_bytes())?;
    Ok(())
}
