//! Reference interpreter over the same IR (ARM and Thumb). Execution helpers are independent of
//! `rt` (see reference_semantics.rs); decoder and host Bus services remain shared. It does not share code generation with
//! the lifter, so lifted code can be checked against it (differential testing), and the
//! dispatcher uses it as the fallback for code that was not lifted (computed jumps into the middle
//! of a function, undecodable instructions are reported, never guessed).
use crate::arm::{HalfMul, Inst, Off, Op2, interwork, pc_read};
use crate::rt::{BUDGET_EXHAUSTED, Bus, Cpu};
#[path = "reference_semantics.rs"]
mod semantics;

fn reg(c: &Cpu, i: usize, addr: u32, thumb: bool) -> u32 {
    if i == 15 {
        pc_read(addr, thumb)
    } else {
        c.r[i]
    }
}

fn operand(c: &Cpu, o: Op2, addr: u32, thumb: bool) -> (u32, bool) {
    match o {
        Op2::Imm { imm8, rot } => semantics::imm_op(imm8, rot, c.c),
        Op2::Const(v) => (v, c.c),
        Op2::Reg { rm, kind, amt } => semantics::shift_imm(kind, reg(c, rm, addr, thumb), amt, c.c),
        Op2::RegShift { rm, kind, rs } => {
            let v = if rm == 15 {
                pc_read(addr, thumb) + 4
            } else {
                c.r[rm]
            };
            semantics::shift_reg(kind, v, reg(c, rs, addr, thumb), c.c)
        }
    }
}

fn offset(c: &Cpu, o: Off, addr: u32, thumb: bool) -> u32 {
    match o {
        Off::Imm(v) => v,
        Off::Reg { rm, kind, amt } => {
            semantics::shift_imm(kind, reg(c, rm, addr, thumb), amt, c.c).0
        }
    }
}

/// Executes the (already decoded) instruction at code address `pc`. Returns the next pc.
pub fn exec(d: &crate::arm::Decoded, pc: u32, c: &mut Cpu, m: &mut dyn Bus) -> u32 {
    let t = d.thumb;
    let addr = pc & !1;
    let next = pc.wrapping_add(d.size);
    if !semantics::cond(d.cond, c) {
        return next;
    }
    let store_val = |c: &Cpu, rd: usize| {
        if rd == 15 {
            pc_read(addr, t) + 4
        } else {
            c.r[rd]
        }
    };
    match d.inst {
        Inst::Dp { op, s, rn, rd, op2 } => {
            let a = reg(c, rn, addr, t);
            let (b, sc) = operand(c, op2, addr, t);
            let (res, nc, nv) = semantics::alu(op, a, b, c.c, sc, c.v);
            if s {
                semantics::set_nz(c, res);
                c.c = nc;
                c.v = nv;
            }
            if !Inst::is_compare(op) {
                if rd == 15 && s {
                    // exception return: CPSR = SPSR
                    return if c.restore_spsr() { res | 1 } else { res & !3 };
                }
                if rd == 15 {
                    return if t { res | 1 } else { res & !3 };
                }
                c.r[rd] = res;
            }
        }
        Inst::Mul {
            acc,
            s,
            rd,
            rn,
            rs,
            rm,
        } => {
            let mut res = reg(c, rm, addr, t).wrapping_mul(reg(c, rs, addr, t));
            if acc {
                res = res.wrapping_add(reg(c, rn, addr, t));
            }
            if s {
                semantics::set_nz(c, res);
            }
            c.r[rd] = res;
        }
        Inst::MulLong {
            signed,
            acc,
            s,
            lo,
            hi,
            rs,
            rm,
        } => {
            let (a, b) = (reg(c, rm, addr, t), reg(c, rs, addr, t));
            let (l, h) = semantics::mul_long(c, signed, acc, s, a, b, c.r[lo], c.r[hi]);
            c.r[lo] = l;
            c.r[hi] = h;
        }
        Inst::MulHalf {
            op: HalfMul::Smlal,
            x,
            y,
            rd,
            rn,
            rs,
            rm,
        } => {
            let (l, h) = semantics::smlal_xy(
                x,
                y,
                reg(c, rm, addr, t),
                reg(c, rs, addr, t),
                c.r[rn],
                c.r[rd],
            );
            c.r[rn] = l;
            c.r[rd] = h;
        }
        Inst::MulHalf {
            op,
            x,
            y,
            rd,
            rn,
            rs,
            rm,
        } => {
            let (a, b, acc) = (
                reg(c, rm, addr, t),
                reg(c, rs, addr, t),
                reg(c, rn, addr, t),
            );
            c.r[rd] = semantics::half_mul(c, op, x, y, a, b, acc);
        }
        Inst::Qalu { op, rd, rn, rm } => {
            let (a, b) = (reg(c, rm, addr, t), reg(c, rn, addr, t));
            c.r[rd] = semantics::qalu(c, op, a, b);
        }
        Inst::Clz { rd, rm } => c.r[rd] = reg(c, rm, addr, t).leading_zeros(),
        Inst::Mrs { rd, spsr } => c.r[rd] = if spsr { c.spsr } else { semantics::cpsr(c) },
        Inst::Msr { spsr, mask, src } => {
            let v = operand(c, src, addr, t).0;
            semantics::msr(c, spsr, mask, v);
        }
        Inst::B { target } => return target,
        Inst::Call { target } => {
            let ret = next;
            c.r[14] = ret;
            return m.call(c, target, ret);
        }
        Inst::Bx { rm, link } => {
            let target = interwork(reg(c, rm, addr, t));
            if !link {
                return target;
            }
            c.r[14] = next;
            return m.call(c, target, next);
        }
        Inst::Mem {
            load,
            byte,
            pre,
            up,
            wb,
            rn,
            rd,
            off,
        } => {
            let base = reg(c, rn, addr, t);
            let o = offset(c, off, addr, t);
            let ax = if up {
                base.wrapping_add(o)
            } else {
                base.wrapping_sub(o)
            };
            let ea = if pre { ax } else { base };
            if load {
                let v = if byte {
                    m.r8(ea) as u32
                } else {
                    semantics::ldr(m, ea)
                };
                if wb {
                    c.r[rn] = ax;
                }
                if rd == 15 {
                    return interwork(v);
                }
                c.r[rd] = v;
            } else {
                let v = store_val(c, rd);
                if byte {
                    m.w8(ea, v as u8);
                } else {
                    semantics::str_(m, ea, v);
                }
                if wb {
                    c.r[rn] = ax;
                }
            }
        }
        Inst::MemH {
            load,
            kind,
            pre,
            up,
            wb,
            rn,
            rd,
            off,
        } => {
            let base = reg(c, rn, addr, t);
            let o = offset(c, off, addr, t);
            let ax = if up {
                base.wrapping_add(o)
            } else {
                base.wrapping_sub(o)
            };
            let ea = if pre { ax } else { base };
            if load {
                let v = semantics::load_h(m, ea, kind);
                if wb {
                    c.r[rn] = ax;
                }
                c.r[rd] = v;
            } else {
                let v = store_val(c, rd);
                m.w16(ea & !1, v as u16);
                if wb {
                    c.r[rn] = ax;
                }
            }
        }
        Inst::MemD {
            load,
            pre,
            up,
            wb,
            rn,
            rd,
            off,
        } => {
            let base = reg(c, rn, addr, t);
            let o = offset(c, off, addr, t);
            let ax = if up {
                base.wrapping_add(o)
            } else {
                base.wrapping_sub(o)
            };
            let ea = if pre { ax } else { base };
            if load {
                let (v0, v1) = (m.r32(ea & !3), m.r32(ea.wrapping_add(4) & !3));
                if wb {
                    c.r[rn] = ax;
                }
                c.r[rd] = v0;
                c.r[rd + 1] = v1;
            } else {
                m.w32(ea & !3, c.r[rd]);
                m.w32(ea.wrapping_add(4) & !3, c.r[rd + 1]);
                if wb {
                    c.r[rn] = ax;
                }
            }
        }
        Inst::Block {
            load,
            pre,
            up,
            wb,
            rn,
            list,
        } => {
            if let Some(v) =
                semantics::block(c, m, load, pre, up, wb, rn, list, pc_read(addr, t) + 4)
            {
                return interwork(v);
            }
        }
        Inst::BlockUser { load, pre, up, wb, rn, list } => {
            // user-bank registers, but the current mode's base register (unit-tested in rt)
            let v = crate::rt::block_user(c, m, load, pre, up, wb, rn, list, pc_read(addr, t) + 4);
            if let Some(v) = v {
                return if c.restore_spsr() { v | 1 } else { v & !3 };
            }
        }
        Inst::Swp { byte, rn, rd, rm } => {
            let (a, v) = (c.r[rn], c.r[rm]);
            c.r[rd] = semantics::swp(m, a, v, byte);
        }
        Inst::Swi { imm } => m.swi(c, if t { imm & 255 } else { (imm >> 16) & 255 }),
        Inst::Cp {
            load,
            cp,
            op1,
            crn,
            crm,
            op2,
            rd,
        } => {
            if load {
                let v = m.mrc(c, cp, op1, crn, crm, op2);
                if rd == 15 {
                    // MRC to r15 transfers NZCV only; unlike MSR it must not
                    // clear or set the sticky saturation Q flag.
                    c.n = v >> 31 != 0;
                    c.z = v & (1 << 30) != 0;
                    c.c = v & (1 << 29) != 0;
                    c.v = v & (1 << 28) != 0;
                } else {
                    c.r[rd] = v;
                }
            } else {
                let v = reg(c, rd, addr, t);
                m.mcr(c, cp, op1, crn, crm, op2, v);
            }
        }
        Inst::Nop => {}
        Inst::Undecoded(w) => panic!("undecodable instruction {w:#010X} at {pc:#010X}"),
    }
    next
}

/// Decodes the instruction at code address `pc` from the bus.
pub fn fetch(m: &mut dyn Bus, pc: u32) -> crate::arm::Decoded {
    let a = pc & !1;
    if pc & 1 != 0 {
        let (h, n) = (m.r16(a), m.r16(a.wrapping_add(2)));
        crate::thumb::decode(h, n, a)
    } else {
        crate::arm::decode(m.r32(a), a)
    }
}

/// Runs a lifted function's code the way the lifted version does: from `entry`, falling through
/// while the next instruction belongs to the function and jumping only to its block starts;
/// returns the first pc it leaves to, or `BUDGET_EXHAUSTED`.
pub fn run(c: &mut Cpu, m: &mut dyn Bus, blocks: &[u32], insts: &[u32], entry: u32) -> u32 {
    let mut pc = entry;
    loop {
        if c.budget == 0 {
            return BUDGET_EXHAUSTED;
        }
        c.budget -= 1;
        let d = fetch(m, pc);
        let fall = pc.wrapping_add(d.size);
        let next = exec(&d, pc, c, m);
        let ok = if next == fall {
            insts.binary_search(&next).is_ok()
        } else {
            blocks.binary_search(&next).is_ok()
        };
        // a call that returned normally continues at its return address (a block start)
        if !ok {
            return next;
        }
        pc = next;
    }
}

/// The pc of the last instruction the interpreter executed (diagnostics: where a stuck boot is).
pub static LAST_PC: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Interprets from `pc` until `stop(pc)` holds (the dispatcher's fallback); returns that pc.
pub fn run_until(c: &mut Cpu, m: &mut dyn Bus, mut pc: u32, stop: &dyn Fn(u32) -> bool) -> u32 {
    loop {
        if stop(pc) {
            return pc;
        }
        if c.budget == 0 {
            // resumable: the machine continues from r15 after its time slice
            c.r[15] = pc;
            return BUDGET_EXHAUSTED;
        }
        c.budget -= 1;
        LAST_PC.store(pc, std::sync::atomic::Ordering::Relaxed);
        let d = fetch(m, pc);
        if let Inst::Undecoded(w) = d.inst {
            panic!("undecodable instruction {w:#010X} at {pc:#010X}");
        }
        pc = exec(&d, pc, c, m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::Ram;
    use crate::rt::Mem;
    fn d(inst: Inst) -> crate::arm::Decoded {
        crate::arm::Decoded {
            cond: 14,
            inst,
            size: 4,
            thumb: false,
        }
    }
    #[test]
    fn compare_and_logical_flags_keep_unaffected_state() {
        let mut c = Cpu::default();
        c.r[0] = 7;
        c.r[1] = 7;
        c.r[2] = 0x1234;
        c.v = true;
        let mut m = Ram::new(&[], 0, 0);
        exec(
            &d(Inst::Dp {
                op: 10,
                s: true,
                rn: 0,
                rd: 2,
                op2: Op2::Reg {
                    rm: 1,
                    kind: 0,
                    amt: 0,
                },
            }),
            0,
            &mut c,
            &mut m,
        );
        assert_eq!(c.r[2], 0x1234);
        assert!(c.z && c.c && !c.v);
        c.v = true;
        exec(
            &d(Inst::Dp {
                op: 13,
                s: true,
                rn: 0,
                rd: 2,
                op2: Op2::Imm { imm8: 0x80, rot: 4 },
            }),
            0,
            &mut c,
            &mut m,
        );
        assert_eq!(c.r[2], 0x8000_0000);
        assert!(c.n && c.c && c.v && !c.z);
    }
    #[test]
    fn calls_record_target_return_and_thumb_lr_bit() {
        let mut c = Cpu::default();
        let mut m = Ram::new(&[], 0, 0);
        assert_eq!(
            exec(&d(Inst::Call { target: 0x200 }), 0x100, &mut c, &mut m),
            0x104
        );
        assert_eq!(c.r[14], 0x104);
        assert_eq!(m.events, [(0x200, 0x104)]);
        let mut bx = d(Inst::Bx { rm: 4, link: true });
        bx.thumb = true;
        bx.size = 2;
        c.r[4] = 0x301;
        assert_eq!(exec(&bx, 0x101, &mut c, &mut m), 0x103);
        assert_eq!(c.r[14], 0x103);
        assert_eq!(m.events[1], (0x301, 0x103));
    }
    #[test]
    fn mrc_pc_updates_nzcv_but_preserves_sticky_q() {
        for seed in 0..8 {
            let mut c = Cpu::default();
            c.q = true;
            let mut m = Ram::new(&[], 0, seed);
            let value = m.mrc(&mut c, 15, 0, 1, 0, 0);
            exec(
                &d(Inst::Cp {
                    load: true,
                    cp: 15,
                    op1: 0,
                    crn: 1,
                    crm: 0,
                    op2: 0,
                    rd: 15,
                }),
                0,
                &mut c,
                &mut m,
            );
            assert!(c.q);
            assert_eq!(
                [c.n, c.z, c.c, c.v],
                [
                    value >> 31 != 0,
                    value & (1 << 30) != 0,
                    value & (1 << 29) != 0,
                    value & (1 << 28) != 0
                ]
            );
        }
    }
    #[test]
    fn pc_store_and_unaligned_word_load_are_distinct() {
        let mut c = Cpu::default();
        let mut m = Ram::new(&[], 0, 0);
        c.r[4] = 0x200;
        c.r[15] = 0xdead_beef;
        exec(
            &d(Inst::Block {
                load: false,
                pre: false,
                up: true,
                wb: false,
                rn: 4,
                list: 0x8000,
            }),
            0x100,
            &mut c,
            &mut m,
        );
        assert_eq!(m.r32(0x200), 0x10c);
        m.w32(0x200, 0x1122_3344);
        c.r[4] = 0x201;
        exec(
            &d(Inst::Mem {
                load: true,
                byte: false,
                pre: true,
                up: true,
                wb: false,
                rn: 4,
                rd: 0,
                off: Off::Imm(0),
            }),
            0,
            &mut c,
            &mut m,
        );
        assert_eq!(c.r[0], 0x4411_2233);
    }
    #[test]
    fn budget_zero_never_fetches_or_invokes_services() {
        let mut c = Cpu::default();
        c.budget = 0;
        let mut m = Ram::new(&[], 0, 0);
        assert_eq!(
            run(&mut c, &mut m, &[0x100], &[0x100], 0x100),
            BUDGET_EXHAUSTED
        );
        assert!(m.events.is_empty());
    }
    #[test]
    fn thumb_fetch_and_failed_arm_condition_have_correct_instruction_sizes() {
        let image = [0x01, 0x20, 0x70, 0x47]; // movs r0,#1; bx lr
        let mut c = Cpu::default();
        c.budget = 8;
        c.r[14] = 0x201;
        let mut m = Ram::new(&image, 0x100, 0);
        assert_eq!(run(&mut c, &mut m, &[0x101], &[0x101, 0x103], 0x101), 0x201);
        assert_eq!(c.r[0], 1);
        assert_eq!(c.budget, 6);
        let image = 0x0280_0001_u32.to_le_bytes(); // addeq r0,r0,#1; Z clear
        let mut c = Cpu::default();
        c.budget = 1;
        let mut m = Ram::new(&image, 0x100, 0);
        assert_eq!(run(&mut c, &mut m, &[0x100], &[0x100], 0x100), 0x104);
        assert_eq!(c.r[0], 0);
        assert_eq!(c.budget, 0);
    }
}
