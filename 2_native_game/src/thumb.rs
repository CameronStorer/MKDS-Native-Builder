//! Thumb (ARMv5T) decoder into the shared IR of `crate::arm`. Thumb instructions are mapped onto
//! their ARM equivalents (flag-setting ALU ops become `s: true`, PUSH/POP become LDM/STM on sp).
//! pc-relative forms are folded at decode time, so the IR never needs Thumb's word-aligned pc.
use crate::arm::{Decoded, Inst, Off, Op2};

fn t(cond: u8, inst: Inst, size: u32) -> Decoded {
    Decoded { cond, inst, size, thumb: true }
}

/// Decodes the Thumb halfword `h` at `addr` (address without the Thumb bit); `next` is the
/// following halfword (used for BL/BLX pairs).
pub fn decode(h: u16, next: u16, addr: u32) -> Decoded {
    let w = h as u32;
    let bad = t(14, Inst::Undecoded(w), 2);
    let ok = |inst| t(14, inst, 2);
    let r = |n: u32| ((w >> n) & 7) as usize;
    let pc = addr.wrapping_add(4);
    match w >> 11 {
        // 1/2: shift by immediate; add/sub register or 3-bit immediate
        0..=2 => ok(Inst::Dp { op: 13, s: true, rn: 0, rd: r(0), op2: Op2::Reg { rm: r(3), kind: (w >> 11) as u8, amt: (w >> 6) & 31 } }),
        3 => {
            let op = if w & 0x200 != 0 { 2 } else { 4 };
            let op2 = if w & 0x400 != 0 { Op2::Const((w >> 6) & 7) } else { Op2::Reg { rm: r(6), kind: 0, amt: 0 } };
            ok(Inst::Dp { op, s: true, rn: r(3), rd: r(0), op2 })
        }
        // 3: mov/cmp/add/sub 8-bit immediate
        4..=7 => {
            let op = [13, 10, 4, 2][((w >> 11) & 3) as usize];
            let rd = r(8);
            ok(Inst::Dp { op, s: true, rn: rd, rd, op2: Op2::Const(w & 0xFF) })
        }
        8 => {
            if w & 0x400 == 0 {
                // 4: ALU operations
                let (rd, rm) = (r(0), r(3));
                let inst = match (w >> 6) & 15 {
                    0 => Inst::Dp { op: 0, s: true, rn: rd, rd, op2: Op2::Reg { rm, kind: 0, amt: 0 } },
                    1 => Inst::Dp { op: 1, s: true, rn: rd, rd, op2: Op2::Reg { rm, kind: 0, amt: 0 } },
                    2 => Inst::Dp { op: 13, s: true, rn: 0, rd, op2: Op2::RegShift { rm: rd, kind: 0, rs: rm } },
                    3 => Inst::Dp { op: 13, s: true, rn: 0, rd, op2: Op2::RegShift { rm: rd, kind: 1, rs: rm } },
                    4 => Inst::Dp { op: 13, s: true, rn: 0, rd, op2: Op2::RegShift { rm: rd, kind: 2, rs: rm } },
                    5 => Inst::Dp { op: 5, s: true, rn: rd, rd, op2: Op2::Reg { rm, kind: 0, amt: 0 } },
                    6 => Inst::Dp { op: 6, s: true, rn: rd, rd, op2: Op2::Reg { rm, kind: 0, amt: 0 } },
                    7 => Inst::Dp { op: 13, s: true, rn: 0, rd, op2: Op2::RegShift { rm: rd, kind: 3, rs: rm } },
                    8 => Inst::Dp { op: 8, s: true, rn: rd, rd: 0, op2: Op2::Reg { rm, kind: 0, amt: 0 } },
                    9 => Inst::Dp { op: 3, s: true, rn: rm, rd, op2: Op2::Const(0) },
                    10 => Inst::Dp { op: 10, s: true, rn: rd, rd: 0, op2: Op2::Reg { rm, kind: 0, amt: 0 } },
                    11 => Inst::Dp { op: 11, s: true, rn: rd, rd: 0, op2: Op2::Reg { rm, kind: 0, amt: 0 } },
                    12 => Inst::Dp { op: 12, s: true, rn: rd, rd, op2: Op2::Reg { rm, kind: 0, amt: 0 } },
                    13 => Inst::Mul { acc: false, s: true, rd, rn: 0, rs: rd, rm },
                    14 => Inst::Dp { op: 14, s: true, rn: rd, rd, op2: Op2::Reg { rm, kind: 0, amt: 0 } },
                    _ => Inst::Dp { op: 15, s: true, rn: 0, rd, op2: Op2::Reg { rm, kind: 0, amt: 0 } },
                };
                ok(inst)
            } else {
                // 5: hi register operations / BX / BLX
                let rd = r(0) | (((w >> 7) & 1) as usize) << 3;
                let rm = ((w >> 3) & 15) as usize;
                let src = if rm == 15 { Op2::Const(pc) } else { Op2::Reg { rm, kind: 0, amt: 0 } };
                match (w >> 8) & 3 {
                    0 => ok(Inst::Dp { op: 4, s: false, rn: rd, rd, op2: src }),
                    1 => ok(Inst::Dp { op: 10, s: true, rn: rd, rd: 0, op2: src }),
                    2 => ok(Inst::Dp { op: 13, s: false, rn: 0, rd, op2: src }),
                    _ => {
                        if w & 7 != 0 {
                            return bad;
                        }
                        ok(Inst::Bx { rm, link: w & 0x80 != 0 })
                    }
                }
            }
        }
        // 6: ldr rd, [pc, #imm8 * 4]: folded to an absolute address
        9 => {
            let at = (pc & !3).wrapping_add((w & 0xFF) * 4);
            let d = at as i64 - pc as i64;
            ok(Inst::Mem { load: true, byte: false, pre: true, up: d >= 0, wb: false, rn: 15, rd: r(8), off: Off::Imm(d.unsigned_abs() as u32) })
        }
        // 7/8: register offset
        10 | 11 => {
            let off = Off::Reg { rm: r(6), kind: 0, amt: 0 };
            let (rn, rd) = (r(3), r(0));
            let inst = if w & 0x200 == 0 {
                Inst::Mem { load: w & 0x800 != 0, byte: w & 0x400 != 0, pre: true, up: true, wb: false, rn, rd, off }
            } else {
                match (w >> 10) & 3 {
                    0 => Inst::MemH { load: false, kind: 1, pre: true, up: true, wb: false, rn, rd, off },
                    1 => Inst::MemH { load: true, kind: 2, pre: true, up: true, wb: false, rn, rd, off },
                    2 => Inst::MemH { load: true, kind: 1, pre: true, up: true, wb: false, rn, rd, off },
                    _ => Inst::MemH { load: true, kind: 3, pre: true, up: true, wb: false, rn, rd, off },
                }
            };
            ok(inst)
        }
        // 9: immediate offset word / byte
        12..=15 => {
            let byte = w & 0x1000 != 0;
            let imm = ((w >> 6) & 31) * if byte { 1 } else { 4 };
            ok(Inst::Mem { load: w & 0x800 != 0, byte, pre: true, up: true, wb: false, rn: r(3), rd: r(0), off: Off::Imm(imm) })
        }
        // 10: halfword immediate
        16 | 17 => ok(Inst::MemH { load: w & 0x800 != 0, kind: 1, pre: true, up: true, wb: false, rn: r(3), rd: r(0), off: Off::Imm(((w >> 6) & 31) * 2) }),
        // 11: sp-relative
        18 | 19 => ok(Inst::Mem { load: w & 0x800 != 0, byte: false, pre: true, up: true, wb: false, rn: 13, rd: r(8), off: Off::Imm((w & 0xFF) * 4) }),
        // 12: add rd, pc/sp, #imm
        20 => ok(Inst::Dp { op: 13, s: false, rn: 0, rd: r(8), op2: Op2::Const((pc & !3).wrapping_add((w & 0xFF) * 4)) }),
        21 => ok(Inst::Dp { op: 4, s: false, rn: 13, rd: r(8), op2: Op2::Const((w & 0xFF) * 4) }),
        22 | 23 => {
            if w & 0xFF00 == 0xB000 {
                // 13: add/sub sp, #imm7 * 4
                let op = if w & 0x80 != 0 { 2 } else { 4 };
                ok(Inst::Dp { op, s: false, rn: 13, rd: 13, op2: Op2::Const((w & 0x7F) * 4) })
            } else if w & 0xF600 == 0xB400 {
                // 14: push / pop
                let load = w & 0x800 != 0;
                let extra = if w & 0x100 != 0 { if load { 0x8000 } else { 0x4000 } } else { 0 };
                let list = (w & 0xFF) as u16 | extra;
                if list == 0 {
                    return bad;
                }
                ok(Inst::Block { load, pre: !load, up: load, wb: true, rn: 13, list })
            } else {
                bad // BKPT, CPS and undefined
            }
        }
        // 15: ldmia / stmia rb!
        24 | 25 => {
            let load = w & 0x800 != 0;
            let rn = r(8);
            let list = (w & 0xFF) as u16;
            if list == 0 {
                return bad;
            }
            ok(Inst::Block { load, pre: false, up: true, wb: !(load && list & (1 << rn) != 0), rn, list })
        }
        // 16/17: conditional branch, SWI
        26 | 27 => {
            let cond = ((w >> 8) & 15) as u8;
            match cond {
                15 => ok(Inst::Swi { imm: w & 0xFF }),
                14 => bad,
                _ => t(cond, Inst::B { target: pc.wrapping_add(((w as u8 as i8 as i32) * 2) as u32) | 1 }, 2),
            }
        }
        // 18: unconditional branch
        28 => ok(Inst::B { target: pc.wrapping_add(((((w & 0x7FF) << 21) as i32) >> 20) as u32) | 1 }),
        // 19: BL / BLX pair (prefix then suffix)
        30 => {
            let n = next as u32;
            let hi = ((((w & 0x7FF) << 21) as i32) >> 9) as u32;
            let lo = (n & 0x7FF) << 1;
            let target = pc.wrapping_add(hi).wrapping_add(lo);
            match n >> 11 {
                31 => t(14, Inst::Call { target: target | 1 }, 4),
                29 if n & 1 == 0 => t(14, Inst::Call { target: target & !3 }, 4),
                _ => bad,
            }
        }
        _ => bad, // lone suffix (29, 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i(h: u16) -> Inst {
        decode(h, 0, 0x0200_0100).inst
    }

    #[test]
    fn decodes_thumb_forms() {
        assert_eq!(i(0x2005), Inst::Dp { op: 13, s: true, rn: 0, rd: 0, op2: Op2::Const(5) }); // movs r0, #5
        assert_eq!(i(0x1888), Inst::Dp { op: 4, s: true, rn: 1, rd: 0, op2: Op2::Reg { rm: 2, kind: 0, amt: 0 } }); // adds r0, r1, r2
        assert_eq!(i(0x4770), Inst::Bx { rm: 14, link: false }); // bx lr
        assert_eq!(i(0x4798), Inst::Bx { rm: 3, link: true }); // blx r3
        assert_eq!(i(0xB510), Inst::Block { load: false, pre: true, up: false, wb: true, rn: 13, list: 0x4010 }); // push {r4, lr}
        assert_eq!(i(0xBD10), Inst::Block { load: true, pre: false, up: true, wb: true, rn: 13, list: 0x8010 }); // pop {r4, pc}
        assert_eq!(i(0xE7FE), Inst::B { target: 0x0200_0101 }); // b .
        let d = decode(0xD0FE, 0, 0x0200_0100); // beq .
        assert_eq!((d.cond, d.inst), (0, Inst::B { target: 0x0200_0101 }));
        // ldr r0, [pc, #4] at 0x02000102: (0x02000106 & ~3) + 4 = 0x02000108, pc read 0x02000106
        assert_eq!(decode(0x4801, 0, 0x0200_0102).inst, Inst::Mem { load: true, byte: false, pre: true, up: true, wb: false, rn: 15, rd: 0, off: Off::Imm(2) });
        // bl / blx pairs
        assert_eq!(decode(0xF000, 0xF802, 0x0200_0100).inst, Inst::Call { target: 0x0200_0109 });
        assert_eq!(decode(0xF000, 0xE802, 0x0200_0100).inst, Inst::Call { target: 0x0200_0108 });
        assert_eq!(decode(0xF7FF, 0xFFFE, 0x0200_0100).inst, Inst::Call { target: 0x0200_0101 });
        assert_eq!(i(0xDF06), Inst::Swi { imm: 6 });
    }
}
