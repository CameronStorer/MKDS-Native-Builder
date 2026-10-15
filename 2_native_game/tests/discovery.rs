//! Whole-program discovery (`lift::discover_program`) on small hand-assembled programs: call
//! targets, `mov lr, pc` return sites, Thumb code pointers in literal pools, and rejection of data
//! words that only look like code addresses.
use recomp::arm::Inst;
use recomp::lift::{decode_at, discover, discover_program, discover_seeded, fall_exits, Image};

fn bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn bl(from: u32, to: u32) -> u32 {
    0xEB00_0000 | ((to.wrapping_sub(from + 8) >> 2) & 0x00FF_FFFF)
}

const UNDEF: u32 = 0xE7F0_00F0; // permanently undefined

#[test]
fn mov_lr_pc_return_site_is_a_block() {
    // mov lr, pc; bx r12; mov r0, #1; bx lr
    let code = bytes(&[0xE1A0_E00F, 0xE12F_FF1C, 0xE3A0_0001, 0xE12F_FF1E]);
    let img = Image::new(0x1000, &code);
    let f = discover(&img, 0x1000, Some((0x1000, 0x1010)), 100).unwrap();
    assert!(f.blocks.contains(&0x1008), "{:X?}", f.blocks);
    assert_eq!(f.insts.len(), 4);
}

#[test]
fn program_discovery_follows_calls_and_code_pointers() {
    let base = 0x1000;
    let w = [
        bl(0x1000, 0x1010), // 0x1000
        0xE1A0_E00F,        // 0x1004 mov lr, pc
        0xE12F_FF1C,        // 0x1008 bx r12
        0xE12F_FF1E,        // 0x100C bx lr (return site)
        0xE59F_1008,        // 0x1010 ldr r1, [pc, #8] -> 0x1020
        0xE59F_2008,        // 0x1014 ldr r2, [pc, #8] -> 0x1024
        0xE12F_FF11,        // 0x1018 bx r1
        0x0000_4770,        // 0x101C thumb: bx lr
        0x0000_101D,        // 0x1020 pointer to the Thumb function
        0x0000_1028,        // 0x1024 "pointer" to data
        UNDEF,              // 0x1028
    ];
    let code = bytes(&w);
    let img = Image::new(base, &code);
    assert!(matches!(decode_at(&img, 0x1028).unwrap().inst, Inst::Undecoded(_)));
    let is_code = |a: u32| (0x1000..0x102C).contains(&(a & !1));
    let funcs = discover_program(&img, &[0x1000], &is_code, 1000);
    assert_eq!(funcs.keys().copied().collect::<Vec<_>>(), vec![0x1000, 0x1010, 0x101D]);
    assert!(funcs[&0x1000].blocks.contains(&0x100C));
    // the caller stops at the callee's entry instead of swallowing it
    assert!(!funcs[&0x1000].insts.contains_key(&0x1010));
}

#[test]
fn computed_jump_into_an_unrolled_run_makes_every_instruction_a_block() {
    // SDK divide shape: add r2, r2, r2, lsl #1; add pc, pc, r2, lsl #2; (dead); 6 x add r0, #1; bx lr
    let mut w = vec![0xE082_2082, 0xE08F_F102, 0xE3A0_0009];
    w.extend([0xE280_0001; 6]);
    w.push(0xE12F_FF1E);
    let code = bytes(&w);
    let img = Image::new(0x1000, &code);
    let f = discover(&img, 0x1000, Some((0x1000, 0x1028)), 100).unwrap();
    for a in (0x100C..=0x1024).step_by(4) {
        assert!(f.blocks.contains(&a), "{a:#X} {:X?}", f.blocks);
    }
    // bounds that cut the run (metadata splitting one routine): the run stops at the bound
    let g = discover(&img, 0x1000, Some((0x1000, 0x1014)), 100).unwrap();
    assert!(g.blocks.contains(&0x1010) && !g.insts.contains_key(&0x1014));
}

#[test]
fn context_save_resume_point_is_a_block() {
    // stmia r1, {r0-r15}; mov r0, #0; bx lr; mov r0, #1 (resumed here: stored pc = addr + 12); bx lr
    let code = bytes(&[0xE881_FFFF, 0xE3A0_0000, 0xE12F_FF1E, 0xE3A0_0001, 0xE12F_FF1E]);
    let img = Image::new(0x1000, &code);
    let f = discover(&img, 0x1000, Some((0x1000, 0x1014)), 100).unwrap();
    assert!(f.blocks.contains(&0x100C), "{:X?}", f.blocks);
}

#[test]
fn fall_exits_and_extra_entry_points() {
    // mov r0, #1; bl 0x2000 (a call that may not return); <end of function>
    let code = bytes(&[0xE3A0_0001, bl(0x1004, 0x2000), 0xE3A0_0002, 0xE12F_FF1E]);
    let img = Image::new(0x1000, &code);
    let f = discover(&img, 0x1000, Some((0x1000, 0x1008)), 100).unwrap();
    assert_eq!(fall_exits(&f), vec![0x1008]);
    // another function enters at 0x1008: with the extra entry point the code there is lifted
    let g = discover_seeded(&img, 0x1000, &[0x1008], &|a| (0x1000..0x1010).contains(&a), 100).unwrap();
    assert!(g.blocks.contains(&0x1008) && g.insts.contains_key(&0x100C));
}

#[path = "../build_support.rs"]
#[allow(dead_code)]
mod build_support;

/// Real ROM (`MKDS_ROM`): the ARM7 sections are found and discovery lifts the program with no
/// undecodable site. Prints the counts (run with `--nocapture`).
#[test]
fn arm7_program_discovery_on_the_rom() {
    let Ok(path) = std::env::var("MKDS_ROM") else {
        eprintln!("MKDS_ROM not set: skipped");
        return;
    };
    let rom = std::fs::read(path).unwrap();
    let (entry, secs) = build_support::arm7_sections(&rom).unwrap();
    let img = Image { segs: secs.iter().map(|s| (s.ram_address, s.data.as_slice())).collect() };
    let ranges: Vec<(u32, u32)> = secs.iter().map(|s| (s.ram_address, s.ram_address + s.data.len() as u32)).collect();
    let is_code = |a: u32| ranges.iter().any(|&(lo, hi)| (lo..hi).contains(&(a & !1)));
    let funcs = discover_program(&img, &[entry], &is_code, 20_000);
    let insts: usize = funcs.values().map(|f| f.insts.len()).sum();
    let undecoded: Vec<_> = funcs.values().flat_map(|f| f.undecoded.iter().copied()).collect();
    for s in &secs {
        let n = funcs.keys().filter(|&&e| (s.ram_address..s.ram_address + s.data.len() as u32).contains(&(e & !1))).count();
        eprintln!("{} {:#010X} {:#X} bytes: {n} functions", s.name, s.ram_address, s.data.len());
    }
    eprintln!("ARM7: {} functions, {} instructions, {} thumb, undecoded {:X?}", funcs.len(), insts,
        funcs.keys().filter(|&&e| e & 1 != 0).count(), &undecoded[..undecoded.len().min(20)]);
    assert!(funcs.len() > 50);
}

#[test]
fn thumb_switch_table_cases_are_blocks() {
    // 0x00: add r1, r1, r1; add r1, pc; ldrh r1, [r1, #8]; lsl r1, #16; asr r1, #16; add r1, pc; bx r1
    // 0x0E: table of 2 offsets from 0x0E (bit 0 = Thumb): 0x05 -> 0x13, 0x07 -> 0x15; then 2 cases: bx lr
    let h: [u16; 11] = [0x1849, 0x4479, 0x8909, 0x0409, 0x1409, 0x4479, 0x4708, 0x0005, 0x0007, 0x4770, 0x4770];
    let code: Vec<u8> = h.iter().flat_map(|x| x.to_le_bytes()).collect();
    let img = Image::new(0x1000, &code);
    let f = discover(&img, 0x1001, Some((0x1000, 0x1016)), 100).unwrap();
    assert!(f.blocks.contains(&0x1013) && f.blocks.contains(&0x1015), "{:X?}", f.blocks);
}

#[test]
fn thumb_switch_split_by_metadata_still_finds_its_cases() {
    // Same switch as above, but the metadata function starts at the ldrh (0x1004): the table base
    // (`add r1, pc` at 0x1002) belongs to the previous function. Seen in ov9_000 at 0x0218173C
    // (Nintendo WFC menu), where the cases were interpreted.
    let h: [u16; 11] = [0x1849, 0x4479, 0x8909, 0x0409, 0x1409, 0x4479, 0x4708, 0x0005, 0x0007, 0x4770, 0x4770];
    let code: Vec<u8> = h.iter().flat_map(|x| x.to_le_bytes()).collect();
    let img = Image::new(0x1000, &code);
    let f = discover(&img, 0x1005, Some((0x1004, 0x1016)), 100).unwrap();
    assert!(f.blocks.contains(&0x1013) && f.blocks.contains(&0x1015), "{:X?}", f.blocks);
}
