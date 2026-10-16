//! SDK leaf semantics. Guest pointers remain DS addresses; accesses go through the bus.
use crate::rt::{self, Cpu};
use super::{enter, ret};

fn compare(c: &mut Cpu, a: u32, b: u32) { rt::subs(c, a, b); }
fn logical(c: &mut Cpu, v: u32) { c.n = v >> 31 != 0; c.z = v == 0; }
fn save(c: &Cpu, m: &mut dyn rt::Bus, regs: &[usize]) {
    let base = c.r[13].wrapping_sub(4 * regs.len() as u32);
    for (i, &r) in regs.iter().enumerate() {
        rt::str_(m, base.wrapping_add(i as u32 * 4), c.r[r]);
    }
}

/// Only r0 changes; NZCVQ are preserved.
fn count_leading_zeros(c: &mut Cpu, _: &mut dyn rt::Bus) { c.r[0] = c.r[0].leading_zeros(); }

/// Two little-endian register pairs; r0/r1 change. The saved r4/r5/lr remain in memory.
fn multiply64(c: &mut Cpu, m: &mut dyn rt::Bus) {
    save(c, m, &[4, 5, 14]);
    let a = c.r[0] as u64 | ((c.r[1] as u64) << 32);
    let b = c.r[2] as u64 | ((c.r[3] as u64) << 32);
    let product = a.wrapping_mul(b);
    c.r[0] = product as u32;
    c.r[1] = (product >> 32) as u32;
}

/// Word reads have ARM9 rotation semantics, including at unaligned byte positions.
/// r0/r2 = length, r1 = 0; N=0/Z=1, C/V/Q unchanged.
fn strlen(c: &mut Cpu, m: &mut dyn rt::Bus) {
    let mut p = c.r[0];
    let mut n = 0u32;
    while rt::ldr(m, p) & 255 != 0 {
        p = p.wrapping_add(1);
        n = n.wrapping_add(1);
    }
    c.r[0] = n; c.r[1] = 0; c.r[2] = n;
    logical(c, 0);
}

/// Internal fill ABI is (value, dst, bytes), unlike the public MI_CpuFill16 macro.
/// r3 is the rounded byte count; signed comparisons also define negative sizes.
fn fill16(c: &mut Cpu, m: &mut dyn rt::Bus) {
    c.r[3] = 0;
    loop {
        compare(c, c.r[3], c.r[2]);
        if c.n == c.v { break; }
        m.w16(c.r[1].wrapping_add(c.r[3]) & !1, c.r[0] as u16);
        c.r[3] = c.r[3].wrapping_add(2);
    }
}

/// Forward halfword copy, including overlap. r12 counts bytes; r3 is the last load.
fn copy16(c: &mut Cpu, m: &mut dyn rt::Bus) {
    c.r[12] = 0;
    loop {
        compare(c, c.r[12], c.r[2]);
        if c.n == c.v { break; }
        c.r[3] = m.r16(c.r[0].wrapping_add(c.r[12]) & !1) as u32;
        m.w16(c.r[1].wrapping_add(c.r[12]) & !1, c.r[3] as u16);
        c.r[12] = c.r[12].wrapping_add(2);
    }
}

/// LDM/STM align without rotating; r0/r1 advance, r2 is last load, r12 is end.
fn copy32(c: &mut Cpu, m: &mut dyn rt::Bus) {
    c.r[12] = c.r[1].wrapping_add(c.r[2]);
    loop {
        compare(c, c.r[1], c.r[12]);
        if c.n == c.v { break; }
        c.r[2] = m.r32(c.r[0] & !3);
        c.r[0] = c.r[0].wrapping_add(4);
        rt::str_(m, c.r[1], c.r[2]);
        c.r[1] = c.r[1].wrapping_add(4);
    }
}

/// Internal fast fill: preserves r4-r9 but retains their stack stores. r2/r3=value;
/// r12 is the end of complete 32-byte chunks; r1 advances in whole words.
fn fill_fast(c: &mut Cpu, m: &mut dyn rt::Bus) {
    save(c, m, &[4, 5, 6, 7, 8, 9]);
    let end = c.r[1].wrapping_add(c.r[2]);
    c.r[12] = c.r[1].wrapping_add(c.r[2] & !31);
    c.r[2] = c.r[0]; c.r[3] = c.r[0];
    for (limit, words) in [(c.r[12], 8), (end, 1)] {
        loop {
            compare(c, c.r[1], limit);
            if c.n == c.v { break; }
            for _ in 0..words {
                rt::str_(m, c.r[1], c.r[0]);
                c.r[1] = c.r[1].wrapping_add(4);
            }
        }
    }
}

/// Byte fill uses halfword read/modify/write at the edges (also safe for DS VRAM).
/// r0 advances, r1 holds replicated/merged fill, r2 remainder, r3/r12 scratch.
fn fill8(c: &mut Cpu, m: &mut dyn rt::Bus) {
    compare(c, c.r[2], 0);
    if c.z { return; }
    logical(c, c.r[0] & 1);
    if !c.z {
        c.r[12] = m.r16(c.r[0].wrapping_sub(1) & !1) as u32 & 255;
        c.r[3] = c.r[12] | (c.r[1] << 8);
        m.w16(c.r[0].wrapping_sub(1) & !1, c.r[3] as u16);
        c.r[0] = c.r[0].wrapping_add(1);
        c.r[2] = rt::subs(c, c.r[2], 1);
        if c.z { return; }
    }
    compare(c, c.r[2], 2);
    if c.c {
        c.r[1] |= c.r[1] << 8;
        logical(c, c.r[0] & 2);
        if !c.z {
            m.w16(c.r[0] & !1, c.r[1] as u16);
            c.r[0] = c.r[0].wrapping_add(2);
            c.r[2] = rt::subs(c, c.r[2], 2);
            if c.z { return; }
        }
        c.r[1] |= c.r[1] << 16;
        c.r[3] = c.r[2] & !3;
        logical(c, c.r[3]);
        if !c.z {
            c.r[2] = c.r[2].wrapping_sub(c.r[3]);
            c.r[12] = c.r[0].wrapping_add(c.r[3]);
            loop {
                rt::str_(m, c.r[0], c.r[1]);
                c.r[0] = c.r[0].wrapping_add(4);
                compare(c, c.r[0], c.r[12]);
                if c.c { break; }
            }
        }
        logical(c, c.r[2] & 2);
        if !c.z {
            m.w16(c.r[0] & !1, c.r[1] as u16);
            c.r[0] = c.r[0].wrapping_add(2);
        }
    }
    logical(c, c.r[2] & 1);
    if !c.z {
        c.r[3] = m.r16(c.r[0] & !1) as u32 & 0xff00;
        c.r[1] = (c.r[1] & 255) | c.r[3];
        m.w16(c.r[0] & !1, c.r[1] as u16);
    }
}

const DIV: u32 = 0x0400_0280;
const NUM: u32 = 0x0400_0290;
const DEN: u32 = 0x0400_0298;
const QUOT: u32 = 0x0400_02a0;
const SQRT: u32 = 0x0400_02b0;

// Bus owns arithmetic edge cases and register side effects. Preserve store width/order.
fn div_s32(c: &mut Cpu, m: &mut dyn rt::Bus, result: u32) {
    save(c, m, &[14]);
    m.w16(DIV, 0); m.w32(NUM, c.r[0]);
    m.w32(DEN, c.r[1]); m.w32(DEN + 4, 0);
    c.r[2] = DEN; c.r[3] = NUM; c.r[12] = DIV;
    wait(c, m, DIV);
    c.r[0] = m.r32(result);
}
fn remainder(c: &mut Cpu, m: &mut dyn rt::Bus) { div_s32(c, m, QUOT + 8); }
fn divide(c: &mut Cpu, m: &mut dyn rt::Bus) { div_s32(c, m, QUOT); }

/// Leaves r0/r1 intact, r2=denominator port, r3=numerator port, r12=0; flags intact.
fn div_async(c: &mut Cpu, m: &mut dyn rt::Bus) {
    m.w16(DIV, 1); m.w32(NUM, 0); m.w32(NUM + 4, c.r[0]);
    m.w32(DEN, c.r[1]); m.w32(DEN + 4, 0);
    c.r[2] = DEN; c.r[3] = NUM; c.r[12] = 0;
}
fn wait(c: &mut Cpu, m: &mut dyn rt::Bus, port: u32) {
    while m.r16(port) & 0x8000 != 0 {}
    logical(c, 0); c.c = false; // rotated immediate in the busy-bit test
}
/// r0=result, r1=sqrt port; N=0/Z=1/C=0, V/Q preserved.
fn sqrt_result(c: &mut Cpu, m: &mut dyn rt::Bus) {
    wait(c, m, SQRT); c.r[1] = SQRT;
    c.r[0] = m.r32(SQRT + 4).wrapping_add(512) >> 10;
}
/// r2=low quotient, r1=rounded high quotient; flags from rounding the low word.
fn div_result(c: &mut Cpu, m: &mut dyn rt::Bus) {
    wait(c, m, DIV);
    c.r[2] = m.r32(QUOT);
    let hi = m.r32(QUOT + 4);
    let lo = rt::adds(c, c.r[2], 1 << 19);
    c.r[1] = hi.wrapping_add(c.c as u32);
    c.r[0] = (lo >> 20) | (c.r[1] << 12);
}

// Typical-call instruction budgets; charged before any observable work.
pub fn count_leading_zeros_block(c: &mut Cpu, m: &mut dyn rt::Bus, pc: u32) -> u32 {
    if !enter(c, pc, 2) { return rt::BUDGET_EXHAUSTED; }
    count_leading_zeros(c, m);
    ret(c)
}

pub fn multiply64_block(c: &mut Cpu, m: &mut dyn rt::Bus, pc: u32) -> u32 {
    if !enter(c, pc, 8) { return rt::BUDGET_EXHAUSTED; }
    multiply64(c, m);
    ret(c)
}

pub fn strlen_block(c: &mut Cpu, m: &mut dyn rt::Bus, pc: u32) -> u32 {
    if !enter(c, pc, 260) { return rt::BUDGET_EXHAUSTED; }
    strlen(c, m);
    ret(c)
}

pub fn fill16_block(c: &mut Cpu, m: &mut dyn rt::Bus, pc: u32) -> u32 {
    if !enter(c, pc, 260) { return rt::BUDGET_EXHAUSTED; }
    fill16(c, m);
    ret(c)
}

pub fn copy16_block(c: &mut Cpu, m: &mut dyn rt::Bus, pc: u32) -> u32 {
    if !enter(c, pc, 324) { return rt::BUDGET_EXHAUSTED; }
    copy16(c, m);
    ret(c)
}

pub fn copy32_block(c: &mut Cpu, m: &mut dyn rt::Bus, pc: u32) -> u32 {
    if !enter(c, pc, 164) { return rt::BUDGET_EXHAUSTED; }
    copy32(c, m);
    ret(c)
}

pub fn fill_fast_block(c: &mut Cpu, m: &mut dyn rt::Bus, pc: u32) -> u32 {
    if !enter(c, pc, 80) { return rt::BUDGET_EXHAUSTED; }
    fill_fast(c, m);
    ret(c)
}

pub fn fill8_block(c: &mut Cpu, m: &mut dyn rt::Bus, pc: u32) -> u32 {
    if !enter(c, pc, 120) { return rt::BUDGET_EXHAUSTED; }
    fill8(c, m);
    ret(c)
}

pub fn remainder_block(c: &mut Cpu, m: &mut dyn rt::Bus, pc: u32) -> u32 {
    if !enter(c, pc, 20) { return rt::BUDGET_EXHAUSTED; }
    remainder(c, m);
    ret(c)
}

pub fn divide_block(c: &mut Cpu, m: &mut dyn rt::Bus, pc: u32) -> u32 {
    if !enter(c, pc, 20) { return rt::BUDGET_EXHAUSTED; }
    divide(c, m);
    ret(c)
}

pub fn div_async_block(c: &mut Cpu, m: &mut dyn rt::Bus, pc: u32) -> u32 {
    if !enter(c, pc, 12) { return rt::BUDGET_EXHAUSTED; }
    div_async(c, m);
    ret(c)
}

pub fn sqrt_result_block(c: &mut Cpu, m: &mut dyn rt::Bus, pc: u32) -> u32 {
    if !enter(c, pc, 10) { return rt::BUDGET_EXHAUSTED; }
    sqrt_result(c, m);
    ret(c)
}

pub fn div_result_block(c: &mut Cpu, m: &mut dyn rt::Bus, pc: u32) -> u32 {
    if !enter(c, pc, 14) { return rt::BUDGET_EXHAUSTED; }
    div_result(c, m);
    ret(c)
}


