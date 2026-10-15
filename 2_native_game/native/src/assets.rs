//! Verified loose-file backing for the block-1 virtual card.
//! Preserve the original address map, FNT and FAT, including directory/file IDs.
//! Unallocated card bytes read as 0xff; no source ROM is opened or reconstructed on disk.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

pub struct Assets {
    pub card: Vec<u8>,
    pub arm9: Vec<u8>,
    pub manifest_sha256: String,
}

fn hash(bytes: &[u8]) -> String {
    mkds_builder::rom::hex(&Sha256::digest(bytes))
}

fn number(v: &Value, label: &str) -> Result<usize, String> {
    v.as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| format!("Invalid asset {label}"))
}

fn word(b: &[u8], off: usize) -> usize {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap()) as usize
}

fn read(root: &Path, rel: &str, entry: &Value) -> Result<Vec<u8>, String> {
    if rel.is_empty()
        || rel.starts_with('/')
        || rel
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        || rel.chars().any(|c| c == '\\' || c == ':' || c.is_control())
    {
        return Err(format!("Unsafe asset path: {rel:?}"));
    }
    let path = root
        .join(rel)
        .canonicalize()
        .map_err(|e| format!("Asset {rel}: {e}"))?;
    if !path.starts_with(root) {
        return Err(format!("Asset escapes its folder: {rel}"));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Asset {rel}: {e}"))?;
    if bytes.len() != number(&entry["size"], "size")?
        || entry["sha256"].as_str() != Some(hash(&bytes).as_str())
    {
        return Err(format!("Asset changed: {rel}. Rebuild from your ROM."));
    }
    Ok(bytes)
}

fn put(card: &mut [u8], off: usize, bytes: &[u8]) -> Result<(), String> {
    let end = off.checked_add(bytes.len()).ok_or("Asset range overflow")?;
    let target = card
        .get_mut(off..end)
        .ok_or("Asset range exceeds virtual card")?;
    target.copy_from_slice(bytes);
    Ok(())
}

/// Both expected hashes must come from generation provenance, not from this folder.
pub fn load(root: &Path, expected_rom: &str, expected_assets: &str) -> Result<Assets, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("Assets folder: {e}"))?;
    let raw =
        std::fs::read(root.join("manifest.json")).map_err(|e| format!("Asset manifest: {e}"))?;
    let m: Value = serde_json::from_slice(&raw).map_err(|e| format!("Asset manifest: {e}"))?;
    if m["format"] != "mkds-assets" || m["format_version"] != 1 {
        return Err("Unsupported asset manifest format".into());
    }
    if expected_rom.len() != 64
        || expected_assets.len() != 64
        || m["rom"]["sha256"].as_str() != Some(expected_rom)
        || m["assets_sha256"].as_str() != Some(expected_assets)
    {
        return Err("Assets do not match generated game provenance".into());
    }
    let size = number(&m["rom"]["size"], "ROM size")?;
    if !(0x200..=512 * 1024 * 1024).contains(&size) {
        return Err("Unsupported virtual card size".into());
    }
    let mut sum = Sha256::new();
    let files = m["files"].as_array().ok_or("Asset manifest lacks files")?;
    let system = m["system"]
        .as_object()
        .ok_or("Asset manifest lacks system")?;
    let mut systems = std::collections::BTreeMap::new();
    for (name, entry) in system {
        systems.insert(
            name.as_str(),
            read(&root, &format!("system/{name}"), entry)?,
        );
    }
    let get = |name: &str| {
        systems
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| format!("Missing system asset: {name}"))
    };
    let header = get("header.bin")?;
    if header.len() != 0x200 {
        return Err("Invalid asset header size".into());
    }
    let fat = get("fat.bin")?;
    if fat.len() % 8 != 0
        || files.len() != fat.len() / 8
        || number(&m["fat_entries"], "FAT count")? != files.len()
    {
        return Err("Asset FAT count mismatch".into());
    }
    let fnt = get("fnt.bin")?;
    if fnt.len() < 8
        || number(&m["fnt_directories"], "directory count")?
            != u16::from_le_bytes([fnt[6], fnt[7]]) as usize
    {
        return Err("Asset FNT directory count mismatch".into());
    }
    mkds_builder::rom::list_files_with_ids(fnt, files.len()).map_err(|e| e.to_string())?;
    let mut card = vec![0xff; size];
    put(&mut card, 0, header)?;
    for (name, offset, length) in [
        ("arm9.bin", 0x20, 0x2c),
        ("arm7.bin", 0x30, 0x3c),
        ("fnt.bin", 0x40, 0x44),
        ("fat.bin", 0x48, 0x4c),
        ("arm9_overlay_table.bin", 0x50, 0x54),
        ("arm7_overlay_table.bin", 0x58, 0x5c),
    ] {
        let bytes = get(name)?;
        if bytes.len() != word(header, length) {
            return Err(format!("Header size mismatch: {name}"));
        }
        put(&mut card, word(header, offset), bytes)?;
    }
    put(&mut card, word(header, 0x68), get("banner.bin")?)?;
    for (id, entry) in files.iter().enumerate() {
        if number(&entry["id"], "file ID")? != id {
            return Err("Asset IDs must be consecutive and ordered".into());
        }
        let bytes = read(
            &root,
            entry["path"].as_str().ok_or("Missing asset path")?,
            entry,
        )?;
        let (start, end) = (word(fat, id * 8), word(fat, id * 8 + 4));
        if end.checked_sub(start) != Some(bytes.len()) {
            return Err(format!("FAT size mismatch for ID {id}"));
        }
        put(&mut card, start, &bytes)?;
        sum.update(format!("{id} {}\n", hash(&bytes)));
    }
    for (name, bytes) in &systems {
        sum.update(format!("{name} {}\n", hash(bytes)));
    }
    if mkds_builder::rom::hex(&sum.finalize()) != expected_assets {
        return Err("Asset manifest content hash mismatch".into());
    }
    let arm9 = mkds_builder::extract::arm9_main(&card).map_err(|e| format!("ARM9 asset: {e}"))?;
    Ok(Assets {
        card,
        arm9,
        manifest_sha256: hash(&raw),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        let mut rom = vec![0; 0x10000];
        let fnt = [8, 0, 0, 0, 0, 0, 1, 0, 1, b'a', 0];
        for (field, offset, bytes) in [
            (0x20, 0x200, vec![0; 16]),
            (0x30, 0x220, vec![0; 4]),
            (0x40, 0x240, fnt.to_vec()),
            (
                0x48,
                0x260,
                [0x8000u32.to_le_bytes(), 0x8004u32.to_le_bytes()].concat(),
            ),
        ] {
            let length = match field {
                0x20 => 0x2c,
                0x30 => 0x3c,
                _ => field + 4,
            };
            rom[field..field + 4].copy_from_slice(&(offset as u32).to_le_bytes());
            rom[length..length + 4].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
            rom[offset..offset + bytes.len()].copy_from_slice(&bytes);
        }
        rom[0x8000..0x8004].copy_from_slice(b"data");
        rom
    }

    #[test]
    fn loose_assets_preserve_ids_and_card_reads_without_source_rom() {
        let rom = fixture();
        let temp = tempfile::tempdir().unwrap();
        let out = temp.path().join("loose assets");
        let sha = hash(&rom);
        let rip = mkds_builder::rip::rip(&rom, &sha, &out, &mut |_| {}).unwrap();
        let expected = rip.manifest["assets_sha256"].as_str().unwrap();
        let assets = load(&out.join("assets"), &sha, expected).unwrap();
        assert_eq!(&assets.card[0x240..0x24b], &rom[0x240..0x24b]);
        assert_eq!(&assets.card[0x260..0x268], &rom[0x260..0x268]);
        let mut card = recomp::card::Card::new(assets.card, 0x1fc2);
        card.cmd = [0xb7, 0, 0, 0x80, 0, 0, 0, 0];
        card.write_ctrl(0x87000000);
        assert_eq!(card.read_data().0, u32::from_le_bytes(*b"data"));
        assert!(load(&out.join("assets"), &"0".repeat(64), expected).is_err());
        std::fs::write(out.join("assets/files/a"), b"edit").unwrap();
        assert!(
            load(&out.join("assets"), &sha, expected)
                .err()
                .unwrap()
                .contains("Asset changed")
        );
    }

    #[test]
    fn rejects_manifest_id_path_and_range_corruption() {
        for mutation in ["id", "path", "range", "version"] {
            let temp = tempfile::tempdir().unwrap();
            let out = temp.path().join("rip");
            let rom = fixture();
            let sha = hash(&rom);
            let mut m = mkds_builder::rip::rip(&rom, &sha, &out, &mut |_| {})
                .unwrap()
                .manifest;
            match mutation {
                "id" => m["files"][0]["id"] = 1.into(),
                "path" => m["files"][0]["path"] = "../outside".into(),
                "range" => m["rom"]["size"] = 512.into(),
                _ => m["format_version"] = 2.into(),
            }
            std::fs::write(out.join("assets/manifest.json"), m.to_string()).unwrap();
            assert!(
                load(
                    &out.join("assets"),
                    &sha,
                    m["assets_sha256"].as_str().unwrap()
                )
                .is_err(),
                "{mutation}"
            );
        }
    }

    #[test]
    #[ignore = "requires MKDS_TEST_ROM pointing to a private local ROM"]
    fn real_rom_rip_card_and_boot_parity() {
        let rom = std::fs::read(std::env::var("MKDS_TEST_ROM").expect("MKDS_TEST_ROM")).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let out = temp.path().join("rip");
        let sha = hash(&rom);
        let m = mkds_builder::rip::rip(&rom, &sha, &out, &mut |_| {})
            .unwrap()
            .manifest;
        let assets = load(
            &out.join("assets"),
            &sha,
            m["assets_sha256"].as_str().unwrap(),
        )
        .unwrap();
        let fat = word(&rom, 0x48);
        for id in 0..word(&rom, 0x4c) / 8 {
            let start = word(&rom, fat + id * 8);
            let end = word(&rom, fat + id * 8 + 4);
            assert_eq!(&rom[start..end], &assets.card[start..end], "FAT ID {id}");
        }
        assert_eq!(assets.arm9, mkds_builder::extract::arm9_main(&rom).unwrap());
        let dispatcher = recomp::dispatch::Dispatcher::new(&[]);
        let mut original =
            recomp::boot::session::Session::new(&rom, &assets.arm9, &dispatcher).unwrap();
        let mut loose =
            recomp::boot::session::Session::new(&assets.card, &assets.arm9, &dispatcher).unwrap();
        for _ in 0..100 {
            original.step(1000).unwrap();
            loose.step(1000).unwrap();
            assert_eq!(original.cpu.r, loose.cpu.r);
            assert_eq!(original.pc, loose.pc);
            assert_eq!(
                original.machine.arm7.as_ref().unwrap().cpu.r,
                loose.machine.arm7.as_ref().unwrap().cpu.r
            );
        }
    }
}
