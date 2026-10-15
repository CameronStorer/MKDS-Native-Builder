//! BIOS software-interrupt replacements (recomp lane L5, T060 part: the `SWI` calls).
//!
//! The ARM9 BIOS is not in the user's ROM, so every `SWI` the game executes is answered here
//! (HLE). The game reaches them through a small block of Thumb wrappers (`bx`-less stubs between
//! `0x02000000` and `0x02000800`, e.g. the VBlankIntrWait wrapper `sub_2000142`, CpuSet `sub_20003D4`,
//! WaitByLoop `sub_2000472`); a lifted wrapper calls [`swi`] with the number and the register file.
//!
//! Implemented (GBATEK "BIOS Functions", ARM9 numbering): 0x00 SoftReset, 0x03 WaitByLoop, 0x04 IntrWait,
//! 0x05 VBlankIntrWait, 0x06 Halt, 0x09 Div, 0x0B CpuSet, 0x0C CpuFastSet, 0x0D Sqrt, 0x0E GetCRC16,
//! 0x0F IsDebugger, 0x10 BitUnPack, 0x11/0x12 LZ77 (WRAM / VRAM), 0x13 Huffman, 0x14/0x15 RLE.
//! Waiting calls do not block here: they return [`Swi::Wait`] and the scheduler decides.
//!
//! Nothing here is game data; it is the documented behaviour of the console's BIOS.
use crate::rt::Mem;

/// What the caller must do after a SWI.
#[derive(Debug, PartialEq, Eq)]
pub enum Swi {
    /// Registers/memory updated; continue after the SWI.
    Done,
    /// Sleep until an interrupt in `mask` is flagged (`discard`: clear old flags first). Halt = mask 0.
    Wait { mask: u32, discard: bool },
    /// Restart the program (SoftReset).
    Reset,
    /// A number this module does not implement, or a call that would hang the real BIOS (divide by zero).
    Unsupported(u32),
}

fn isqrt(v: u32) -> u32 {
    let mut r = (v as f64).sqrt() as u32;
    while (r as u64) * (r as u64) > v as u64 {
        r -= 1;
    }
    while ((r + 1) as u64) * ((r + 1) as u64) <= v as u64 {
        r += 1;
    }
    r
}

pub fn crc16(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
        }
    }
    crc
}

fn cpu_set(r: &mut [u32; 16], m: &mut dyn Mem, fast: bool) {
    let (mut s, mut d, ctl) = (r[0], r[1], r[2]);
    let mut n = ctl & 0x1F_FFFF;
    let fill = ctl & (1 << 24) != 0;
    let word = fast || ctl & (1 << 26) != 0;
    if fast {
        n = (n + 7) & !7;
    }
    let unit = if word { 4 } else { 2 };
    let first = if word { m.r32(s & !3) } else { m.r16(s & !1) as u32 };
    for _ in 0..n {
        let v = if fill {
            first
        } else if word {
            m.r32(s & !3)
        } else {
            m.r16(s & !1) as u32
        };
        if word {
            m.w32(d & !3, v);
        } else {
            m.w16(d & !1, v as u16);
        }
        s = s.wrapping_add(unit);
        d = d.wrapping_add(unit);
    }
}

fn read_bytes(m: &mut dyn Mem, a: u32, n: usize) -> Vec<u8> {
    (0..n as u32).map(|i| m.r8(a.wrapping_add(i))).collect()
}

/// Write decompressed output: bytewise to WRAM-style destinations, halfword-wise to VRAM-style ones.
fn put(m: &mut dyn Mem, dst: u32, out: &[u8], vram: bool) {
    if vram {
        for (i, c) in out.chunks(2).enumerate() {
            let v = c[0] as u16 | (*c.get(1).unwrap_or(&0) as u16) << 8;
            m.w16(dst.wrapping_add(2 * i as u32), v);
        }
    } else {
        for (i, &b) in out.iter().enumerate() {
            m.w8(dst.wrapping_add(i as u32), b);
        }
    }
}

fn lz77(m: &mut dyn Mem, src: u32, dst: u32, vram: bool) {
    let size = (m.r32(src) >> 8) as usize;
    let mut p = src + 4;
    let mut out: Vec<u8> = Vec::with_capacity(size);
    while out.len() < size {
        let flags = m.r8(p);
        p += 1;
        for bit in (0..8).rev() {
            if out.len() >= size {
                break;
            }
            if flags >> bit & 1 != 0 {
                let (b0, b1) = (m.r8(p) as usize, m.r8(p + 1) as usize);
                p += 2;
                let len = (b0 >> 4) + 3;
                let disp = ((b0 & 0xF) << 8 | b1) + 1;
                for _ in 0..len {
                    if out.len() >= size {
                        break;
                    }
                    let v = if disp <= out.len() { out[out.len() - disp] } else { 0 };
                    out.push(v);
                }
            } else {
                out.push(m.r8(p));
                p += 1;
            }
        }
    }
    put(m, dst, &out, vram);
}

fn rle(m: &mut dyn Mem, src: u32, dst: u32, vram: bool) {
    let size = (m.r32(src) >> 8) as usize;
    let mut p = src + 4;
    let mut out: Vec<u8> = Vec::with_capacity(size);
    while out.len() < size {
        let f = m.r8(p);
        p += 1;
        if f & 0x80 != 0 {
            let n = (f & 0x7F) as usize + 3;
            let v = m.r8(p);
            p += 1;
            out.extend(std::iter::repeat(v).take(n));
        } else {
            let n = (f & 0x7F) as usize + 1;
            out.extend(read_bytes(m, p, n));
            p += n as u32;
        }
    }
    out.truncate(size);
    put(m, dst, &out, vram);
}

fn huffman(m: &mut dyn Mem, src: u32, dst: u32) {
    let hdr = m.r32(src);
    let bits = hdr & 0xF; // 4 or 8 bits per symbol
    let size = (hdr >> 8) as usize;
    let tree = src + 4;
    let tree_len = (m.r8(tree) as u32 + 1) * 2;
    let mut stream = tree + tree_len;
    let mut word = 0u32;
    let mut avail = 0u32;
    let (mut out_word, mut out_bits) = (0u32, 0u32);
    let mut written = 0usize;
    let mut cur = tree + 1;
    while written < size {
        if avail == 0 {
            word = m.r32(stream);
            stream += 4;
            avail = 32;
        }
        let bit = word >> 31 & 1;
        word <<= 1;
        avail -= 1;
        let node = m.r8(cur) as u32;
        let next = (cur & !1) + (node & 0x3F) * 2 + 2 + bit;
        let leaf = if bit == 0 { node & 0x80 } else { node & 0x40 } != 0;
        if leaf {
            let v = m.r8(next) as u32 & ((1 << bits) - 1);
            out_word |= v << out_bits;
            out_bits += bits;
            if out_bits == 32 {
                m.w32(dst.wrapping_add(written as u32), out_word);
                written += 4;
                out_word = 0;
                out_bits = 0;
            }
            cur = tree + 1;
        } else {
            cur = next;
        }
    }
}

fn bit_unpack(m: &mut dyn Mem, src: u32, dst: u32, info: u32) {
    let src_len = m.r16(info) as u32;
    let (sw, dw) = (m.r8(info + 2) as u32, m.r8(info + 3) as u32);
    let ofs_word = m.r32(info + 4);
    let (ofs, zero) = (ofs_word & 0x7FFF_FFFF, ofs_word >> 31 != 0);
    let (mut word, mut filled, mut w) = (0u32, 0u32, 0u32);
    for i in 0..src_len {
        let b = m.r8(src + i) as u32;
        for k in 0..(8 / sw.max(1)) {
            let mut v = b >> (k * sw) & ((1 << sw) - 1);
            if v != 0 || zero {
                v = v.wrapping_add(ofs);
            }
            word |= (v & if dw >= 32 { u32::MAX } else { (1 << dw) - 1 }) << filled;
            filled += dw;
            if filled >= 32 {
                m.w32(dst + 4 * w, word);
                w += 1;
                word = 0;
                filled = 0;
            }
        }
    }
}

/// Run BIOS call `num` on register file `r` (r0..r3 are arguments/results). Returns what to do next.
pub fn swi(num: u32, r: &mut [u32; 16], m: &mut dyn Mem) -> Swi {
    match num {
        0x00 => Swi::Reset,
        0x03 => Swi::Done, // WaitByLoop: a pure delay; the host does not need to burn cycles
        0x04 => Swi::Wait { mask: r[1], discard: r[0] != 0 }, // IntrWait(discard_old, flags)
        0x05 => Swi::Wait { mask: 1, discard: true },         // VBlankIntrWait
        0x06 => Swi::Wait { mask: 0, discard: false },        // Halt
        0x09 => {
            let (n, d) = (r[0] as i32, r[1] as i32);
            if d == 0 {
                return Swi::Unsupported(num);
            }
            let (q, rem) = (n.wrapping_div(d), n.wrapping_rem(d));
            r[0] = q as u32;
            r[1] = rem as u32;
            r[3] = q.unsigned_abs();
            Swi::Done
        }
        0x0B => {
            cpu_set(r, m, false);
            Swi::Done
        }
        0x0C => {
            cpu_set(r, m, true);
            Swi::Done
        }
        0x0D => {
            r[0] = isqrt(r[0]);
            Swi::Done
        }
        0x0E => {
            let data = read_bytes(m, r[1], r[2] as usize & !1);
            r[0] = crc16(r[0] as u16, &data) as u32;
            Swi::Done
        }
        0x0F => {
            r[0] = 0; // no debugger attached
            Swi::Done
        }
        0x10 => {
            bit_unpack(m, r[0], r[1], r[2]);
            Swi::Done
        }
        0x11 | 0x12 => {
            lz77(m, r[0], r[1], num == 0x12);
            Swi::Done
        }
        0x13 => {
            huffman(m, r[0], r[1]);
            Swi::Done
        }
        0x14 | 0x15 => {
            rle(m, r[0], r[1], num == 0x15);
            Swi::Done
        }
        _ => Swi::Unsupported(num),
    }
}

/// Machine entry point (kept from the first version of this module): run the SWIs that need no scheduler
/// decision against the CPU's registers. Returns `true` when handled; `false` for waits (4-6), reset,
/// divide by zero and unknown numbers, which the caller deals with.
pub fn swi_pure<M: Mem>(n: u32, c: &mut crate::rt::Cpu, m: &mut M) -> bool {
    swi(n, &mut c.r, m) == Swi::Done
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct M(HashMap<u32, u8>);
    impl Mem for M {
        fn r8(&mut self, a: u32) -> u8 {
            *self.0.get(&a).unwrap_or(&0)
        }
        fn r16(&mut self, a: u32) -> u16 {
            self.r8(a) as u16 | (self.r8(a + 1) as u16) << 8
        }
        fn r32(&mut self, a: u32) -> u32 {
            self.r16(a) as u32 | (self.r16(a + 2) as u32) << 16
        }
        fn w8(&mut self, a: u32, v: u8) {
            self.0.insert(a, v);
        }
        fn w16(&mut self, a: u32, v: u16) {
            self.w8(a, v as u8);
            self.w8(a + 1, (v >> 8) as u8);
        }
        fn w32(&mut self, a: u32, v: u32) {
            self.w16(a, v as u16);
            self.w16(a + 2, (v >> 16) as u16);
        }
    }
    fn put_bytes(m: &mut M, a: u32, b: &[u8]) {
        for (i, &x) in b.iter().enumerate() {
            m.w8(a + i as u32, x);
        }
    }
    fn get_bytes(m: &mut M, a: u32, n: usize) -> Vec<u8> {
        read_bytes(m, a, n)
    }

    #[test]
    fn div_and_sqrt() {
        let mut m = M::default();
        let mut r = [0u32; 16];
        r[0] = (-7i32) as u32;
        r[1] = 2;
        assert_eq!(swi(9, &mut r, &mut m), Swi::Done);
        assert_eq!((r[0] as i32, r[1] as i32, r[3]), (-3, -1, 3));
        r[1] = 0;
        assert_eq!(swi(9, &mut r, &mut m), Swi::Unsupported(9));
        r[0] = 99_999_999;
        swi(0x0D, &mut r, &mut m);
        assert_eq!(r[0], 9999);
    }

    #[test]
    fn cpu_set_copy_and_fill() {
        let mut m = M::default();
        put_bytes(&mut m, 0x100, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let mut r = [0u32; 16];
        (r[0], r[1], r[2]) = (0x100, 0x200, 2 | 1 << 26);
        swi(0x0B, &mut r, &mut m);
        assert_eq!(get_bytes(&mut m, 0x200, 8), [1, 2, 3, 4, 5, 6, 7, 8]);
        (r[0], r[1], r[2]) = (0x100, 0x300, 3 | 1 << 24); // 16-bit fill
        swi(0x0B, &mut r, &mut m);
        assert_eq!(get_bytes(&mut m, 0x300, 6), [1, 2, 1, 2, 1, 2]);
        (r[0], r[1], r[2]) = (0x100, 0x400, 1 | 1 << 24); // fast fill rounds 1 word up to 8
        swi(0x0C, &mut r, &mut m);
        assert_eq!(get_bytes(&mut m, 0x41C, 4), [1, 2, 3, 4]);
        assert_eq!(get_bytes(&mut m, 0x420, 1), [0]);
    }

    #[test]
    fn crc16_matches_modbus_check_value() {
        assert_eq!(crc16(0xFFFF, b"123456789"), 0x4B37);
        let mut m = M::default();
        put_bytes(&mut m, 0x10, b"123456789\0");
        let mut r = [0u32; 16];
        (r[0], r[1], r[2]) = (0xFFFF, 0x10, 8);
        swi(0x0E, &mut r, &mut m);
        assert_eq!(r[0], crc16(0xFFFF, b"12345678") as u32);
    }

    #[test]
    fn lz77_literal_and_backreference() {
        let mut m = M::default();
        // size 8: flags 0b0010_0000 -> 2 literals 'a','b' then a back-reference (len 6, disp 2), rest unused
        // header: type 1, size 8
        put_bytes(&mut m, 0x100, &[0x10, 8, 0, 0, 0b0010_0000, b'a', b'b', 0x30 | 0, 1]);
        // flags bit7=0 -> 'a'; bit6=0 -> 'b'; bit5=1 -> ref b0=0x30 (len 6), b1=1 (disp 2)
        let mut r = [0u32; 16];
        (r[0], r[1]) = (0x100, 0x200);
        swi(0x11, &mut r, &mut m);
        assert_eq!(get_bytes(&mut m, 0x200, 8), *b"abababab");
        (r[0], r[1]) = (0x100, 0x300);
        swi(0x12, &mut r, &mut m);
        assert_eq!(get_bytes(&mut m, 0x300, 8), *b"abababab");
    }

    #[test]
    fn rle_runs_and_raw() {
        let mut m = M::default();
        put_bytes(&mut m, 0x100, &[0x30, 7, 0, 0, 0x82, 9, 0x01, 1, 2]);
        let mut r = [0u32; 16];
        (r[0], r[1]) = (0x100, 0x200);
        swi(0x14, &mut r, &mut m);
        assert_eq!(get_bytes(&mut m, 0x200, 7), [9, 9, 9, 9, 9, 1, 2]);
    }

    #[test]
    fn huffman_two_symbol_tree() {
        let mut m = M::default();
        // 8-bit symbols; tree: size byte 1 (=> 4 bytes), root node at +1: both children leaves, offset 0;
        // leaves at +2 (bit 0) and +3 (bit 1) = 0xAA, 0x55.
        // header: 0x28 (huffman, 8 bit), decompressed size 4
        // stream (MSB first): bits 0,1,0,1 repeated -> bytes AA 55 AA 55
        let word: u32 = 0b0101_0101_0101_0101_0101_0101_0101_0101;
        let mut data = vec![0x28, 4, 0, 0, 1, 0xC0, 0xAA, 0x55];
        data.extend(word.to_le_bytes());
        put_bytes(&mut m, 0x100, &data);
        let mut r = [0u32; 16];
        (r[0], r[1]) = (0x100, 0x200);
        swi(0x13, &mut r, &mut m);
        assert_eq!(get_bytes(&mut m, 0x200, 4), [0xAA, 0x55, 0xAA, 0x55]);
    }

    #[test]
    fn bit_unpack_1bit_to_8bit() {
        let mut m = M::default();
        put_bytes(&mut m, 0x100, &[0b0000_0101]);
        // info: src_len 1, src width 1, dst width 8, offset 0x10 (add to nonzero only)
        put_bytes(&mut m, 0x180, &[1, 0, 1, 8, 0x10, 0, 0, 0]);
        let mut r = [0u32; 16];
        (r[0], r[1], r[2]) = (0x100, 0x200, 0x180);
        swi(0x10, &mut r, &mut m);
        // bits LSB first: 1,0,1,0,0,0,0,0 -> 0x11 0x00 0x11 0x00 | 0 0 0 0
        assert_eq!(get_bytes(&mut m, 0x200, 8), [0x11, 0, 0x11, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn swi_pure_handles_only_pure_calls() {
        let mut m = M::default();
        let mut c = crate::rt::Cpu::default();
        c.r[0] = 100;
        assert!(swi_pure(0x0D, &mut c, &mut m));
        assert_eq!(c.r[0], 10);
        assert!(!swi_pure(5, &mut c, &mut m));
        assert!(!swi_pure(0x77, &mut c, &mut m));
    }

    #[test]
    fn waits_report_masks() {
        let mut m = M::default();
        let mut r = [0u32; 16];
        assert_eq!(swi(5, &mut r, &mut m), Swi::Wait { mask: 1, discard: true });
        (r[0], r[1]) = (1, 0x40);
        assert_eq!(swi(4, &mut r, &mut m), Swi::Wait { mask: 0x40, discard: true });
        assert_eq!(swi(0x77, &mut r, &mut m), Swi::Unsupported(0x77));
    }
}
