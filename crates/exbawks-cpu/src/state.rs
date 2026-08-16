use serde::{Deserialize, Serialize};

/// A general-purpose guest register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Gpr {
    /// EAX.
    Eax = 0,
    /// ECX.
    Ecx = 1,
    /// EDX.
    Edx = 2,
    /// EBX.
    Ebx = 3,
    /// ESP.
    Esp = 4,
    /// EBP.
    Ebp = 5,
    /// ESI.
    Esi = 6,
    /// EDI.
    Edi = 7,
}

/// A guest segment register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Segment {
    /// ES.
    Es = 0,
    /// CS.
    Cs = 1,
    /// SS.
    Ss = 2,
    /// DS.
    Ds = 3,
    /// FS.
    Fs = 4,
    /// GS.
    Gs = 5,
}

/// Cached state for one guest segment.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C)]
pub struct SegmentState {
    /// The visible selector.
    pub selector: u16,
    /// The hidden descriptor attributes.
    pub attributes: u16,
    /// The cached segment base.
    pub base: u32,
    /// The cached segment limit.
    pub limit: u32,
}

/// Guest x87 state that requires explicit lowering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C)]
pub struct X87State {
    /// The x87 control word.
    pub control: u16,
    /// The x87 status word.
    pub status: u16,
    /// The x87 tag word.
    pub tag: u16,
    /// The x87 opcode field.
    pub opcode: u16,
    /// Eight packed 80-bit values stored in 16-byte slots.
    pub registers: [[u8; 16]; 8],
}

impl Default for X87State {
    fn default() -> Self {
        Self { control: 0x037F, status: 0, tag: 0xFFFF, opcode: 0, registers: [[0; 16]; 8] }
    }
}

/// Complete architectural state for one guest CPU thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C, align(64))]
pub struct CpuState {
    /// General-purpose registers in x86 encoding order.
    pub gpr: [u32; 8],
    /// The guest instruction pointer.
    pub eip: u32,
    /// The guest EFLAGS value.
    pub eflags: u32,
    /// Cached segment state.
    pub segments: [SegmentState; 6],
    /// The eight guest XMM registers.
    pub xmm: [u128; 8],
    /// The MXCSR register.
    pub mxcsr: u32,
    /// The x87 register file.
    pub x87: X87State,
    /// The deterministic virtualized time-stamp counter.
    ///
    /// The interpreter advances it once per retired instruction; the
    /// virtual-clock design (boot plan D3) will formalize its scaling. It
    /// never reads the host time-stamp counter.
    pub tsc: u64,
}

impl CpuState {
    /// The byte offset of the general-purpose register file.
    pub const GPR_OFFSET: usize = core::mem::offset_of!(CpuState, gpr);
    /// The byte offset of the guest instruction pointer.
    pub const EIP_OFFSET: usize = core::mem::offset_of!(CpuState, eip);
    /// The byte offset of the guest EFLAGS value.
    pub const EFLAGS_OFFSET: usize = core::mem::offset_of!(CpuState, eflags);

    /// Returns the byte offset of one general-purpose register.
    #[must_use]
    pub const fn gpr_offset(register: Gpr) -> usize {
        Self::GPR_OFFSET + (register as usize) * 4
    }

    /// Reads one general-purpose register.
    #[must_use]
    pub const fn get(&self, register: Gpr) -> u32 {
        self.gpr[register as usize]
    }

    /// Writes one general-purpose register.
    pub fn set(&mut self, register: Gpr, value: u32) {
        self.gpr[register as usize] = value;
    }

    /// Reads one cached segment.
    #[must_use]
    pub const fn segment(&self, segment: Segment) -> SegmentState {
        self.segments[segment as usize]
    }

    /// Writes one cached segment.
    pub fn set_segment(&mut self, segment: Segment, value: SegmentState) {
        self.segments[segment as usize] = value;
    }
}

impl Default for CpuState {
    fn default() -> Self {
        Self {
            gpr: [0; 8],
            eip: 0,
            eflags: 0x0000_0002,
            segments: [SegmentState::default(); 6],
            xmm: [0; 8],
            mxcsr: 0x1F80,
            x87: X87State::default(),
            tsc: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_access_uses_x86_encoding_order() {
        let mut state = CpuState::default();
        state.set(Gpr::Esp, 0x1234_5678);
        assert_eq!(state.gpr[4], 0x1234_5678);
        assert_eq!(state.get(Gpr::Esp), 0x1234_5678);
    }
}
