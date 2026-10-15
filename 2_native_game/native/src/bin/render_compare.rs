//! Block 2 comparator: renders every `MKSNAP01` snapshot in a folder with the software reference and the GPU path and
//! reports how many pixels match. Snapshots hold game graphics: keep them and the images written here local.
//! `render_compare <snapshot-dir> [--min PCT] [--floor PCT] [--images OUT-DIR] [--self-test]`
//!   --min    required average share of identical pixels (default 99)
//!   --floor  required share for every single frame (default 97)
//!   --images write `<name>_soft.ppm`, `<name>_gpu.ppm`, `<name>_diff.ppm` for frames below --min
//!   --self-test compare software against software (checks the tool; must be 100%)
//!   --dump   write every snapshot's software screens as `<name>_screens.ppm` into the --images folder
use mkds_native::gpu::{Diff, Gpu, Renderer3d, Rgba};
use recomp::machine::RenderSnapshot;
use std::path::PathBuf;

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn ppm(pixels: &[Rgba], w: usize, h: usize) -> Vec<u8> {
    let mut o = format!("P6\n{w} {h}\n255\n").into_bytes();
    for p in pixels {
        o.extend_from_slice(&[(p >> 24) as u8, (p >> 16) as u8, (p >> 8) as u8]);
    }
    o
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().ok_or("usage: render_compare <snapshot-dir> [--min PCT] [--floor PCT] [--images DIR] [--self-test]")?);
    let (mut min, mut floor, mut images, mut self_test, mut dump) = (99.0f64, 97.0f64, None::<PathBuf>, false, false);
    while let Some(a) = args.next() {
        let mut num = |name: &str| args.next().and_then(|v| v.parse::<f64>().ok()).ok_or(format!("{name} needs a number"));
        match a.as_str() {
            "--min" => min = num("--min")?,
            "--floor" => floor = num("--floor")?,
            "--images" => images = Some(args.next().ok_or("--images needs a folder")?.into()),
            "--self-test" => self_test = true,
            "--dump" => dump = true,
            _ => return Err(format!("unknown option {a}")),
        }
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "mksnap"))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(format!("no .mksnap files in {}", dir.display()));
    }
    if let Some(out) = &images {
        std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    }
    let gpu = if self_test { None } else { Some(Gpu::headless()?) };
    let mut renderer = gpu.as_ref().map(|g| Renderer3d::new(g, 1));
    if let Some(g) = &gpu {
        let info = g.adapter.get_info();
        println!("GPU: {} ({:?})", info.name, info.backend);
    }
    let (mut sum, mut worst, mut failed) = (0.0, (101.0f64, String::new()), 0usize);
    for path in &files {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let snap = RenderSnapshot::decode(&std::fs::read(path).map_err(|e| e.to_string())?)?;
        if let (Some(out), true) = (&images, dump) {
            std::fs::write(out.join(format!("{name}_screens.ppm")), ppm(&snap.screens().iter().map(|p| p << 8 | 255).collect::<Vec<_>>(), 256, 384))
                .map_err(|e| e.to_string())?;
        }
        let soft = snap.layer3d();
        let other = match (&gpu, renderer.as_mut()) {
            (Some(g), Some(r)) => {
                let t = r.render(g, &snap);
                g.read_rgba(t)
            }
            _ => snap.layer3d(),
        };
        let d = Diff::of(&soft, &other);
        let pct = d.percent();
        sum += pct;
        if pct < worst.0 {
            worst = (pct, name.clone());
        }
        let bad = pct < floor;
        failed += bad as usize;
        println!("{name}  polys {:5}  identical {pct:7.3}%  max channel diff {:3}{}", snap.polys.len(), d.max_channel, if bad { "  BELOW FLOOR" } else { "" });
        if let (Some(out), true) = (&images, pct < min) {
            let diff: Vec<Rgba> = soft.iter().zip(&other).map(|(a, b)| if a == b { 0x0000_00FF } else { 0xFF00_FFFF }).collect();
            for (suffix, px) in [("soft", &soft), ("gpu", &other), ("diff", &diff)] {
                std::fs::write(out.join(format!("{name}_{suffix}.ppm")), ppm(px, 256, 192)).map_err(|e| e.to_string())?;
            }
        }
    }
    let avg = sum / files.len() as f64;
    println!("{} frames: average {avg:.3}% identical, worst {:.3}% ({}), {failed} below the {floor}% floor", files.len(), worst.0, worst.1);
    if avg < min || failed > 0 {
        return Err(format!("FAILED: needs average >= {min}% and every frame >= {floor}%"));
    }
    println!("PASSED");
    Ok(())
}
