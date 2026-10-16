//! Native functions (block 3, `4_docs/BLOCK3_PLAN.md`): readable Rust that replaces a whole guest
//! function once the function dictionary knows what it does. Contains no game code or data: each
//! entry is our own implementation, found by the address of the function it replaces.
//!
//! Contract for every native function:
//! - It has the shape of a lifted block ([`LiftedFn`]) and is installed by the [`Dispatcher`] at the
//!   function's entry block of its segment (overlays share addresses, so the segment matters). It
//!   runs when execution reaches that entry, the way the lifted code would.
//! - It does the function's whole job: reads arguments from r0-r3 / the stack, works on DS memory
//!   through the bus (an array; DS addresses stay DS addresses), sets the results, and returns where
//!   the original's `bx lr` would go ([`ret`]). Registers the original leaves changed must end up the
//!   same (the shadow checker compares r0-r3, r12, sp, lr and the flags); stack bytes below the entry
//!   sp are dead and not compared.
//! - It pays its instruction budget first ([`enter`]), so time keeps moving and a slice can end.
//! - It is a leaf: it may not run other guest code. In the game, `Bus::call` does not run the callee
//!   (calls continue in the dispatch loop), so functions that call others are not native yet.
//!
//! [`Dispatcher`]: crate::dispatch::Dispatcher
use crate::mem::LiftedFn;
use crate::rt::{self, Cpu};

/// One native function: the segment and entry (bit 0 = Thumb) of the function it replaces.
pub struct Native {
    pub segment: &'static str,
    pub entry: u32,
    pub name: &'static str,
    pub f: LiftedFn,
}

/// Charges `cost` instructions; `false` means the slice is out of budget and the caller must return
/// [`rt::BUDGET_EXHAUSTED`] (the dispatch loop comes back to the same entry later).
#[inline]
pub fn enter(c: &mut Cpu, pc: u32, cost: u32) -> bool {
    if rt::spend(c, cost) {
        return true;
    }
    c.r[15] = pc;
    false
}

/// The pc after the function returns (`bx lr`).
#[inline]
pub fn ret(c: &Cpu) -> u32 {
    crate::arm::interwork(c.r[14])
}

/// Every native function (on trial until it passes the shadow checker), in no particular order.
pub static NATIVE: &[Native] = &[];

/// The natives that passed the shadow checker over whole recorded races (`native-verified` in the
/// dictionary): what the game installs.
pub static VERIFIED: &[Native] = &[];

/// What a game or tool installs: [`VERIFIED`], or for diagnostics per `MKDS_NATIVES`: `off` = none (the
/// exact translation everywhere), `all` = every [`NATIVE`] including those on trial.
pub fn selected() -> &'static [Native] {
    match std::env::var("MKDS_NATIVES").as_deref() {
        Ok("off") => &[],
        Ok("all") => NATIVE,
        _ => VERIFIED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::Dispatcher;
    use crate::mem::Lifted;
    use crate::rt::{Bus, BUDGET_EXHAUSTED};

    fn lifted_double(c: &mut Cpu, _m: &mut dyn Bus, pc: u32) -> u32 {
        if pc == 0x0200_0000 {
            c.r[0] = c.r[0].wrapping_mul(2);
            return 0x0200_0010; // a second block of the same function
        }
        c.r[1] = 0xdead;
        ret(c)
    }
    fn native_double(c: &mut Cpu, _m: &mut dyn Bus, pc: u32) -> u32 {
        if !enter(c, pc, 3) {
            return BUDGET_EXHAUSTED;
        }
        c.r[0] = c.r[0].wrapping_mul(2) | 1;
        ret(c)
    }
    static TABLE: &[Lifted] = &[Lifted { segment: "arm9_main", entry: 0x0200_0000, blocks: &[0x0200_0000, 0x0200_0010], insts: &[], f: lifted_double }];
    static NAT: &[Native] = &[
        Native { segment: "arm9_main", entry: 0x0200_0000, name: "double", f: native_double },
        Native { segment: "ov9_000", entry: 0x0200_0000, name: "elsewhere", f: native_double },
    ];

    #[test]
    fn natives_replace_only_the_entry_of_their_own_segment() {
        let d = Dispatcher::new(TABLE);
        let mut m = crate::mem::Ram::new(&[], 0, 1);
        let mut c = Cpu { budget: 10, ..Cpu::default() };
        c.r[0] = 5;
        c.r[14] = 0x0200_1000;
        assert_eq!(d.get(0x0200_0000).unwrap()(&mut c, &mut m, 0x0200_0000), 0x0200_0010, "lifted before");
        d.set_natives(NAT);
        c.r[0] = 5;
        assert_eq!(d.get(0x0200_0000).unwrap()(&mut c, &mut m, 0x0200_0000), 0x0200_1000);
        assert_eq!((c.r[0], c.budget), (11, 7), "native ran and paid its cost");
        assert!(d.get(0x0200_0010).is_some(), "the function's other blocks stay lifted");
        c.budget = 0;
        assert_eq!(d.get(0x0200_0000).unwrap()(&mut c, &mut m, 0x0200_0000), BUDGET_EXHAUSTED);
        assert_eq!(c.r[15], 0x0200_0000, "resumes at the entry");
        d.set_natives(&[]);
        assert_eq!(d.get(0x0200_0000).unwrap() as usize, lifted_double as LiftedFn as usize, "removable");
    }
}
