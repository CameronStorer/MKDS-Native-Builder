use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
};

pub(super) fn read_png(path: &Path) -> Result<(u32, u32, Vec<u32>), String> {
    let load = || -> Result<_, Box<dyn std::error::Error>> {
        let mut decoder = png::Decoder::new(BufReader::new(File::open(path)?));
        decoder.set_limits(png::Limits {
            bytes: 512 * 1024 * 1024,
        });
        // Any PNG an image editor writes (RGB, grey, palette, 16-bit) becomes 8-bit with alpha.
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16 | png::Transformations::ALPHA);
        let mut reader = decoder.read_info()?;
        let info = reader.info();
        if info.width > 8192 || info.height > 8192 {
            return Err("replacement exceeds 8192 texels".into());
        }
        let mut bytes = vec![0; reader.output_buffer_size().ok_or("PNG too large")?];
        let frame = reader.next_frame(&mut bytes)?;
        let data = &bytes[..frame.buffer_size()];
        let pixels = match (frame.color_type, frame.bit_depth) {
            (png::ColorType::Rgba, png::BitDepth::Eight) => {
                data.chunks_exact(4).map(|b| u32::from_be_bytes(b.try_into().unwrap())).collect()
            }
            (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight) => {
                data.chunks_exact(2).map(|b| u32::from_be_bytes([b[0], b[0], b[0], b[1]])).collect()
            }
            (c, d) => return Err(format!("unsupported PNG layout {c:?} {d:?}").into()),
        };
        Ok((frame.width, frame.height, pixels))
    };
    load().map_err(|e| format!("{}: {e}", path.display()))
}

pub(super) fn write_png(
    path: &Path,
    width: u32,
    height: u32,
    pixels: &[u32],
) -> Result<(), String> {
    let write = || -> Result<(), Box<dyn std::error::Error>> {
        let file = match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        let bytes: Vec<u8> = pixels.iter().flat_map(|p| p.to_be_bytes()).collect();
        writer.write_image_data(&bytes)?;
        writer.finish()?;
        Ok(())
    };
    write().map_err(|e| format!("{}: {e}", path.display()))
}

type DumpJob = (u64, u32, u32, Vec<u32>);
pub(super) struct Dumper {
    sender: Option<mpsc::Sender<DumpJob>>,
    worker: Option<thread::JoinHandle<()>>,
    seen: HashSet<u64>,
}
impl Dumper {
    pub fn new(dir: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel::<DumpJob>();
        let worker = thread::spawn(move || {
            if let Err(e) = fs::create_dir_all(&dir) {
                eprintln!("Texture dump {}: {e}", dir.display());
                return;
            }
            for (id, width, height, pixels) in receiver {
                if let Err(e) =
                    write_png(&dir.join(format!("{id:016x}.png")), width, height, &pixels)
                {
                    eprintln!("Texture dump: {e}");
                }
            }
        });
        Self {
            sender: Some(sender),
            worker: Some(worker),
            seen: HashSet::new(),
        }
    }
    pub fn needs(&mut self, id: u64) -> bool {
        self.seen.insert(id)
    }
    pub fn send(&self, id: u64, width: usize, height: usize, pixels: Vec<u32>) {
        let _ = self
            .sender
            .as_ref()
            .unwrap()
            .send((id, width as u32, height as u32, pixels));
    }
}
impl Drop for Dumper {
    fn drop(&mut self) {
        self.sender.take();
        // Configuration changes and renderer shutdown flush pending files; rendering never waits.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone)]
pub(super) struct Replacement {
    pub pixels: Arc<Vec<u32>>,
    pub factor: u32,
}
enum PackEntry {
    Pending(PathBuf),
    Ready(Replacement),
    Rejected,
}
#[derive(Default)]
pub(super) struct Pack {
    entries: HashMap<u64, PackEntry>,
}
impl Pack {
    pub fn index(dir: &Path) -> Result<Self, String> {
        let mut entries = HashMap::new();
        for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().is_none_or(|s| s != "png") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem.len() != 16
                || !stem
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                continue;
            }
            let id = u64::from_str_radix(stem, 16).unwrap();
            entries.insert(id, PackEntry::Pending(path));
        }
        Ok(Self { entries })
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn get(&mut self, id: u64, width: usize, height: usize) -> Option<Replacement> {
        let entry = self.entries.get_mut(&id)?;
        if let PackEntry::Pending(path) = entry {
            let loaded = read_png(path).and_then(|(w, h, pixels)| {
                let f = w as usize / width;
                if !(1..=8).contains(&f) || w as usize != width * f || h as usize != height * f {
                    return Err(format!("{}: expected {width}x{height} times the same integer factor 1-8, got {w}x{h}", path.display()));
                }
                Ok(Replacement { pixels: Arc::new(pixels), factor: f as u32 })
            });
            *entry = match loaded {
                Ok(replacement) => PackEntry::Ready(replacement),
                Err(e) => {
                    eprintln!("Ignoring texture replacement {e}");
                    PackEntry::Rejected
                }
            };
        }
        match entry {
            PackEntry::Ready(r) => Some(r.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn tempdir() -> tempfile::TempDir {
        let root = std::env::temp_dir().join("codex_b2_tex");
        fs::create_dir_all(&root).unwrap();
        tempfile::tempdir_in(root).unwrap()
    }
    #[test]
    fn editor_pngs_without_alpha_load_as_opaque() {
        let dir = tempdir();
        for (color, bytes) in [(png::ColorType::Rgb, vec![1u8, 2, 3]), (png::ColorType::Grayscale, vec![9u8])] {
            let path = dir.path().join(format!("{color:?}.png"));
            let mut e = png::Encoder::new(BufWriter::new(File::create(&path).unwrap()), 1, 1);
            e.set_color(color);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&bytes).unwrap();
            let want = if bytes.len() == 3 { 0x010203ff } else { 0x090909ff };
            assert_eq!(read_png(&path).unwrap(), (1, 1, vec![want]));
        }
    }

    #[test]
    fn dump_once_and_keep_existing_rgba() {
        let dir = tempdir();
        let mut dumper = Dumper::new(dir.path().into());
        for id in [42, 42, 43] {
            if dumper.needs(id) {
                dumper.send(id, 1, 1, vec![0x12345600]);
            }
        }
        drop(dumper);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
        let path = dir.path().join("000000000000002a.png");
        assert_eq!(read_png(&path).unwrap(), (1, 1, vec![0x12345600]));
        let mut dumper = Dumper::new(dir.path().into());
        assert!(dumper.needs(42));
        dumper.send(42, 1, 1, vec![0xffffffff]);
        drop(dumper);
        assert_eq!(read_png(&path).unwrap().2, vec![0x12345600]);
    }
    #[test]
    fn wrong_size_is_warned_and_rejected_once() {
        let dir = tempdir();
        let path = dir.path().join("000000000000002a.png");
        write_png(&path, 16, 8, &[0; 128]).unwrap();
        let mut pack = Pack::index(dir.path()).unwrap();
        assert_eq!(pack.len(), 1);
        assert!(pack.get(42, 8, 8).is_none());
        assert!(matches!(pack.entries[&42], PackEntry::Rejected));
        // Even replacing the file cannot retry or warn until the pack is reloaded.
        fs::remove_file(&path).unwrap();
        write_png(&path, 16, 16, &[0; 256]).unwrap();
        assert!(pack.get(42, 8, 8).is_none());
        assert_eq!(
            Pack::index(dir.path())
                .unwrap()
                .get(42, 8, 8)
                .unwrap()
                .factor,
            2
        );
    }
}
