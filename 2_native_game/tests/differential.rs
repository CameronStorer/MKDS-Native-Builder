//! Differential test: every lifted function must behave exactly like the reference interpreter
//! on random register/memory states: same exit pc, registers, flags, memory writes and the same
//! calls / BIOS calls / coprocessor writes in the same order. Selects the exact
//! ROM image used by the build report; budget-exhausted cases are counted separately.
use recomp::interp;
use recomp::mem::Ram;
use recomp::rt::{BUDGET_EXHAUSTED, Cpu};

include!(concat!(env!("OUT_DIR"), "/lifted.rs"));

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn random_cpu(rng: &mut Rng) -> Cpu {
    let mut c = Cpu::default();
    for i in 0..13 {
        c.r[i] = if rng.next() & 1 == 0 {
            rng.next() as u32
        } else {
            0x0230_0000 + ((rng.next() as u32 & 0xFFC) | 0x1000)
        };
    }
    c.r[13] = 0x0230_8000;
    c.r[14] = 0xDEAD_0000;
    let f = rng.next();
    c.n = f & 1 != 0;
    c.z = f & 2 != 0;
    c.c = f & 4 != 0;
    c.v = f & 8 != 0;
    c.q = f & 16 != 0;
    c.ctl = 0x1f | ((f as u32) & 0xc0);
    c.spsr = rng.next() as u32;
    c.budget = 3000;
    c
}

#[test]
fn lifted_matches_interpreter() {
    let rom_path = std::env::var_os("MKDS_ROM")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gamefile.nds")
        });
    let Ok(rom) = std::fs::read(rom_path) else {
        assert!(
            LIFTED.is_empty(),
            "compiled ROM functions exist but the test ROM is unavailable"
        );
        eprintln!("library-only build: synthetic reference tests still run");
        return;
    };
    let mut images = std::collections::BTreeMap::new();
    for section in mkds_builder::extract::arm9_sections(&rom).unwrap() {
        images.insert(section.name, (section.ram_address, section.data));
    }
    for overlay in mkds_builder::extract::arm9_overlays(&rom).unwrap() {
        images.insert(
            format!("ov9_{:03}", overlay.id),
            (overlay.ram_address, overlay.data),
        );
    }
    eprintln!("{} functions lifted", LIFTED.len());
    assert!(!LIFTED.is_empty(), "no function could be lifted");
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let (mut runs, mut compared, mut stores, mut events, mut exhausted) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    for l in LIFTED {
        let (base, image) = images.get(l.segment).expect("lifted image absent from ROM");
        for _ in 0..30 {
            let seed = rng.next();
            let start = random_cpu(&mut rng);
            let (mut c1, mut m1) = (start.clone(), Ram::new(image, *base, seed));
            let (mut c2, mut m2) = (start.clone(), Ram::new(image, *base, seed));
            let e1 = (l.f)(&mut c1, &mut m1, l.entry);
            let e2 = interp::run(&mut c2, &mut m2, l.blocks, l.insts, l.entry);
            runs += 1;
            if e1 == BUDGET_EXHAUSTED || e2 == BUDGET_EXHAUSTED {
                exhausted += 1;
                continue; // budgets are spent per block vs per instruction: stopping points differ
            }
            assert_eq!(e1, e2, "exit pc differs for {:#X}", l.entry);
            assert_eq!(
                c1.state(),
                c2.state(),
                "registers/flags differ for {:#X}",
                l.entry
            );
            assert_eq!(
                m1.writes, m2.writes,
                "memory writes differ for {:#X}",
                l.entry
            );
            assert_eq!(
                m1.events, m2.events,
                "calls/BIOS/coprocessor events differ for {:#X}",
                l.entry
            );
            assert_eq!(
                c1.budget, c2.budget,
                "completed function instruction count differs for {:#X}",
                l.entry
            );
            compared += 1;
            stores += m1.writes.len();
            events += m1.events.len();
        }
    }
    assert!(
        compared > 0,
        "all runs exhausted their budgets; no completed comparison"
    );
    eprintln!(
        "{runs} runs, {compared} completed comparisons, {exhausted} budget-exhausted cases, {stores} bytes written, {events} call/BIOS/CP events"
    );
}
