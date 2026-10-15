//! `mkds_gen` contract tests. Input-validation tests need no ROM; the end-to-end test runs only
//! when `MKDS_TEST_ROM` (and `MKDS_TEST_METADATA`) name a supported ROM and address-only metadata.
#![cfg(feature = "gen")]
use std::process::Command;

fn mkds_gen() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mkds_gen"))
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("mkds_gen_test_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn explicit_inputs_are_required() {
    for args in [vec![], vec!["--rom", "x.nds"], vec!["--rom", "x.nds", "--metadata", "m.csv"]] {
        let out = mkds_gen().args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
    }
}

#[test]
fn unreadable_rom_writes_nothing() {
    let dir = scratch("norom");
    let out = mkds_gen()
        .args(["--rom", "does-not-exist.nds", "--metadata", "does-not-exist.csv", "--out"])
        .arg(&dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(!dir.exists(), "failed run must not leave an output directory");
}

#[test]
fn end_to_end_accounts_for_every_entry_and_protects_foreign_dirs() {
    let (Some(rom), Some(meta)) = (std::env::var_os("MKDS_TEST_ROM"), std::env::var_os("MKDS_TEST_METADATA")) else {
        eprintln!("skipped: MKDS_TEST_ROM / MKDS_TEST_METADATA not set");
        return;
    };
    let dir = scratch("e2e");
    let run = |out: &std::path::Path| mkds_gen().arg("--rom").arg(&rom).arg("--metadata").arg(&meta).arg("--out").arg(out).output().unwrap();
    let first = run(&dir);
    assert_eq!(first.status.code(), Some(0), "{}", String::from_utf8_lossy(&first.stderr));
    let report = std::fs::read_to_string(dir.join("generation.json")).unwrap();
    let entries = std::fs::read_to_string(&meta).unwrap().lines().skip(1).filter(|l| !l.trim().is_empty()).count();
    let csv = std::fs::read_to_string(dir.join("coverage.csv")).unwrap();
    let rows = csv.lines().skip(1).filter(|l| !l.starts_with("discovered,")).count();
    assert_eq!(rows, entries, "one coverage row per metadata entry");
    // generator-discovered code: our ARM9 IRQ stub and the ARM7 program at its run-time addresses
    assert!(csv.lines().any(|l| l.starts_with("discovered,bios9,0xFFFF0018,")));
    assert!(csv.lines().any(|l| l.starts_with("discovered,arm7_main,0x02380000,")), "ARM7 entry lifted");
    assert!(csv.lines().filter(|l| l.starts_with("discovered,arm7_")).count() > 100);
    assert!(csv.lines().filter(|l| l.starts_with("discovered,")).all(|l| l.split(',').nth(7) == Some("0")), "no undecodable ARM7 site");
    assert!(report.contains("\"lift_cap\": null"));
    let lib = std::fs::read_to_string(dir.join("lifted/src/lib.rs")).unwrap();
    for item in ["ROM_SHA256", "pub static LIFTED7:", "pub const ARM7_SEGMENTS:", "pub static ARM7_SIGS:", "\"bios9\""] {
        assert!(lib.contains(item), "{item}");
    }
    // regenerating over our own output works; a foreign non-empty directory is refused
    assert_eq!(run(&dir).status.code(), Some(0));
    let foreign = scratch("foreign");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join("keep.txt"), "mine").unwrap();
    assert_eq!(run(&foreign).status.code(), Some(1));
    assert_eq!(std::fs::read_to_string(foreign.join("keep.txt")).unwrap(), "mine");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&foreign);
}

/// Real-ROM check of the emitted overlay signatures: with one overlay's image in an otherwise
/// blank RAM, exactly that overlay verifies even though several overlays share addresses.
#[test]
fn emitted_signatures_identify_exactly_the_resident_overlay() {
    let (Some(rom), Some(meta)) = (std::env::var_os("MKDS_TEST_ROM"), std::env::var_os("MKDS_TEST_METADATA")) else {
        eprintln!("skipped: MKDS_TEST_ROM / MKDS_TEST_METADATA not set");
        return;
    };
    let dir = scratch("sigs");
    let out = mkds_gen().arg("--rom").arg(&rom).arg("--metadata").arg(&meta).arg("--out").arg(&dir).arg("--no-game").output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let text = std::fs::read_to_string(dir.join("lifted/src/lib.rs")).unwrap();
    let mut sigs: Vec<(String, Vec<(u32, u32, u64)>)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("recomp::residency::SegmentSig { segment: \"") {
            sigs.push((rest.split('"').next().unwrap().to_string(), Vec::new()));
        } else if let Some(rest) = line.strip_prefix("recomp::residency::CodeRange { start: ") {
            let num = |s: &str| u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).unwrap();
            let start = num(rest.split(',').next().unwrap()) as u32;
            let len: u32 = rest.split("len: ").nth(1).unwrap().split(',').next().unwrap().trim().parse().unwrap();
            let hash = num(rest.split("hash: ").nth(1).unwrap().split(' ').next().unwrap());
            sigs.last_mut().unwrap().1.push((start, len, hash));
        }
    }
    assert!(sigs.len() >= 3, "expected overlay signatures, got {}", sigs.len());
    let bytes = std::fs::read(&rom).unwrap();
    let overlays = mkds_builder::extract::arm9_overlays(&bytes).unwrap();
    for target in &overlays {
        let name = format!("ov9_{:03}", target.id);
        if !sigs.iter().any(|(s, _)| *s == name) {
            continue;
        }
        let verified: Vec<&str> = sigs
            .iter()
            .filter(|(_, ranges)| {
                ranges.iter().all(|&(start, len, hash)| {
                    let Some(o) = start.checked_sub(target.ram_address) else { return false };
                    target.data.get(o as usize..(o + len) as usize).is_some_and(|b| recomp::residency::fnv1a64(b) == hash)
                })
            })
            .map(|(s, _)| s.as_str())
            .collect();
        assert_eq!(verified, [name.as_str()], "image of {name} must verify exactly itself");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
