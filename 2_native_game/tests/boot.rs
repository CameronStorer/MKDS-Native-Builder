//! Boot probe (T060): load the user's ROM into the DS memory model, run from the ARM9 entry point
//! under `Machine` (lifted code, interpreter for anything not lifted) and report where it stops and
//! which services nothing implements yet. `cargo test --test boot -- --ignored --nocapture`.
//! Set MKDS_BOOT_BUDGET (instructions, default 20,000,000) and MKDS_ROM as usual.
use recomp::dispatch::Dispatcher;
use recomp::ds_mem;
use recomp::machine::Machine;
use recomp::rt::{BUDGET_EXHAUSTED, Bus, Cpu, Mem};

include!(concat!(env!("OUT_DIR"), "/lifted.rs"));

#[test]
#[ignore]
fn boot_probe() {
    let rom_path = std::env::var_os("MKDS_ROM")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gamefile.nds"));
    let Ok(rom) = std::fs::read(rom_path) else {
        eprintln!("no ROM: skipped");
        return;
    };
    let mut hw = ds_mem::Bus::new();
    for s in mkds_builder::extract::arm9_sections(&rom).unwrap() {
        for (i, b) in s.data.iter().enumerate() {
            hw.w8(s.ram_address.wrapping_add(i as u32), *b);
        }
        eprintln!("loaded {} at {:#010X} ({} bytes, bss {})", s.name, s.ram_address, s.data.len(), s.bss_size);
    }
    let entry = u32::from_le_bytes(rom[0x24..0x28].try_into().unwrap());
    let disp = if std::env::var_os("MKDS_BOOT_INTERP").is_some() { Dispatcher::new(&[]) } else { Dispatcher::new(LIFTED) };
    let mut m = Machine::new(hw, &disp);
    // the loader hands over an already-decompressed image: skip the SDK start-up decompression
    if std::env::var_os("MKDS_BOOT_NOUNPACK").is_some() {
        m.hook(0x0200_0934, recomp::machine::Hook::Nop);
    }
    m.arm7_hle_boot();
    let mut c = Cpu::default();
    c.r[13] = 0x0380_FF00; // boot stack (the real one is set by _start)
    c.budget = std::env::var("MKDS_BOOT_BUDGET").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000_000);
    let stop = 0xFFFF_FFF0;
    let end = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| m.call(&mut c, entry, stop)));
    match end {
        Ok(pc) if pc == BUDGET_EXHAUSTED => eprintln!("budget exhausted; r15 area: r14={:#010X} sp={:#010X}", c.r[14], c.r[13]),
        Ok(pc) => eprintln!("returned to {pc:#010X}"),
        Err(e) => eprintln!("PANIC: {:?}", e.downcast_ref::<String>().cloned().or(e.downcast_ref::<&str>().map(|s| s.to_string()))),
    }
    eprintln!("last interpreted pc {:#010X}", recomp::interp::LAST_PC.load(std::sync::atomic::Ordering::Relaxed));
    if let Ok(list) = std::env::var("MKDS_PEEK") {
        for a in list.split(',') {
            let a = u32::from_str_radix(a.trim_start_matches("0x"), 16).unwrap();
            eprintln!("peek {a:#010X} = {:#010X}", m.hw.r32(a));
        }
    }
    eprintln!("ipc_out {:08X?}", m.hw.ipc_out);
    eprintln!("regs: {:08X?}", c.r);
    eprintln!("budget left {}, hw cycles {}, gx words {}, ipc words {}", c.budget, m.hw.cycles, m.hw.gx.len(), m.hw.ipc_out.len());
    let mut u: Vec<_> = m.unhandled.iter().collect();
    u.sort_by(|a, b| b.1.cmp(a.1));
    for (k, v) in u {
        eprintln!("unhandled: {k} x{v}");
    }
}
