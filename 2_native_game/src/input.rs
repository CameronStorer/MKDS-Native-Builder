//! DS keypad pins and per-CPU keypad interrupt control.
//!
//! Host state is active-high; MMIO is active-low. No game-specific input masks,
//! shared-RAM layouts or bindings are embedded here. See private input work log
//! for primary-source references and the pending ARM7/touch integration.
pub const KEYINPUT: u32 = 0x0400_0130;
pub const KEYCNT: u32 = 0x0400_0132;
pub const EXTKEYIN: u32 = 0x0400_0136;
pub const IRQ_KEYPAD: u32 = 1 << 12;
pub const IRQ_LID_OPEN: u32 = 1 << 22;

/// Host button mask bits0..9: A,B,Select,Start,Right,Left,Up,Down,R,L;
/// bits10/11: X,Y. Pen/debug/lid are separate pins, not normal buttons.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostInput {
    pub buttons: u16,
    pub pen_down: bool,
    pub debug_down: bool,
    pub lid_closed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Core { Arm9, Arm7 }
impl Core {
    fn index(self) -> usize { match self { Self::Arm9 => 0, Self::Arm7 => 1 } }
}

/// Requests to OR into each CPU's IF register. Does not apply IE/IME or clear IF.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IrqEvents { pub arm9: u32, pub arm7: u32 }

#[derive(Clone, Debug, Default)]
pub struct Keypad {
    /// Latest host pin snapshot; unknown button bits are discarded.
    state: HostInput,
    /// Separate ARM9/ARM7 control words. Only selected low10 bits enter matching.
    keycnt: [u16; 2],
}

impl Keypad {
    pub fn new() -> Self { Self::default() }
    pub fn snapshot(&self) -> HostInput { self.state }
    pub fn keyinput(&self) -> u16 { (!self.state.buttons) & 0x03ff }
    pub fn extkeyin(&self) -> u16 {
        // Bits2,4,5 are high. Debug bit3 and pen bit6 are active-low;
        // hinge bit7 is high when closed. Upper byte reads zero.
        ((!self.state.buttons >> 10) & 3) | 0x34
            | ((!self.state.debug_down as u16) << 3)
            | ((!self.state.pen_down as u16) << 6)
            | ((self.state.lid_closed as u16) << 7)
    }
    pub fn control(&self, core: Core) -> u16 { self.keycnt[core.index()] }

    /// Apply one ordered host snapshot. Emits only false->true keypad matches;
    /// repeated held states cannot repeatedly set IF after guest acknowledgement.
    /// Enabling KEYCNT while held does not itself inject a press event.
    pub fn update(&mut self, mut next: HostInput) -> IrqEvents {
        next.buttons &= 0x0fff;
        let before = self.state.buttons & 0x03ff;
        let after = next.buttons & 0x03ff;
        let mut irq = IrqEvents::default();
        for (i, cnt) in self.keycnt.iter().copied().enumerate() {
            let selected = cnt & 0x03ff;
            let matches = |held: u16| if cnt & 0x8000 != 0 {
                held & selected == selected
            } else { held & selected != 0 };
            if cnt & 0x4000 != 0 && !matches(before) && matches(after) {
                if i == 0 { irq.arm9 |= IRQ_KEYPAD; } else { irq.arm7 |= IRQ_KEYPAD; }
            }
        }
        if self.state.lid_closed && !next.lid_closed { irq.arm7 |= IRQ_LID_OPEN; }
        self.state = next;
        irq
    }

    /// None leaves unsupported registers/core access to the system bus.
    pub fn read16(&self, core: Core, address: u32) -> Option<u16> {
        match address {
            KEYINPUT => Some(self.keyinput()),
            KEYCNT => Some(self.control(core)),
            EXTKEYIN if core == Core::Arm7 => Some(self.extkeyin()),
            _ => None,
        }
    }
    pub fn read8(&self, core: Core, address: u32) -> Option<u8> {
        self.read16(core, address & !1).map(|v| (v >> (8 * (address & 1))) as u8)
    }
    pub fn read32(&self, core: Core, address: u32) -> Option<u32> {
        if address == KEYINPUT { Some(self.keyinput() as u32 | ((self.control(core) as u32) << 16)) }
        else { None }
    }
    /// Returns whether handled. Writes to input pins are ignored; KEYCNT writes
    /// retain the complete word, with reserved bits inert in interrupt matching.
    pub fn write16(&mut self, core: Core, address: u32, value: u16) -> bool {
        match address {
            KEYCNT => { self.keycnt[core.index()] = value; true }
            KEYINPUT => true,
            EXTKEYIN if core == Core::Arm7 => true,
            _ => false,
        }
    }
    pub fn write8(&mut self, core: Core, address: u32, value: u8) -> bool {
        if address & !1 == KEYCNT {
            let shift = 8 * (address & 1);
            let old = self.control(core);
            self.keycnt[core.index()] = (old & !(0xff << shift)) | ((value as u16) << shift);
            true
        } else { self.read8(core, address).is_some() }
    }
    pub fn write32(&mut self, core: Core, address: u32, value: u32) -> bool {
        if address == KEYINPUT { self.keycnt[core.index()] = (value >> 16) as u16; true }
        else { false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn held(buttons: u16) -> HostInput { HostInput { buttons, ..HostInput::default() } }
    #[test]
    fn active_low_pins_and_arm7_visibility() {
        let mut k = Keypad::new();
        assert_eq!(k.keyinput(), 0x03ff);
        assert_eq!(k.extkeyin(), 0x007f);
        k.update(HostInput { buttons: 1 | (1<<9) | (1<<10), pen_down: true, debug_down: true, lid_closed: true });
        assert_eq!(k.keyinput(), 0x01fe);
        assert_eq!(k.extkeyin(), 0xb6);
        assert_eq!(k.read16(Core::Arm9, EXTKEYIN), None);
        assert_eq!(k.read16(Core::Arm7, EXTKEYIN), Some(0xb6));
    }
    #[test]
    fn all_button_and_selection_combinations_match_logic() {
        let mut k = Keypad::new();
        for selected in 0..1024 {
            for buttons in 0..1024 {
                for and in [false, true] {
                    k.update(held(0));
                    k.write16(Core::Arm9, KEYCNT, 0x4000 | selected | if and { 0x8000 } else { 0 });
                    let got = k.update(held(buttons));
                    let expected = selected != 0 && if and { buttons & selected == selected } else { buttons & selected != 0 };
                    assert_eq!(got.arm9 == IRQ_KEYPAD, expected, "mask{selected:x} held{buttons:x} AND{and}");
                    assert_eq!(got.arm7, 0);
                }
            }
        }
    }
    #[test]
    fn independent_controls_hold_release_and_enable() {
        let mut k = Keypad::new();
        k.write16(Core::Arm9, KEYCNT, 0x4001);
        k.write16(Core::Arm7, KEYCNT, 0xc003);
        assert_eq!(k.update(held(1)), IrqEvents { arm9: IRQ_KEYPAD, arm7: 0 });
        assert_eq!(k.update(held(3)), IrqEvents { arm9: 0, arm7: IRQ_KEYPAD });
        assert_eq!(k.update(held(3)), IrqEvents::default());
        k.write16(Core::Arm9, KEYCNT, 0);
        k.write16(Core::Arm9, KEYCNT, 0x4001);
        assert_eq!(k.update(held(3)), IrqEvents::default());
        k.update(held(0));
        assert_eq!(k.update(held(1)).arm9, IRQ_KEYPAD);
    }
    #[test]
    fn partial_and_word_access_preserve_other_control_bytes() {
        let mut k = Keypad::new();
        k.write16(Core::Arm9, KEYCNT, 0xc123);
        k.write8(Core::Arm9, KEYCNT, 0x55);
        assert_eq!(k.control(Core::Arm9), 0xc155);
        k.write8(Core::Arm9, KEYCNT+1, 0x40);
        assert_eq!(k.read32(Core::Arm9, KEYINPUT), Some(0x405503ff));
        k.write32(Core::Arm7, KEYINPUT, 0xc0030000);
        assert_eq!(k.read8(Core::Arm7, KEYCNT+1), Some(0xc0));
        assert_eq!(k.keyinput(), 0x3ff);
        assert_eq!(k.read16(Core::Arm9, KEYINPUT+1), None);
    }
    #[test]
    fn extra_keys_and_lid_do_not_trigger_keypad_interrupt() {
        let mut k = Keypad::new();
        k.write16(Core::Arm9, KEYCNT, 0xffff);
        k.write16(Core::Arm7, KEYCNT, 0x4001);
        let closed = HostInput { buttons: 0xffff & !0x03ff, pen_down: true, lid_closed: true, debug_down: true };
        assert_eq!(k.update(closed), IrqEvents::default());
        assert_eq!(k.snapshot().buttons, 0x0c00);
        assert_eq!(k.update(held(0)), IrqEvents { arm9: 0, arm7: IRQ_LID_OPEN });
        assert_eq!(k.update(held(0)), IrqEvents::default());
    }
}
