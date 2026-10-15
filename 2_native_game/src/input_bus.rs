//! Core-specific keypad overlay for a guest memory implementation.
//! This executes ROM code through the dispatcher; it does not embed a game's
//! ARM7 shared-memory publisher, touch protocol or button bindings.
use crate::dispatch::Dispatcher;
use crate::input::{Core, Keypad, EXTKEYIN};
use crate::rt::{Bus, Cpu, Mem};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedService {
    Swi(u32),
    ReadCoprocessor { cp: u8, op1: u8, crn: u8, crm: u8, op2: u8 },
    WriteCoprocessor { cp: u8, op1: u8, crn: u8, crm: u8, op2: u8 },
}

/// `memory` must provide the selected core's address space. This adapter adds
/// keypad visibility/control; it is not a full ARM7 bus or sound HLE.
pub struct InputBus<'a> {
    pub core: Core,
    pub memory: &'a mut dyn Mem,
    pub keypad: &'a mut Keypad,
    pub dispatcher: &'a Dispatcher,
    /// Any entry makes a run incomplete; unsupported services exhaust the CPU
    /// budget so a caller cannot accidentally certify placeholder results.
    pub unsupported: Vec<UnsupportedService>,
}

impl<'a> InputBus<'a> {
    pub fn new(core: Core, memory: &'a mut dyn Mem, keypad: &'a mut Keypad, dispatcher: &'a Dispatcher) -> Self {
        Self { core, memory, keypad, dispatcher, unsupported: Vec::new() }
    }
    fn stop(&mut self, cpu: &mut Cpu, service: UnsupportedService) {
        self.unsupported.push(service);
        cpu.budget = 0;
    }
}

impl Mem for InputBus<'_> {
    fn r8(&mut self, address: u32) -> u8 {
        self.keypad.read8(self.core, address).unwrap_or_else(|| self.memory.r8(address))
    }
    fn r16(&mut self, address: u32) -> u16 {
        self.keypad.read16(self.core, address).unwrap_or_else(|| self.memory.r16(address))
    }
    fn r32(&mut self, address: u32) -> u32 {
        if self.core == Core::Arm7 && address == EXTKEYIN - 2 {
            // Low RCNT half belongs to backing memory; high half is EXTKEYIN.
            return self.memory.r16(address) as u32 | ((self.keypad.extkeyin() as u32) << 16);
        }
        self.keypad.read32(self.core, address).unwrap_or_else(|| self.memory.r32(address))
    }
    fn w8(&mut self, address: u32, value: u8) {
        if !self.keypad.write8(self.core, address, value) { self.memory.w8(address, value); }
    }
    fn w16(&mut self, address: u32, value: u16) {
        if !self.keypad.write16(self.core, address, value) { self.memory.w16(address, value); }
    }
    fn w32(&mut self, address: u32, value: u32) {
        if self.core == Core::Arm7 && address == EXTKEYIN - 2 {
            self.memory.w16(address, value as u16); // read-only extended pins
        } else if !self.keypad.write32(self.core, address, value) { self.memory.w32(address, value); }
    }
}

impl Bus for InputBus<'_> {
    fn call(&mut self, cpu: &mut Cpu, target: u32, ret: u32) -> u32 {
        let dispatcher = self.dispatcher;
        dispatcher.call(cpu, self, target, ret)
    }
    fn swi(&mut self, cpu: &mut Cpu, number: u32) { self.stop(cpu, UnsupportedService::Swi(number)); }
    fn mrc(&mut self, cpu: &mut Cpu, cp: u8, op1: u8, crn: u8, crm: u8, op2: u8) -> u32 {
        self.stop(cpu, UnsupportedService::ReadCoprocessor { cp, op1, crn, crm, op2 });
        0
    }
    fn mcr(&mut self, cpu: &mut Cpu, cp: u8, op1: u8, crn: u8, crm: u8, op2: u8, _value: u32) {
        self.stop(cpu, UnsupportedService::WriteCoprocessor { cp, op1, crn, crm, op2 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{HostInput, KEYINPUT, KEYCNT};
    use crate::mem::Ram;
    #[test]
    fn overlays_pins_and_combines_extended_word_without_corrupting_rcnt() {
        let dispatch = Dispatcher::new(&[]);
        let mut memory = Ram::new(&[], 0, 1);
        memory.w16(EXTKEYIN-2, 0x8144);
        let mut keypad = Keypad::new();
        keypad.update(HostInput { buttons: 1<<10, ..HostInput::default() });
        let mut bus = InputBus::new(Core::Arm7, &mut memory, &mut keypad, &dispatch);
        assert_eq!(bus.r16(KEYINPUT), 0x3ff);
        assert_eq!(bus.r16(EXTKEYIN), 0x7e);
        assert_eq!(bus.r32(EXTKEYIN-2), 0x007e8144);
        bus.w32(EXTKEYIN-2, 0xdead1234);
        assert_eq!(bus.r32(EXTKEYIN-2), 0x007e1234);
        bus.w32(KEYINPUT, 0xc0030000);
        assert_eq!(bus.r16(KEYCNT), 0xc003);
        assert_eq!(bus.keypad.control(Core::Arm9), 0);
    }
    #[test]
    fn unsupported_service_cannot_pass_as_success() {
        let dispatch = Dispatcher::new(&[]);
        let mut memory = Ram::new(&[], 0, 1);
        let mut keypad = Keypad::new();
        let mut bus = InputBus::new(Core::Arm7, &mut memory, &mut keypad, &dispatch);
        let mut cpu = Cpu::default();
        bus.swi(&mut cpu, 9);
        assert_eq!(cpu.budget, 0);
        assert_eq!(bus.unsupported, vec![UnsupportedService::Swi(9)]);
    }
}
