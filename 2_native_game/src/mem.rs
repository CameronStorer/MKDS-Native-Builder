//! Test memory: the ARM9 image (read-only base) plus a write overlay; unmapped reads return
//! deterministic pseudo-random bytes so both implementations see identical data. As a [`Bus`] it
//! stubs calls, BIOS calls and coprocessor reads deterministically (and records them), so
//! lifted code and the interpreter can be compared function by function.
use crate::rt::{Bus, Cpu, Mem};
use std::collections::BTreeMap;

pub struct Ram<'a> {
    pub image: &'a [u8],
    pub base: u32,
    pub seed: u64,
    pub writes: BTreeMap<u32, u8>,
    /// Calls (target, return), BIOS calls (0xFFFF_0000 | n, 0) and MCRs (0xFFFE_0000, value), in order.
    pub events: Vec<(u32, u32)>,
}

impl<'a> Ram<'a> {
    pub fn new(image: &'a [u8], base: u32, seed: u64) -> Self {
        Ram { image, base, seed, writes: BTreeMap::new(), events: Vec::new() }
    }
    fn mix(&self, a: u64) -> u64 {
        let mut x = a ^ self.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        x ^= x >> 33;
        x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        x ^= x >> 29;
        x
    }
    fn byte(&self, a: u32) -> u8 {
        if let Some(&b) = self.writes.get(&a) {
            return b;
        }
        if let Some(&b) = a.checked_sub(self.base).and_then(|o| self.image.get(o as usize)) {
            return b;
        }
        self.mix(a as u64) as u8
    }
    /// What a stubbed callee leaves in the caller-saved registers.
    fn clobber(&mut self, c: &mut Cpu, key: u64) {
        let k = self.mix(key ^ (self.events.len() as u64) << 40);
        for (i, r) in [0usize, 1, 2, 3, 12].into_iter().enumerate() {
            c.r[r] = (k >> (i * 7)) as u32;
        }
    }
}

impl Mem for Ram<'_> {
    fn r8(&mut self, a: u32) -> u8 {
        self.byte(a)
    }
    fn r16(&mut self, a: u32) -> u16 {
        u16::from_le_bytes([self.byte(a), self.byte(a.wrapping_add(1))])
    }
    fn r32(&mut self, a: u32) -> u32 {
        u32::from_le_bytes([self.byte(a), self.byte(a.wrapping_add(1)), self.byte(a.wrapping_add(2)), self.byte(a.wrapping_add(3))])
    }
    fn w8(&mut self, a: u32, v: u8) {
        self.writes.insert(a, v);
    }
    fn w16(&mut self, a: u32, v: u16) {
        for (i, b) in v.to_le_bytes().into_iter().enumerate() {
            self.writes.insert(a.wrapping_add(i as u32), b);
        }
    }
    fn w32(&mut self, a: u32, v: u32) {
        for (i, b) in v.to_le_bytes().into_iter().enumerate() {
            self.writes.insert(a.wrapping_add(i as u32), b);
        }
    }
}

impl Bus for Ram<'_> {
    fn call(&mut self, c: &mut Cpu, target: u32, ret: u32) -> u32 {
        self.events.push((target, ret));
        self.clobber(c, target as u64);
        ret
    }
    fn swi(&mut self, c: &mut Cpu, n: u32) {
        self.events.push((0xFFFF_0000 | n, 0));
        self.clobber(c, 0x5000 + n as u64);
    }
    fn mrc(&mut self, _c: &mut Cpu, cp: u8, op1: u8, crn: u8, crm: u8, op2: u8) -> u32 {
        self.mix(u64::from_le_bytes([cp, op1, crn, crm, op2, 0, 0, 0x77])) as u32
    }
    fn mcr(&mut self, _c: &mut Cpu, _cp: u8, _op1: u8, _crn: u8, _crm: u8, _op2: u8, v: u32) {
        self.events.push((0xFFFE_0000, v));
    }
}

pub type LiftedFn = fn(&mut Cpu, &mut dyn Bus, u32) -> u32;

/// One lifted function: its segment (`arm9_main`, `arm9_autoload1_itcm`, `ov9_000`...: overlays
/// can share addresses, so a pc only identifies code together with the loaded segment), entry
/// (bit 0 = Thumb), block starts, instruction addresses and code.
pub struct Lifted {
    pub segment: &'static str,
    pub entry: u32,
    pub blocks: &'static [u32],
    pub insts: &'static [u32],
    pub f: LiftedFn,
}
