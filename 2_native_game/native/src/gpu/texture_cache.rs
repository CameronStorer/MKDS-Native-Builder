use super::texture_pack::{Dumper, Pack};
use recomp::texture::{TexParams, decode};
use std::{collections::HashMap, path::PathBuf};

/// CPU mirror lets the GPU arena grow without changing any offset already assigned
/// in this frame. Uploads and the final bind group are made after all textures resolve.
pub(super) struct TextureCache {
    texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    entries: HashMap<u64, (u32, u32)>,
    pixels: Vec<u32>,
    uploaded: usize,
    dump: Option<Dumper>,
    pack: Pack,
    /// Texture memory of the previous frame and the slots its textures resolved to: while the bytes are unchanged
    /// (most frames), a texture's slot is reused without hashing its memory again.
    last_memory: (Vec<u8>, Vec<u8>),
    memo: HashMap<(u32, u32), (u32, u32)>,
}
const CAPACITY: usize = 16 * 1024 * 1024;
// A paged integer texture avoids the much smaller storage-buffer binding limit
// (a single legal 1024x1024 texture at factor 8 needs 256 MiB).
const SIDE: usize = 1024;
const PAGE: usize = SIDE * SIDE;
impl TextureCache {
    fn arena(device: &wgpu::Device, layers: u32) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("3d cached texels"),
            size: wgpu::Extent3d {
                width: SIDE as u32,
                height: SIDE as u32,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | if cfg!(test) {
                    wgpu::TextureUsages::COPY_SRC
                } else {
                    wgpu::TextureUsages::empty()
                },
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        (texture, view)
    }
    pub fn new(device: &wgpu::Device) -> Self {
        let (texture, view) = Self::arena(device, (CAPACITY / PAGE) as u32);
        Self {
            texture,
            view,
            entries: HashMap::new(),
            pixels: Vec::new(),
            uploaded: 0,
            dump: None,
            pack: Pack::default(),
            last_memory: (Vec::new(), Vec::new()),
            memo: HashMap::new(),
        }
    }
    pub fn set_dump(&mut self, path: Option<PathBuf>) {
        self.dump = path.map(Dumper::new);
    }
    pub fn set_pack(&mut self, path: Option<PathBuf>) -> Result<usize, String> {
        let pack = match path {
            Some(path) => Pack::index(&path)?,
            None => Pack::default(),
        };
        let count = pack.len();
        self.pack = pack;
        self.clear();
        Ok(count)
    }
    fn clear(&mut self) {
        self.memo.clear();
        self.entries.clear();
        self.pixels.clear();
        self.uploaded = 0;
    }
    pub fn begin_frame(&mut self, tex: &[u8], pal: &[u8]) {
        // Eviction is only legal before assigning this frame's offsets. Lazy PNG
        // results survive arena eviction, so no file is decoded or warned twice.
        if self.pixels.len() > CAPACITY / 2 {
            self.clear();
        }
        if self.last_memory.0 != tex || self.last_memory.1 != pal {
            self.memo.clear();
            self.last_memory = (tex.to_vec(), pal.to_vec());
        }
    }
    pub fn get(&mut self, reg: u32, base: u32, tex: &[u8], pal: &[u8]) -> (u32, u32) {
        if let Some(&slot) = self.memo.get(&(reg, base)) {
            return slot;
        }
        let slot = self.resolve(reg, base, tex, pal);
        self.memo.insert((reg, base), slot);
        slot
    }
    fn resolve(&mut self, reg: u32, base: u32, tex: &[u8], pal: &[u8]) -> (u32, u32) {
        let p = TexParams::from_reg(reg);
        if p.format == 0 {
            return (0, 1);
        }
        let id = fingerprint(&p, base, tex, pal);
        let mut original = None;
        if let Some(dump) = &mut self.dump {
            if dump.needs(id) {
                let pixels = decode(&p, base, tex, pal);
                dump.send(id, p.width, p.height, pixels.clone());
                original = Some(pixels);
            }
        }
        if let Some(&entry) = self.entries.get(&id) {
            return entry;
        }
        let offset = self.pixels.len() as u32;
        let factor = if let Some(r) = self.pack.get(id, p.width, p.height) {
            self.pixels.extend_from_slice(&r.pixels);
            r.factor
        } else {
            self.pixels
                .extend(original.unwrap_or_else(|| decode(&p, base, tex, pal)));
            1
        };
        self.entries.insert(id, (offset, factor));
        (offset, factor)
    }
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.uploaded == self.pixels.len() {
            return;
        }
        let layers = self.pixels.len().div_ceil(PAGE) as u32;
        if layers > self.texture.depth_or_array_layers() {
            let limit = device.limits().max_texture_array_layers;
            assert!(
                layers <= limit,
                "3D texture frame exceeds the device array arena limit ({limit} layers)"
            );
            (self.texture, self.view) = Self::arena(device, layers.next_power_of_two().min(limit));
            self.uploaded = 0;
        }
        // Re-upload the last partial row when appending. Complete rows use the CPU
        // mirror directly, and only the final row needs padding. Old offsets remain
        // valid even when a frame grows across several pages or replaces the arena.
        let mut at = self.uploaded / SIDE * SIDE;
        while at < self.pixels.len() {
            let page_end = ((at / PAGE + 1) * PAGE).min(self.pixels.len());
            let rows = (page_end - at) / SIDE;
            let write = |start: usize, rows: usize, pixels: &[u32]| {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: ((start % PAGE) / SIDE) as u32,
                            z: (start / PAGE) as u32,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytemuck::cast_slice(pixels),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(SIDE as u32 * 4),
                        rows_per_image: Some(rows as u32),
                    },
                    wgpu::Extent3d {
                        width: SIDE as u32,
                        height: rows as u32,
                        depth_or_array_layers: 1,
                    },
                );
            };
            if rows > 0 {
                write(at, rows, &self.pixels[at..at + rows * SIDE]);
                at += rows * SIDE;
            }
            if at < page_end {
                let mut row = [0; SIDE];
                row[..page_end - at].copy_from_slice(&self.pixels[at..page_end]);
                write(at, 1, &row);
                at = page_end;
            }
        }
        self.uploaded = self.pixels.len();
    }
}

struct Fnv(u64);
impl Fnv {
    fn bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x100000001b3);
        }
    }
}
pub(super) fn fingerprint(p: &TexParams, base: u32, tex: &[u8], pal: &[u8]) -> u64 {
    // Version-independent wire encoding: format + alpha flag (u8), dimensions
    // (u32 LE), then each reachable byte range prefixed by its length (u64 LE).
    // Addresses, wrap/flip and coordinate transform state are deliberately absent.
    let mut h = Fnv(0xcbf29ce484222325);
    h.bytes(&[
        p.format,
        u8::from((2..=4).contains(&p.format) && p.color0_transparent),
    ]);
    h.bytes(&(p.width as u32).to_le_bytes());
    h.bytes(&(p.height as u32).to_le_bytes());
    let n = p.width * p.height;
    let bytes = match p.format {
        2 | 5 => n / 4,
        3 => n / 2,
        7 => n * 2,
        _ => n,
    };
    let mut range = |data: &[u8], start: usize, len: usize| {
        // Missing source bytes decode as zero; include length to distinguish truncation.
        let bytes = data
            .get(start..start.saturating_add(len).min(data.len()))
            .unwrap_or(&[]);
        h.bytes(&(bytes.len() as u64).to_le_bytes());
        h.bytes(bytes);
    };
    range(tex, p.offset, bytes);
    let po = (base as usize) << if p.format == 2 { 3 } else { 4 };
    if p.format == 5 {
        let index = if p.offset >= 0x40000 {
            0x30000 + ((p.offset - 0x40000) >> 1)
        } else {
            0x20000 + (p.offset >> 1)
        };
        range(tex, index, n / 8);
        for block in 0..n / 16 {
            let at = index + block * 2;
            let info = tex.get(at).copied().unwrap_or(0) as u16
                | ((tex.get(at + 1).copied().unwrap_or(0) as u16) << 8);
            let count = match info >> 14 {
                0 => 3,
                2 => 4,
                _ => 2,
            };
            range(pal, po + (info as usize & 0x3fff) * 4, count * 2);
        }
    } else {
        let count = match p.format {
            1 => 32,
            2 => 4,
            3 => 16,
            4 => 256,
            6 => 8,
            _ => 0,
        };
        range(pal, po, count * 2);
    }
    h.0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arena_growth_preserves_this_frames_offsets() {
        let gpu = super::super::super::Gpu::headless().expect("GPU required for arena test");
        let mut cache = TextureCache::new(&gpu.device);
        (cache.texture, cache.view) = TextureCache::arena(&gpu.device, 1);
        let p = 7 << 26;
        assert_eq!(cache.get(p, 0, &[0xff; 128], &[]), (0, 1));
        cache.upload(&gpu.device, &gpu.queue);
        let first = cache.pixels[..64].to_vec();
        // Add a full 1024x1024 texture, forcing growth after an offset was assigned.
        let large = p | (7 << 20) | (7 << 23);
        assert_eq!(cache.get(large, 0, &[0; 128], &[]), (64, 1));
        cache.upload(&gpu.device, &gpu.queue);
        assert_eq!(cache.texture.depth_or_array_layers(), 2);
        assert_eq!(cache.get(p, 0, &[0xff; 128], &[]), (0, 1));
        assert_eq!(&cache.pixels[..64], first);
        assert_eq!(cache.uploaded, PAGE + 64);
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("arena growth regression"),
            size: (SIDE * 8) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        for layer in 0..2 {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &cache.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: layer as u64 * SIDE as u64 * 4,
                        bytes_per_row: Some(SIDE as u32 * 4),
                        rows_per_image: Some(1),
                    },
                },
                wgpu::Extent3d {
                    width: SIDE as u32,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
        }
        gpu.queue.submit([encoder.finish()]);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.unwrap());
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let bytes = readback.slice(..).get_mapped_range().unwrap();
        let pixels: &[u32] = bytemuck::cast_slice(&bytes);
        assert_eq!(&pixels[..64], first);
        assert!(pixels[64..].iter().all(|&p| p == 0));
    }
    #[test]
    fn stable_fnv_id() {
        let p = TexParams::from_reg(7 << 26);
        let tex: Vec<u8> = (0..128).collect();
        assert_eq!(fingerprint(&p, 0, &tex, &[]), 0x54f44bc2fc27cc32);
        let mut fnv = Fnv(0xcbf29ce484222325);
        fnv.bytes(b"hello");
        assert_eq!(fnv.0, 0xa430d84680aabd0b);
    }
    #[test]
    fn ids_ignore_addresses_and_sampling_but_keep_alpha() {
        for format in 1..=7 {
            let p = TexParams::from_reg(format << 26);
            let mut moved = p;
            moved.offset = 0x40000;
            moved.repeat_s = true;
            moved.flip_t = true;
            moved.transform_mode = 2;
            let mut tex = vec![0x39; 0x80000];
            // Compressed descriptors in the two slots reference the same relative palettes.
            tex[0x20000..0x20008].fill(0);
            tex[0x30000..0x30008].fill(0);
            let pal = vec![0x63; 0x18000];
            assert_eq!(
                fingerprint(&p, 0, &tex, &pal),
                fingerprint(&moved, 5, &tex, &pal)
            );
            if (2..=4).contains(&format) {
                moved.color0_transparent = true;
                assert_ne!(
                    fingerprint(&p, 0, &tex, &pal),
                    fingerprint(&moved, 5, &tex, &pal)
                );
            }
        }
    }
    #[test]
    fn content_key_tracks_source_and_indirect_palette() {
        let mut tex = vec![0; 0x80000];
        let mut pal = vec![0; 0x18000];
        for format in 1..=7 {
            let p = TexParams::from_reg(format << 26);
            let key = fingerprint(&p, 0, &tex, &pal);
            tex[0x70000] ^= 1;
            pal[0x17000] ^= 1;
            assert_eq!(
                key,
                fingerprint(&p, 0, &tex, &pal),
                "unrelated writes, format {format}"
            );
            tex[0] ^= 1;
            assert_ne!(
                key,
                fingerprint(&p, 0, &tex, &pal),
                "image writes, format {format}"
            );
            tex[0] ^= 1;
            if format != 7 {
                pal[0] ^= 1;
                assert_ne!(
                    key,
                    fingerprint(&p, 0, &tex, &pal),
                    "palette writes, format {format}"
                );
                pal[0] ^= 1;
            }
        }
        let p = TexParams::from_reg(5 << 26);
        tex[0x20000] = 16;
        tex[0x20001] = 0xc0;
        let key = fingerprint(&p, 0, &tex, &pal);
        pal[64] = 1;
        assert_ne!(key, fingerprint(&p, 0, &tex, &pal));
        pal[64] = 0;
        tex[0x20001] = 0;
        assert_ne!(key, fingerprint(&p, 0, &tex, &pal));
    }
}
