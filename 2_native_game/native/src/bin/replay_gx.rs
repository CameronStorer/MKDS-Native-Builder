//! Replay a private GXTR0001 trace against the matching local VRAM snapshot.
use recomp::{dispatch::Dispatcher, machine::Machine, rt::Mem};
use std::io::{BufReader, Read};
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let trace = args
        .next()
        .ok_or("usage: replay_gx <trace> <matching-capture-directory>")?;
    let dir = std::path::PathBuf::from(args.next().ok_or("missing capture directory")?);
    if args.next().is_some() {
        return Err("unexpected replay argument".into());
    }
    let mut input = BufReader::new(std::fs::File::open(trace)?);
    let mut header = [0; 8];
    input.read_exact(&mut header)?;
    if &header != b"GXTR0001" {
        return Err("Unknown GX trace format".into());
    }
    let dispatcher = Dispatcher::new(&[]);
    let mut machine = Machine::new(recomp::ds_mem::Bus::new(), &dispatcher);
    let mut records = 0u64;
    loop {
        let mut record = [0u8; 9];
        if input.read(&mut record[..1])? == 0 {
            break;
        }
        input.read_exact(&mut record[1..])?;
        let off = u32::from_le_bytes(record[..4].try_into()?);
        let value = u32::from_le_bytes(record[4..8].try_into()?);
        if !(0x400..0x600).contains(&off) || off & 3 != 0 || record[8] != 4 {
            return Err(format!(
                "Unsupported GX record {records}: offset {off:#x}, width {}",
                record[8]
            )
            .into());
        }
        machine.hw.gx.push((off, value, record[8]));
        if machine.hw.gx.len() >= 4096 {
            machine.sync_gx();
        }
        records += 1;
    }
    machine.sync_gx();
    for (name, target) in [
        ("io.bin", &mut machine.hw.io),
        ("palette.bin", &mut machine.hw.palette),
        ("oam.bin", &mut machine.hw.oam),
    ] {
        let source = std::fs::read(dir.join(name))?;
        if target.len() != source.len() {
            return Err(format!("Invalid snapshot length: {name}").into());
        }
        target.copy_from_slice(&source);
    }
    for (i, target) in machine.hw.vram.iter_mut().enumerate() {
        let source = std::fs::read(dir.join(format!("vram_{i}.bin")))?;
        if target.len() != source.len() {
            return Err(format!("Invalid VRAM snapshot {i}").into());
        }
        target.copy_from_slice(&source);
    }
    let layer = machine.render_frame3d();
    let a = recomp::gfx2d::render_bus_with_3d(&mut machine.hw, 0, Some(&layer));
    let b = recomp::gfx2d::render_bus(&mut machine.hw, 1);
    let a_top = machine.hw.r16(0x0400_0304) & 0x8000 != 0;
    let (top, bottom) = if a_top { (a, b) } else { (b, a) };
    std::fs::write(dir.join("replay_top.ppm"), recomp::gfx2d::to_ppm(&top))?;
    std::fs::write(
        dir.join("replay_bottom.ppm"),
        recomp::gfx2d::to_ppm(&bottom),
    )?;
    eprintln!(
        "Replayed {records} writes; {} swaps; {} completed polygons; unknown {:?}",
        machine.gx.swaps,
        machine.frame_polys.len(),
        machine.gx.unknown.iter().take(16).collect::<Vec<_>>()
    );
    Ok(())
}
