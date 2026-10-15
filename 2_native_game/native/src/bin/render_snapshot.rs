//! Private local renderer diagnostic; snapshots contain user-ROM assets and stay local.
use recomp::rt::Mem;
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: render_snapshot <capture-dir>")?,
    );
    let mut hw = recomp::ds_mem::Bus::new();
    for (name, target) in [
        ("io.bin", &mut hw.io),
        ("palette.bin", &mut hw.palette),
        ("oam.bin", &mut hw.oam),
    ] {
        let source = std::fs::read(dir.join(name))?;
        if target.len() != source.len() {
            return Err(format!("Invalid snapshot length: {name}").into());
        }
        target.copy_from_slice(&source);
    }
    for (i, target) in hw.vram.iter_mut().enumerate() {
        let source = std::fs::read(dir.join(format!("vram_{i}.bin")))?;
        if target.len() != source.len() {
            return Err(format!("Invalid VRAM snapshot: {i}").into());
        }
        target.copy_from_slice(&source);
    }
    eprintln!(
        "Display A={:#x} B={:#x}",
        hw.r32(0x0400_0000),
        hw.r32(0x0400_1000)
    );
    for (engine, name) in [(0, "rerender_a.ppm"), (1, "rerender_b.ppm")] {
        let pixels = recomp::gfx2d::render_bus(&mut hw, engine);
        std::fs::write(dir.join(name), recomp::gfx2d::to_ppm(&pixels))?;
    }
    Ok(())
}
