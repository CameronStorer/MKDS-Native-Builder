//! Instruction set of the DS's ARM946E-S (ARMv5TE): one IR for both ARM and Thumb code.
//! `decode` handles 32-bit ARM words, `crate::thumb::decode` 16-bit Thumb halfwords; both produce
//! [`Decoded`]. Anything not understood is `Undecoded`, which makes the lifter route that code
//! through the interpreter fallback rather than guess.
//!
//! Code addresses carry the instruction set in bit 0 (1 = Thumb), as BX does: every pc value the
//! lifter, interpreter and dispatcher pass around is `address | thumb`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op2 {
    /// Rotated 8-bit immediate (carry from bit 31 when rotated).
    Imm { imm8: u32, rot: u32 },
    /// A constant that leaves the carry alone (Thumb immediates, folded pc-relative values).
    Const(u32),
    /// Register shifted by an immediate (kind 0 LSL, 1 LSR, 2 ASR, 3 ROR/RRX).
    Reg { rm: usize, kind: u8, amt: u32 },
    /// Register shifted by the bottom byte of another register.
    RegShift { rm: usize, kind: u8, rs: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Off {
    Imm(u32),
    Reg { rm: usize, kind: u8, amt: u32 },
}

/// DSP multiplies (ARMv5TE): SMLAxy, SMLAWy, SMULWy, SMLALxy, SMULxy. `x`/`y` pick the top
/// (true) or bottom halves of rm / rs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HalfMul {
    Smla,
    Smlaw,
    Smulw,
    Smlal,
    Smul,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inst {
    /// Data processing (op 0 AND .. 15 MVN). `rd == 15` writes the pc without changing state.
    Dp { op: u8, s: bool, rn: usize, rd: usize, op2: Op2 },
    Mul { acc: bool, s: bool, rd: usize, rn: usize, rs: usize, rm: usize },
    /// UMULL/UMLAL/SMULL/SMLAL: rdhi:rdlo (+)= rm * rs.
    MulLong { signed: bool, acc: bool, s: bool, lo: usize, hi: usize, rs: usize, rm: usize },
    /// DSP multiply; for Smlal `rn` is RdLo and `rd` RdHi.
    MulHalf { op: HalfMul, x: bool, y: bool, rd: usize, rn: usize, rs: usize, rm: usize },
    /// QADD (0), QSUB (1), QDADD (2), QDSUB (3): rd = sat(rm op [2*]rn).
    Qalu { op: u8, rd: usize, rn: usize, rm: usize },
    Clz { rd: usize, rm: usize },
    /// rd = CPSR (or SPSR).
    Mrs { rd: usize, spsr: bool },
    /// Write the masked fields (bit 0 control, bit 3 flags) of CPSR/SPSR.
    Msr { spsr: bool, mask: u8, src: Op2 },
    /// Branch within the current instruction set to an absolute target (bit 0 = state).
    B { target: u32 },
    /// Call (BL / BLX imm / Thumb BL pair): lr = return address, then the target (bit 0 = state).
    Call { target: u32 },
    /// BX rm (and BLX rm when `link`): target state from bit 0.
    Bx { rm: usize, link: bool },
    Mem { load: bool, byte: bool, pre: bool, up: bool, wb: bool, rn: usize, rd: usize, off: Off },
    /// Halfword / signed transfers; kind 1 = H, 2 = SB, 3 = SH.
    MemH { load: bool, kind: u8, pre: bool, up: bool, wb: bool, rn: usize, rd: usize, off: Off },
    /// LDRD / STRD of rd, rd+1.
    MemD { load: bool, pre: bool, up: bool, wb: bool, rn: usize, rd: usize, off: Off },
    Block { load: bool, pre: bool, up: bool, wb: bool, rn: usize, list: u16 },
    /// LDM/STM with `^`: user-mode registers, or (load with r15) an exception return.
    BlockUser { load: bool, pre: bool, up: bool, wb: bool, rn: usize, list: u16 },
    Swp { byte: bool, rn: usize, rd: usize, rm: usize },
    /// Software interrupt (BIOS call); the number as the code passes it (ARM: bits 23..16 used by the BIOS).
    Swi { imm: u32 },
    /// MRC (load) / MCR (store) between rd and coprocessor `cp`.
    Cp { load: bool, cp: u8, op1: u8, crn: u8, crm: u8, op2: u8, rd: usize },
    /// PLD and other hints.
    Nop,
    Undecoded(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub cond: u8,
    pub inst: Inst,
    /// Bytes this instruction occupies (4 for ARM and Thumb BL pairs, else 2).
    pub size: u32,
    pub thumb: bool,
}

impl Inst {
    pub fn is_compare(op: u8) -> bool {
        (8..=11).contains(&op)
    }

    /// True if this instruction can set the program counter other than by falling through
    /// (calls return to the next instruction and do not count).
    pub fn writes_pc(&self) -> bool {
        match *self {
            Inst::Dp { op, rd, .. } => rd == 15 && !Self::is_compare(op),
            Inst::Mem { load, rd, .. } => load && rd == 15,
            Inst::Block { load, list, .. } | Inst::BlockUser { load, list, .. } => load && list & 0x8000 != 0,
            Inst::Bx { link, .. } => !link,
            Inst::B { .. } => true,
            _ => false,
        }
    }

    /// Registers this instruction would read r15 from in a way that depends on the address (for
    /// reporting); the value read is [`pc_read`].
    pub fn is_call(&self) -> bool {
        matches!(*self, Inst::Call { .. } | Inst::Bx { link: true, .. })
    }
}

/// The value an instruction at `addr` reads from r15: ARM addr + 8, Thumb addr + 4.
pub fn pc_read(addr: u32, thumb: bool) -> u32 {
    (addr & !1).wrapping_add(if thumb { 4 } else { 8 })
}

/// Where a pc value loaded from memory or a register goes (BX semantics): bit 0 set = Thumb,
/// clear = ARM (word aligned).
pub fn interwork(t: u32) -> u32 {
    if t & 1 != 0 { t } else { t & !3 }
}

/// Decodes the ARM word `w` at `addr` (an ARM address, bit 0 clear).
pub fn decode(w: u32, addr: u32) -> Decoded {
    let cond = (w >> 28) as u8;
    let d = |inst| Decoded { cond, inst, size: 4, thumb: false };
    let bad = d(Inst::Undecoded(w));
    let bit = |n: u32| (w >> n) & 1 != 0;
    let (rn, rd, rs, rm) = (((w >> 16) & 15) as usize, ((w >> 12) & 15) as usize, ((w >> 8) & 15) as usize, (w & 15) as usize);
    let pc = addr.wrapping_add(8);
    if cond == 0xF {
        // unconditional space (ARMv5): BLX imm, PLD
        if (w >> 25) & 7 == 5 {
            let off = (((w << 8) as i32) >> 6) as u32 | ((w >> 23) & 2);
            return Decoded { cond: 14, inst: Inst::Call { target: pc.wrapping_add(off) | 1 }, size: 4, thumb: false };
        }
        if w & 0x0D70_F000 == 0x0550_F000 {
            return Decoded { cond: 14, inst: Inst::Nop, size: 4, thumb: false };
        }
        return bad;
    }
    let op1 = (w >> 25) & 7;
    let inst = match op1 {
        0 | 1 => {
            // misc instructions: 0001 0xx0 .... with bit 4/7 patterns, not data processing
            let misc = op1 == 0 && (w >> 23) & 3 == 2 && !bit(20) && !(bit(7) && bit(4));
            if op1 == 0 && bit(4) && bit(7) {
                // multiplies and extra load/stores
                let sh = (w >> 5) & 3;
                if sh == 0 {
                    if (w >> 24) & 0xF == 0 {
                        if bit(23) {
                            Inst::MulLong { signed: bit(22), acc: bit(21), s: bit(20), lo: rd, hi: rn, rs, rm }
                        } else if (w >> 22) & 1 == 0 {
                            Inst::Mul { acc: bit(21), s: bit(20), rd: rn, rn: rd, rs, rm }
                        } else {
                            return bad;
                        }
                    } else if (w >> 23) & 0x1F == 2 && (w >> 20) & 3 == 0 && (w >> 8) & 15 == 0 {
                        Inst::Swp { byte: bit(22), rn, rd, rm }
                    } else {
                        return bad;
                    }
                } else {
                    let load = bit(20);
                    let off = if bit(22) { Off::Imm(((w >> 4) & 0xF0) | (w & 0xF)) } else { Off::Reg { rm, kind: 0, amt: 0 } };
                    let (pre, up, wb) = (bit(24), bit(23), bit(21) || !bit(24));
                    if !pre && bit(21) {
                        return bad;
                    }
                    match (load, sh) {
                        (_, 1) => Inst::MemH { load, kind: 1, pre, up, wb, rn, rd, off },
                        (true, k) => Inst::MemH { load, kind: k as u8, pre, up, wb, rn, rd, off },
                        (false, 2) if rd % 2 == 0 && rd != 14 => Inst::MemD { load: true, pre, up, wb, rn, rd, off },
                        (false, 3) if rd % 2 == 0 && rd != 14 => Inst::MemD { load: false, pre, up, wb, rn, rd, off },
                        _ => return bad,
                    }
                }
            } else if misc {
                let op = (w >> 21) & 3;
                match ((w >> 4) & 15, op) {
                    (0, 0) | (0, 2) => Inst::Mrs { rd, spsr: bit(22) },
                    (0, 1) | (0, 3) => Inst::Msr { spsr: bit(22), mask: ((w >> 16) & 15) as u8, src: Op2::Reg { rm, kind: 0, amt: 0 } },
                    (1, 1) => Inst::Bx { rm, link: false },
                    (3, 1) => Inst::Bx { rm, link: true },
                    (1, 3) => Inst::Clz { rd, rm },
                    (5, q) => Inst::Qalu { op: q as u8, rd, rn, rm },
                    (k, op) if k & 9 == 8 => {
                        let (x, y) = (bit(5), bit(6));
                        match op {
                            0 => Inst::MulHalf { op: HalfMul::Smla, x, y, rd: rn, rn: rd, rs, rm },
                            1 => Inst::MulHalf { op: if x { HalfMul::Smulw } else { HalfMul::Smlaw }, x: false, y, rd: rn, rn: rd, rs, rm },
                            2 => Inst::MulHalf { op: HalfMul::Smlal, x, y, rd: rn, rn: rd, rs, rm },
                            _ => Inst::MulHalf { op: HalfMul::Smul, x, y, rd: rn, rn: rd, rs, rm },
                        }
                    }
                    _ => return bad, // BKPT and reserved
                }
            } else if op1 == 1 && (w >> 23) & 3 == 2 && !bit(20) {
                // MSR immediate (0011 0x10), or undefined (0011 0x00)
                if !bit(21) {
                    return bad;
                }
                Inst::Msr { spsr: bit(22), mask: ((w >> 16) & 15) as u8, src: Op2::Imm { imm8: w & 0xFF, rot: (w >> 8) & 15 } }
            } else {
                let op = ((w >> 21) & 15) as u8;
                let s = bit(20);
                // rd == 15 with S: exception return (CPSR <- SPSR), handled by the executors
                let op2 = if op1 == 1 {
                    Op2::Imm { imm8: w & 0xFF, rot: (w >> 8) & 15 }
                } else if bit(4) {
                    Op2::RegShift { rm, kind: ((w >> 5) & 3) as u8, rs }
                } else {
                    Op2::Reg { rm, kind: ((w >> 5) & 3) as u8, amt: (w >> 7) & 31 }
                };
                Inst::Dp { op, s, rn, rd, op2 }
            }
        }
        2 | 3 => {
            if op1 == 3 && bit(4) {
                return bad; // media / undefined
            }
            let off = if op1 == 3 { Off::Reg { rm, kind: ((w >> 5) & 3) as u8, amt: (w >> 7) & 31 } } else { Off::Imm(w & 0xFFF) };
            // LDRT/STRT (post-indexed with W) behave as plain post-indexed accesses outside user mode
            Inst::Mem { load: bit(20), byte: bit(22), pre: bit(24), up: bit(23), wb: bit(21) || !bit(24), rn, rd, off }
        }
        4 => {
            let list = w as u16;
            let load = bit(20);
            if list == 0 || rn == 15 {
                return bad;
            }
            // ARMv5: writeback with the base in a load list leaves the loaded value
            let wb = bit(21) && !(load && list & (1 << rn) != 0);
            if bit(22) {
                // `^`: user-bank transfer (OS thread switch), or with pc an exception return
                Inst::BlockUser { load, pre: bit(24), up: bit(23), wb, rn, list }
            } else {
                Inst::Block { load, pre: bit(24), up: bit(23), wb, rn, list }
            }
        }
        5 => {
            let target = pc.wrapping_add((((w << 8) as i32) >> 6) as u32);
            if bit(24) { Inst::Call { target } } else { Inst::B { target } }
        }
        6 => return bad, // LDC/STC
        _ => {
            if bit(24) {
                Inst::Swi { imm: w & 0x00FF_FFFF }
            } else if bit(4) {
                Inst::Cp { load: bit(20), cp: ((w >> 8) & 15) as u8, op1: ((w >> 21) & 7) as u8, crn: rn as u8, crm: rm as u8, op2: ((w >> 5) & 7) as u8, rd }
            } else {
                return bad; // CDP
            }
        }
    };
    d(inst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i(w: u32) -> Inst {
        decode(w, 0x0200_0000).inst
    }

    #[test]
    fn decodes_common_forms() {
        assert_eq!(i(0xE12FFF1E), Inst::Bx { rm: 14, link: false }); // bx lr
        assert_eq!(i(0xE12FFF33), Inst::Bx { rm: 3, link: true }); // blx r3
        assert_eq!(i(0xE1A0F00E), Inst::Dp { op: 13, s: false, rn: 0, rd: 15, op2: Op2::Reg { rm: 14, kind: 0, amt: 0 } });
        assert_eq!(i(0xE2800001), Inst::Dp { op: 4, s: false, rn: 0, rd: 0, op2: Op2::Imm { imm8: 1, rot: 0 } });
        assert_eq!(i(0xEA000002), Inst::B { target: 0x0200_0010 });
        assert_eq!(i(0xEBFFFFFE), Inst::Call { target: 0x0200_0000 });
        assert_eq!(i(0xFA000000), Inst::Call { target: 0x0200_0009 }); // blx to Thumb
        assert_eq!(i(0xFB000000), Inst::Call { target: 0x0200_000B }); // blx with H
        assert_eq!(i(0xE5910004), Inst::Mem { load: true, byte: false, pre: true, up: true, wb: false, rn: 1, rd: 0, off: Off::Imm(4) });
        assert_eq!(i(0xE92D4010), Inst::Block { load: false, pre: true, up: false, wb: true, rn: 13, list: 0x4010 });
        assert_eq!(i(0xE10F0000), Inst::Mrs { rd: 0, spsr: false });
        assert_eq!(i(0xE121F000), Inst::Msr { spsr: false, mask: 1, src: Op2::Reg { rm: 0, kind: 0, amt: 0 } });
        assert_eq!(i(0xE16F0F11), Inst::Clz { rd: 0, rm: 1 });
        assert_eq!(i(0xE1A00112), Inst::Dp { op: 13, s: false, rn: 0, rd: 0, op2: Op2::RegShift { rm: 2, kind: 0, rs: 1 } }); // lsl r0, r2, r1
        assert_eq!(i(0xE0C10392), Inst::MulLong { signed: true, acc: false, s: false, lo: 0, hi: 1, rs: 3, rm: 2 }); // smull r0, r1, r2, r3
        assert_eq!(i(0xE12100A2), Inst::MulHalf { op: HalfMul::Smulw, x: false, y: false, rd: 1, rn: 0, rs: 0, rm: 2 }); // smulwb r1, r2, r0
        assert_eq!(i(0xE1C020D0), Inst::MemD { load: true, pre: true, up: true, wb: false, rn: 0, rd: 2, off: Off::Imm(0) }); // ldrd r2, [r0]
        assert_eq!(i(0xEE190F11), Inst::Cp { load: true, cp: 15, op1: 0, crn: 9, crm: 1, op2: 0, rd: 0 }); // mrc p15,0,r0,c9,c1,0
        assert_eq!(i(0xEF000006), Inst::Swi { imm: 6 });
        assert_eq!(i(0xE1030091), Inst::Swp { byte: false, rn: 3, rd: 0, rm: 1 });
    }
}
