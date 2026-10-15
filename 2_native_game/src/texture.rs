//! DS texture decoding (T061 / lane L6): TEXIMAGE_PARAM + PLTT_BASE + texture / palette memory ->
//! RGBA8. `tex` is the assembled 512 KB texture image space (slots 0..3 of 128 KB), `pal` the
//! 96 KB texture palette space. Documented console formats only; no game data.

pub const FORMAT_NAMES: [&str; 8] = ["none", "A3I5", "4-color", "16-color", "256-color", "4x4 compressed", "A5I3", "direct"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TexParams {
    /// Byte offset into texture memory.
    pub offset: usize,
    pub repeat_s: bool,
    pub repeat_t: bool,
    pub flip_s: bool,
    pub flip_t: bool,
    pub width: usize,
    pub height: usize,
    pub format: u8,
    pub color0_transparent: bool,
    pub transform_mode: u8,
}

impl TexParams {
    pub fn from_reg(v: u32) -> TexParams {
        TexParams {
            offset: ((v & 0xFFFF) as usize) << 3,
            repeat_s: v & (1 << 16) != 0,
            repeat_t: v & (1 << 17) != 0,
            flip_s: v & (1 << 18) != 0,
            flip_t: v & (1 << 19) != 0,
            width: 8 << ((v >> 20) & 7),
            height: 8 << ((v >> 23) & 7),
            format: ((v >> 26) & 7) as u8,
            color0_transparent: v & (1 << 29) != 0,
            transform_mode: (v >> 30) as u8,
        }
    }
}

fn rgb555(c: u16, a: u8) -> u32 {
    let e = |v: u16| {
        let v = (v & 31) as u32;
        (v << 3) | (v >> 2)
    };
    (e(c) << 24) | (e(c >> 5) << 16) | (e(c >> 10) << 8) | a as u32
}

fn pal16(pal: &[u8], byte_off: usize, idx: usize) -> u16 {
    let a = byte_off + idx * 2;
    if a + 1 < pal.len() { u16::from_le_bytes([pal[a], pal[a + 1]]) } else { 0 }
}

/// Decode to packed RGBA (`0xRRGGBBAA`), row-major, `width * height` entries.
pub fn decode(p: &TexParams, pltt_base: u32, tex: &[u8], pal: &[u8]) -> Vec<u32> {
    let (w, h) = (p.width, p.height);
    let mut out = vec![0u32; w * h];
    let b = |i: usize| tex.get(p.offset.wrapping_add(i)).copied().unwrap_or(0);
    let pal_off16 = (pltt_base as usize) << 4;
    let pal_off8 = (pltt_base as usize) << 3;
    match p.format {
        1 => {
            for i in 0..w * h {
                let v = b(i);
                let a = (v >> 5) as u32;
                out[i] = rgb555(pal16(pal, pal_off16, (v & 31) as usize), (a * 255 / 7) as u8);
            }
        }
        2 => {
            for i in 0..w * h {
                let idx = ((b(i / 4) >> ((i % 4) * 2)) & 3) as usize;
                out[i] = if idx == 0 && p.color0_transparent { 0 } else { rgb555(pal16(pal, pal_off8, idx), 255) };
            }
        }
        3 => {
            for i in 0..w * h {
                let idx = ((b(i / 2) >> ((i % 2) * 4)) & 15) as usize;
                out[i] = if idx == 0 && p.color0_transparent { 0 } else { rgb555(pal16(pal, pal_off16, idx), 255) };
            }
        }
        4 => {
            for i in 0..w * h {
                let idx = b(i) as usize;
                out[i] = if idx == 0 && p.color0_transparent { 0 } else { rgb555(pal16(pal, pal_off16, idx), 255) };
            }
        }
        5 => {
            let slot_idx_base = if p.offset >= 0x40000 { 0x30000 + ((p.offset - 0x40000) >> 1) } else { 0x20000 + (p.offset >> 1) };
            let bw = w / 4;
            for by in 0..h / 4 {
                for bx in 0..bw {
                    let blk = by * bw + bx;
                    let texel = |i: usize| b(blk * 4 + i);
                    let ia = slot_idx_base + blk * 2;
                    let info = tex.get(ia).copied().unwrap_or(0) as u16 | (tex.get(ia + 1).copied().unwrap_or(0) as u16) << 8;
                    let po = pal_off16 + ((info & 0x3FFF) as usize) * 4;
                    let c = |i: usize| pal16(pal, po, i);
                    let mix = |x: u16, y: u16, wx: u32, wy: u32| -> u16 {
                        let ch = |s: u32| (((x as u32 >> s) & 31) * wx + ((y as u32 >> s) & 31) * wy) / (wx + wy);
                        (ch(0) | (ch(5) << 5) | (ch(10) << 10)) as u16
                    };
                    let colors: [Option<u16>; 4] = match info >> 14 {
                        0 => [Some(c(0)), Some(c(1)), Some(c(2)), None],
                        1 => [Some(c(0)), Some(c(1)), Some(mix(c(0), c(1), 1, 1)), None],
                        2 => [Some(c(0)), Some(c(1)), Some(c(2)), Some(c(3))],
                        _ => [Some(c(0)), Some(c(1)), Some(mix(c(0), c(1), 5, 3)), Some(mix(c(0), c(1), 3, 5))],
                    };
                    for y in 0..4 {
                        let row = texel(y);
                        for x in 0..4 {
                            let sel = ((row >> (x * 2)) & 3) as usize;
                            out[(by * 4 + y) * w + bx * 4 + x] = match colors[sel] {
                                Some(col) => rgb555(col, 255),
                                None => 0,
                            };
                        }
                    }
                }
            }
        }
        6 => {
            for i in 0..w * h {
                let v = b(i);
                let a = (v >> 3) as u32;
                out[i] = rgb555(pal16(pal, pal_off16, (v & 7) as usize), (a * 255 / 31) as u8);
            }
        }
        7 => {
            for i in 0..w * h {
                let c = u16::from_le_bytes([b(i * 2), b(i * 2 + 1)]);
                out[i] = rgb555(c, if c & 0x8000 != 0 { 255 } else { 0 });
            }
        }
        _ => {}
    }
    out
}

/// Texel lookup with the repeat / flip rules; `s`, `t` in texel units (any integer).
pub fn wrap(p: &TexParams, s: i32, t: i32) -> (usize, usize) {
    fn one(v: i32, size: i32, repeat: bool, flip: bool) -> usize {
        if !repeat {
            return v.clamp(0, size - 1) as usize;
        }
        let m = v.rem_euclid(size * if flip { 2 } else { 1 });
        (if flip && m >= size { 2 * size - 1 - m } else { m }) as usize
    }
    (one(s, p.width as i32, p.repeat_s, p.flip_s), one(t, p.height as i32, p.repeat_t, p.flip_t))
}

/// Build the texture image space (512 KB) and texture palette space (96 KB) from the VRAM banks
/// A..G (`vram[0..7]`) and their VRAMCNT bytes (`cnt[0..7]`: bits 0-2 MST, 3-4 OFS, 7 enable).
/// Banks A-D with MST 3 are texture slots OFS; E / F / G with MST 3 are texture palette slots.
pub fn assemble(vram: &[Vec<u8>], cnt: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut tex = vec![0u8; 0x80000];
    let mut pal = vec![0u8; 0x18000];
    for i in 0..7usize {
        let c = cnt.get(i).copied().unwrap_or(0);
        if c & 0x80 == 0 || c & 7 != 3 {
            continue;
        }
        let ofs = ((c >> 3) & 3) as usize;
        let b = &vram[i];
        match i {
            0..=3 => tex[ofs * 0x20000..ofs * 0x20000 + b.len().min(0x20000)].copy_from_slice(&b[..b.len().min(0x20000)]),
            4 => pal[..b.len().min(0x10000)].copy_from_slice(&b[..b.len().min(0x10000)]),
            _ => {
                let slot = (ofs & 1) + (ofs >> 1) * 4;
                let at = slot * 0x4000;
                if at + 0x4000 <= pal.len() {
                    pal[at..at + b.len().min(0x4000)].copy_from_slice(&b[..b.len().min(0x4000)]);
                }
            }
        }
    }
    (tex, pal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(format: u32, sz: u32, extra: u32) -> TexParams {
        TexParams::from_reg((format << 26) | (sz << 20) | (sz << 23) | extra)
    }

    #[test]
    fn param_fields() {
        let p = TexParams::from_reg((7 << 26) | (2 << 20) | (3 << 23) | (1 << 16) | (1 << 19) | 0x10);
        assert_eq!((p.width, p.height, p.format, p.offset), (32, 64, 7, 0x80));
        assert!(p.repeat_s && !p.repeat_t && p.flip_t && !p.flip_s);
    }

    #[test]
    fn direct_and_paletted() {
        let p = params(7, 0, 0);
        let mut tex = vec![0u8; 512];
        tex[0] = 0x1F; // pure red, alpha bit clear -> transparent
        tex[2] = 0x1F;
        tex[3] = 0x80; // red, opaque
        let d = decode(&p, 0, &tex, &[]);
        assert_eq!((d[0], d[1]), (0xFF00_0000, 0xFF00_00FF));
        let p4 = params(4, 0, 1 << 29);
        let tex = (0..64u8).collect::<Vec<_>>();
        let mut pal = vec![0u8; 512];
        pal[2 * 5] = 0xE0;
        pal[2 * 5 + 1] = 0x03; // index 5 = green
        let d = decode(&p4, 0, &tex, &pal);
        assert_eq!(d[0], 0, "colour 0 transparent");
        assert_eq!(d[5], 0x00FF_00FF);
    }

    #[test]
    fn alpha_formats_and_four_colour() {
        let mut tex = vec![0u8; 64];
        tex[0] = (7 << 5) | 1; // A3I5: alpha 7, index 1
        let mut pal = vec![0u8; 64];
        pal[2] = 0x1F;
        let d = decode(&params(1, 0, 0), 0, &tex, &pal);
        assert_eq!(d[0], 0xFF00_00FF);
        tex[0] = 0b1110_0100; // 2bpp: indices 0,1,2,3
        pal[0] = 0;
        pal[2] = 0x1F;
        pal[4] = 0xE0;
        pal[5] = 0x03;
        let d = decode(&params(2, 0, 0), 0, &tex, &pal);
        assert_eq!((d[1], d[2]), (0xFF00_00FF, 0x00FF_00FF));
    }

    #[test]
    fn compressed_blend_mode() {
        // one 8x8 texture = 2x2 blocks; block 0 uses mode 3 (blend), texels: row0 = 0,1,2,3
        let p = params(5, 0, 0);
        let mut tex = vec![0u8; 0x30000];
        tex[0] = 0b1110_0100;
        tex[0x20000] = 0;
        tex[0x20001] = 0xC0; // mode 3, palette offset 0
        let mut pal = vec![0u8; 64];
        pal[0..2].copy_from_slice(&0x7C00u16.to_le_bytes()); // blue
        pal[2..4].copy_from_slice(&0x001Fu16.to_le_bytes()); // red
        let d = decode(&p, 0, &tex, &pal);
        assert_eq!(d[0], 0x0000_FFFF);
        assert_eq!(d[1], 0xFF00_00FF);
        assert_ne!(d[2], d[3], "5:3 and 3:5 blends differ");
    }

    #[test]
    fn assembling_banks() {
        let mut vram: Vec<Vec<u8>> = vec![vec![0; 0x20000], vec![0; 0x20000], vec![0; 0x20000], vec![0; 0x20000], vec![0; 0x10000], vec![0; 0x4000], vec![0; 0x4000]];
        vram[1][0] = 7; // B -> texture slot 2
        vram[5][0] = 9; // F -> palette slot 4
        let cnt = [0, 0x80 | 3 | (2 << 3), 0, 0, 0, 0x80 | 3 | (2 << 3), 0];
        let (tex, pal) = assemble(&vram, &cnt);
        assert_eq!(tex[2 * 0x20000], 7);
        assert_eq!(pal[4 * 0x4000], 9);
    }

    #[test]
    fn wrapping() {
        let p = TexParams::from_reg((1 << 16) | (1 << 18)); // repeat + flip S, clamp T
        assert_eq!(wrap(&p, 9, -5), (6, 0)); // 8 wide: 8..15 mirrored
        assert_eq!(wrap(&p, 17, 99), (1, 7));
    }
}
