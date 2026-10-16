//! Argument / return analysis for the function dictionary (block 3): which of r0-r3 a function reads
//! before writing them (its register arguments) and whether its callers use r0 / r1 after it returns
//! (its result). Register liveness, iterated over the whole program because a call passes the callee's
//! arguments and a tail call passes its result on. A starting point for the dictionary's `args` and
//! `returns`, not a proof: stack arguments and calls through pointers are not followed (a call through a
//! pointer is assumed to read r0-r3).
use crate::arm::{Decoded, Inst, Off, Op2};
use crate::lift::Func;
use std::collections::BTreeMap;

const ARGS: u16 = 0b1111;
/// What a call may overwrite (AAPCS caller-saved): r0-r3, r12, lr.
const CLOBBER: u16 = ARGS | 1 << 12 | 1 << 14;

/// One function's result: register masks (bit n = rn).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Signature {
    /// Of r0-r3: read before written on some path from the entry.
    pub args: u16,
    /// Of r0-r1: read by a caller after a call returns (or passed on by a tail-calling function).
    pub returns: u16,
    /// Direct call sites (calls and tail calls) found in the analysed code.
    pub callers: u32,
}

/// What an instruction does to the registers, apart from calls and control flow.
fn uses(d: &Decoded) -> (u16, u16) {
    let bit = |r: usize| 1u16 << r;
    let op2 = |o: &Op2| match *o {
        Op2::Reg { rm, .. } => bit(rm),
        Op2::RegShift { rm, rs, .. } => bit(rm) | bit(rs),
        _ => 0,
    };
    let off = |o: &Off| match *o {
        Off::Reg { rm, .. } => bit(rm),
        Off::Imm(_) => 0,
    };
    let list = |l: u16| l;
    match d.inst {
        Inst::Dp { op, rn, rd, op2: o, .. } => {
            let reads = op2(&o) | if op == 13 || op == 15 { 0 } else { bit(rn) };
            (reads, if Inst::is_compare(op) { 0 } else { bit(rd) })
        }
        Inst::Mul { acc, rd, rn, rs, rm, .. } => (bit(rm) | bit(rs) | if acc { bit(rn) } else { 0 }, bit(rd)),
        Inst::MulLong { acc, lo, hi, rs, rm, .. } => (bit(rm) | bit(rs) | if acc { bit(lo) | bit(hi) } else { 0 }, bit(lo) | bit(hi)),
        Inst::MulHalf { rd, rn, rs, rm, .. } => (bit(rm) | bit(rs) | bit(rn) | bit(rd), bit(rd) | bit(rn)),
        Inst::Qalu { rd, rn, rm, .. } => (bit(rm) | bit(rn), bit(rd)),
        Inst::Clz { rd, rm } => (bit(rm), bit(rd)),
        Inst::Mrs { rd, .. } => (0, bit(rd)),
        Inst::Msr { src, .. } => (op2(&src), 0),
        Inst::Bx { rm, .. } => (bit(rm), 0),
        Inst::Mem { load, wb, pre, rn, rd, off: o, .. } | Inst::MemH { load, wb, pre, rn, rd, off: o, .. } => {
            let base = if wb || !pre { bit(rn) } else { 0 };
            if load { (bit(rn) | off(&o), bit(rd) | base) } else { (bit(rn) | off(&o) | bit(rd), base) }
        }
        Inst::MemD { load, wb, pre, rn, rd, off: o, .. } => {
            let pair = bit(rd) | bit(rd + 1);
            let base = if wb || !pre { bit(rn) } else { 0 };
            if load { (bit(rn) | off(&o), pair | base) } else { (bit(rn) | off(&o) | pair, base) }
        }
        Inst::Block { load, wb, rn, list: l, .. } | Inst::BlockUser { load, wb, rn, list: l, .. } => {
            let base = if wb { bit(rn) } else { 0 };
            if load { (bit(rn), list(l) | base) } else { (bit(rn) | list(l), base) }
        }
        Inst::Swp { rn, rd, rm, .. } => (bit(rn) | bit(rm), bit(rd)),
        Inst::Swi { .. } => (ARGS, ARGS),
        Inst::Cp { load, rd, .. } => if load { (0, if rd == 15 { 0 } else { bit(rd) }) } else { (bit(rd), 0) },
        Inst::B { .. } | Inst::Call { .. } | Inst::Nop | Inst::Undecoded(_) => (0, 0),
    }
}

/// `bx lr`, `bx r3` (Thumb code pops the return address into a low register first), `pop {.., pc}`,
/// `ldr pc, [sp], #4`, `mov pc, lr`: the function returns. (A `bx` through another register is a tail jump at
/// worst, which also leaves the function.)
fn is_return(d: &Decoded) -> bool {
    match d.inst {
        Inst::Bx { link: false, .. } => true,
        Inst::Block { load: true, rn: 13, list, .. } => list & 0x8000 != 0,
        Inst::Mem { load: true, rn: 13, rd: 15, .. } => true,
        Inst::Dp { op: 13, rd: 15, op2: Op2::Reg { rm: 14, kind: 0, amt: 0 }, .. } => true,
        _ => false,
    }
}

/// What happens after the instruction at `a`, for liveness.
enum Flow {
    /// Continue at these addresses inside the function.
    Next(Vec<u32>),
    /// A call to `target` (`None`: through a register), then `next`.
    Call(Option<u32>, u32),
    /// Leave the function: a return, or a tail call / jump out (`Some(target)` when known).
    Return,
    Out(Option<u32>),
}

fn flow(f: &Func, a: u32, d: &Decoded) -> Flow {
    let next = Func::next(a, d);
    let inside = |t: u32| f.insts.contains_key(&t);
    let conditional = d.cond != 14;
    match d.inst {
        Inst::Call { target } => Flow::Call(Some(target), next),
        Inst::Bx { link: true, .. } => Flow::Call(None, next),
        _ if is_return(d) && !conditional => Flow::Return,
        Inst::B { target } if inside(target) => Flow::Next(if conditional { vec![target, next] } else { vec![target] }),
        Inst::B { target } if !conditional => Flow::Out(Some(target)),
        Inst::B { target } => Flow::Next(vec![next, target]), // conditional tail call: handled by the caller of `flow`
        _ if d.inst.writes_pc() => {
            // a computed jump (switch table) can reach any block; a conditional one may also fall through
            let mut t: Vec<u32> = f.blocks.iter().copied().collect();
            if conditional || is_return(d) {
                t.push(next);
            }
            if is_return(d) {
                return Flow::Next(t); // conditional return: see the caller of `flow`
            }
            Flow::Next(t)
        }
        _ if inside(next) => Flow::Next(vec![next]),
        _ => Flow::Out(None),
    }
}

/// `funcs`: every analysed function by key, with its code. `resolve(caller, target)` names the function a
/// direct call or tail call from `caller` lands in (overlays share addresses, so the caller's segment matters).
pub fn analyse<K: Ord + Clone>(funcs: &BTreeMap<K, &Func>, resolve: &dyn Fn(&K, u32) -> Option<K>) -> BTreeMap<K, Signature> {
    let mut sig: BTreeMap<K, Signature> = funcs.keys().map(|k| (k.clone(), Signature::default())).collect();
    for (k, f) in funcs {
        for (&a, d) in &f.insts {
            let target = match (d.inst, flow(f, a, d)) {
                (_, Flow::Call(Some(t), _)) | (_, Flow::Out(Some(t))) => Some(t),
                (Inst::B { target }, _) if !f.insts.contains_key(&target) => Some(target),
                _ => None,
            };
            if let Some(callee) = target.and_then(|t| resolve(k, t)) {
                if let Some(s) = sig.get_mut(&callee) {
                    s.callers += 1;
                }
            }
        }
    }
    // r0/r1 a function itself may set as a result: its own writes and those of functions it tail-jumps to
    // (what an ordinary call leaves in r0-r3 is scratch by convention). A caller reading r1 after calling a
    // function that never sets it is reading something else, not a 64-bit result
    let mut may_write: BTreeMap<K, u16> = funcs.keys().map(|k| (k.clone(), 0)).collect();
    loop {
        let mut changed = false;
        for (k, f) in funcs {
            let mut w = may_write[k];
            for (&a, d) in &f.insts {
                w |= uses(d).1 & 0b11;
                if let Flow::Out(t) = flow(f, a, d) {
                    w |= t.and_then(|t| resolve(k, t)).and_then(|c| may_write.get(&c).copied()).unwrap_or(0b11);
                }
                if let Inst::B { target } = d.inst {
                    if !f.insts.contains_key(&target) {
                        w |= resolve(k, target).and_then(|c| may_write.get(&c).copied()).unwrap_or(0b11);
                    }
                }
            }
            if w != may_write[k] {
                may_write.insert(k.clone(), w);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // monotone: masks only grow, so this ends
    loop {
        let mut changed = false;
        for (k, f) in funcs {
            let ret_live = sig[k].returns;
            // a callee we cannot see (through a pointer, or unresolved) is assumed to read r0-r3: right for
            // our arguments (a thunk passes them all on), but such an assumed read of r1 after a call is not
            // a caller using a 64-bit result, so results come from the liveness without that assumption
            let known = |t: Option<u32>| t.and_then(|t| resolve(k, t)).and_then(|c| sig.get(&c)).map(|s| s.args);
            let args_of = |t: Option<u32>| known(t).unwrap_or(ARGS);
            let seen_of = |t: Option<u32>| known(t).unwrap_or(0);
            let args = liveness(f, ret_live, &args_of).get(&f.entry).copied().unwrap_or(0) & ARGS;
            let live = liveness(f, ret_live, &seen_of);
            let mut found: Vec<(K, u16)> = Vec::new();
            for (&a, d) in &f.insts {
                match flow(f, a, d) {
                    Flow::Call(Some(t), next) => {
                        if let Some(c) = resolve(k, t) {
                            found.push((c, live.get(&next).copied().unwrap_or(ret_live) & 0b11));
                        }
                    }
                    Flow::Out(Some(t)) => {
                        if let Some(c) = resolve(k, t) {
                            found.push((c, ret_live));
                        }
                    }
                    _ => {}
                }
                if let (Inst::B { target }, true) = (d.inst, d.cond != 14 && !f.insts.contains_key(&match d.inst { Inst::B { target } => target, _ => 0 })) {
                    if let Some(c) = resolve(k, target) {
                        found.push((c, ret_live));
                    }
                }
            }
            let s = sig.get_mut(k).unwrap();
            if s.args | args != s.args {
                s.args |= args;
                changed = true;
            }
            for (c, r) in found {
                let r = r & may_write.get(&c).copied().unwrap_or(0b11);
                if let Some(s) = sig.get_mut(&c) {
                    if s.returns | r != s.returns {
                        s.returns |= r;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            return sig;
        }
    }
}

/// Registers live before each instruction of `f`, given what is live at its returns.
fn liveness(f: &Func, ret_live: u16, args_of: &dyn Fn(Option<u32>) -> u16) -> BTreeMap<u32, u16> {
    let mut live: BTreeMap<u32, u16> = f.insts.keys().map(|&a| (a, 0)).collect();
    loop {
        let mut changed = false;
        for (&a, d) in f.insts.iter().rev() {
            let at = |t: &u32| live.get(t).copied().unwrap_or(0);
            let (reads, writes) = uses(d);
            let kill = if d.cond == 14 { writes } else { 0 };
            let conditional_exit = |out: u16| if d.cond != 14 { out | at(&Func::next(a, d)) } else { out };
            let after = match flow(f, a, d) {
                Flow::Next(ts) => {
                    let mut out: u16 = ts.iter().map(at).fold(0, |x, y| x | y);
                    if is_return(d) {
                        out |= ret_live;
                    }
                    if let Inst::B { target } = d.inst {
                        if !f.insts.contains_key(&target) {
                            out |= args_of(Some(target)) | ret_live;
                        }
                    }
                    out
                }
                Flow::Call(t, next) => {
                    let callee = args_of(t);
                    let out = at(&next) & !if d.cond == 14 { CLOBBER } else { 0 };
                    out | callee | if d.cond != 14 { at(&next) } else { 0 }
                }
                Flow::Return => ret_live,
                Flow::Out(t) => conditional_exit(args_of(t) | ret_live),
            };
            let before = reads | (after & !kill);
            if before != live[&a] {
                live.insert(a, before);
                changed = true;
            }
        }
        if !changed {
            return live;
        }
    }
}

/// The dictionary's spelling of a signature: `a0:u32, a1:u32` and `u32` / `u64` / `void` (empty when no
/// direct caller shows whether a result is used).
pub fn describe(s: &Signature) -> (String, String) {
    let args: Vec<String> = (0..4).filter(|r| s.args & (1 << r) != 0).map(|r| format!("a{r}:u32")).collect();
    let returns = match (s.returns, s.callers) {
        (0, 0) => "",
        (0, _) => "void",
        (r, _) if r & 2 != 0 => "u64",
        _ => "u32",
    };
    (args.join(", "), returns.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lift::{discover, Image};

    fn arm(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn arguments_and_results_follow_calls() {
        // 0x00: add r0, r0, r1 ; bx lr                      -> args r0 r1, result used by the caller
        // 0x10: push {lr} ; mov r1, #2 ; bl 0x00 ; add r0, r0, #1 ; pop {pc}   -> args r0 (r1 is set first)
        // 0x30: push {lr} ; mov r0, r2 ; bl 0x10 ; pop {pc}                     -> args r2; result passed on
        let mut w = vec![0u32; 16];
        w[0] = 0xE080_0001; // add r0, r0, r1
        w[1] = 0xE12F_FF1E; // bx lr
        w[4] = 0xE52D_E004; // push {lr}
        w[5] = 0xE3A0_1002; // mov r1, #2
        w[6] = 0xEBFF_FFF8; // bl 0x00  (0x18 + 8 - 8 words)
        w[7] = 0xE280_0001; // add r0, r0, #1
        w[8] = 0xE49D_F004; // pop {pc}
        w[12] = 0xE52D_E004; // push {lr}
        w[13] = 0xE1A0_0002; // mov r0, r2
        w[14] = 0xEBFF_FFF4; // bl 0x10  (0x38 + 8 - 12 words)
        w[15] = 0xE49D_F004; // pop {pc}
        let bytes = arm(&w);
        let img = Image::new(0x0200_0000, &bytes);
        let fs: Vec<Func> = [0x0200_0000, 0x0200_0010, 0x0200_0030].iter().map(|&e| discover(&img, e, Some((e, e + 0x20)), 100).unwrap()).collect();
        let funcs: BTreeMap<u32, &Func> = fs.iter().map(|f| (f.entry, f)).collect();
        let sig = analyse(&funcs, &|_, t| funcs.contains_key(&t).then_some(t));
        assert_eq!(sig[&0x0200_0000].args, 0b0011);
        assert_eq!(sig[&0x0200_0010].args, 0b0001, "r1 is written before the call");
        assert_eq!(sig[&0x0200_0030].args, 0b0100, "r2 only");
        assert_eq!(sig[&0x0200_0000].returns, 0b01, "its caller adds 1 to r0");
        assert_eq!((sig[&0x0200_0030].returns, sig[&0x0200_0030].callers), (0, 0), "nobody calls it");
        assert_eq!(describe(&sig[&0x0200_0000]), ("a0:u32, a1:u32".into(), "u32".into()));
        assert_eq!(describe(&sig[&0x0200_0030]).1, "", "unknown without callers");
    }
}
