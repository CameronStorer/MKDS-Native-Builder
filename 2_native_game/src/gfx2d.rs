//! 2D engine, software (T061 / lane L6): composes one screen (256x192) from the I/O registers,
//! palette, VRAM and OAM of one engine. Implemented: DISPCNT display modes 0/1, text backgrounds
//! (4 and 8 bit, flips, scroll with wrap, all four sizes), regular sprites (1D and 2D tile mapping,
//! all shapes and sizes, flips, priorities), backdrop. Not yet: affine / extended / bitmap
//! backgrounds, rotated sprites, windows, mosaic, blending and brightness, extended palettes.
//! Documented console behaviour only; no game data.

pub const W: usize = 256;
pub const H: usize = 192;

pub struct Engine2d<'a> {
    /// The engine's register block (0x00..0x70: DISPCNT at 0, BGxCNT at 8, scroll at 0x10).
    pub regs: &'a [u8],
    /// Reads a byte of VRAM at a console address (0x06000000.. for A, 0x06200000.. for B).
    pub vram: &'a dyn Fn(u32) -> u8,
    /// BG VRAM base for this engine (0x06000000 or 0x06200000) and OBJ VRAM base (0x06400000 / 0x06600000).
    pub bg_base: u32,
    pub obj_base: u32,
    /// 1 KB palette: 0x000 BG, 0x200 OBJ (RGB555).
    pub pal: &'a [u8],
    /// 1 KB OAM for this engine.
    pub oam: &'a [u8],
    /// The 3D engine's frame (packed `0xRRGGBBAA`, alpha 0 = nothing drawn), shown as BG0 when DISPCNT bit 3 is set.
    pub layer3d: Option<&'a [u32]>,
    /// GPU-only extended palettes: four 8 KiB BG slots and one 8 KiB OBJ slot.
    pub ext_bg: Option<&'a [u8]>,
    pub ext_obj: Option<&'a [u8]>,
}

fn rgb(c: u16) -> u32 {
    let e = |v: u16| {
        let v = (v & 31) as u32;
        (v << 3) | (v >> 2)
    };
    (e(c) << 24) | (e(c >> 5) << 16) | (e(c >> 10) << 8) | 0xFF
}

const OBJ_SIZES: [[(usize, usize); 4]; 3] = [
    [(8, 8), (16, 16), (32, 32), (64, 64)],
    [(16, 8), (32, 8), (32, 16), (64, 32)],
    [(8, 16), (8, 32), (16, 32), (32, 64)],
];

impl Engine2d<'_> {
    fn r16(&self, off: usize) -> u32 {
        self.regs.get(off).copied().unwrap_or(0) as u32
            | (self.regs.get(off + 1).copied().unwrap_or(0) as u32) << 8
    }
    fn dispcnt(&self) -> u32 {
        self.r16(0) | self.r16(2) << 16
    }
    fn pal(&self, i: usize) -> u16 {
        (self.pal.get(i * 2).copied().unwrap_or(0) as u16)
            | (self.pal.get(i * 2 + 1).copied().unwrap_or(0) as u16) << 8
    }
    fn extended(palette: Option<&[u8]>, index: usize) -> u16 {
        let bytes = palette.and_then(|p| p.get(index * 2..index * 2 + 2));
        bytes.map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
    }

    /// One text-background pixel: palette colour index (0 = transparent) and the resolved colour.
    fn bg_pixel(&self, bg: usize, x: usize, y: usize) -> Option<u32> {
        let d = self.dispcnt();
        let cnt = self.r16(8 + 2 * bg);
        let (hofs, vofs) = (
            self.r16(0x10 + 4 * bg) & 0x1FF,
            self.r16(0x12 + 4 * bg) & 0x1FF,
        );
        let char_base = self.bg_base + ((d >> 24) & 7) * 0x10000 + ((cnt >> 2) & 15) * 0x4000;
        let screen_base = self.bg_base + ((d >> 27) & 7) * 0x10000 + ((cnt >> 8) & 31) * 0x800;
        let size = (cnt >> 14) & 3;
        let (px, py) = (
            (x as u32 + hofs) & if size & 1 != 0 { 511 } else { 255 },
            (y as u32 + vofs) & if size & 2 != 0 { 511 } else { 255 },
        );
        // each 256x256 screen block is 0x800 bytes; wide maps put the second block right after the first,
        // 512x512 maps order them 0 1 / 2 3
        let block = match size {
            0 => 0,
            1 => px / 256,
            2 => py / 256,
            _ => (py / 256) * 2 + px / 256,
        };
        let (tx, ty) = ((px % 256) / 8, (py % 256) / 8);
        let ea = screen_base + block * 0x800 + (ty * 32 + tx) * 2;
        let ent = (self.vram)(ea) as u32 | (self.vram)(ea + 1) as u32 * 256;
        let (tile, hf, vf, pal) = (
            ent & 0x3FF,
            ent & 0x400 != 0,
            ent & 0x800 != 0,
            (ent >> 12) as usize,
        );
        let (mut ix, mut iy) = ((px % 8) as u32, (py % 8) as u32);
        if hf {
            ix = 7 - ix;
        }
        if vf {
            iy = 7 - iy;
        }
        if cnt & 0x80 != 0 {
            let idx = (self.vram)(char_base + tile * 64 + iy * 8 + ix) as usize;
            (idx != 0).then(|| {
                let slot = if bg < 2 && cnt & 0x2000 != 0 {
                    bg + 2
                } else {
                    bg
                };
                rgb(if d & (1 << 30) != 0 {
                    Self::extended(self.ext_bg, slot * 4096 + pal * 256 + idx)
                } else {
                    self.pal(idx)
                })
            })
        } else {
            let b = (self.vram)(char_base + tile * 32 + iy * 4 + ix / 2) as usize;
            let idx = if ix & 1 == 0 { b & 15 } else { b >> 4 };
            (idx != 0).then(|| rgb(self.pal(pal * 16 + idx)))
        }
    }

    /// Affine tile backgrounds, including mode 5's extended 16-bit tile maps.
    fn affine_pixel(&self, bg: usize, x: usize, y: usize, extended: bool) -> Option<u32> {
        let d = self.dispcnt();
        let cnt = self.r16(8 + 2 * bg);
        let reg = 0x20 + (bg - 2) * 0x10;
        let signed16 = |offset| self.r16(offset) as u16 as i16 as i32;
        let reference =
            |offset| ((self.r16(offset) | (self.r16(offset + 2) << 16)) as i32) << 4 >> 4;
        let sx =
            (reference(reg + 8) + signed16(reg) * x as i32 + signed16(reg + 2) * y as i32) >> 8;
        let sy =
            (reference(reg + 12) + signed16(reg + 4) * x as i32 + signed16(reg + 6) * y as i32)
                >> 8;
        let size = (cnt >> 14) & 3;
        let bitmap = extended && cnt & 0x80 != 0;
        let (width, height) = if bitmap {
            [(128, 128), (256, 256), (512, 256), (512, 512)][size as usize]
        } else {
            let side = 128i32 << size;
            (side, side)
        };
        let (sx, sy) = if cnt & 0x2000 != 0 {
            (sx.rem_euclid(width), sy.rem_euclid(height))
        } else if sx < 0 || sy < 0 || sx >= width || sy >= height {
            return None;
        } else {
            (sx, sy)
        };
        let read16 = |a| (self.vram)(a) as u16 | ((self.vram)(a + 1) as u16) << 8;
        if bitmap {
            let base = self.bg_base + ((cnt >> 8) & 31) * 0x4000;
            let offset = (sy * width + sx) as u32;
            return if cnt & 4 != 0 {
                let color = read16(base + offset * 2);
                (color & 0x8000 != 0).then(|| rgb(color))
            } else {
                let index = (self.vram)(base + offset) as usize;
                (index != 0).then(|| rgb(self.pal(index)))
            };
        }
        let base = self.bg_base + ((d >> 27) & 7) * 0x10000 + ((cnt >> 8) & 31) * 0x800;
        let map_index = (sy / 8 * (width / 8) + sx / 8) as u32;
        let entry = if extended {
            read16(base + map_index * 2)
        } else {
            (self.vram)(base + map_index) as u16
        };
        let tile = if extended { entry & 1023 } else { entry };
        let (mut ix, mut iy) = ((sx % 8) as u32, (sy % 8) as u32);
        if extended && entry & 0x400 != 0 {
            ix = 7 - ix;
        }
        if extended && entry & 0x800 != 0 {
            iy = 7 - iy;
        }
        let chars = self.bg_base + ((d >> 24) & 7) * 0x10000 + ((cnt >> 2) & 15) * 0x4000;
        let index = (self.vram)(chars + tile as u32 * 64 + iy * 8 + ix) as usize;
        (index != 0).then(|| {
            rgb(if extended && d & (1 << 30) != 0 {
                Self::extended(
                    self.ext_bg,
                    bg * 4096 + (entry >> 12) as usize * 256 + index,
                )
            } else {
                self.pal(index)
            })
        })
    }

    fn background_pixel(&self, bg: usize, x: usize, y: usize) -> Option<u32> {
        let mode = self.dispcnt() & 7;
        let kind = match (mode, bg) {
            (0, _) | (1, 0 | 1 | 3) | (2..=5, 0 | 1) | (3, 2) => 0,
            (1 | 2 | 4, 2) | (2, 3) => 1,
            (3..=5, 3) | (5, 2) => 2,
            _ => return None,
        };
        match kind {
            0 => self.bg_pixel(bg, x, y),
            1 => self.affine_pixel(bg, x, y, false),
            _ => self.affine_pixel(bg, x, y, true),
        }
    }

    /// Sprites for one scanline: (priority, colour) per x, first sprite in OAM order wins.
    fn sprites_line(&self, y: usize, window: bool) -> [Option<(u8, u32, bool)>; W] {
        let mut line = [None; W];
        let d = self.dispcnt();
        let one_d = d & (1 << 4) != 0;
        let boundary = 32u32 << ((d >> 20) & 3);
        for i in 0..128usize {
            let (a0, a1, a2) = (
                self.r_oam(i * 8),
                self.r_oam(i * 8 + 2),
                self.r_oam(i * 8 + 4),
            );
            let affine = a0 & 0x100 != 0;
            if !affine && a0 & 0x200 != 0 {
                continue;
            }
            let mode = (a0 >> 10) & 3;
            if (mode == 2) != window {
                continue;
            }
            let shape = ((a0 >> 14) & 3) as usize;
            if shape == 3 {
                continue;
            }
            let (w, h) = OBJ_SIZES[shape][((a1 >> 14) & 3) as usize];
            let double = affine && a0 & 0x200 != 0;
            let (bw, bh) = if double { (w * 2, h * 2) } else { (w, h) };
            let oy = (a0 & 0xFF) as usize;
            let rel = (y + 256 - oy) & 0xFF;
            if rel >= bh {
                continue;
            }
            let ox = ((a1 & 0x1FF) as i32) << 23 >> 23;
            let (hf, vf) = (a1 & 0x1000 != 0, a1 & 0x2000 != 0);
            let c256 = a0 & 0x2000 != 0;
            let prio = ((a2 >> 10) & 3) as u8;
            let matrix = ((a1 >> 9) & 31) as usize * 32;
            let coefficient = |n: usize| self.r_oam(matrix + 6 + n * 8) as u16 as i16 as i32;
            for sx in 0..bw {
                let px = ox + sx as i32;
                if !(0..W as i32).contains(&px)
                    || line[px as usize].is_some_and(|(p, _, _)| p <= prio)
                {
                    continue;
                }
                let (rx, ry) = if affine {
                    let dx = sx as i32 - bw as i32 / 2;
                    let dy = rel as i32 - bh as i32 / 2;
                    let rx = ((coefficient(0) * dx + coefficient(1) * dy) >> 8) + w as i32 / 2;
                    let ry = ((coefficient(2) * dx + coefficient(3) * dy) >> 8) + h as i32 / 2;
                    if rx < 0 || ry < 0 || rx >= w as i32 || ry >= h as i32 {
                        continue;
                    }
                    (rx as usize, ry as usize)
                } else {
                    (
                        if hf { w - 1 - sx } else { sx },
                        if vf { h - 1 - rel } else { rel },
                    )
                };
                let (tx, ty) = (rx / 8, ry / 8);
                let tile = a2 & 0x3FF;
                let bpt = if c256 { 64u32 } else { 32 };
                let base = self.obj_base;
                let tile_addr = if one_d {
                    base + tile * boundary + (ty * (w / 8) + tx) as u32 * bpt
                } else {
                    base + (tile + (tx as u32) * (bpt / 32) + (ty as u32) * 32) * 32
                };
                let (ix, iy) = ((rx % 8) as u32, (ry % 8) as u32);
                let col = if mode == 3 {
                    if a2 >> 12 == 0 {
                        continue;
                    }
                    let address = if d & 0x40 != 0 {
                        base + tile * (128 << ((d >> 22) & 1)) + (ry * w + rx) as u32 * 2
                    } else {
                        let width = if d & 0x20 != 0 { 256 } else { 128 };
                        let tiles = width / 8;
                        base + ((tile % tiles) * 8
                            + (tile / tiles) * 8 * width
                            + ry as u32 * width
                            + rx as u32)
                            * 2
                    };
                    let color =
                        (self.vram)(address) as u16 | ((self.vram)(address + 1) as u16) << 8;
                    (color & 0x8000 != 0).then(|| rgb(color))
                } else if c256 {
                    let idx = (self.vram)(tile_addr + iy * 8 + ix) as usize;
                    (idx != 0).then(|| {
                        rgb(if d & (1 << 31) != 0 {
                            Self::extended(self.ext_obj, (a2 >> 12) as usize * 256 + idx)
                        } else {
                            self.pal(256 + idx)
                        })
                    })
                } else {
                    let b = (self.vram)(tile_addr + iy * 4 + ix / 2) as usize;
                    let idx = if ix & 1 == 0 { b & 15 } else { b >> 4 };
                    (idx != 0).then(|| rgb(self.pal(256 + ((a2 >> 12) as usize) * 16 + idx)))
                };
                if let Some(c) = col {
                    line[px as usize] = Some((prio, c, mode == 1));
                }
            }
        }
        line
    }

    fn r_oam(&self, off: usize) -> u32 {
        self.oam.get(off).copied().unwrap_or(0) as u32
            | (self.oam.get(off + 1).copied().unwrap_or(0) as u32) << 8
    }

    fn window_mask(&self, d: u32, x: usize, y: usize, object: bool) -> u32 {
        if d & 0xe000 == 0 {
            return 0x3f;
        }
        let contains = |range: u32, point: usize| {
            let (start, end) = ((range >> 8) as usize, (range & 255) as usize);
            if start <= end {
                point >= start && point < end
            } else {
                point >= start || point < end
            }
        };
        for i in 0..2 {
            if d & (0x2000 << i) != 0
                && contains(self.r16(0x40 + i * 2), x)
                && contains(self.r16(0x44 + i * 2), y)
            {
                return (self.r16(0x48) >> (i * 8)) & 0x3f;
            }
        }
        (self.r16(0x4a) >> if object { 8 } else { 0 }) & 0x3f
    }

    /// Up to three layers at one pixel in `render`'s scan order, without its early stops: `(layer, colour, semi,
    /// is_3d)` (layer 0-3 BG, 4 OBJ; the 3D layer as layer 0 with `is_3d`). Three is enough for both stacks
    /// [`Engine2d::fold`] derives: two layers besides the (single) 3D one.
    fn scan(&self, d: u32, x: usize, y: usize, sprites: &[Option<(u8, u32, bool)>; W], mask: u32) -> ([(usize, u32, bool, bool); 3], usize) {
        let mut found = [(5usize, 0u32, false, false); 3];
        let mut n = 0;
        'p: for prio in 0..4u32 {
            if let Some((sp, c, semi)) = sprites[x] {
                if mask & 16 != 0 && sp as u32 == prio {
                    found[n] = (4, c, semi, false);
                    n += 1;
                    if n == 3 {
                        break 'p;
                    }
                }
            }
            for bg in 0..4usize {
                if mask & (1 << bg) == 0 {
                    continue;
                }
                if bg == 0 && d & 8 != 0 && d & 0x100 != 0 && self.r16(8) & 3 == prio {
                    found[n] = (0, 0, true, true);
                } else if d & (0x100 << bg) != 0 && self.r16(8 + 2 * bg) & 3 == prio {
                    match self.background_pixel(bg, x, y) {
                        Some(c) => found[n] = (bg, c, false, false),
                        None => continue,
                    }
                } else {
                    continue;
                }
                n += 1;
                if n == 3 {
                    break 'p;
                }
            }
        }
        (found, n)
    }

    /// The two-layer stack `render` builds from scanned layers: `three_d` = `None` (no 3D pixel here) or the alpha
    /// of a 3D pixel; same early stops as `render`.
    fn fold(&self, mask: u32, scanned: &[(usize, u32, bool, bool)], three_d: Option<u32>) -> [(usize, u32, bool); 2] {
        let blend = self.r16(0x50);
        let mut layers = [(5usize, rgb(self.pal(0)), false); 2];
        let mut count = 0;
        for &(layer, color, semi, is_3d) in scanned {
            let color = match (is_3d, three_d) {
                (true, None) => continue,
                (true, Some(alpha)) => alpha,
                _ => color,
            };
            layers[count] = (layer, color, semi);
            count += 1;
            let alpha = (semi && (layer != 0 || color & 255 != 255)) || (blend >> 6) & 3 == 1 && blend & (1 << layer) != 0;
            if count == 2 || mask & 32 == 0 || blend & 0x3f00 == 0 || !alpha {
                break;
            }
        }
        layers
    }

    /// `render`'s blend of a two-layer stack (before master brightness). `d`: DISPCNT; `mask`: window mask.
    fn blend_stack(&self, d: u32, mask: u32, layers: [(usize, u32, bool); 2]) -> u32 {
        let blend = self.r16(0x50);
        let coefficients = self.r16(0x52);
        let ((first, color, semi), (second, behind, _)) = (layers[0], layers[1]);
        let mut best = color | 255;
        if mask & 32 != 0 {
            let mode = (blend >> 6) & 3;
            let target = blend & (1 << first) != 0;
            if (semi || mode == 1 && target) && blend & (0x100 << second) != 0 {
                let (ea, eb, divisor) = if first == 0 && d & 8 != 0 {
                    (color & 255, 255 - (color & 255), 255)
                } else {
                    ((coefficients & 31).min(16), ((coefficients >> 8) & 31).min(16), 16)
                };
                let channel = |shift: u32| ((((color >> shift) & 255u32) * ea + ((behind >> shift) & 255) * eb) / divisor).min(255);
                best = channel(24) << 24 | channel(16) << 16 | channel(8) << 8 | 255;
            } else if target && matches!(mode, 2 | 3) {
                best = brighten(color, mode, (self.r16(0x54) & 31).min(16));
            }
        }
        best
    }

    /// The screen with its 3D layer left open, so the 3D can be drawn elsewhere (another resolution, the GPU) and
    /// combined afterwards by [`Split::compose`] or the GPU compositor. `Split::compose(layer3d)` equals `render()`
    /// with the same `layer3d`.
    pub fn render_split(&self) -> Split {
        let d = self.dispcnt();
        let blend = self.r16(0x50);
        let coefficients = self.r16(0x52);
        let master = self.r16(0x6c);
        let mut split = Split {
            base: vec![0xFFFF_FFFFu32; W * H],
            info: vec![0u8; W * H],
            other: vec![0u32; W * H],
            eva: (coefficients & 31).min(16),
            evb: ((coefficients >> 8) & 31).min(16),
            bright: ((blend >> 6) & 3, (self.r16(0x54) & 31).min(16)),
            master: (master >> 14, (master & 31).min(16)),
        };
        let mode = (d >> 16) & 3;
        if mode == 0 || d & 0x80 != 0 {
            split.master = (0, 0);
            return split; // display off / forced blank: white, no 3D
        }
        if mode != 1 {
            split.base.fill(0);
            split.master = (0, 0);
            return split; // VRAM / main-memory display: not supported here
        }
        let three_d_on = d & 8 != 0 && d & 0x100 != 0;
        let blend_mode = (blend >> 6) & 3;
        for y in 0..H {
            let sprites = if d & 0x1000 != 0 { self.sprites_line(y, false) } else { [None; W] };
            let object_window = if d & 0x9000 == 0x9000 { self.sprites_line(y, true) } else { [None; W] };
            for x in 0..W {
                let i = y * W + x;
                let mask = self.window_mask(d, x, y, object_window[x].is_some());
                // One scan per pixel; both stacks (without and with a 3D pixel) are folded from it.
                let (scanned, n) = self.scan(d, x, y, &sprites, mask);
                let scanned = &scanned[..n];
                split.base[i] = self.blend_stack(d, mask, self.fold(mask, scanned, None));
                if !three_d_on || mask & 1 == 0 {
                    continue;
                }
                // Where would a translucent 3D pixel sit, and what is next to it?
                let with = self.fold(mask, scanned, Some(0x80));
                let effects = mask & 32 != 0;
                if with[0].0 == 0 {
                    // 3D on top. Translucent: `render` collected the real layer behind it. Opaque: `render` stops at
                    // the 3D pixel, so "behind" is the backdrop and any blend returns the 3D colour itself.
                    let (second, behind) = (with[1].0, with[1].1);
                    let translucent_blends = effects && blend & (0x100 << second) != 0;
                    let opaque_blends = effects && blend & (0x100 << 5) != 0;
                    let brightens = effects && blend & 1 != 0 && matches!(blend_mode, 2 | 3);
                    split.info[i] = 1 | (translucent_blends as u8) << 2 | (opaque_blends as u8) << 3 | (brightens as u8) << 4;
                    split.other[i] = behind;
                } else if with[1].0 == 0 {
                    // 3D directly under the top layer: either they blend, or the top layer shows with a result that
                    // may differ from `base` (whose second layer is what lies under the 3D).
                    let (first, color, semi) = with[0];
                    let target = blend & (1 << first) != 0;
                    if effects && (semi || blend_mode == 1 && target) && blend & 0x100 != 0 {
                        split.info[i] = 2;
                        split.other[i] = color;
                    } else {
                        split.info[i] = 3;
                        split.other[i] = self.blend_stack(d, mask, [with[0], (0, 0, true)]);
                    }
                }
            }
        }
        split
    }

    /// Render the whole screen as packed `0xRRGGBBAA`.
    pub fn render(&self) -> Vec<u32> {
        let d = self.dispcnt();
        let mut out = vec![0xFFFF_FFFFu32; W * H];
        let mode = (d >> 16) & 3;
        if mode == 0 {
            return out; // display off: white
        }
        if d & 0x80 != 0 {
            return out;
        } // forced blank
        if mode != 1 {
            return vec![0u32; W * H]; // VRAM / main-memory display: not supported here
        }
        let backdrop = rgb(self.pal(0));
        let blend = self.r16(0x50);
        let coefficients = self.r16(0x52);
        for y in 0..H {
            let sprites = if d & 0x1000 != 0 {
                self.sprites_line(y, false)
            } else {
                [None; W]
            };
            let object_window = if d & 0x9000 == 0x9000 {
                self.sprites_line(y, true)
            } else {
                [None; W]
            };
            for x in 0..W {
                let mask = self.window_mask(d, x, y, object_window[x].is_some());
                let mut layers = [(5usize, backdrop, false); 2];
                let mut count = 0;
                let mut add = |layer, color, semi| {
                    layers[count] = (layer, color, semi);
                    count += 1;
                    let alpha = (semi && (layer != 0 || color & 255 != 255))
                        || (blend >> 6) & 3 == 1 && blend & (1 << layer) != 0;
                    count == 2 || mask & 32 == 0 || blend & 0x3f00 == 0 || !alpha
                };
                // Objects precede backgrounds at equal priority; the second
                // visible layer is needed for alpha effects and 3D composition.
                'p: for prio in 0..4u32 {
                    if let Some((sp, c, semi)) = sprites[x] {
                        if mask & 16 != 0 && sp as u32 == prio && add(4, c, semi) {
                            break 'p;
                        }
                    }
                    for bg in 0..4usize {
                        if mask & (1 << bg) == 0 {
                            continue;
                        }
                        if bg == 0 && d & 8 != 0 && d & 0x100 != 0 && self.r16(8) & 3 == prio {
                            if let Some(l) = self.layer3d {
                                let c = l[y * W + x];
                                if c & 0xFF != 0 && add(0, c, true) {
                                    break 'p;
                                }
                            }
                            continue;
                        }
                        if d & (0x100 << bg) != 0 && self.r16(8 + 2 * bg) & 3 == prio {
                            if let Some(c) = self.background_pixel(bg, x, y) {
                                if add(bg, c, false) {
                                    break 'p;
                                }
                            }
                        }
                    }
                }
                let ((first, color, semi), (second, behind, _)) = (layers[0], layers[1]);
                let mut best = color | 255;
                if mask & 32 != 0 {
                    let mode = (blend >> 6) & 3;
                    let target = blend & (1 << first) != 0;
                    if (semi || mode == 1 && target) && blend & (0x100 << second) != 0 {
                        let (ea, eb, divisor) = if first == 0 && d & 8 != 0 {
                            (color & 255, 255 - (color & 255), 255)
                        } else {
                            (
                                (coefficients & 31).min(16),
                                ((coefficients >> 8) & 31).min(16),
                                16,
                            )
                        };
                        let channel = |shift: u32| {
                            ((((color >> shift) & 255u32) * ea + ((behind >> shift) & 255) * eb)
                                / divisor)
                                .min(255)
                        };
                        best = channel(24) << 24 | channel(16) << 16 | channel(8) << 8 | 255;
                    } else if target && matches!(mode, 2 | 3) {
                        let amount = (self.r16(0x54) & 31).min(16);
                        let channel = |shift: u32| {
                            let value = (color >> shift) & 255;
                            if mode == 2 {
                                value + (255 - value) * amount / 16
                            } else {
                                value - value * amount / 16
                            }
                        };
                        best = channel(24) << 24 | channel(16) << 16 | channel(8) << 8 | 255;
                    }
                }
                out[y * W + x] = best;
            }
        }
        let master = self.r16(0x6c);
        let factor = (master & 31).min(16);
        if matches!(master >> 14, 1 | 2) && factor != 0 {
            for pixel in &mut out {
                let channel = |shift: u32| {
                    let value = (*pixel >> shift) & 255u32;
                    if master >> 14 == 1 {
                        value + (255 - value) * factor / 16
                    } else {
                        value - value * factor / 16
                    }
                };
                *pixel = (channel(24) << 24) | (channel(16) << 16) | (channel(8) << 8) | 255;
            }
        }
        out
    }
}

/// Brightness up (`mode` 2) or down (3) of a packed colour by `amount`/16, as BLDCNT / BLDY do.
fn brighten(color: u32, mode: u32, amount: u32) -> u32 {
    let channel = |shift: u32| {
        let value = (color >> shift) & 255;
        if mode == 2 { value + (255 - value) * amount / 16 } else { value - value * amount / 16 }
    };
    channel(24) << 24 | channel(16) << 16 | channel(8) << 8 | 255
}

/// One engine's screen with the 3D layer left open (see [`Engine2d::render_split`]). Everything is at 256x192; the
/// 3D layer may be sampled at any resolution, each 3D sample combining with the record of the DS pixel it lies in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    /// The colour wherever no 3D pixel shows (packed `0xRRGGBBAA`, before master brightness).
    pub base: Vec<u32>,
    /// Per pixel, bits 0-1: 0 = a 3D pixel here is hidden; 1 = it is on top; 2 = it blends under the layer above
    /// (`other`) with `eva`/`evb`; 3 = it makes the layers above show as `other`. For 1: bit 2 = a translucent 3D pixel
    /// blends with `other` (the layer behind it) by its alpha, bit 3 = an opaque one counts as blended (no
    /// brightness), bit 4 = brightness (`bright`) applies when it does not blend.
    pub info: Vec<u8>,
    pub other: Vec<u32>,
    pub eva: u32,
    pub evb: u32,
    /// BLDCNT mode (2 = up, 3 = down) and BLDY amount.
    pub bright: (u32, u32),
    /// MASTER_BRIGHT mode (1 = up, 2 = down) and factor, applied last.
    pub master: (u32, u32),
}

impl Split {
    /// The colour of one sample: DS pixel `i` with the 3D sample `c3` (packed `0xRRGGBBAA`, alpha 0 = none).
    /// The GPU compositor's shader is this function; keep them in step.
    pub fn pixel(&self, i: usize, c3: u32) -> u32 {
        let info = self.info[i] as u32;
        let a3 = c3 & 255;
        let other = self.other[i];
        let mut c = match info & 3 {
            _ if a3 == 0 => self.base[i],
            0 => self.base[i],
            1 => {
                let blends = if a3 == 255 { info & 8 != 0 } else { info & 4 != 0 };
                if blends {
                    // An opaque pixel gives `other` weight 0: the 3D colour itself, but no brightness.
                    let ch = |s: u32| ((((c3 >> s) & 255) * a3 + ((other >> s) & 255) * (255 - a3)) / 255).min(255);
                    ch(24) << 24 | ch(16) << 16 | ch(8) << 8 | 255
                } else if info & 16 != 0 {
                    brighten(c3, self.bright.0, self.bright.1)
                } else {
                    c3 | 255
                }
            }
            2 => {
                let ch = |s: u32| ((((other >> s) & 255) * self.eva + ((c3 >> s) & 255) * self.evb) / 16).min(255);
                ch(24) << 24 | ch(16) << 16 | ch(8) << 8 | 255
            }
            _ => other,
        };
        if matches!(self.master.0, 1 | 2) && self.master.1 != 0 {
            c = brighten(c, if self.master.0 == 1 { 2 } else { 3 }, self.master.1);
        }
        c
    }

    /// The finished screen at 256x192 from a 3D layer of the same size (the software path).
    pub fn compose(&self, layer3d: Option<&[u32]>) -> Vec<u32> {
        (0..W * H).map(|i| self.pixel(i, layer3d.map_or(0, |l| l[i]))).collect()
    }
}

/// Render engine `engine` (0 = A, 1 = B) from the live memory model.
pub fn render_bus(hw: &mut crate::ds_mem::Bus, engine: usize) -> Vec<u32> {
    render_bus_with_3d(hw, engine, None)
}

/// As [`render_bus`], with the 3D frame available to engine A's BG0.
pub fn render_bus_with_3d(
    hw: &mut crate::ds_mem::Bus,
    engine: usize,
    layer3d: Option<&[u32]>,
) -> Vec<u32> {
    render_memory(&hw.io, &hw.palette, &hw.oam, &hw.vram, engine, layer3d)
}

/// Render from borrowed GPU memory; shared by live-bus and immutable-snapshot callers.
pub(crate) fn render_memory(
    io: &[u8], palette: &[u8], oam_bytes: &[u8], vram: &[Vec<u8>],
    engine: usize, layer3d: Option<&[u32]>,
) -> Vec<u32> {
    with_engine(io, palette, oam_bytes, vram, engine, layer3d, |e| e.render())
}

/// As [`render_memory`], with the 3D layer left open ([`Engine2d::render_split`]).
pub(crate) fn split_memory(io: &[u8], palette: &[u8], oam_bytes: &[u8], vram: &[Vec<u8>], engine: usize) -> Split {
    with_engine(io, palette, oam_bytes, vram, engine, None, |e| e.render_split())
}

/// Builds engine `engine` over borrowed GPU memory and hands it to `f`.
fn with_engine<R>(
    io: &[u8], palette: &[u8], oam_bytes: &[u8], vram: &[Vec<u8>],
    engine: usize, layer3d: Option<&[u32]>, f: impl FnOnce(&Engine2d) -> R,
) -> R {
    let io_off = if engine == 0 { 0 } else { 0x1000 };
    let regs: Vec<u8> = (0..0x70).map(|i| io[io_off + i]).collect();
    let pal: Vec<u8> = palette[engine * 0x400..engine * 0x400 + 0x400].to_vec();
    let oam: Vec<u8> = oam_bytes[engine * 0x400..engine * 0x400 + 0x400].to_vec();
    let (ext_bg, ext_obj) = extended_palettes(io, vram, engine);
    let (bg, obj) = if engine == 0 {
        (0x0600_0000, 0x0640_0000)
    } else {
        (0x0620_0000, 0x0660_0000)
    };
    // Resolve bank mappings once per frame, rather than doing a CPU bus lookup
    // (and scanning all nine banks) for every tile/palette sample of every pixel.
    let mut bg_bytes = vec![0u8; if engine == 0 { 0x80000 } else { 0x20000 }];
    let mut obj_bytes = vec![0u8; if engine == 0 { 0x40000 } else { 0x20000 }];
    for bank in 0..9 {
        let cnt = io[if bank < 7 {
            0x240 + bank
        } else {
            0x248 + bank - 7
        }];
        if cnt & 0x80 == 0 {
            continue;
        }
        let Some(base) = crate::ds_mem::vram_place(bank, cnt & 7, (cnt >> 3) & 3) else {
            continue;
        };
        for (start, bytes) in [(bg, &mut bg_bytes), (obj, &mut obj_bytes)] {
            if base < start || base as u64 >= start as u64 + bytes.len() as u64 {
                continue;
            }
            let offset = (base - start) as usize;
            for (to, from) in bytes[offset..].iter_mut().zip(&vram[bank]) {
                *to |= *from;
            }
        }
    }
    let read = |a: u32| {
        a.checked_sub(bg)
            .and_then(|i| bg_bytes.get(i as usize))
            .or_else(|| a.checked_sub(obj).and_then(|i| obj_bytes.get(i as usize)))
            .copied()
            .unwrap_or(0)
    };
    f(&Engine2d {
        regs: &regs,
        vram: &read,
        bg_base: bg,
        obj_base: obj,
        pal: &pal,
        oam: &oam,
        layer3d,
        ext_bg: Some(&ext_bg),
        ext_obj: Some(&ext_obj),
    })
}

/// Extended palettes are GPU mappings, not CPU BG/OBJ VRAM windows.
fn extended_palettes(io: &[u8], vram: &[Vec<u8>], engine: usize) -> (Vec<u8>, Vec<u8>) {
    let mut bg = vec![0; 0x8000];
    let mut obj = vec![0; 0x2000];
    for (bank, reg) in [
        0x240, 0x241, 0x242, 0x243, 0x244, 0x245, 0x246, 0x248, 0x249,
    ]
    .into_iter()
    .enumerate()
    {
        let control = io[reg];
        if control & 0x80 == 0 {
            continue;
        }
        let mapping = match (engine, bank, control & 7) {
            (0, 4, 4) | (1, 7, 2) => Some((false, 0, 0x8000)),
            (0, 5 | 6, 4) => Some((false, ((control as usize >> 3) & 1) * 0x4000, 0x4000)),
            (0, 5 | 6, 5) | (1, 8, 3) => Some((true, 0, 0x2000)),
            _ => None,
        };
        if let Some((is_obj, offset, size)) = mapping {
            let target = if is_obj { &mut obj } else { &mut bg };
            for (to, from) in target[offset..offset + size]
                .iter_mut()
                .zip(&vram[bank][..size])
            {
                *to |= *from;
            }
        }
    }
    (bg, obj)
}

/// Binary PPM (P6) of a packed-RGBA frame.
pub fn to_ppm(frame: &[u32]) -> Vec<u8> {
    let mut out = format!(
        "P6
{W} {H}
255
"
    )
    .into_bytes();
    for &p in frame {
        out.extend_from_slice(&[(p >> 24) as u8, (p >> 16) as u8, (p >> 8) as u8]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn mode5_affine_minimap_uses_transforms_flips_and_extended_palette() {
        use crate::rt::Mem;
        let mut hw = crate::ds_mem::Bus::new();
        hw.w8(0x0400_0242, 0x84);
        hw.w8(0x0400_0248, 0x82);
        hw.w32(0x0400_1000, (1 << 30) | (1 << 16) | 0x400 | 5);
        hw.w16(0x0400_100c, 0x0104); // BG2: extended tiles, chars 0x4000, map 0x800
        hw.w16(0x0400_1020, 256);
        hw.w16(0x0400_1026, 256);
        hw.w16(0x0620_0800, 0x5401); // palette 5, horizontal flip, tile 1
        hw.w8(0x0620_4047, 3);
        let offset = 2 * 0x2000 + 5 * 0x200 + 3 * 2;
        hw.vram[7][offset..offset + 2].copy_from_slice(&0x001fu16.to_le_bytes());
        assert_eq!(render_bus(&mut hw, 1)[0], 0xff0000ff);
        hw.w32(0x0400_1028, 0x0fff_ff00); // -1 pixel reference: transparent without wrap
        assert_eq!(render_bus(&mut hw, 1)[0], 0x000000ff);
        assert_eq!(render_bus(&mut hw, 1)[1], 0xff0000ff);
    }

    #[test]
    fn mode5_direct_bitmap_obeys_pixel_alpha() {
        use crate::rt::Mem;
        let mut hw = crate::ds_mem::Bus::new();
        hw.w8(0x0400_0242, 0x84);
        hw.w32(0x0400_1000, (1 << 16) | 0x800 | 5);
        hw.w16(0x0400_100e, 0x0084); // BG3 direct-colour bitmap
        hw.w16(0x0400_1030, 256);
        hw.w16(0x0400_1036, 256);
        hw.w16(0x0620_0000, 0x801f);
        hw.w16(0x0620_0002, 0x001f);
        hw.w16(0x0500_0400, 0x03e0);
        let image = render_bus(&mut hw, 1);
        assert_eq!(image[0], 0xff0000ff);
        assert_eq!(
            image[1], 0x00ff00ff,
            "transparent bitmap pixel exposes backdrop"
        );
    }

    #[test]
    fn sub_engine_uses_extended_palette_bank_and_selected_bg_slot() {
        use crate::rt::Mem;
        let mut hw = crate::ds_mem::Bus::new();
        hw.w8(0x0400_0242, 0x84); // C: sub BG data
        hw.w8(0x0400_0248, 0x82); // H: sub extended BG palette
        hw.w32(0x0400_1000, (1 << 30) | (1 << 16) | 0x100);
        hw.w16(0x0400_1008, 0x2180); // slot 2, 8bpp, screen base 1
        hw.w8(0x0620_0000, 3);
        hw.w16(0x0620_0800, 0x5000); // tile 0, extended palette 5
        let offset = 2 * 0x2000 + 5 * 0x200 + 3 * 2;
        hw.vram[7][offset..offset + 2].copy_from_slice(&0x03e0u16.to_le_bytes());
        assert_eq!(render_bus(&mut hw, 1)[0], 0x00ff00ff);
        hw.w8(0x0400_0248, 0); // bank disabled: no accidental normal-palette fallback
        assert_eq!(render_bus(&mut hw, 1)[0], 0x000000ff);
    }

    #[test]
    fn master_brightness_and_forced_blank_apply_to_the_output() {
        use crate::rt::Mem;
        let mut hw = crate::ds_mem::Bus::new();
        hw.w32(0x0400_0000, 1 << 16);
        hw.w16(0x0500_0000, 31);
        hw.w16(0x0400_006c, 0x8010);
        assert!(render_bus(&mut hw, 0).iter().all(|p| *p == 0x000000ff));
        hw.w16(0x0400_006c, 0x401f); // clamp coefficient to 16
        assert!(render_bus(&mut hw, 0).iter().all(|p| *p == 0xffffffff));
        hw.w16(0x0400_006c, 0);
        hw.w32(0x0400_0000, (1 << 16) | 0x80);
        assert!(render_bus(&mut hw, 0).iter().all(|p| *p == 0xffffffff));
    }

    fn regs(dispcnt: u32) -> Vec<u8> {
        let mut r = vec![0u8; 0x70];
        r[0..4].copy_from_slice(&dispcnt.to_le_bytes());
        r
    }
    fn set16(r: &mut [u8], off: usize, v: u16) {
        r[off..off + 2].copy_from_slice(&v.to_le_bytes());
    }

    /// `render_split` + `Split::compose` must equal `render` for any registers, sprites and 3D layer: random
    /// blend modes, coefficients, windows, master brightness, layer priorities and 3D pixels (none, opaque,
    /// translucent), with garbage VRAM and OAM so every layer shows something.
    #[test]
    fn split_then_compose_equals_render_for_random_screens() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let vram_seed = next();
        let read = move |a: u32| ((a as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ vram_seed).rotate_left(a & 31) as u8;
        for case in 0..60 {
            let mut r = vec![0u8; 0x70];
            for b in r.iter_mut() {
                *b = next() as u8;
            }
            let set = |r: &mut Vec<u8>, off: usize, v: u32| r[off..off + 2].copy_from_slice(&(v as u16).to_le_bytes());
            let dispcnt = (1 << 16) | 8 | (next() as u32 & 0xff00) | 0x100; // graphics mode, BG0 = 3D, BG0 on
            r[..4].copy_from_slice(&(dispcnt & !0x87).to_le_bytes()); // mode 0 text BGs, no forced blank
            set(&mut r, 0x6c, match case % 4 { 0 => 0, 1 => 0x4000 | 9, 2 => 0x8000 | 16, _ => next() as u32 });
            let pal: Vec<u8> = (0..1024).map(|_| next() as u8).collect();
            let oam: Vec<u8> = (0..1024).map(|_| next() as u8).collect();
            let layer: Vec<u32> = (0..W * H)
                .map(|_| {
                    let v = next();
                    let rgb = (v as u32) & 0xffff_ff00;
                    match v >> 62 {
                        0 => 0,
                        1 => rgb | 255,
                        _ => rgb | ((v >> 40) as u32 % 254 + 1),
                    }
                })
                .collect();
            let engine = |layer3d| Engine2d {
                regs: &r,
                vram: &read,
                bg_base: 0x0600_0000,
                obj_base: 0x0640_0000,
                pal: &pal,
                oam: &oam,
                layer3d,
                ext_bg: None,
                ext_obj: None,
            };
            let want = engine(Some(&layer)).render();
            let got = engine(None).render_split().compose(Some(&layer));
            let bad = want.iter().zip(&got).position(|(a, b)| a != b);
            assert!(bad.is_none(), "case {case}: pixel {bad:?} differs ({:08x} vs {:08x})", want[bad.unwrap()], got[bad.unwrap()]);
        }
    }

    fn pal_with(entries: &[(usize, u16)]) -> Vec<u8> {
        let mut p = vec![0u8; 1024];
        for &(i, c) in entries {
            p[i * 2..i * 2 + 2].copy_from_slice(&c.to_le_bytes());
        }
        p
    }

    #[test]
    fn text_bg_tile_flip_and_scroll() {
        // BG0 on, mode 0, screen block 0, char block 0 (char base 0, screen base 0 from BGCNT)
        let mut r = regs(1 << 16 | 0x100);
        set16(&mut r, 8, (1 << 8) | 0); // screen base 1 (0x800), char base 0, 16-colour
        let mut vram: HashMap<u32, u8> = HashMap::new();
        // tile 1: pixel (0,0) = index 3 (low nibble of first byte)
        vram.insert(32, 0x03);
        // map entry (0,0) = tile 1, palette 2; entry (1,0) = tile 1 with hflip
        vram.insert(0x800, 1);
        vram.insert(0x801, 0x20);
        vram.insert(0x802, 1);
        vram.insert(0x803, 0x24);
        let pal = pal_with(&[(0, 0x001F), (2 * 16 + 3, 0x7C00)]);
        let read = |a: u32| *vram.get(&(a - 0x0600_0000)).unwrap_or(&0);
        let e = Engine2d {
            regs: &r,
            vram: &read,
            bg_base: 0x0600_0000,
            obj_base: 0x0640_0000,
            pal: &pal,
            oam: &[0; 1024],
            layer3d: None,
            ext_bg: None,
            ext_obj: None,
        };
        let img = e.render();
        assert_eq!(img[0], 0x0000_FFFF, "blue tile pixel"); // 0x7C00 = blue
        assert_eq!(img[1], 0xFF00_00FF, "transparent shows backdrop (red)");
        assert_eq!(
            img[8 + 7],
            0x0000_FFFF,
            "hflipped tile: pixel lands at x=7 of tile 1"
        );
        // scroll right by 8: tile 1's pixel moves to x=0
        let mut r2 = r.clone();
        set16(&mut r2, 0x10, 8);
        let e2 = Engine2d {
            regs: &r2,
            vram: &read,
            bg_base: 0x0600_0000,
            obj_base: 0x0640_0000,
            pal: &pal,
            oam: &[0; 1024],
            layer3d: None,
            ext_bg: None,
            ext_obj: None,
        };
        assert_eq!(e2.render()[7], 0x0000_FFFF);
    }

    #[test]
    fn windows_and_blend_targets_control_composition() {
        let mut r = regs((1 << 16) | 0x0300 | 0x2000);
        set16(&mut r, 8, 0x100); // BG0 red, priority 0
        set16(&mut r, 10, 0x201); // BG1 blue, priority 1
        set16(&mut r, 0x40, 0x0008);
        set16(&mut r, 0x44, 0x0008);
        set16(&mut r, 0x48, 0x23); // inside: both BGs and effects
        set16(&mut r, 0x4a, 0x02); // outside: BG1 only, effects disabled
        set16(&mut r, 0x50, 0x0241); // BG0 over BG1
        set16(&mut r, 0x52, 0x0808);
        let read = |a: u32| match a.checked_sub(0x0600_0000) {
            Some(0..=31) => 0x11,
            Some(32..=63) => 0x22,
            Some(0x1000..=0x17ff) => {
                if a & 1 == 0 {
                    1
                } else {
                    0
                }
            }
            _ => 0,
        };
        let pal = pal_with(&[(1, 0x001f), (2, 0x7c00)]);
        let draw = |r: &[u8]| {
            Engine2d {
                regs: r,
                vram: &read,
                bg_base: 0x0600_0000,
                obj_base: 0x0640_0000,
                pal: &pal,
                oam: &[0; 1024],
                layer3d: None,
                ext_bg: None,
                ext_obj: None,
            }
            .render()
        };
        let image = draw(&r);
        assert_eq!(image[0], 0x7f007fff, "red/blue blend inside the window");
        assert_eq!(image[8], 0x0000ffff, "BG0 is masked outside the window");
        set16(&mut r, 0x50, 0x00c1); // darken BG0
        set16(&mut r, 0x54, 8);
        assert_eq!(draw(&r)[0], 0x800000ff);
        set16(&mut r, 0x40, 0xf808); // wrapped horizontal window
        assert_eq!(draw(&r)[255], 0x800000ff);
    }

    #[test]
    fn affine_objects_keep_their_center_and_obey_object_priority() {
        let r = regs((1 << 16) | 0x1010);
        let mut oam = vec![0; 1024];
        for i in 0..128 {
            set16(&mut oam, i * 8, 0x200);
        }
        set16(&mut oam, 0, 20 | 0x100);
        set16(&mut oam, 2, 20);
        set16(&mut oam, 4, 2 << 10); // red affine object, priority 2
        set16(&mut oam, 6, 256);
        set16(&mut oam, 30, 256); // identity, matrix 0
        set16(&mut oam, 8, 20);
        set16(&mut oam, 10, 20);
        set16(&mut oam, 12, 1); // green regular object, priority 0, later in OAM
        let read = |a: u32| match a.checked_sub(0x0640_0000) {
            Some(0..=31) => 0x11,
            Some(32..=63) => 0x22,
            _ => 0,
        };
        let pal = pal_with(&[(257, 0x001f), (258, 0x03e0)]);
        let draw = |oam: &[u8]| {
            Engine2d {
                regs: &r,
                vram: &read,
                bg_base: 0x0600_0000,
                obj_base: 0x0640_0000,
                pal: &pal,
                oam,
                layer3d: None,
                ext_bg: None,
                ext_obj: None,
            }
            .render()
        };
        assert_eq!(
            draw(&oam)[20 * W + 20],
            0x00ff00ff,
            "later object with higher priority wins"
        );
        set16(&mut oam, 12, 1 | (2 << 10));
        assert_eq!(
            draw(&oam)[20 * W + 20],
            0xff0000ff,
            "OAM order breaks equal-priority ties"
        );
        set16(&mut oam, 8, 0x200);
        set16(&mut oam, 0, 20 | 0x300); // double bounding area, texture stays centered
        let image = draw(&oam);
        assert_eq!(image[20 * W + 20], 0x000000ff);
        assert_eq!(image[24 * W + 24], 0xff0000ff);
        assert_eq!(image[31 * W + 31], 0xff0000ff);
        assert_eq!(image[32 * W + 32], 0x000000ff);
    }

    #[test]
    fn bitmap_object_uses_direct_color_alpha_and_bitmap_mapping() {
        let r = regs((1 << 16) | 0x1040); // bitmap 1D, 128-byte tile steps
        let mut oam = vec![0; 1024];
        for i in 0..128 {
            set16(&mut oam, i * 8, 0x200);
        }
        set16(&mut oam, 0, 0x0c00);
        set16(&mut oam, 4, 0xf001);
        let read = |a: u32| match a.checked_sub(0x0640_0080) {
            Some(0) => 0x1f,
            Some(1) => 0x80,
            Some(2) => 0x1f,
            _ => 0,
        };
        let pal = pal_with(&[(0, 0x03e0)]);
        let image = Engine2d {
            regs: &r,
            vram: &read,
            bg_base: 0x0600_0000,
            obj_base: 0x0640_0000,
            pal: &pal,
            oam: &oam,
            layer3d: None,
            ext_bg: None,
            ext_obj: None,
        }
        .render();
        assert_eq!(image[0], 0xff0000ff);
        assert_eq!(
            image[1], 0x00ff00ff,
            "direct-color bit15 controls transparency"
        );
    }

    #[test]
    fn sprite_above_background_at_equal_priority() {
        let mut r = regs(1 << 16 | 0x100 | 0x1000 | 0x10); // BG0 + OBJ, 1D mapping
        set16(&mut r, 8, 1 << 8); // screen map at 0x800 (all entries = tile 0)
        let mut vram: HashMap<u32, u8> = HashMap::new();
        vram.insert(0, 0x11); // BG tile 0 all index 1 at row 0
        vram.insert(0x400000, 0x22); // obj tile 0, pixel(0,0)/(1,0) index 2
        for i in 0..4 {
            vram.insert(i, 0x11);
        }
        let mut oam = vec![0u8; 1024];
        oam[0..2].copy_from_slice(&0u16.to_le_bytes()); // y=0, normal, square
        oam[2..4].copy_from_slice(&0u16.to_le_bytes()); // x=0, size 8x8
        oam[4..6].copy_from_slice(&0u16.to_le_bytes()); // tile 0, prio 0, pal 0
        for i in 1..128 {
            oam[i * 8] = 160; // park the rest off-screen at y=160 (width irrelevant) ... and hide
            oam[i * 8 + 1] = 0x02; // hidden
        }
        let pal = pal_with(&[(1, 0x03E0), (256 + 2, 0x001F)]);
        let read = |a: u32| *vram.get(&(a - 0x0600_0000)).unwrap_or(&0);
        let e = Engine2d {
            regs: &r,
            vram: &read,
            bg_base: 0x0600_0000,
            obj_base: 0x0640_0000,
            pal: &pal,
            oam: &oam,
            layer3d: None,
            ext_bg: None,
            ext_obj: None,
        };
        let img = e.render();
        assert_eq!(img[0], 0xFF00_00FF, "sprite (red) over BG (green)");
        assert_eq!(
            img[2], 0x00FF_00FF,
            "BG where the sprite pixel is transparent... index 0 of sprite -> BG visible"
        );
    }

    #[test]
    fn render_from_bus_and_ppm() {
        let mut hw = crate::ds_mem::Bus::new();
        hw.io[0] = 0;
        hw.io[2] = 1; // DISPCNT bits 16-17 = 1: normal display
        hw.palette[0] = 0x1F; // backdrop red
        let f = render_bus(&mut hw, 0);
        assert_eq!(f[0], 0xFF00_00FF);
        let ppm = to_ppm(&f);
        assert!(ppm.starts_with(
            b"P6
256 192
255
"
        ));
        assert_eq!(ppm.len(), 15 + W * H * 3);
        assert_eq!(&ppm[15..18], &[0xFF, 0, 0]);
    }

    #[test]
    fn display_off_is_white() {
        let r = regs(0);
        let read = |_a: u32| 0u8;
        let e = Engine2d {
            regs: &r,
            vram: &read,
            bg_base: 0,
            obj_base: 0,
            pal: &[0; 1024],
            oam: &[0; 1024],
            layer3d: None,
            ext_bg: None,
            ext_obj: None,
        };
        assert!(e.render().iter().all(|&p| p == 0xFFFF_FFFF));
    }
}
