use recomp::texture::{TexParams, decode};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
};

/// Packed RGBA texels in a persistent GPU storage arena. Hash only memory reachable by
/// this texture, including compressed block descriptors and their indirect palettes.
pub(super) struct TextureCache {
    pub buffer: wgpu::Buffer,
    entries: HashMap<(u32, u32, u64), u32>,
    used: usize,
}
const CAPACITY: usize = 16 * 1024 * 1024;
impl TextureCache {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("3d cached texels"),
                size: (CAPACITY * 4) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            entries: HashMap::new(),
            used: 0,
        }
    }
    pub fn begin_frame(&mut self) {
        // Leave enough space for a frame's unique decoded textures (the source VRAM
        // is only 512 KiB). Reset before generating offsets, never during a frame.
        if self.used > CAPACITY / 2 {
            self.entries.clear();
            self.used = 0;
        }
    }
    pub fn get(&mut self, queue: &wgpu::Queue, reg: u32, base: u32, tex: &[u8], pal: &[u8]) -> u32 {
        let p = TexParams::from_reg(reg);
        if p.format == 0 {
            return 0;
        }
        let key = (reg, base, fingerprint(&p, base, tex, pal));
        if let Some(&offset) = self.entries.get(&key) {
            return offset;
        }
        let pixels = decode(&p, base, tex, pal);
        assert!(
            self.used + pixels.len() <= CAPACITY,
            "3D texture arena exhausted"
        );
        let offset = self.used as u32;
        queue.write_buffer(
            &self.buffer,
            offset as u64 * 4,
            bytemuck::cast_slice(&pixels),
        );
        self.used += pixels.len();
        self.entries.insert(key, offset);
        offset
    }
}
fn fingerprint(p: &TexParams, base: u32, tex: &[u8], pal: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let n = p.width * p.height;
    let bytes = match p.format {
        2 | 5 => n / 4,
        3 => n / 2,
        7 => n * 2,
        _ => n,
    };
    let mut range = |data: &[u8], start: usize, len: usize| {
        // Missing source bytes decode as zero; include length to distinguish truncation.
        data.get(start..start.saturating_add(len).min(data.len()))
            .unwrap_or(&[])
            .hash(&mut h);
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
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
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
