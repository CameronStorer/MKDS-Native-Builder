//! ROM -> Rust source: reads constant tables out of the ARM9 program and emits them as a
//! generated Rust module, so the repository itself needs to contain no ROM-derived numbers.
use crate::rom::{BuildError, err};
use std::fmt::Write;

const BASE: u32 = 0x0200_0000;

#[derive(Clone, Copy)]
pub enum Elem {
    I32,
    U16,
}

/// One table: Rust constant name, RAM address in the decompressed ARM9, element type, count.
pub struct Table {
    pub name: &'static str,
    pub addr: u32,
    pub elem: Elem,
    pub count: usize,
}

/// One ARM9 overlay after loading its FAT file and expanding BLZ data when flagged.
/// `data` contains the initialized image only; `bss_size` is separately zero-filled by the loader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arm9Overlay {
    pub id: u32,
    pub ram_address: u32,
    pub ram_size: u32,
    pub bss_size: u32,
    pub static_init_start: u32,
    pub static_init_end: u32,
    pub file_id: u32,
    /// Low 24 bits of the packed overlay table field.
    pub compressed_size: u32,
    /// High 8 bits of the packed overlay table field; bit 0 = BLZ, bit 1 = verify hash.
    pub flags: u8,
    /// Decompressed bytes loaded at `ram_address`; does not include BSS.
    pub data: Vec<u8>,
}

/// One initialized ARM9 image section as laid out by the Nitro SDK module-params block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arm9Section {
    /// Matches function-table labels such as `arm9_main` and `arm9_autoload1_itcm`.
    pub name: String,
    pub ram_address: u32,
    pub bss_size: u32,
    pub data: Vec<u8>,
}

impl Arm9Overlay {
    pub fn is_compressed(&self) -> bool {
        self.flags & 1 != 0
    }

    pub fn verifies_hash(&self) -> bool {
        self.flags & 2 != 0
    }
}

/// Addresses are for AMCE revision 0 (found by matching the port's former hard-coded tables).
pub const TABLES: &[Table] = &[
    Table {
        name: "SPRING_X",
        addr: 0x0215_4DE4,
        elem: Elem::I32,
        count: 25,
    },
    Table {
        name: "SPRING_Y",
        addr: 0x0215_4E48,
        elem: Elem::I32,
        count: 25,
    },
    Table {
        name: "SPRING_Z",
        addr: 0x0215_4EAC,
        elem: Elem::I32,
        count: 25,
    },
    Table {
        name: "ATAN",
        addr: 0x0216_15B8,
        elem: Elem::U16,
        count: 129,
    },
    Table {
        name: "BRAKE_SURFACE",
        addr: 0x0216_CAD6,
        elem: Elem::U16,
        count: 19,
    },
];

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
}

/// The static ARM9 image (BLZ: backwards LZ) expanded to its load layout at 0x0200_0000.
pub fn arm9_main(rom: &[u8]) -> Result<Vec<u8>, BuildError> {
    let bad = || BuildError("ARM9 image is malformed.".into());
    let off = u32_at(rom, 0x20).ok_or_else(bad)? as usize;
    let size = u32_at(rom, 0x2C).ok_or_else(bad)? as usize;
    let mut arm9 = rom
        .get(off..off.checked_add(size).ok_or_else(bad)?)
        .ok_or_else(bad)?;
    let magic = [0xDE, 0xC0, 0x06, 0x21, 0x21, 0x06, 0xC0, 0xDE];
    if arm9.len() >= 12 && arm9[arm9.len() - 12..arm9.len() - 4] == magic {
        arm9 = &arm9[..arm9.len() - 12];
    }
    decompress_blz(arm9).ok_or_else(bad)
}

/// Splits the expanded ARM9 into its main image and SDK-declared autoload sections.
/// Addresses and section order come from the module-params block embedded in ARM9.
pub fn arm9_sections(rom: &[u8]) -> Result<Vec<Arm9Section>, BuildError> {
    let bad = || BuildError("ARM9 module parameters are malformed.".into());
    let image = arm9_main(rom)?;
    let base = u32_at(rom, 0x28).ok_or_else(bad)?;
    let magic = [0x21, 0x06, 0xC0, 0xDE, 0xDE, 0xC0, 0x06, 0x21];
    let limit = image.len().min(0x8000);
    let module_params = image[..limit]
        .windows(magic.len())
        .position(|w| w == magic)
        .and_then(|p| p.checked_sub(0x1C));
    let Some(module_params) = module_params else {
        return Ok(vec![Arm9Section {
            name: "arm9_main".into(),
            ram_address: base,
            bss_size: 0,
            data: image,
        }]);
    };
    let word = |i: usize| -> Result<u32, BuildError> {
        u32_at(&image, module_params.checked_add(i * 4).ok_or_else(bad)?).ok_or_else(bad)
    };
    let (autoload_start, autoload_end, main_end_addr, bss_start, bss_end) =
        (word(0)?, word(1)?, word(2)?, word(3)?, word(4)?);
    let main_len = main_end_addr.checked_sub(base).ok_or_else(bad)? as usize;
    if main_len > image.len() || bss_end < bss_start {
        return Err(bad());
    }
    let mut sections = vec![Arm9Section {
        name: "arm9_main".into(),
        ram_address: base,
        bss_size: bss_end - bss_start,
        data: image[..main_len].to_vec(),
    }];
    if autoload_start == 0 && autoload_end == 0 {
        return Ok(sections);
    }
    let mut table_pos = autoload_start.checked_sub(base).ok_or_else(bad)? as usize;
    let table_end = autoload_end.checked_sub(base).ok_or_else(bad)? as usize;
    if table_pos > table_end || table_end > image.len() {
        return Err(bad());
    }
    let mut data_pos = main_len;
    let mut section_index = 1usize;
    while table_pos < table_end {
        let rec_end = table_pos.checked_add(12).ok_or_else(bad)?;
        let rec = image.get(table_pos..rec_end).ok_or_else(bad)?;
        let ram_address = u32::from_le_bytes(rec[0..4].try_into().unwrap());
        let size = u32::from_le_bytes(rec[4..8].try_into().unwrap()) as usize;
        let bss_size = u32::from_le_bytes(rec[8..12].try_into().unwrap());
        table_pos = rec_end;
        let data_end = data_pos.checked_add(size).ok_or_else(bad)?;
        let data = image.get(data_pos..data_end).ok_or_else(bad)?;
        sections.push(Arm9Section {
            name: format!("arm9_autoload{section_index}_{}", arm9_region(ram_address)),
            ram_address,
            bss_size,
            data: data.to_vec(),
        });
        data_pos = data_end;
        section_index += 1;
    }
    Ok(sections)
}

fn arm9_region(address: u32) -> &'static str {
    match address {
        0x0100_0000..0x0200_0000 => "itcm",
        0x0200_0000..0x0240_0000 => "main",
        0x0240_0000..0x0300_0000 => "dtcm",
        0x0300_0000..0x0380_0000 => "shared_wram",
        0x0380_0000..0x0400_0000 => "arm7_wram",
        _ => "other",
    }
}

/// ARM9 overlay table + FAT payloads from the user's ROM, with compressed code expanded.
/// NDS overlay records are eight little-endian u32 fields; the last word packs flags in
/// its high byte and compressed size in the low 24 bits.
pub fn arm9_overlays(rom: &[u8]) -> Result<Vec<Arm9Overlay>, BuildError> {
    let bad = || BuildError("ARM9 overlay data is malformed.".into());
    let table_off = u32_at(rom, 0x50).ok_or_else(bad)? as usize;
    let table_len = u32_at(rom, 0x54).ok_or_else(bad)? as usize;
    if table_len == 0 {
        return Ok(Vec::new());
    }
    if table_off < 0x200 || table_len % 32 != 0 {
        return Err(bad());
    }
    let table_end = table_off.checked_add(table_len).ok_or_else(bad)?;
    let table = rom.get(table_off..table_end).ok_or_else(bad)?;
    let fat_off = u32_at(rom, 0x48).ok_or_else(bad)? as usize;
    let fat_len = u32_at(rom, 0x4C).ok_or_else(bad)? as usize;
    if fat_off < 0x200 || fat_len % 8 != 0 {
        return Err(bad());
    }
    let fat_end = fat_off.checked_add(fat_len).ok_or_else(bad)?;
    let fat = rom.get(fat_off..fat_end).ok_or_else(bad)?;
    let mut overlays = Vec::with_capacity(table_len / 32);
    for entry in table.chunks_exact(32) {
        let field = |i: usize| u32::from_le_bytes(entry[i * 4..i * 4 + 4].try_into().unwrap());
        let id = field(0);
        let ram_address = field(1);
        let ram_size = field(2);
        let bss_size = field(3);
        let static_init_start = field(4);
        let static_init_end = field(5);
        let file_id = field(6);
        let size_flags = field(7);
        let fat_entry = (file_id as usize).checked_mul(8).ok_or_else(bad)?;
        let fat_record_end = fat_entry.checked_add(8).ok_or_else(bad)?;
        let range = fat.get(fat_entry..fat_record_end).ok_or_else(bad)?;
        let file_start = u32::from_le_bytes(range[..4].try_into().unwrap()) as usize;
        let file_end = u32::from_le_bytes(range[4..].try_into().unwrap()) as usize;
        if file_start < 0x200 || file_start > file_end {
            return Err(bad());
        }
        let stored = rom.get(file_start..file_end).ok_or_else(bad)?;
        let flags = (size_flags >> 24) as u8;
        let compressed_size = size_flags & 0x00FF_FFFF;
        if compressed_size as usize > stored.len() {
            return Err(bad());
        }
        let data = if flags & 1 != 0 {
            decompress_blz(stored).ok_or_else(bad)?
        } else {
            stored.to_vec()
        };
        if data.len() != ram_size as usize || static_init_end < static_init_start {
            return Err(bad());
        }
        overlays.push(Arm9Overlay {
            id,
            ram_address,
            ram_size,
            bss_size,
            static_init_start,
            static_init_end,
            file_id,
            compressed_size,
            flags,
            data,
        });
    }
    Ok(overlays)
}

fn decompress_blz(data: &[u8]) -> Option<Vec<u8>> {
    let n = data.len();
    let hdr = u32_at(data, n.checked_sub(8)?)?;
    let inc = u32_at(data, n - 4)? as usize;
    if inc == 0 {
        return Some(data.to_vec());
    }
    let (enc_len, hdr_len) = ((hdr & 0x00FF_FFFF) as usize, (hdr >> 24) as usize);
    let mut out = vec![0u8; n + inc];
    out[..n].copy_from_slice(data);
    let mut rp = n.checked_sub(hdr_len)?;
    let mut wp = n + inc;
    let stop = n.checked_sub(enc_len)?;
    while wp > 0 && rp > stop {
        rp -= 1;
        let flags = out[rp];
        for bit in 0..8 {
            if rp <= stop || wp == 0 {
                break;
            }
            if flags & (0x80 >> bit) != 0 {
                if rp < 2 + stop {
                    return None;
                }
                rp -= 1;
                let b1 = out[rp] as usize;
                rp -= 1;
                let b2 = out[rp] as usize;
                let len = (b1 >> 4) + 3;
                let pos = (((b1 & 0xF) << 8) | b2) + 3;
                for _ in 0..len {
                    if wp == 0 || wp + pos > out.len() {
                        return None;
                    }
                    wp -= 1;
                    out[wp] = out[wp + pos];
                }
            } else {
                rp -= 1;
                wp -= 1;
                out[wp] = out[rp];
            }
        }
    }
    Some(out)
}

/// Reads one table from the expanded image.
pub fn read_table(arm9: &[u8], t: &Table) -> Result<Vec<i64>, BuildError> {
    let width = match t.elem {
        Elem::I32 => 4,
        Elem::U16 => 2,
    };
    let start = t
        .addr
        .checked_sub(BASE)
        .ok_or_else(|| BuildError(format!("{}: bad address", t.name)))? as usize;
    let bytes = arm9
        .get(start..start + width * t.count)
        .ok_or_else(|| BuildError(format!("{}: outside ARM9 image", t.name)))?;
    Ok(bytes
        .chunks_exact(width)
        .map(|c| match t.elem {
            Elem::I32 => i32::from_le_bytes(c.try_into().unwrap()) as i64,
            Elem::U16 => u16::from_le_bytes(c.try_into().unwrap()) as i64,
        })
        .collect())
}

/// Generates the Rust source for every table, with a header recording the ROM it came from.
pub fn generate(rom: &[u8], rom_sha256: &str) -> Result<String, BuildError> {
    if rom.len() < 0x200 {
        return err("ROM too small.");
    }
    let arm9 = arm9_main(rom)?;
    let mut s = format!(
        "// @generated by mkds_builder from the user's ROM (SHA-256 {rom_sha256}). Do not edit or commit.\n"
    );
    for t in TABLES {
        let ty = match t.elem {
            Elem::I32 => "i32",
            Elem::U16 => "u16",
        };
        let vals = read_table(&arm9, t)?;
        let list: Vec<String> = vals.iter().map(|v| v.to_string()).collect();
        writeln!(
            s,
            "pub const {}: [{ty}; {}] = [{}];",
            t.name,
            t.count,
            list.join(", ")
        )
        .unwrap();
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// If the reference ROM and the hand port are both present, generated tables must equal the
    /// port's hard-coded ones (no game values are stored in this test).
    #[test]
    fn matches_hardcoded_tables_in_reference_rom() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let Ok(rom) = std::fs::read(root.join("6_data/rom/gamefile.nds")) else {
            return;
        };
        let Ok(src) = std::fs::read_to_string(root.join("3_hand_port/vm_model/src/kart/damage.rs")) else {
            return;
        };
        let arm9 = arm9_main(&rom).unwrap();
        for t in &TABLES[..3] {
            let vals: Vec<String> = read_table(&arm9, t)
                .unwrap()
                .iter()
                .map(|v| v.to_string())
                .collect();
            assert!(
                src.contains(&vals.join(", ")),
                "{} differs from damage.rs",
                t.name
            );
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(generate(&[0u8; 0x300], "x").is_err());
    }

    #[test]
    fn reads_arm9_overlay_metadata_and_fat_file() {
        let mut rom = vec![0u8; 0x22C];
        rom[0x50..0x54].copy_from_slice(&0x200u32.to_le_bytes());
        rom[0x54..0x58].copy_from_slice(&32u32.to_le_bytes());
        rom[0x48..0x4C].copy_from_slice(&0x220u32.to_le_bytes());
        rom[0x4C..0x50].copy_from_slice(&8u32.to_le_bytes());
        let fields = [
            7u32,
            0x0200_1000,
            4,
            8,
            0x0200_1000,
            0x0200_1004,
            0,
            (2 << 24) | 4,
        ];
        for (i, value) in fields.into_iter().enumerate() {
            rom[0x200 + i * 4..0x204 + i * 4].copy_from_slice(&value.to_le_bytes());
        }
        rom[0x220..0x224].copy_from_slice(&0x228u32.to_le_bytes());
        rom[0x224..0x228].copy_from_slice(&0x22Cu32.to_le_bytes());
        rom[0x228..0x22C].copy_from_slice(&[1, 2, 3, 4]);

        let overlays = arm9_overlays(&rom).unwrap();
        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].id, 7);
        assert_eq!(overlays[0].ram_address, 0x0200_1000);
        assert_eq!(overlays[0].bss_size, 8);
        assert_eq!(overlays[0].data, [1, 2, 3, 4]);
        assert!(!overlays[0].is_compressed());
        assert!(overlays[0].verifies_hash());
    }

    #[test]
    fn expands_reference_rom_arm9_overlays_when_available() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let Ok(rom) = std::fs::read(root.join("6_data/rom/gamefile.nds")) else {
            return;
        };
        let sections = arm9_sections(&rom).unwrap();
        assert_eq!(
            sections.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["arm9_main", "arm9_autoload1_itcm", "arm9_autoload2_dtcm",]
        );
        assert_eq!(
            sections.iter().map(|s| s.data.len()).collect::<Vec<_>>(),
            [1_504_032, 32_672, 224,]
        );
        let overlays = arm9_overlays(&rom).unwrap();
        assert!(!overlays.is_empty());
        assert!(overlays.iter().all(|o| o.data.len() == o.ram_size as usize));
        assert!(overlays.iter().all(Arm9Overlay::is_compressed));
    }
}
