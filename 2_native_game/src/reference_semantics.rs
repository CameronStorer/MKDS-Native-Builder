//! Independent execution oracle. Do not replace these with `rt` helper calls:
//! agreement with a shared implementation would hide runtime bugs.
use crate::arm::HalfMul;
use crate::rt::{Cpu, Mem};

pub fn cond(cc: u8, c: &Cpu) -> bool {
    match cc {
        0 => c.z,
        1 => !c.z,
        2 => c.c,
        3 => !c.c,
        4 => c.n,
        5 => !c.n,
        6 => c.v,
        7 => !c.v,
        8 => c.c && !c.z,
        9 => !c.c || c.z,
        10 => c.n == c.v,
        11 => c.n != c.v,
        12 => !c.z && c.n == c.v,
        13 => c.z || c.n != c.v,
        14 => true,
        _ => panic!("unsupported condition code"),
    }
}
pub fn set_nz(c: &mut Cpu, v: u32) {
    c.n = v & 0x8000_0000 != 0;
    c.z = v == 0;
}
pub fn shift_imm(kind: u8, value: u32, n: u32, carry: bool) -> (u32, bool) {
    assert!(n < 32);
    match kind {
        0 if n == 0 => (value, carry),
        0 => (
            ((value as u64) << n) as u32,
            (((value as u64) << n) >> 32) & 1 != 0,
        ),
        1 | 2 => {
            let amount = if n == 0 { 32 } else { n };
            let result = if kind == 2 {
                ((value as i32 as i64) >> amount) as u32
            } else {
                ((value as u64) >> amount) as u32
            };
            (result, value as u64 & (1_u64 << (amount - 1)) != 0)
        }
        3 if n == 0 => (((carry as u32) << 31) | (value >> 1), value & 1 != 0),
        3 => {
            let result = (value >> n) | (value << (32 - n));
            (result, result >> 31 != 0)
        }
        _ => panic!("invalid immediate shift"),
    }
}
pub fn shift_reg(kind: u8, value: u32, amount: u32, carry: bool) -> (u32, bool) {
    let n = amount & 255;
    if n == 0 {
        return (value, carry);
    }
    match kind {
        0 if n < 32 => (
            ((value as u64) << n) as u32,
            ((value as u64) >> (32 - n)) & 1 != 0,
        ),
        0 => (0, n == 32 && value & 1 != 0),
        1 if n < 32 => (value >> n, value & (1 << (n - 1)) != 0),
        1 => (0, n == 32 && value >> 31 != 0),
        2 => (
            ((value as i32 as i64) >> n.min(32)) as u32,
            value & (1 << (n.min(32) - 1)) != 0,
        ),
        3 => {
            let result = value.rotate_right(n & 31);
            (result, result >> 31 != 0)
        }
        _ => panic!("invalid register shift"),
    }
}
pub fn imm_op(imm8: u32, rot: u32, carry: bool) -> (u32, bool) {
    let value = imm8.rotate_right(rot * 2);
    (value, if rot == 0 { carry } else { value >> 31 != 0 })
}
fn arithmetic(a: u32, b: u32, extra: bool, subtract: bool) -> (u32, bool, bool) {
    let signed = if subtract {
        a as i32 as i64 - b as i32 as i64 - extra as i64
    } else {
        a as i32 as i64 + b as i32 as i64 + extra as i64
    };
    let (value, carry) = if subtract {
        (
            a.wrapping_sub(b).wrapping_sub(extra as u32),
            a as u64 >= b as u64 + extra as u64,
        )
    } else {
        let wide = a as u64 + b as u64 + extra as u64;
        (wide as u32, wide > u32::MAX as u64)
    };
    (
        value,
        carry,
        signed < i32::MIN as i64 || signed > i32::MAX as i64,
    )
}
pub fn alu(op: u8, a: u32, b: u32, c: bool, sc: bool, v: bool) -> (u32, bool, bool) {
    match op {
        0 | 8 => (a & b, sc, v),
        1 | 9 => (a ^ b, sc, v),
        12 => (a | b, sc, v),
        13 => (b, sc, v),
        14 => (a & !b, sc, v),
        15 => (!b, sc, v),
        2 | 10 => arithmetic(a, b, false, true),
        3 => arithmetic(b, a, false, true),
        4 | 11 => arithmetic(a, b, false, false),
        5 => arithmetic(a, b, c, false),
        6 => arithmetic(a, b, !c, true),
        7 => arithmetic(b, a, !c, true),
        _ => panic!("invalid ALU opcode"),
    }
}
pub fn mul_long(
    c: &mut Cpu,
    signed: bool,
    acc: bool,
    s: bool,
    a: u32,
    b: u32,
    lo: u32,
    hi: u32,
) -> (u32, u32) {
    let mut result = if signed {
        (a as i32 as i64 * b as i32 as i64) as u64
    } else {
        a as u64 * b as u64
    };
    if acc {
        result = result.wrapping_add(((hi as u64) << 32) | lo as u64);
    }
    if s {
        c.n = result >> 63 != 0;
        c.z = result == 0;
    }
    (result as u32, (result >> 32) as u32)
}
fn half(v: u32, top: bool) -> i64 {
    if top {
        (v >> 16) as u16 as i16 as i64
    } else {
        v as u16 as i16 as i64
    }
}
pub fn smlal_xy(x: bool, y: bool, a: u32, b: u32, lo: u32, hi: u32) -> (u32, u32) {
    let result = (((hi as u64) << 32) | lo as u64).wrapping_add((half(a, x) * half(b, y)) as u64);
    (result as u32, (result >> 32) as u32)
}
pub fn half_mul(c: &mut Cpu, op: HalfMul, x: bool, y: bool, a: u32, b: u32, acc: u32) -> u32 {
    let product = if matches!(op, HalfMul::Smulw | HalfMul::Smlaw) {
        (a as i32 as i64 * half(b, y)) >> 16
    } else {
        half(a, x) * half(b, y)
    };
    let result = if matches!(op, HalfMul::Smla | HalfMul::Smlaw) {
        product + acc as i32 as i64
    } else {
        product
    };
    if matches!(op, HalfMul::Smla | HalfMul::Smlaw) {
        c.q |= result < i32::MIN as i64 || result > i32::MAX as i64;
    }
    assert!(
        op != HalfMul::Smlal,
        "long DSP multiply must use pair result"
    );
    result as u32
}
pub fn qalu(c: &mut Cpu, op: u8, a: u32, b: u32) -> u32 {
    let clamp = |v: i64| v.clamp(i32::MIN as i64, i32::MAX as i64);
    let mut b = b as i32 as i64;
    if op >= 2 {
        let doubled = b * 2;
        b = clamp(doubled);
        c.q |= doubled != b;
    }
    let value = if op & 1 == 0 {
        a as i32 as i64 + b
    } else {
        a as i32 as i64 - b
    };
    let result = clamp(value);
    c.q |= value != result;
    result as u32
}
pub fn cpsr(c: &Cpu) -> u32 {
    (c.n as u32) << 31
        | (c.z as u32) << 30
        | (c.c as u32) << 29
        | (c.v as u32) << 28
        | (c.q as u32) << 27
        | (c.ctl & 0xdf)
}
pub fn msr(c: &mut Cpu, spsr: bool, mask: u8, value: u32) {
    if spsr {
        for byte in 0..4 {
            if mask & (1 << byte) != 0 {
                let bits = 255_u32 << (8 * byte);
                c.spsr = (c.spsr & !bits) | (value & bits);
            }
        }
        return;
    }
    if mask & 8 != 0 {
        c.n = value & (1 << 31) != 0;
        c.z = value & (1 << 30) != 0;
        c.c = value & (1 << 29) != 0;
        c.v = value & (1 << 28) != 0;
        c.q = value & (1 << 27) != 0;
    }
    if mask & 1 != 0 {
        // a mode change swaps the banked registers (Cpu::set_mode)
        c.set_mode(value & 0x1f);
        c.ctl = value & 0xdf;
    }
}
pub fn ldr<M: Mem + ?Sized>(m: &mut M, a: u32) -> u32 {
    m.r32(a & !3).rotate_right(8 * (a & 3))
}
pub fn str_<M: Mem + ?Sized>(m: &mut M, a: u32, v: u32) {
    m.w32(a & !3, v);
}
pub fn load_h<M: Mem + ?Sized>(m: &mut M, a: u32, kind: u8) -> u32 {
    match kind {
        1 => m.r16(a & !1) as u32,
        2 => m.r8(a) as i8 as i32 as u32,
        3 => m.r16(a & !1) as i16 as i32 as u32,
        _ => panic!("invalid transfer kind"),
    }
}
pub fn swp<M: Mem + ?Sized>(m: &mut M, a: u32, v: u32, byte: bool) -> u32 {
    if byte {
        let old = m.r8(a) as u32;
        m.w8(a, v as u8);
        old
    } else {
        let old = ldr(m, a);
        m.w32(a & !3, v);
        old
    }
}
pub fn block<M: Mem + ?Sized>(
    c: &mut Cpu,
    m: &mut M,
    load: bool,
    pre: bool,
    up: bool,
    wb: bool,
    rn: usize,
    list: u16,
    pc_store: u32,
) -> Option<u32> {
    assert!(list != 0);
    let base = c.r[rn];
    let count = list.count_ones() as i64;
    let offset = if up {
        if pre { 4 } else { 0 }
    } else {
        -4 * count + if pre { 0 } else { 4 }
    };
    let mut index = 0;
    let mut target = None;
    for register in 0..16 {
        if list & (1 << register) == 0 {
            continue;
        }
        let address = base.wrapping_add((offset + 4 * index) as u32) & !3;
        if load {
            let value = m.r32(address);
            if register == 15 {
                target = Some(value);
            } else {
                c.r[register] = value;
            }
        } else {
            m.w32(
                address,
                if register == 15 {
                    pc_store
                } else {
                    c.r[register]
                },
            );
        }
        index += 1;
    }
    if wb {
        c.r[rn] = base.wrapping_add((if up { 4 * count } else { -4 * count }) as u32);
    }
    target
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_arithmetic_carry_borrow_and_overflow() {
        assert_eq!(alu(4, u32::MAX, 1, false, false, false), (0, true, false));
        assert_eq!(
            alu(5, 0x7fff_ffff, 0, true, false, false),
            (0x8000_0000, false, true)
        );
        assert_eq!(alu(2, 0, 1, false, false, false), (u32::MAX, false, false));
        assert_eq!(alu(6, 0, 0, false, false, false), (u32::MAX, false, false));
        assert_eq!(alu(6, 0, 0, true, false, false), (0, true, false));
        assert_eq!(
            alu(2, 0x8000_0000, 1, false, false, false),
            (0x7fff_ffff, true, true)
        );
    }
    #[test]
    fn runtime_arithmetic_boundary_matrix() {
        let values = [
            0,
            1,
            2,
            0x7fff_fffe,
            0x7fff_ffff,
            0x8000_0000,
            0x8000_0001,
            0xffff_fffe,
            u32::MAX,
        ];
        for op in 0..16 {
            for a in values {
                for b in values {
                    for f in 0..8 {
                        assert_eq!(
                            alu(op, a, b, f & 1 != 0, f & 2 != 0, f & 4 != 0),
                            crate::rt::alu(op, a, b, f & 1 != 0, f & 2 != 0, f & 4 != 0)
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn shift_edge_cases_and_runtime_matrix() {
        assert_eq!(shift_imm(1, 0x8000_0001, 0, false), (0, true));
        assert_eq!(shift_imm(2, 0x8000_0001, 0, false), (u32::MAX, true));
        assert_eq!(shift_imm(3, 2, 0, true), (0x8000_0001, false));
        assert_eq!(shift_reg(0, 0x8000_0001, 32, true), (0, true));
        assert_eq!(shift_reg(0, 0x8000_0001, 33, true), (0, false));
        assert_eq!(shift_reg(3, 0x8000_0001, 256, false), (0x8000_0001, false));
        for value in [0, 1, 0x8000_0000, 0x8000_0001, u32::MAX] {
            for kind in 0..4 {
                for n in 0..256 {
                    for c in [false, true] {
                        assert_eq!(
                            shift_reg(kind, value, n, c),
                            crate::rt::shift_reg(kind, value, n, c)
                        );
                        if n < 32 {
                            assert_eq!(
                                shift_imm(kind, value, n, c),
                                crate::rt::shift_imm(kind, value, n, c)
                            );
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn sticky_q_and_dsp_overflow_do_not_saturate_multiply_result() {
        let mut c = Cpu::default();
        assert_eq!(qalu(&mut c, 0, 0x7fff_ffff, 1), 0x7fff_ffff);
        assert!(c.q);
        assert_eq!(qalu(&mut c, 1, 3, 1), 2);
        assert!(c.q);
        c.q = false;
        assert_eq!(
            half_mul(&mut c, HalfMul::Smla, false, false, 1, 1, 0x7fff_ffff),
            0x8000_0000
        );
        assert!(c.q);
        assert_eq!(
            half_mul(&mut c, HalfMul::Smulw, false, false, 0x10000, 0xfffe, 0),
            0xffff_fffe
        );
    }
    #[test]
    fn block_modes_order_and_writeback() {
        use crate::mem::Ram;
        for (up, pre, first, end) in [
            (true, false, 0x100, 0x108),
            (true, true, 0x104, 0x108),
            (false, false, 0xfc, 0xf8),
            (false, true, 0xf8, 0xf8),
        ] {
            let mut c = Cpu::default();
            c.r[4] = 0x100;
            c.r[0] = 0xaabb_ccdd;
            c.r[2] = 0x1234_5678;
            let mut m = Ram::new(&[], 0, 5);
            block(&mut c, &mut m, false, pre, up, true, 4, 5, 0);
            assert_eq!(c.r[4], end);
            assert_eq!(m.r32(first), 0xaabb_ccdd);
            assert_eq!(m.r32(first + 4), 0x1234_5678);
            c.r[4] = 0x100;
            block(&mut c, &mut m, true, pre, up, true, 4, 5, 0);
            assert_eq!((c.r[0], c.r[2], c.r[4]), (0xaabb_ccdd, 0x1234_5678, end));
        }
    }
    #[test]
    fn all_conditions_and_status_masks() {
        for flags in 0..16 {
            let mut c = Cpu::default();
            c.n = flags & 1 != 0;
            c.z = flags & 2 != 0;
            c.c = flags & 4 != 0;
            c.v = flags & 8 != 0;
            for cc in 0..15 {
                assert_eq!(cond(cc, &c), crate::rt::cond(cc, &c));
            }
            for mask in 0..16 {
                let mut a = c.clone();
                let mut b = c.clone();
                msr(&mut a, false, mask, 0xa812_34d3);
                b.msr(false, mask, 0xa812_34d3);
                assert_eq!(a.state(), b.state());
                assert_eq!(cpsr(&a), b.cpsr());
            }
        }
    }
}
