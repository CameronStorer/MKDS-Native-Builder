//! Original build-tool metadata parsing. Contains no game instructions or tables.
use std::collections::BTreeSet;

#[derive(Debug, PartialEq, Eq)]
pub struct Boundary {
    pub segment: String,
    pub entry: u32,
    pub size: u32,
    pub thumb: bool,
}

pub fn boundaries(csv: &str) -> Result<Vec<Boundary>, String> {
    let mut lines = csv.lines();
    let header = lines.next().ok_or("empty boundary file")?.trim_start_matches('\u{feff}');
    let legacy = header.starts_with("name,segment,ea,size,");
    if !legacy && header != "segment,entry,size,mode" {
        return Err("expected segment,entry,size,mode (or private FUNCTION_TABLE header)".into());
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (line, text) in lines.enumerate() {
        if text.trim().is_empty() { continue; }
        let fields: Vec<_> = text.splitn(5, ',').collect();
        let parse = || -> Result<Boundary, String> {
            if fields.len() < 4 { return Err("missing boundary fields".into()); }
            let (seg, addr, size, mode) = if legacy {
                (fields[1], fields[2], fields[3], "arm")
            } else { (fields[0], fields[1], fields[2], fields[3]) };
            if !legacy && fields.len() != 4 { return Err("address-only metadata has extra fields".into()); }
            if seg.is_empty() || !seg.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return Err("invalid segment identifier".into());
            }
            let addr = addr.trim();
            let entry = u32::from_str_radix(addr.strip_prefix("0x").unwrap_or(addr), 16)
                .map_err(|_| "invalid hexadecimal entry")?;
            let size: u32 = size.trim().parse().map_err(|_| "invalid decimal size")?;
            if size == 0 || entry.checked_add(size).is_none() { return Err("empty or overflowing boundary".into()); }
            let thumb = match mode.trim() { "arm" => false, "thumb" => true, _ => return Err("mode must be arm or thumb".into()) };
            // Legacy inventory has no mode column and can include Thumb entries.
            // Preserve its ARM assumption for the old spike, but let discovery
            // refuse halfword-only entries instead of rejecting the entire file.
            if entry % if thumb || legacy { 2 } else { 4 } != 0 { return Err("unaligned entry for instruction mode".into()); }
            Ok(Boundary { segment: seg.into(), entry, size, thumb })
        };
        let item = parse().map_err(|e| format!("boundary line {}: {e}", line + 2))?;
        if !seen.insert((item.segment.clone(), item.entry)) { return Err(format!("duplicate boundary at {:#X} in {}", item.entry, item.segment)); }
        out.push(item);
    }
    if out.is_empty() { return Err("boundary file contains no entries".into()); }
    Ok(out)
}

/// One piece of the ARM7 program as it sits in memory after its crt0 ran.
#[derive(Debug, PartialEq, Eq)]
pub struct Arm7Section {
    pub name: String,
    pub ram_address: u32,
    pub data: Vec<u8>,
}

/// The ARM7 binary's entry and its code sections at their run-time addresses: the static part
/// (`arm7_main`) and each autoload section (`arm7_auto_XXXXXXXX`, by destination). The ARM7 has no
/// function metadata and copies most of its code at boot, so discovery must see these addresses.
/// The crt0 autoload descriptor `(list_start, list_end, data_start)` is located by its arithmetic
/// (12-byte `(dest, size, bss)` entries whose sizes add up to `list_start - data_start`), not by
/// a fixed offset; an ARM7 without a valid descriptor is refused.
pub fn arm7_sections(rom: &[u8]) -> Result<(u32, Vec<Arm7Section>), String> {
    let w = |o: usize| rom.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let hdr = (w(0x30), w(0x34), w(0x38), w(0x3C));
    let (Some(off), Some(entry), Some(ram), Some(size)) = hdr else { return Err("ROM header too short".into()) };
    let image = rom.get(off as usize..(off as usize).checked_add(size as usize).ok_or("bad ARM7 size")?).ok_or("ARM7 binary outside the ROM")?;
    let rd = |a: u32| a.checked_sub(ram).and_then(|o| image.get(o as usize..o as usize + 4)).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let end = ram.wrapping_add(size);
    let within = |a: u32| a >= ram && a <= end;
    let scan_end = ram + (size.min(0x800) & !3);
    let mut found = None;
    for p in (ram..scan_end.saturating_sub(8)).step_by(4) {
        let (Some(ls), Some(le), Some(ds)) = (rd(p), rd(p + 4), rd(p + 8)) else { continue };
        if !(within(ls) && within(le) && within(ds)) || le <= ls || (le - ls) % 12 != 0 || ds >= ls {
            continue;
        }
        let entries: Vec<(u32, u32)> = (ls..le).step_by(12).filter_map(|e| Some((rd(e)?, rd(e + 4)?))).collect();
        let total: u64 = entries.iter().map(|&(_, s)| s as u64).sum();
        if entries.len() as u32 == (le - ls) / 12 && total == (ls - ds) as u64 && entries.iter().all(|&(d, s)| s > 0 && d % 4 == 0) {
            found = Some((ds, entries));
            break;
        }
    }
    let (ds, entries) = found.ok_or("ARM7 crt0 autoload descriptor not found")?;
    let slice = |a: u32, n: u32| image[(a - ram) as usize..(a - ram + n) as usize].to_vec();
    let mut out = vec![Arm7Section { name: "arm7_main".into(), ram_address: ram, data: slice(ram, ds - ram) }];
    let mut src = ds;
    for (dest, n) in entries {
        out.push(Arm7Section { name: format!("arm7_auto_{dest:08X}"), ram_address: dest, data: slice(src, n) });
        src += n;
    }
    Ok((entry, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arm7_autoload_sections_are_found_by_their_arithmetic() {
        let mut rom = vec![0u8; 0x200];
        let put = |rom: &mut Vec<u8>, o: usize, v: u32| rom[o..o + 4].copy_from_slice(&v.to_le_bytes());
        // ARM7 at ROM 0x100, RAM 0x02380000, 0x40 bytes: 0x10 static, 8 + 4 autoload bytes, 2 list entries
        for (o, v) in [(0x30, 0x100), (0x34, 0x0238_0000), (0x38, 0x0238_0000), (0x3C, 0x34)] {
            put(&mut rom, o, v);
        }
        put(&mut rom, 0x104, 0x0238_001C); // list start
        put(&mut rom, 0x108, 0x0238_0034); // list end
        put(&mut rom, 0x10C, 0x0238_0010); // data start
        rom[0x110..0x11C].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 9, 9, 9]);
        for (o, v) in [(0x11C, 0x037F_8000), (0x120, 8), (0x124, 0), (0x128, 0x0600_0000), (0x12C, 4), (0x130, 0x10)] {
            put(&mut rom, o, v);
        }
        let (entry, s) = arm7_sections(&rom).unwrap();
        assert_eq!(entry, 0x0238_0000);
        assert_eq!(s.iter().map(|s| (s.name.as_str(), s.ram_address, s.data.len())).collect::<Vec<_>>(),
            vec![("arm7_main", 0x0238_0000, 0x10), ("arm7_auto_037F8000", 0x037F_8000, 8), ("arm7_auto_06000000", 0x0600_0000, 4)]);
        assert_eq!(s[2].data, [9, 9, 9, 9]);
        put(&mut rom, 0x120, 9); // sizes no longer add up
        assert!(arm7_sections(&rom).is_err());
    }
    #[test]
    fn address_only_keeps_mode_and_overlay_identity() {
        let b = boundaries("segment,entry,size,mode\narm9_main,0x02000000,8,arm\noverlay_1,0x02000000,2,thumb\n").unwrap();
        assert_eq!(b.len(), 2);
        assert!(b[1].thumb);
    }
    #[test]
    fn private_notes_are_never_read_as_fields() {
        let b = boundaries("name,segment,ea,size,status,meaning\nsub_test,arm9_main,0x02000000,8,known,\"note,with comma\"\n").unwrap();
        assert_eq!(b[0].size, 8);
        assert!(boundaries("name,segment,ea,size,status\nsub_test,arm9_main,0x02000142,2,known\n").is_ok());
    }
    #[test]
    fn bad_metadata_is_rejected() {
        for row in ["arm9_main,0x02000002,4,arm", "arm9_main,0xFFFFFFFC,8,arm",
                    "arm9_main,0x02000000,0,arm", "arm9_main,0x02000000,4,guess",
                    "arm9_main,0x02000000,4,arm,private note"] {
            assert!(boundaries(&format!("segment,entry,size,mode\n{row}\n")).is_err(), "{row}");
        }
        assert!(boundaries("segment,entry,size,mode\narm9_main,0x02000000,4,arm\narm9_main,0x02000000,4,arm\n").is_err());
    }
}
