//! Block 2 comparator: renders every `MKSNAP01` snapshot in a folder with the software reference and the GPU path and
//! reports how many pixels match. Snapshots hold game graphics: keep them and the images written here local.
//! `render_compare <snapshot-dir> [--min PCT] [--floor PCT] [--images OUT-DIR] [--self-test]`
//!   --min    required average share of identical pixels (default 99)
//!   --floor  required share for every single frame (default 97)
//!   --images write `<name>_soft.ppm`, `<name>_gpu.ppm`, `<name>_diff.ppm` for frames below --min
//!   --self-test compare software against software (checks the tool; must be 100%)
//!   --split-check  check that both screens rebuilt from `render_split` + the software 3D layer equal the
//!                  software screens exactly (the CPU form of the GPU compositor)
//!   --composite    full screens through the GPU compositor: with the software 3D layer (must be exact) and
//!                  with the GPU 3D layer (gated by --min/--floor)
//!   --dump-textures DIR  asynchronously dump original RGBA8 textures by stable ID
//!   --texture-pack DIR   lazily load replacements named by stable ID
//!   --upscale-pack IN OUT  standalone helper: nearest-upscale dumped PNGs by 2
//!   --dump   write every snapshot's software screens as `<name>_screens.ppm` into the --images folder
use mkds_native::gpu::{Compositor, Diff, Gpu, Renderer3d, Rgba, compose};
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
    let first = args.next().ok_or("usage: render_compare <snapshot-dir> [--min PCT] [--floor PCT] [--images DIR] [--self-test]")?;
    if first == "--upscale-pack" {
        let input = PathBuf::from(args.next().ok_or("--upscale-pack needs IN OUT")?);
        let output = PathBuf::from(args.next().ok_or("--upscale-pack needs IN OUT")?);
        return upscale_pack(&input, &output);
    }
    let dir = PathBuf::from(first);
    let (mut dump_textures, mut texture_pack) = (None::<PathBuf>, None::<PathBuf>);
    let (mut min, mut floor, mut images, mut self_test, mut dump, mut split_check, mut composite) = (
        99.0f64,
        97.0f64,
        None::<PathBuf>,
        false,
        false,
        false,
        false,
    );
    while let Some(a) = args.next() {
        let mut num = |name: &str| {
            args.next()
                .and_then(|v| v.parse::<f64>().ok())
                .ok_or(format!("{name} needs a number"))
        };
        match a.as_str() {
            "--min" => min = num("--min")?,
            "--floor" => floor = num("--floor")?,
            "--dump-textures" => {
                dump_textures = Some(args.next().ok_or("--dump-textures needs a folder")?.into())
            }
            "--texture-pack" => {
                texture_pack = Some(args.next().ok_or("--texture-pack needs a folder")?.into())
            }
            "--images" => images = Some(args.next().ok_or("--images needs a folder")?.into()),
            "--self-test" => self_test = true,
            "--dump" => dump = true,
            "--split-check" => split_check = true,
            "--composite" => composite = true,
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
    let gpu = if self_test {
        None
    } else {
        Some(Gpu::headless()?)
    };
    let mut renderer = gpu.as_ref().map(|g| Renderer3d::new(g, 1));
    if let Some(r) = &mut renderer {
        r.set_texture_dump(dump_textures);
        if texture_pack.is_some() {
            println!(
                "Indexed {} replacement PNGs",
                r.set_texture_pack(texture_pack)?
            );
        }
    }
    let mut render_time = std::time::Duration::ZERO;
    let mut render_readback_time = std::time::Duration::ZERO;
    let mut compositor = gpu.as_ref().map(|g| Compositor::new(g, 1));
    let mut exact_failures = 0usize;
    if let Some(g) = &gpu {
        let info = g.adapter.get_info();
        println!("GPU: {} ({:?})", info.name, info.backend);
    }
    let (mut sum, mut worst, mut failed) = (0.0, (101.0f64, String::new()), 0usize);
    for path in &files {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let snap = RenderSnapshot::decode(&std::fs::read(path).map_err(|e| e.to_string())?)?;
        if let (Some(out), true) = (&images, dump) {
            std::fs::write(
                out.join(format!("{name}_screens.ppm")),
                ppm(
                    &snap
                        .screens()
                        .iter()
                        .map(|p| p << 8 | 255)
                        .collect::<Vec<_>>(),
                    256,
                    384,
                ),
            )
            .map_err(|e| e.to_string())?;
        }
        if split_check {
            let layer = snap.layer3d();
            let (a, b, a_top) = snap.splits();
            let (a, b) = (a.compose(Some(&layer)), b.compose(None));
            let (top, bottom) = if a_top { (a, b) } else { (b, a) };
            let rebuilt: Vec<Rgba> = top.into_iter().chain(bottom).map(|p| p >> 8).collect();
            let soft = snap.screens();
            let d = Diff::of(&soft, &rebuilt);
            if d.identical != d.pixels {
                failed += 1;
                println!(
                    "{name}  split differs: {} pixels, max channel {}",
                    d.pixels - d.identical,
                    d.max_channel
                );
            }
            sum += d.percent();
            if d.percent() < worst.0 {
                worst = (d.percent(), name.clone());
            }
            continue;
        }
        if let (true, Some(g), Some(r), Some(c)) =
            (composite, &gpu, renderer.as_mut(), compositor.as_mut())
        {
            let soft: Vec<Rgba> = snap.screens().iter().map(|p| p << 8 | 255).collect();
            let (a, b, a_top) = snap.splits();
            let soft3d = compose::upload_layer(g, &snap.layer3d());
            let exact = g.read_rgba(c.render(g, &a, &b, a_top, &soft3d));
            let e = Diff::of(&soft, &exact);
            if e.identical != e.pixels {
                exact_failures += 1;
                println!(
                    "{name}  compositor with the software 3D layer differs: {} pixels",
                    e.pixels - e.identical
                );
            }
            let layer = r.render(g, &snap);
            let other = g.read_rgba(c.render(g, &a, &b, a_top, layer));
            let d = Diff::of(&soft, &other);
            let pct = d.percent();
            sum += pct;
            if pct < worst.0 {
                worst = (pct, name.clone());
            }
            let bad = pct < floor;
            failed += bad as usize;
            println!(
                "{name}  screens identical {pct:7.3}%  max channel diff {:3}{}",
                d.max_channel,
                if bad { "  BELOW FLOOR" } else { "" }
            );
            if let (Some(out), true) = (&images, pct < min) {
                for (suffix, px) in [("soft_screens", &soft), ("gpu_screens", &other)] {
                    std::fs::write(out.join(format!("{name}_{suffix}.ppm")), ppm(px, 256, 384))
                        .map_err(|e| e.to_string())?;
                }
            }
            continue;
        }
        let soft = snap.layer3d();
        let other = match (&gpu, renderer.as_mut()) {
            (Some(g), Some(r)) => {
                let start = std::time::Instant::now();
                let t = r.render(g, &snap);
                render_time += start.elapsed();
                let pixels = g.read_rgba(t);
                render_readback_time += start.elapsed();
                pixels
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
        println!(
            "{name}  polys {:5}  identical {pct:7.3}%  max channel diff {:3}{}",
            snap.polys.len(),
            d.max_channel,
            if bad { "  BELOW FLOOR" } else { "" }
        );
        if let (Some(out), true) = (&images, pct < min) {
            let diff: Vec<Rgba> = soft
                .iter()
                .zip(&other)
                .map(|(a, b)| if a == b { 0x0000_00FF } else { 0xFF00_FFFF })
                .collect();
            for (suffix, px) in [("soft", &soft), ("gpu", &other), ("diff", &diff)] {
                std::fs::write(out.join(format!("{name}_{suffix}.ppm")), ppm(px, 256, 192))
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    let avg = sum / files.len() as f64;
    println!(
        "{} frames: average {avg:.3}% identical, worst {:.3}% ({}), {failed} below the {floor}% floor",
        files.len(),
        worst.0,
        worst.1
    );
    if composite {
        println!(
            "compositor with the software 3D layer: {} of {} frames exact",
            files.len() - exact_failures,
            files.len()
        );
    }
    if avg < min || failed > 0 || exact_failures > 0 {
        return Err(format!(
            "FAILED: needs average >= {min}% and every frame >= {floor}%"
        ));
    }
    if !self_test && !composite && !split_check {
        println!(
            "Render CPU submission: {:.3} ms/frame; render + GPU completion/readback: {:.3} ms/frame (includes first-use decoding)",
            render_time.as_secs_f64() * 1000.0 / files.len() as f64,
            render_readback_time.as_secs_f64() * 1000.0 / files.len() as f64
        );
    }
    // Closing the writer here guarantees that --dump-textures is complete at exit.
    drop(renderer);
    println!("PASSED");
    Ok(())
}

/// Standalone corpus helper; it never needs a GPU or reads snapshots.
fn upscale_pack(input: &std::path::Path, output: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(output).map_err(|e| e.to_string())?;
    let mut count = 0;
    for entry in std::fs::read_dir(input).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_none_or(|s| s != "png") {
            continue;
        }
        let mut reader = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(&path).map_err(|e| e.to_string())?,
        ))
        .read_info()
        .map_err(|e| e.to_string())?;
        let mut bytes = vec![0; reader.output_buffer_size().ok_or("PNG too large")?];
        let info = reader.next_frame(&mut bytes).map_err(|e| e.to_string())?;
        if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
            return Err(format!("{}: expected RGBA8", path.display()));
        }
        let (w, h) = (info.width as usize, info.height as usize);
        let mut scaled = vec![0; w * h * 16];
        for y in 0..h * 2 {
            for x in 0..w * 2 {
                scaled[(y * w * 2 + x) * 4..][..4]
                    .copy_from_slice(&bytes[((y / 2) * w + x / 2) * 4..][..4]);
            }
        }
        let file = std::fs::File::create(output.join(path.file_name().unwrap()))
            .map_err(|e| e.to_string())?;
        let mut encoder = png::Encoder::new(
            std::io::BufWriter::new(file),
            info.width * 2,
            info.height * 2,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer
            .write_image_data(&scaled)
            .map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
        count += 1;
    }
    println!("Upscaled {count} textures by 2");
    Ok(())
}
