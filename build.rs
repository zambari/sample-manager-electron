//! Embeds assets/icon.ico into the Windows .exe so Explorer and the taskbar
//! show the app icon. No resource compiler needed: we write the compiled
//! resource (.res) file ourselves and hand it to the linker, which accepts
//! .res files directly (both link.exe and lld-link).

use std::{env, fs, path::PathBuf};

const ICON: &str = "assets/icon.ico";
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const LANG_EN_US: u16 = 0x0409;

fn main() {
    println!("cargo:rerun-if-changed={ICON}");
    println!("cargo:rerun-if-changed=build.rs");

    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os != "windows" || target_env != "msvc" {
        return;
    }

    let ico = fs::read(ICON).expect("read assets/icon.ico");
    let res = ico_to_res(&ico);
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("icon.res");
    fs::write(&out, res).expect("write icon.res");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// Turns an .ico file into a .res holding one RT_ICON per image plus the
/// RT_GROUP_ICON (id 1) that indexes them.
fn ico_to_res(ico: &[u8]) -> Vec<u8> {
    assert!(u16_at(ico, 2) == 1, "{ICON} is not an icon file");
    let count = u16_at(ico, 4) as usize;

    let mut res = Vec::new();
    // A .res file starts with an empty entry.
    push_entry(&mut res, 0, 0, 0, &[]);

    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes());
    group.extend_from_slice(&1u16.to_le_bytes());
    group.extend_from_slice(&(count as u16).to_le_bytes());

    for i in 0..count {
        let e = 6 + i * 16;
        let size = u32_at(ico, e + 8) as usize;
        let offset = u32_at(ico, e + 12) as usize;
        let id = (i + 1) as u16;
        push_entry(&mut res, RT_ICON, id, 0x1010, &ico[offset..offset + size]);

        // GRPICONDIRENTRY: same as the .ico entry, but a 2-byte id replaces
        // the 4-byte file offset.
        group.extend_from_slice(&ico[e..e + 12]);
        group.extend_from_slice(&id.to_le_bytes());
    }

    push_entry(&mut res, RT_GROUP_ICON, 1, 0x1030, &group);
    res
}

fn push_entry(res: &mut Vec<u8>, kind: u16, name: u16, flags: u16, data: &[u8]) {
    let empty = kind == 0;
    res.extend_from_slice(&(data.len() as u32).to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes()); // header size
    res.extend_from_slice(&[0xFF, 0xFF]);
    res.extend_from_slice(&kind.to_le_bytes());
    res.extend_from_slice(&[0xFF, 0xFF]);
    res.extend_from_slice(&name.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // data version
    res.extend_from_slice(&flags.to_le_bytes());
    res.extend_from_slice(&(if empty { 0 } else { LANG_EN_US }).to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // version
    res.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    res.extend_from_slice(data);
    while res.len() % 4 != 0 {
        res.push(0);
    }
}
