//! ARM7 IRQ stub round trip through the interpreter (lives here because `rt.rs` is also
//! `#[path]`-included by `mkds_gen`, which has no `mem`/`interp`).
use recomp::rt::*;

#[test]
fn irq_through_the_stub_returns_to_thumb_with_banks_restored() {
    let mut m = recomp::mem::Ram::new(&[], 0, 1);
    for (i, w) in ARM7_IRQ_STUB.iter().enumerate() {
        m.w32(0x18 + 4 * i as u32, *w);
    }
    m.w32(0x0200_0200, 0xE3A0_5077); // handler: mov r5, #0x77
    m.w32(0x0200_0204, 0xE12F_FF1E); //          bx lr
    m.w32(0x03FF_FFFC, 0x0200_0200);
    let mut c = Cpu::default();
    c.r[13] = 0x0210_0000;
    c.banked[2][0] = 0x0220_0000; // IRQ-mode sp
    c.r[0] = 11;
    c.z = true;
    let v = take_irq(&mut c, 0x0200_0101, 0);
    assert_eq!((v, c.ctl & 0x1F, c.r[13]), (0x18, 0x12, 0x0220_0000));
    let pc = recomp::interp::run_until(&mut c, &mut m, v, &|p| p == 0x0200_0101);
    assert_eq!(pc, 0x0200_0101);
    assert_eq!((c.r[5], c.r[0], c.ctl & 0x1F, c.r[13], c.banked[2][0], c.z), (0x77, 11, 0x1F, 0x0210_0000, 0x0220_0000, true));
}
