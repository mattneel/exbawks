//! The OHCI host controller, as much of it as a title's driver needs.
//!
//! A title drives this register file directly — its USB stack is linked
//! into the executable — so the model has to behave like the hardware
//! rather than like an interface. The driver writes an operational mode,
//! hands over a communication area, reads the root port to find what is
//! attached, and then builds endpoint and transfer descriptors in guest
//! memory for the controller to walk.
//!
//! Everything here is pure logic: guest memory arrives through a trait, and
//! no host pointer reaches it.

use crate::gamepad::{GamepadState, REPORT_BYTES};

/// Guest physical memory, as the controller reaches it.
///
/// The controller both reads descriptors and writes completions, so a
/// defect here corrupts guest memory rather than merely returning a wrong
/// value. Every address it follows comes from the guest and is checked.
pub trait UsbMemory {
    /// Reads one dword, or `None` when the address is not readable.
    fn read_dword(&self, physical: u32) -> Option<u32>;

    /// Writes one dword, reporting whether it landed.
    fn write_dword(&self, physical: u32, value: u32) -> bool;

    /// Writes a run of bytes, reporting whether all of it landed.
    fn write_bytes(&self, physical: u32, bytes: &[u8]) -> bool;
}

/// The register offsets a driver uses, from the controller's base.
///
/// The whole file is named even where this model only latches a register,
/// because a driver writes all of them during its initialisation sweep and
/// a name is what makes an access trace readable.
#[allow(dead_code)]
mod register {
    pub const REVISION: u32 = 0x00;
    pub const CONTROL: u32 = 0x04;
    pub const COMMAND_STATUS: u32 = 0x08;
    pub const INTERRUPT_STATUS: u32 = 0x0C;
    pub const INTERRUPT_ENABLE: u32 = 0x10;
    pub const INTERRUPT_DISABLE: u32 = 0x14;
    pub const HCCA: u32 = 0x18;
    pub const PERIOD_CURRENT_ED: u32 = 0x1C;
    pub const CONTROL_HEAD_ED: u32 = 0x20;
    pub const CONTROL_CURRENT_ED: u32 = 0x24;
    pub const BULK_HEAD_ED: u32 = 0x28;
    pub const BULK_CURRENT_ED: u32 = 0x2C;
    pub const DONE_HEAD: u32 = 0x30;
    pub const FM_INTERVAL: u32 = 0x34;
    pub const FM_REMAINING: u32 = 0x38;
    pub const FM_NUMBER: u32 = 0x3C;
    pub const PERIODIC_START: u32 = 0x40;
    pub const LS_THRESHOLD: u32 = 0x44;
    pub const RH_DESCRIPTOR_A: u32 = 0x48;
    pub const RH_DESCRIPTOR_B: u32 = 0x4C;
    pub const RH_STATUS: u32 = 0x50;
    pub const RH_PORT_STATUS: u32 = 0x54;
}

/// Root-hub port status bits, which are also its change bits shifted up.
#[allow(dead_code)]
mod port {
    /// A device is attached.
    pub const CONNECTED: u32 = 1 << 0;
    /// The port is enabled.
    pub const ENABLED: u32 = 1 << 1;
    /// The port is suspended.
    pub const SUSPENDED: u32 = 1 << 2;
    /// The port is powered.
    pub const POWERED: u32 = 1 << 8;
    /// A low-speed device is attached.
    pub const LOW_SPEED: u32 = 1 << 9;
    /// The connect status changed since the driver last cleared it.
    pub const CONNECT_CHANGE: u32 = 1 << 16;
    /// The enable status changed.
    pub const ENABLE_CHANGE: u32 = 1 << 17;
    /// The reset finished.
    pub const RESET_CHANGE: u32 = 1 << 20;

    /// Writing these bits clears the matching status.
    pub const CLEAR_ENABLE: u32 = 1 << 0;
    pub const SET_ENABLE: u32 = 1 << 1;
    pub const SET_SUSPEND: u32 = 1 << 2;
    pub const CLEAR_SUSPEND: u32 = 1 << 3;
    pub const SET_RESET: u32 = 1 << 4;
    pub const SET_POWER: u32 = 1 << 8;
    pub const CLEAR_POWER: u32 = 1 << 9;
}

/// The functional states `HcControl` selects.
const STATE_MASK: u32 = 0xC0;
const STATE_OPERATIONAL: u32 = 0x80;

/// `HcCommandStatus` bits.
const COMMAND_RESET: u32 = 1 << 0;
const COMMAND_CONTROL_FILLED: u32 = 1 << 1;
const COMMAND_BULK_FILLED: u32 = 1 << 2;

/// `HcInterruptStatus` bits this controller raises.
const INTERRUPT_WRITEBACK_DONE: u32 = 1 << 1;
const INTERRUPT_START_OF_FRAME: u32 = 1 << 2;
const INTERRUPT_ROOT_HUB_CHANGE: u32 = 1 << 6;

/// One emulated host controller with a single root port.
///
/// Only the first controller is modelled, because the title only programs
/// the first (ADR 0019).
#[derive(Debug)]
pub struct OhciController {
    /// The operational register file, indexed by offset over four.
    registers: [u32; 32],
    /// The root port's status, kept apart because its writes are commands.
    port_status: u32,
    /// Whether a device is attached to the root port.
    attached: bool,
    /// The frame counter the driver reads and the schedule advances.
    frame: u64,
    /// The gamepad state the next interrupt transfer will report.
    state: GamepadState,
    /// How many times the driver has written the root port.
    port_writes: u64,
    /// Reads and writes per register offset, for finding out what a
    /// driver is waiting on when it stops making progress.
    accesses: [(u64, u64); 32],
}

impl Default for OhciController {
    fn default() -> Self {
        let mut registers = [0_u32; 32];
        // Revision 1.0, as every OHCI reports.
        registers[(register::REVISION / 4) as usize] = 0x10;
        // One downstream port, always powered, not power-switched.
        registers[(register::RH_DESCRIPTOR_A / 4) as usize] = 0x0000_1201;
        // The default frame interval a driver expects to find.
        registers[(register::FM_INTERVAL / 4) as usize] = 0x2EDF;
        Self {
            registers,
            port_status: port::POWERED,
            attached: false,
            frame: 0,
            state: GamepadState::default(),
            port_writes: 0,
            accesses: [(0, 0); 32],
        }
    }
}

impl OhciController {
    /// Attaches or detaches the gamepad, raising the change the driver
    /// polls for.
    ///
    /// A driver learns about a device from the connect-change bit; setting
    /// the connected bit without it leaves an enumerating driver waiting.
    pub fn set_attached(&mut self, attached: bool) {
        if self.attached == attached {
            return;
        }
        self.attached = attached;
        if attached {
            self.port_status |= port::CONNECTED | port::CONNECT_CHANGE;
        } else {
            self.port_status &= !(port::CONNECTED | port::ENABLED);
            self.port_status |= port::CONNECT_CHANGE;
        }
        self.raise(INTERRUPT_ROOT_HUB_CHANGE);
        tracing::debug!(attached, port = format_args!("{:#010x}", self.port_status), "usb port");
    }

    /// Records the controller state the next report will carry.
    pub fn set_state(&mut self, state: GamepadState) {
        self.state = state;
    }

    /// Whether the driver has put the controller into its running state.
    #[must_use]
    pub fn operational(&self) -> bool {
        self.registers[(register::CONTROL / 4) as usize] & STATE_MASK == STATE_OPERATIONAL
    }

    /// The communication area the driver handed over, if it has.
    #[must_use]
    pub fn hcca(&self) -> u32 {
        self.registers[(register::HCCA / 4) as usize] & !0xFF
    }

    /// The root port's status word, as the driver would read it.
    #[must_use]
    pub fn port_status(&self) -> u32 {
        self.port_status
    }

    /// How many times the driver has written the root port. A driver that
    /// never writes it has not noticed the device.
    #[must_use]
    pub fn port_writes(&self) -> u64 {
        self.port_writes
    }

    /// The gamepad state the next report will carry.
    #[must_use]
    pub fn state(&self) -> GamepadState {
        self.state
    }

    /// The report the interrupt endpoint would return.
    #[must_use]
    pub fn report(&self) -> [u8; REPORT_BYTES] {
        self.state.report()
    }

    /// Reads and writes per register offset.
    #[must_use]
    pub fn accesses(&self) -> [(u64, u64); 32] {
        self.accesses
    }

    /// Raises an interrupt status bit.
    fn raise(&mut self, bits: u32) {
        self.registers[(register::INTERRUPT_STATUS / 4) as usize] |= bits;
    }

    /// Serves one register read.
    #[must_use]
    pub fn read(&mut self, offset: u32) -> u32 {
        if let Some(counts) = self.accesses.get_mut((offset / 4) as usize) {
            counts.0 = counts.0.saturating_add(1);
        }
        match offset {
            register::RH_PORT_STATUS => self.port_status,
            register::FM_NUMBER => (self.frame & 0xFFFF) as u32,
            // The remaining count runs down within a frame; a driver that
            // polls it wants to see it move.
            register::FM_REMAINING => self.registers[(register::FM_INTERVAL / 4) as usize] & 0x3FFF,
            // Reading either interrupt register reports what is enabled.
            register::INTERRUPT_DISABLE => {
                self.registers[(register::INTERRUPT_ENABLE / 4) as usize]
            }
            _ => self.registers.get((offset / 4) as usize).copied().unwrap_or(0),
        }
    }

    /// Accepts one register write.
    pub fn write(&mut self, offset: u32, value: u32) {
        if let Some(counts) = self.accesses.get_mut((offset / 4) as usize) {
            counts.1 = counts.1.saturating_add(1);
        }
        match offset {
            register::RH_PORT_STATUS => self.write_port(value),
            register::INTERRUPT_STATUS => {
                // Write one to clear, as every status register here is.
                let index = (register::INTERRUPT_STATUS / 4) as usize;
                self.registers[index] &= !value;
            }
            register::INTERRUPT_ENABLE => {
                let index = (register::INTERRUPT_ENABLE / 4) as usize;
                self.registers[index] |= value;
            }
            register::INTERRUPT_DISABLE => {
                let index = (register::INTERRUPT_ENABLE / 4) as usize;
                self.registers[index] &= !value;
            }
            register::COMMAND_STATUS => {
                let index = (register::COMMAND_STATUS / 4) as usize;
                // The reset bit is self-clearing: a driver sets it and
                // polls until the controller reports it done, so latching
                // it would spin forever.
                self.registers[index] |= value & !COMMAND_RESET;
                if value & COMMAND_RESET != 0 {
                    self.reset();
                }
            }
            register::RH_STATUS => {
                // Writing the global-power bits is accepted and has no
                // effect on a hub whose port is always powered.
            }
            // The read-only registers a driver may still write to.
            register::REVISION | register::FM_REMAINING | register::FM_NUMBER => {}
            _ => {
                if let Some(slot) = self.registers.get_mut((offset / 4) as usize) {
                    *slot = value;
                }
            }
        }
    }

    /// Applies a write to the root port, whose bits are commands.
    fn write_port(&mut self, value: u32) {
        self.port_writes = self.port_writes.saturating_add(1);
        // The change bits are write-one-to-clear.
        self.port_status &= !(value & 0x001F_0000);

        if value & port::CLEAR_ENABLE != 0 {
            self.port_status &= !port::ENABLED;
        }
        if value & port::SET_ENABLE != 0 && self.attached {
            self.port_status |= port::ENABLED;
        }
        if value & port::SET_SUSPEND != 0 {
            self.port_status |= port::SUSPENDED;
        }
        if value & port::CLEAR_SUSPEND != 0 {
            self.port_status &= !port::SUSPENDED;
        }
        if value & port::SET_POWER != 0 {
            self.port_status |= port::POWERED;
        }
        if value & port::CLEAR_POWER != 0 {
            self.port_status &= !port::POWERED;
        }
        if value & port::SET_RESET != 0 {
            // A reset on an occupied port completes immediately and leaves
            // the port enabled, which is what the driver waits for before
            // it addresses the device.
            if self.attached {
                self.port_status |= port::ENABLED | port::RESET_CHANGE | port::ENABLE_CHANGE;
            } else {
                self.port_status |= port::RESET_CHANGE;
            }
            self.raise(INTERRUPT_ROOT_HUB_CHANGE);
        }
    }

    /// Returns the controller to its post-reset state, keeping whatever is
    /// attached attached.
    fn reset(&mut self) {
        let attached = self.attached;
        let state = self.state;
        let writes = self.port_writes;
        let accesses = self.accesses;
        *self = Self::default();
        self.accesses = accesses;
        self.attached = attached;
        self.state = state;
        self.port_writes = writes;
        if attached {
            self.port_status |= port::CONNECTED | port::CONNECT_CHANGE;
            // The change survives the reset, so it has to be announced
            // again: a driver that resets the controller after a device
            // appeared would otherwise never hear about it.
            self.raise(INTERRUPT_ROOT_HUB_CHANGE);
        }
    }

    /// Advances the schedule by one frame.
    ///
    /// The frame number is what a driver's interrupt endpoint is scheduled
    /// against, and the communication area's head is where it looks for
    /// the frame's descriptor list.
    pub fn advance_frame(&mut self, memory: &dyn UsbMemory) {
        if !self.operational() {
            return;
        }
        self.frame = self.frame.wrapping_add(1);
        let hcca = self.hcca();
        if hcca != 0 {
            // The communication area carries the frame number, which a
            // driver reads from memory rather than from the register.
            let frame = (self.frame & 0xFFFF) as u32;
            memory.write_dword(hcca.wrapping_add(0x80), frame);
        }
        self.raise(INTERRUPT_START_OF_FRAME);
    }

    /// Whether an interrupt is pending and enabled.
    #[must_use]
    pub fn interrupt_pending(&self) -> bool {
        let status = self.registers[(register::INTERRUPT_STATUS / 4) as usize];
        let enabled = self.registers[(register::INTERRUPT_ENABLE / 4) as usize];
        status & enabled != 0
    }

    /// The head of the control list the driver built, if it filled one.
    #[must_use]
    pub fn control_list(&self) -> Option<u32> {
        let filled =
            self.registers[(register::COMMAND_STATUS / 4) as usize] & COMMAND_CONTROL_FILLED != 0;
        let head = self.registers[(register::CONTROL_HEAD_ED / 4) as usize];
        (filled && head != 0).then_some(head)
    }

    /// The head of the bulk list, if the driver filled one.
    #[must_use]
    pub fn bulk_list(&self) -> Option<u32> {
        let filled =
            self.registers[(register::COMMAND_STATUS / 4) as usize] & COMMAND_BULK_FILLED != 0;
        let head = self.registers[(register::BULK_HEAD_ED / 4) as usize];
        (filled && head != 0).then_some(head)
    }

    /// Reports a completed transfer to the driver's done queue.
    pub fn complete(&mut self, memory: &dyn UsbMemory, descriptor: u32) {
        let previous = self.registers[(register::DONE_HEAD / 4) as usize];
        // The done queue is a list the controller pushes onto, and the
        // next pointer lives in the descriptor's fourth word.
        memory.write_dword(descriptor.wrapping_add(12), previous);
        self.registers[(register::DONE_HEAD / 4) as usize] = descriptor;
        self.raise(INTERRUPT_WRITEBACK_DONE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Guest memory as a flat block, for the descriptor walk.
    #[derive(Default)]
    struct FakeMemory {
        words: RefCell<std::collections::HashMap<u32, u32>>,
    }

    impl UsbMemory for FakeMemory {
        fn read_dword(&self, physical: u32) -> Option<u32> {
            Some(self.words.borrow().get(&physical).copied().unwrap_or(0))
        }

        fn write_dword(&self, physical: u32, value: u32) -> bool {
            self.words.borrow_mut().insert(physical, value);
            true
        }

        fn write_bytes(&self, physical: u32, bytes: &[u8]) -> bool {
            for (index, byte) in bytes.iter().enumerate() {
                let address = physical.wrapping_add(index as u32);
                let word = address & !3;
                let shift = (address & 3) * 8;
                let mut words = self.words.borrow_mut();
                let existing = words.get(&word).copied().unwrap_or(0);
                let updated = (existing & !(0xFF << shift)) | (u32::from(*byte) << shift);
                words.insert(word, updated);
            }
            true
        }
    }

    #[test]
    fn a_fresh_controller_reports_an_empty_powered_port() {
        let mut controller = OhciController::default();
        assert_eq!(controller.read(register::REVISION), 0x10, "OHCI revision 1.0");
        assert_eq!(controller.read(register::RH_PORT_STATUS) & port::CONNECTED, 0);
        assert_ne!(controller.read(register::RH_PORT_STATUS) & port::POWERED, 0);
        assert!(!controller.operational(), "and it is not running yet");
    }

    #[test]
    fn attaching_a_device_raises_the_change_a_driver_waits_for() {
        let mut controller = OhciController::default();
        controller.set_attached(true);

        let status = controller.read(register::RH_PORT_STATUS);
        assert_ne!(status & port::CONNECTED, 0, "the port reports a device");
        assert_ne!(
            status & port::CONNECT_CHANGE,
            0,
            "and the change bit, without which a driver never looks"
        );
    }

    #[test]
    fn a_change_bit_clears_when_the_driver_writes_it_back() {
        let mut controller = OhciController::default();
        controller.set_attached(true);
        controller.write(register::RH_PORT_STATUS, port::CONNECT_CHANGE);

        let status = controller.read(register::RH_PORT_STATUS);
        assert_eq!(status & port::CONNECT_CHANGE, 0, "the change is acknowledged");
        assert_ne!(status & port::CONNECTED, 0, "but the device is still there");
    }

    #[test]
    fn resetting_an_occupied_port_enables_it() {
        let mut controller = OhciController::default();
        controller.set_attached(true);
        controller.write(register::RH_PORT_STATUS, port::SET_RESET);

        let status = controller.read(register::RH_PORT_STATUS);
        assert_ne!(status & port::ENABLED, 0, "the port comes up enabled");
        assert_ne!(status & port::RESET_CHANGE, 0, "and says the reset finished");
    }

    #[test]
    fn resetting_an_empty_port_leaves_it_disabled() {
        let mut controller = OhciController::default();
        controller.write(register::RH_PORT_STATUS, port::SET_RESET);
        assert_eq!(controller.read(register::RH_PORT_STATUS) & port::ENABLED, 0);
    }

    #[test]
    fn the_command_reset_bit_does_not_latch() {
        // A driver sets the reset bit and polls until the controller
        // reports it done; latching it would spin forever.
        let mut controller = OhciController::default();
        controller.write(register::CONTROL, STATE_OPERATIONAL);
        controller.write(register::COMMAND_STATUS, COMMAND_RESET);

        assert_eq!(controller.read(register::COMMAND_STATUS) & COMMAND_RESET, 0);
        assert!(!controller.operational(), "and the reset returned it to its default state");
    }

    #[test]
    fn a_reset_keeps_whatever_is_plugged_in() {
        let mut controller = OhciController::default();
        controller.set_attached(true);
        controller.write(register::COMMAND_STATUS, COMMAND_RESET);
        assert_ne!(controller.read(register::RH_PORT_STATUS) & port::CONNECTED, 0);
    }

    #[test]
    fn the_interrupt_status_register_clears_by_writing_ones() {
        let mut controller = OhciController::default();
        controller.set_attached(true);
        assert_ne!(controller.read(register::INTERRUPT_STATUS) & INTERRUPT_ROOT_HUB_CHANGE, 0);

        controller.write(register::INTERRUPT_STATUS, INTERRUPT_ROOT_HUB_CHANGE);
        assert_eq!(controller.read(register::INTERRUPT_STATUS) & INTERRUPT_ROOT_HUB_CHANGE, 0);
    }

    #[test]
    fn an_interrupt_is_pending_only_once_the_driver_enables_it() {
        let mut controller = OhciController::default();
        controller.set_attached(true);
        assert!(!controller.interrupt_pending(), "nothing is enabled yet");

        controller.write(register::INTERRUPT_ENABLE, INTERRUPT_ROOT_HUB_CHANGE);
        assert!(controller.interrupt_pending(), "and now the driver would be told");
    }

    #[test]
    fn advancing_a_frame_writes_the_number_into_the_communication_area() {
        let memory = FakeMemory::default();
        let mut controller = OhciController::default();
        controller.write(register::HCCA, 0x0010_0000);
        controller.write(register::CONTROL, STATE_OPERATIONAL);

        controller.advance_frame(&memory);
        assert_eq!(controller.read(register::FM_NUMBER), 1);
        assert_eq!(
            memory.read_dword(0x0010_0080),
            Some(1),
            "a driver reads the frame from memory, not only from the register"
        );
    }

    #[test]
    fn a_controller_that_is_not_running_does_not_advance() {
        let memory = FakeMemory::default();
        let mut controller = OhciController::default();
        controller.write(register::HCCA, 0x0010_0000);

        controller.advance_frame(&memory);
        assert_eq!(controller.read(register::FM_NUMBER), 0, "a stopped controller stands still");
    }

    #[test]
    fn a_completion_pushes_onto_the_done_queue() {
        let memory = FakeMemory::default();
        let mut controller = OhciController::default();

        controller.complete(&memory, 0x0020_0000);
        assert_eq!(controller.read(register::DONE_HEAD), 0x0020_0000);

        controller.complete(&memory, 0x0020_0100);
        assert_eq!(controller.read(register::DONE_HEAD), 0x0020_0100, "the newest is the head");
        assert_eq!(
            memory.read_dword(0x0020_0100 + 12),
            Some(0x0020_0000),
            "and it points at the one before"
        );
    }

    #[test]
    fn the_control_list_is_only_offered_once_the_driver_fills_it() {
        let mut controller = OhciController::default();
        controller.write(register::CONTROL_HEAD_ED, 0x0030_0000);
        assert_eq!(controller.control_list(), None, "a head with no filled bit is not a list");

        controller.write(register::COMMAND_STATUS, COMMAND_CONTROL_FILLED);
        assert_eq!(controller.control_list(), Some(0x0030_0000));
    }

    #[test]
    fn a_register_beyond_the_file_is_harmless() {
        // Device space is guest-addressable, so a stray access must not
        // panic or reach past the register file.
        let mut controller = OhciController::default();
        controller.write(0xFFFF_FFFF, 0xDEAD_BEEF);
        assert_eq!(controller.read(0xFFFF_FFFF), 0);
        assert_eq!(controller.read(0x1000), 0);
    }
}
