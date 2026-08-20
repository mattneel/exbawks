//! The tier-0 interpreter (ADR 0008): full user-mode stepping over checked
//! guest memory.
//!
//! `step` fetches, decodes, and executes exactly one instruction at the guest
//! EIP. The implemented set covers memory operands, the integer ALU across
//! 8, 16, and 32-bit widths, near control flow, stack traffic, string
//! operations with repeat prefixes (committing per completed iteration, as
//! hardware restartability requires), CPUID, and RDTSC; floating point and
//! SIMD arrive in later stages and report `ExecError::Unsupported`.
//!
//! Flags follow the architectural definitions. Where hardware leaves a flag
//! undefined (auxiliary carry after shifts, sign/zero after `mul`, every flag
//! except carry after `bt`), the interpreter preserves the previous guest
//! value; each operation merges only its defined mask, mirroring the ADR 0006
//! emitter strategy so the differential harness compares like with like.

use exbawks_memory::{GuestMemory, MemoryError};
use exbawks_types::{GUEST_PAGE_SIZE, GuestVa};
use iced_x86::{
    Code, ConditionCode, Decoder, DecoderError, DecoderOptions, Instruction, Mnemonic, OpKind,
    Register,
};
use thiserror::Error;

use crate::{CpuState, Segment, flags};

/// A tier-0 interpreter failure.
#[derive(Debug, Error)]
pub enum ExecError {
    /// The instruction is outside the interpreter's implemented set.
    #[error("instruction at {address} is outside the interpreter's implemented set")]
    Unsupported {
        /// The guest instruction address.
        address: GuestVa,
    },
    /// The bytes at the guest EIP do not decode.
    #[error("invalid guest instruction at {address}")]
    InvalidInstruction {
        /// The guest instruction address.
        address: GuestVa,
    },
    /// A guest memory access failed.
    #[error(transparent)]
    Memory(#[from] MemoryError),
    /// A divide instruction raised the divide-error condition.
    #[error("divide error at {address}")]
    Divide {
        /// The guest instruction address.
        address: GuestVa,
    },
}

/// An operand width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Width {
    B,
    W,
    D,
}

impl Width {
    const fn bytes(self) -> usize {
        match self {
            Self::B => 1,
            Self::W => 2,
            Self::D => 4,
        }
    }

    const fn bits(self) -> u32 {
        match self {
            Self::B => 8,
            Self::W => 16,
            Self::D => 32,
        }
    }

    const fn mask(self) -> u32 {
        match self {
            Self::B => 0xFF,
            Self::W => 0xFFFF,
            Self::D => 0xFFFF_FFFF,
        }
    }

    const fn sign_bit(self) -> u32 {
        1 << (self.bits() - 1)
    }

    const fn from_bytes(bytes: usize) -> Option<Self> {
        match bytes {
            1 => Some(Self::B),
            2 => Some(Self::W),
            4 => Some(Self::D),
            _ => None,
        }
    }
}

/// One resolved instruction operand.
#[derive(Debug, Clone, Copy)]
enum Operand {
    Register(Register),
    Memory(GuestVa),
    Immediate(u32),
}

/// How one executed instruction affects the instruction pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Execution continues at the next sequential instruction.
    Next,
    /// The instruction wrote the guest EIP itself.
    Jumped,
}

/// Executes one instruction at the guest EIP and advances it.
///
/// State mutation is ordered so every fallible guest memory access happens
/// before registers, flags, or the EIP change; a fault leaves `CpuState`
/// exactly as it was. The one exception is a repeated string instruction,
/// which commits ESI/EDI/ECX and flags per completed iteration so a
/// mid-repeat fault leaves the partial progress hardware would leave; the
/// EIP still never moves on a fault, keeping the instruction restartable.
pub fn step(state: &mut CpuState, memory: &dyn GuestMemory) -> Result<(), ExecError> {
    step_with_ports(state, memory, &NoPorts)
}

/// Where `in` and `out` instructions read and write.
///
/// The console's I/O ports are a device surface like its MMIO ranges; a
/// caller without one passes [`NoPorts`], which answers zero and swallows
/// writes — the behavior of a bus with nothing listening.
pub trait PortBus {
    /// One port read of `bytes` bytes, zero-extended.
    fn port_read(&self, port: u16, bytes: u8) -> u32;

    /// One port write of the low `bytes` bytes of `value`.
    fn port_write(&self, port: u16, bytes: u8, value: u32);
}

/// A bus with nothing listening: reads are zero, writes vanish.
pub struct NoPorts;

impl PortBus for NoPorts {
    fn port_read(&self, _port: u16, _bytes: u8) -> u32 {
        0
    }

    fn port_write(&self, _port: u16, _bytes: u8, _value: u32) {}
}

/// [`step`], with I/O port instructions routed to `ports`.
pub fn step_with_ports(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    ports: &dyn PortBus,
) -> Result<(), ExecError> {
    let address = GuestVa(state.eip);
    let instruction = decode_at(state.eip, memory, address)?;
    match instruction.mnemonic() {
        Mnemonic::Out | Mnemonic::In => execute_port_io(state, ports, &instruction, address)?,
        _ => {
            if execute(state, memory, &instruction, address)? == Flow::Next {
                state.eip = state.eip.wrapping_add(instruction.len() as u32);
            }
            state.tsc = state.tsc.wrapping_add(1);
            return Ok(());
        }
    }
    state.eip = state.eip.wrapping_add(instruction.len() as u32);
    state.tsc = state.tsc.wrapping_add(1);
    Ok(())
}

/// `in`/`out` between EAX's low bytes and a port named by DX or an
/// immediate. The string forms stay unsupported until a title uses one.
fn execute_port_io(
    state: &mut CpuState,
    ports: &dyn PortBus,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<(), ExecError> {
    let accumulator_first = instruction.mnemonic() == Mnemonic::In;
    let port_operand = u32::from(accumulator_first);
    let port = match instruction.op_kind(port_operand) {
        OpKind::Immediate8 => u16::from(instruction.immediate8()),
        OpKind::Register if instruction.op_register(port_operand) == Register::DX => {
            (state.gpr[2] & 0xFFFF) as u16
        }
        _ => return Err(ExecError::Unsupported { address }),
    };
    let accumulator = instruction.op_register(if accumulator_first { 0 } else { 1 });
    let bytes = match accumulator {
        Register::AL => 1_u8,
        Register::AX => 2,
        Register::EAX => 4,
        _ => return Err(ExecError::Unsupported { address }),
    };
    let mask: u32 = match bytes {
        1 => 0xFF,
        2 => 0xFFFF,
        _ => u32::MAX,
    };
    if accumulator_first {
        let value = ports.port_read(port, bytes) & mask;
        state.gpr[0] = (state.gpr[0] & !mask) | value;
    } else {
        ports.port_write(port, bytes, state.gpr[0] & mask);
    }
    Ok(())
}

/// Fetches and decodes one instruction, straddling a page boundary when the
/// first page ends mid-instruction.
fn decode_at(
    eip: u32,
    memory: &dyn GuestMemory,
    address: GuestVa,
) -> Result<Instruction, ExecError> {
    const MAX_INSTRUCTION_LEN: usize = 15;

    let mut buffer = [0_u8; MAX_INSTRUCTION_LEN];
    let page_mask = GUEST_PAGE_SIZE - 1;
    let to_page_end = (GUEST_PAGE_SIZE - (eip & page_mask)) as usize;
    let first = MAX_INSTRUCTION_LEN.min(to_page_end);
    memory.fetch(address, &mut buffer[..first])?;

    let mut available = first;
    loop {
        let mut decoder =
            Decoder::with_ip(32, &buffer[..available], u64::from(eip), DecoderOptions::NONE);
        let instruction = decoder.decode();
        if !instruction.is_invalid() {
            return Ok(instruction);
        }
        if decoder.last_error() == DecoderError::NoMoreBytes && available < MAX_INSTRUCTION_LEN {
            memory.fetch(GuestVa(eip.wrapping_add(available as u32)), &mut buffer[available..])?;
            available = MAX_INSTRUCTION_LEN;
            continue;
        }
        return Err(ExecError::InvalidInstruction { address });
    }
}

fn execute(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<Flow, ExecError> {
    if let Some(flow) = branch(state, memory, instruction, address)? {
        return Ok(flow);
    }
    // The floating-point unit owns its own mnemonics (ADR 0018), and the
    // SSE unit owns the vector ones beside it.
    if let Some(result) = crate::x87::execute(state, memory, instruction, address) {
        result?;
        return Ok(Flow::Next);
    }
    if let Some(result) = crate::sse::execute(state, memory, instruction, address) {
        result?;
        return Ok(Flow::Next);
    }
    execute_straightline(state, memory, instruction, address)?;
    Ok(Flow::Next)
}

/// Executes the EIP-writing instruction families; returns `None` for every
/// other mnemonic.
fn branch(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<Option<Flow>, ExecError> {
    let fallthrough = address.0.wrapping_add(instruction.len() as u32);
    match instruction.mnemonic() {
        Mnemonic::Jmp => {
            let target = branch_target(state, memory, instruction, address)?;
            state.eip = target;
            Ok(Some(Flow::Jumped))
        }
        Mnemonic::Call => {
            let target = branch_target(state, memory, instruction, address)?;
            push_value(state, memory, Width::D, fallthrough)?;
            state.eip = target;
            Ok(Some(Flow::Jumped))
        }
        Mnemonic::Ret => {
            // Only the 32-bit near returns; `66 C3` pops 2 bytes on hardware.
            if !matches!(instruction.code(), Code::Retnd | Code::Retnd_imm16) {
                return Err(ExecError::Unsupported { address });
            }
            let target = pop_value(state, memory, Width::D)?;
            if instruction.code() == Code::Retnd_imm16 {
                state.gpr[4] = state.gpr[4].wrapping_add(u32::from(instruction.immediate16()));
            }
            state.eip = target;
            Ok(Some(Flow::Jumped))
        }
        Mnemonic::Loop | Mnemonic::Loope | Mnemonic::Loopne => {
            // Only the ECX-counted 32-bit forms exist in XDK code.
            if !matches!(
                instruction.code(),
                Code::Loop_rel8_32_ECX | Code::Loope_rel8_32_ECX | Code::Loopne_rel8_32_ECX
            ) {
                return Err(ExecError::Unsupported { address });
            }
            let counter = state.gpr[1].wrapping_sub(1);
            state.gpr[1] = counter;
            let zero = state.eflags & flags::ZERO != 0;
            let taken = counter != 0
                && match instruction.mnemonic() {
                    Mnemonic::Loope => zero,
                    Mnemonic::Loopne => !zero,
                    _ => true,
                };
            Ok(Some(jump_if(state, instruction, taken)))
        }
        Mnemonic::Jecxz => {
            if instruction.code() != Code::Jecxz_rel8_32 {
                return Err(ExecError::Unsupported { address });
            }
            let taken = state.gpr[1] == 0;
            Ok(Some(jump_if(state, instruction, taken)))
        }
        mnemonic => {
            // Jcc: a condition code with a near-branch operand.
            if instruction.condition_code() != ConditionCode::None
                && mnemonic_class(mnemonic).is_none()
            {
                if instruction.op_kind(0) != OpKind::NearBranch32 {
                    return Err(ExecError::Unsupported { address });
                }
                let taken = condition_met(state.eflags, instruction.condition_code());
                return Ok(Some(jump_if(state, instruction, taken)));
            }
            Ok(None)
        }
    }
}

fn jump_if(state: &mut CpuState, instruction: &Instruction, taken: bool) -> Flow {
    if taken {
        state.eip = instruction.near_branch32();
        Flow::Jumped
    } else {
        Flow::Next
    }
}

/// Resolves a near jump or call target: relative, register, or memory.
fn branch_target(
    state: &CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<u32, ExecError> {
    match instruction.op_kind(0) {
        OpKind::NearBranch32 => Ok(instruction.near_branch32()),
        OpKind::Register => {
            read_register(state, instruction.op_register(0), address).and_then(|value| {
                if instruction.op_register(0).size() == 4 {
                    Ok(value)
                } else {
                    Err(ExecError::Unsupported { address })
                }
            })
        }
        OpKind::Memory => {
            // Far-pointer operands (m16:16 is also 4 bytes) must not pass as
            // near indirect targets, so the exact near codes are required.
            if !matches!(instruction.code(), Code::Jmp_rm32 | Code::Call_rm32) {
                return Err(ExecError::Unsupported { address });
            }
            let location = effective_address(state, instruction, address, true)?;
            Ok(read_memory(memory, location, Width::D)?)
        }
        _ => Err(ExecError::Unsupported { address }),
    }
}

/// Pushes one value; the stack write commits before ESP moves.
fn push_value(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    width: Width,
    value: u32,
) -> Result<(), ExecError> {
    let new_esp = state.gpr[4].wrapping_sub(width.bytes() as u32);
    write_memory(memory, GuestVa(new_esp), width, value)?;
    state.gpr[4] = new_esp;
    Ok(())
}

/// Pops one value; the stack read commits before ESP moves.
fn pop_value(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    width: Width,
) -> Result<u32, ExecError> {
    let value = read_memory(memory, GuestVa(state.gpr[4]), width)?;
    state.gpr[4] = state.gpr[4].wrapping_add(width.bytes() as u32);
    Ok(value)
}

fn execute_straightline(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<(), ExecError> {
    match instruction.mnemonic() {
        Mnemonic::Nop | Mnemonic::Pause => Ok(()),
        // Cache control: the interpreter models no cache, so writing one
        // back or invalidating it changes nothing it can observe.
        Mnemonic::Wbinvd | Mnemonic::Invd | Mnemonic::Clflush => Ok(()),
        // The interrupt flag: the guest runs in ring zero and may gate
        // interrupt delivery. The run loop checks the flag before it
        // delivers, so clearing it here really defers the interrupt.
        Mnemonic::Cli => {
            state.eflags &= !flags::INTERRUPT;
            Ok(())
        }
        Mnemonic::Sti => {
            state.eflags |= flags::INTERRUPT;
            Ok(())
        }
        Mnemonic::Mov => mov(state, memory, instruction, address),
        Mnemonic::Movzx => widen(state, memory, instruction, address, false),
        Mnemonic::Movsx => widen(state, memory, instruction, address, true),
        Mnemonic::Lea => lea(state, instruction, address),
        Mnemonic::Xchg => xchg(state, memory, instruction, address),
        Mnemonic::Xadd => xadd(state, memory, instruction, address),
        Mnemonic::Cmpxchg => cmpxchg(state, memory, instruction, address),
        Mnemonic::Cmpxchg8b => cmpxchg8b(state, memory, instruction, address),
        Mnemonic::Add => binary_alu(state, memory, instruction, address, AluKind::Add),
        Mnemonic::Adc => binary_alu(state, memory, instruction, address, AluKind::Adc),
        Mnemonic::Sub => binary_alu(state, memory, instruction, address, AluKind::Sub),
        Mnemonic::Sbb => binary_alu(state, memory, instruction, address, AluKind::Sbb),
        Mnemonic::And => binary_alu(state, memory, instruction, address, AluKind::And),
        Mnemonic::Or => binary_alu(state, memory, instruction, address, AluKind::Or),
        Mnemonic::Xor => binary_alu(state, memory, instruction, address, AluKind::Xor),
        Mnemonic::Cmp => binary_alu(state, memory, instruction, address, AluKind::Cmp),
        Mnemonic::Test => binary_alu(state, memory, instruction, address, AluKind::Test),
        Mnemonic::Inc => step_by_one(state, memory, instruction, address, true),
        Mnemonic::Dec => step_by_one(state, memory, instruction, address, false),
        Mnemonic::Neg => neg(state, memory, instruction, address),
        Mnemonic::Not => not(state, memory, instruction, address),
        Mnemonic::Shl | Mnemonic::Sal => shift(state, memory, instruction, address, ShiftKind::Shl),
        Mnemonic::Shr => shift(state, memory, instruction, address, ShiftKind::Shr),
        Mnemonic::Sar => shift(state, memory, instruction, address, ShiftKind::Sar),
        Mnemonic::Rol => shift(state, memory, instruction, address, ShiftKind::Rol),
        Mnemonic::Ror => shift(state, memory, instruction, address, ShiftKind::Ror),
        Mnemonic::Rcl => shift(state, memory, instruction, address, ShiftKind::Rcl),
        Mnemonic::Rcr => shift(state, memory, instruction, address, ShiftKind::Rcr),
        Mnemonic::Shld => double_shift(state, memory, instruction, address, true),
        Mnemonic::Shrd => double_shift(state, memory, instruction, address, false),
        Mnemonic::Mul => mul(state, memory, instruction, address),
        Mnemonic::Imul => imul(state, memory, instruction, address),
        Mnemonic::Div => div(state, memory, instruction, address, false),
        Mnemonic::Idiv => div(state, memory, instruction, address, true),
        Mnemonic::Bswap => bswap(state, instruction, address),
        Mnemonic::Bt => bit_test(state, memory, instruction, address, BitOp::Test),
        Mnemonic::Bts => bit_test(state, memory, instruction, address, BitOp::Set),
        Mnemonic::Btr => bit_test(state, memory, instruction, address, BitOp::Reset),
        Mnemonic::Btc => bit_test(state, memory, instruction, address, BitOp::Complement),
        Mnemonic::Bsf => bit_scan(state, memory, instruction, address, true),
        Mnemonic::Bsr => bit_scan(state, memory, instruction, address, false),
        Mnemonic::Cbw => {
            let value = i32::from(state.gpr[0] as u8 as i8) as u32;
            state.gpr[0] = (state.gpr[0] & 0xFFFF_0000) | (value & 0xFFFF);
            Ok(())
        }
        Mnemonic::Cwde => {
            state.gpr[0] = i32::from(state.gpr[0] as u16 as i16) as u32;
            Ok(())
        }
        Mnemonic::Cwd => {
            let fill = if state.gpr[0] & 0x8000 != 0 { 0xFFFF } else { 0 };
            state.gpr[2] = (state.gpr[2] & 0xFFFF_0000) | fill;
            Ok(())
        }
        Mnemonic::Cdq => {
            state.gpr[2] = if state.gpr[0] & 0x8000_0000 != 0 { 0xFFFF_FFFF } else { 0 };
            Ok(())
        }
        Mnemonic::Clc => {
            state.eflags &= !flags::CARRY;
            Ok(())
        }
        Mnemonic::Stc => {
            state.eflags |= flags::CARRY;
            Ok(())
        }
        Mnemonic::Cmc => {
            state.eflags ^= flags::CARRY;
            Ok(())
        }
        Mnemonic::Cld => {
            state.eflags &= !flags::DIRECTION;
            Ok(())
        }
        Mnemonic::Std => {
            state.eflags |= flags::DIRECTION;
            Ok(())
        }
        Mnemonic::Lahf => {
            let low = (state.eflags & 0xD5) | 0x02;
            state.gpr[0] = (state.gpr[0] & 0xFFFF_00FF) | (low << 8);
            Ok(())
        }
        Mnemonic::Sahf => {
            let ah = (state.gpr[0] >> 8) & 0xFF;
            state.eflags = (state.eflags & !0xD5) | (ah & 0xD5) | 0x02;
            Ok(())
        }
        Mnemonic::Push => {
            let (width, value) = match instruction.op_kind(0) {
                OpKind::Immediate8to32 => (Width::D, instruction.immediate8to32() as u32),
                OpKind::Immediate32 => (Width::D, instruction.immediate32()),
                OpKind::Immediate8to16 => {
                    (Width::W, u32::from(instruction.immediate8to16() as u16))
                }
                OpKind::Immediate16 => (Width::W, u32::from(instruction.immediate16())),
                _ => {
                    let width = operand_width(instruction, 0, address)?;
                    let source = operand(state, instruction, 0, address)?;
                    // `push esp` reads the value before the decrement.
                    (width, read_operand(state, memory, source, width)?)
                }
            };
            push_value(state, memory, width, value)
        }
        Mnemonic::Pop => {
            let width = operand_width(instruction, 0, address)?;
            let value = read_memory(memory, GuestVa(state.gpr[4]), width)?;
            let old_esp = state.gpr[4];
            // The increment commits before the destination resolves, so an
            // ESP-relative memory destination sees the new stack pointer;
            // `pop esp` then overwrites the increment with the loaded value.
            state.gpr[4] = old_esp.wrapping_add(width.bytes() as u32);
            let dst = operand(state, instruction, 0, address)
                .and_then(|dst| write_operand(state, memory, dst, width, value));
            if let Err(error) = dst {
                state.gpr[4] = old_esp;
                return Err(error);
            }
            Ok(())
        }
        Mnemonic::Pushad => {
            let esp = state.gpr[4];
            let mut frame = [0_u8; 32];
            for (slot, value) in frame.chunks_exact_mut(4).zip(
                [
                    state.gpr[7],
                    state.gpr[6],
                    state.gpr[5],
                    esp,
                    state.gpr[3],
                    state.gpr[2],
                    state.gpr[1],
                    state.gpr[0],
                ]
                .iter(),
            ) {
                slot.copy_from_slice(&value.to_le_bytes());
            }
            // One 32-byte write keeps the eight pushes all-or-nothing.
            let base = esp.wrapping_sub(32);
            memory.write(GuestVa(base), &frame)?;
            state.gpr[4] = base;
            Ok(())
        }
        Mnemonic::Popad => {
            let mut frame = [0_u8; 32];
            memory.read(GuestVa(state.gpr[4]), &mut frame)?;
            let value = |slot: usize| {
                u32::from_le_bytes([
                    frame[slot * 4],
                    frame[slot * 4 + 1],
                    frame[slot * 4 + 2],
                    frame[slot * 4 + 3],
                ])
            };
            state.gpr[7] = value(0);
            state.gpr[6] = value(1);
            state.gpr[5] = value(2);
            // The stored ESP image is discarded.
            state.gpr[3] = value(4);
            state.gpr[2] = value(5);
            state.gpr[1] = value(6);
            state.gpr[0] = value(7);
            state.gpr[4] = state.gpr[4].wrapping_add(32);
            Ok(())
        }
        Mnemonic::Movsb => {
            string_op(state, memory, instruction, address, StringKind::Movs, Width::B)
        }
        Mnemonic::Movsw => {
            string_op(state, memory, instruction, address, StringKind::Movs, Width::W)
        }
        Mnemonic::Movsd => {
            string_op(state, memory, instruction, address, StringKind::Movs, Width::D)
        }
        Mnemonic::Stosb => {
            string_op(state, memory, instruction, address, StringKind::Stos, Width::B)
        }
        Mnemonic::Stosw => {
            string_op(state, memory, instruction, address, StringKind::Stos, Width::W)
        }
        Mnemonic::Stosd => {
            string_op(state, memory, instruction, address, StringKind::Stos, Width::D)
        }
        Mnemonic::Lodsb => {
            string_op(state, memory, instruction, address, StringKind::Lods, Width::B)
        }
        Mnemonic::Lodsw => {
            string_op(state, memory, instruction, address, StringKind::Lods, Width::W)
        }
        Mnemonic::Lodsd => {
            string_op(state, memory, instruction, address, StringKind::Lods, Width::D)
        }
        Mnemonic::Scasb => {
            string_op(state, memory, instruction, address, StringKind::Scas, Width::B)
        }
        Mnemonic::Scasw => {
            string_op(state, memory, instruction, address, StringKind::Scas, Width::W)
        }
        Mnemonic::Scasd => {
            string_op(state, memory, instruction, address, StringKind::Scas, Width::D)
        }
        Mnemonic::Cmpsb => {
            string_op(state, memory, instruction, address, StringKind::Cmps, Width::B)
        }
        Mnemonic::Cmpsw => {
            string_op(state, memory, instruction, address, StringKind::Cmps, Width::W)
        }
        Mnemonic::Cmpsd => {
            string_op(state, memory, instruction, address, StringKind::Cmps, Width::D)
        }
        Mnemonic::Cpuid => {
            cpuid(state);
            Ok(())
        }
        Mnemonic::Rdtsc => {
            state.gpr[0] = state.tsc as u32;
            state.gpr[2] = (state.tsc >> 32) as u32;
            Ok(())
        }
        Mnemonic::Pushfd => push_value(state, memory, Width::D, state.eflags),
        Mnemonic::Popfd => {
            const WRITABLE: u32 = flags::ARITHMETIC
                | flags::DIRECTION
                | flags::INTERRUPT
                | flags::ALIGNMENT_CHECK
                | flags::ID;
            let value = pop_value(state, memory, Width::D)?;
            state.eflags = (value & WRITABLE) | (state.eflags & !WRITABLE) | 0x02;
            Ok(())
        }
        Mnemonic::Fldcw => {
            let source = operand(state, instruction, 0, address)?;
            let value = read_operand(state, memory, source, Width::W)?;
            state.x87.control = value as u16;
            Ok(())
        }
        Mnemonic::Wait => Ok(()),
        Mnemonic::Leave => {
            // Only the 32-bit form; `66 C9` pops 2 bytes into BP on hardware.
            if instruction.code() != Code::Leaved {
                return Err(ExecError::Unsupported { address });
            }
            let frame = state.gpr[5];
            let value = read_memory(memory, GuestVa(frame), Width::D)?;
            state.gpr[4] = frame.wrapping_add(4);
            state.gpr[5] = value;
            Ok(())
        }
        _ => {
            if instruction.condition_code() != ConditionCode::None {
                return conditional(state, memory, instruction, address);
            }
            Err(ExecError::Unsupported { address })
        }
    }
}

/// Handles `SETcc` and `CMOVcc` (every other conditional mnemonic is
/// control flow and stays unsupported in this stage).
fn conditional(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<(), ExecError> {
    let met = condition_met(state.eflags, instruction.condition_code());
    match mnemonic_class(instruction.mnemonic()) {
        Some(ConditionalClass::Set) => {
            let dst = operand(state, instruction, 0, address)?;
            write_operand(state, memory, dst, Width::B, u32::from(met))
        }
        Some(ConditionalClass::Mov) => {
            let width = operand_width(instruction, 0, address)?;
            let src = operand(state, instruction, 1, address)?;
            // Hardware reads the source even when the condition fails.
            let value = read_operand(state, memory, src, width)?;
            if met {
                let dst = operand(state, instruction, 0, address)?;
                write_operand(state, memory, dst, width, value)?;
            }
            Ok(())
        }
        None => Err(ExecError::Unsupported { address }),
    }
}

enum ConditionalClass {
    Set,
    Mov,
}

fn mnemonic_class(mnemonic: Mnemonic) -> Option<ConditionalClass> {
    match mnemonic {
        Mnemonic::Seta
        | Mnemonic::Setae
        | Mnemonic::Setb
        | Mnemonic::Setbe
        | Mnemonic::Sete
        | Mnemonic::Setg
        | Mnemonic::Setge
        | Mnemonic::Setl
        | Mnemonic::Setle
        | Mnemonic::Setne
        | Mnemonic::Setno
        | Mnemonic::Setnp
        | Mnemonic::Setns
        | Mnemonic::Seto
        | Mnemonic::Setp
        | Mnemonic::Sets => Some(ConditionalClass::Set),
        Mnemonic::Cmova
        | Mnemonic::Cmovae
        | Mnemonic::Cmovb
        | Mnemonic::Cmovbe
        | Mnemonic::Cmove
        | Mnemonic::Cmovg
        | Mnemonic::Cmovge
        | Mnemonic::Cmovl
        | Mnemonic::Cmovle
        | Mnemonic::Cmovne
        | Mnemonic::Cmovno
        | Mnemonic::Cmovnp
        | Mnemonic::Cmovns
        | Mnemonic::Cmovo
        | Mnemonic::Cmovp
        | Mnemonic::Cmovs => Some(ConditionalClass::Mov),
        _ => None,
    }
}

fn condition_met(eflags: u32, code: ConditionCode) -> bool {
    let carry = eflags & flags::CARRY != 0;
    let zero = eflags & flags::ZERO != 0;
    let sign = eflags & flags::SIGN != 0;
    let overflow = eflags & flags::OVERFLOW != 0;
    let parity = eflags & flags::PARITY != 0;
    match code {
        ConditionCode::None => false,
        ConditionCode::o => overflow,
        ConditionCode::no => !overflow,
        ConditionCode::b => carry,
        ConditionCode::ae => !carry,
        ConditionCode::e => zero,
        ConditionCode::ne => !zero,
        ConditionCode::be => carry || zero,
        ConditionCode::a => !carry && !zero,
        ConditionCode::s => sign,
        ConditionCode::ns => !sign,
        ConditionCode::p => parity,
        ConditionCode::np => !parity,
        ConditionCode::l => sign != overflow,
        ConditionCode::ge => sign == overflow,
        ConditionCode::le => zero || sign != overflow,
        ConditionCode::g => !zero && sign == overflow,
    }
}

// --- operand resolution -------------------------------------------------

fn operand(
    state: &CpuState,
    instruction: &Instruction,
    index: u32,
    address: GuestVa,
) -> Result<Operand, ExecError> {
    match instruction.op_kind(index) {
        OpKind::Register => Ok(Operand::Register(instruction.op_register(index))),
        OpKind::Memory => {
            Ok(Operand::Memory(effective_address(state, instruction, address, true)?))
        }
        OpKind::Immediate8 => Ok(Operand::Immediate(u32::from(instruction.immediate8()))),
        OpKind::Immediate8to16 => {
            Ok(Operand::Immediate(u32::from(instruction.immediate8to16() as u16)))
        }
        OpKind::Immediate8to32 => Ok(Operand::Immediate(instruction.immediate8to32() as u32)),
        OpKind::Immediate16 => Ok(Operand::Immediate(u32::from(instruction.immediate16()))),
        OpKind::Immediate32 => Ok(Operand::Immediate(instruction.immediate32())),
        _ => Err(ExecError::Unsupported { address }),
    }
}

/// Computes one 32-bit effective address with wrapping arithmetic.
///
/// The cached segment base participates unless the caller is `lea`, which
/// architecturally produces the raw offset.
/// The guest memory an instruction executes against.
pub(crate) type GuestMemoryRef<'a> = &'a dyn GuestMemory;

/// One memory operand's effective address, with its segment applied.
pub(crate) fn effective_address_for(
    state: &CpuState,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<GuestVa, ExecError> {
    effective_address(state, instruction, address, true)
}

fn effective_address(
    state: &CpuState,
    instruction: &Instruction,
    address: GuestVa,
    with_segment: bool,
) -> Result<GuestVa, ExecError> {
    let base = instruction.memory_base();
    let index = instruction.memory_index();
    // 16-bit addressing forms never appear in 32-bit XDK code.
    if base != Register::None && base.size() != 4 {
        return Err(ExecError::Unsupported { address });
    }
    if index != Register::None && index.size() != 4 {
        return Err(ExecError::Unsupported { address });
    }

    let mut offset = instruction.memory_displacement32();
    if base != Register::None {
        offset = offset.wrapping_add(register_value(state, base, address)?);
    }
    if index != Register::None {
        let scaled =
            register_value(state, index, address)?.wrapping_mul(instruction.memory_index_scale());
        offset = offset.wrapping_add(scaled);
    }
    if with_segment {
        offset = offset.wrapping_add(segment_base(state, instruction.memory_segment(), address)?);
    }
    Ok(GuestVa(offset))
}

fn segment_base(state: &CpuState, segment: Register, address: GuestVa) -> Result<u32, ExecError> {
    let segment = match segment {
        Register::ES => Segment::Es,
        Register::CS => Segment::Cs,
        Register::SS => Segment::Ss,
        Register::DS => Segment::Ds,
        Register::FS => Segment::Fs,
        Register::GS => Segment::Gs,
        _ => return Err(ExecError::Unsupported { address }),
    };
    Ok(state.segment(segment).base)
}

/// Reads one 32-bit general-purpose register for address generation.
fn register_value(
    state: &CpuState,
    register: Register,
    address: GuestVa,
) -> Result<u32, ExecError> {
    gpr_index(register).map(|index| state.gpr[index]).ok_or(ExecError::Unsupported { address })
}

const fn gpr_index(register: Register) -> Option<usize> {
    match register {
        Register::EAX => Some(0),
        Register::ECX => Some(1),
        Register::EDX => Some(2),
        Register::EBX => Some(3),
        Register::ESP => Some(4),
        Register::EBP => Some(5),
        Register::ESI => Some(6),
        Register::EDI => Some(7),
        _ => None,
    }
}

/// How a register name maps onto the 32-bit register file.
enum RegisterSlot {
    Low8(usize),
    High8(usize),
    Wide16(usize),
    Wide32(usize),
}

const fn register_slot(register: Register) -> Option<RegisterSlot> {
    match register {
        Register::AL => Some(RegisterSlot::Low8(0)),
        Register::CL => Some(RegisterSlot::Low8(1)),
        Register::DL => Some(RegisterSlot::Low8(2)),
        Register::BL => Some(RegisterSlot::Low8(3)),
        Register::AH => Some(RegisterSlot::High8(0)),
        Register::CH => Some(RegisterSlot::High8(1)),
        Register::DH => Some(RegisterSlot::High8(2)),
        Register::BH => Some(RegisterSlot::High8(3)),
        Register::AX => Some(RegisterSlot::Wide16(0)),
        Register::CX => Some(RegisterSlot::Wide16(1)),
        Register::DX => Some(RegisterSlot::Wide16(2)),
        Register::BX => Some(RegisterSlot::Wide16(3)),
        Register::SP => Some(RegisterSlot::Wide16(4)),
        Register::BP => Some(RegisterSlot::Wide16(5)),
        Register::SI => Some(RegisterSlot::Wide16(6)),
        Register::DI => Some(RegisterSlot::Wide16(7)),
        Register::EAX => Some(RegisterSlot::Wide32(0)),
        Register::ECX => Some(RegisterSlot::Wide32(1)),
        Register::EDX => Some(RegisterSlot::Wide32(2)),
        Register::EBX => Some(RegisterSlot::Wide32(3)),
        Register::ESP => Some(RegisterSlot::Wide32(4)),
        Register::EBP => Some(RegisterSlot::Wide32(5)),
        Register::ESI => Some(RegisterSlot::Wide32(6)),
        Register::EDI => Some(RegisterSlot::Wide32(7)),
        _ => None,
    }
}

fn read_register(state: &CpuState, register: Register, address: GuestVa) -> Result<u32, ExecError> {
    match register_slot(register).ok_or(ExecError::Unsupported { address })? {
        RegisterSlot::Low8(index) => Ok(state.gpr[index] & 0xFF),
        RegisterSlot::High8(index) => Ok((state.gpr[index] >> 8) & 0xFF),
        RegisterSlot::Wide16(index) => Ok(state.gpr[index] & 0xFFFF),
        RegisterSlot::Wide32(index) => Ok(state.gpr[index]),
    }
}

fn write_register(
    state: &mut CpuState,
    register: Register,
    value: u32,
    address: GuestVa,
) -> Result<(), ExecError> {
    match register_slot(register).ok_or(ExecError::Unsupported { address })? {
        RegisterSlot::Low8(index) => {
            state.gpr[index] = (state.gpr[index] & 0xFFFF_FF00) | (value & 0xFF);
        }
        RegisterSlot::High8(index) => {
            state.gpr[index] = (state.gpr[index] & 0xFFFF_00FF) | ((value & 0xFF) << 8);
        }
        RegisterSlot::Wide16(index) => {
            state.gpr[index] = (state.gpr[index] & 0xFFFF_0000) | (value & 0xFFFF);
        }
        RegisterSlot::Wide32(index) => state.gpr[index] = value,
    }
    Ok(())
}

fn operand_width(
    instruction: &Instruction,
    index: u32,
    address: GuestVa,
) -> Result<Width, ExecError> {
    match instruction.op_kind(index) {
        OpKind::Register => Width::from_bytes(instruction.op_register(index).size())
            .ok_or(ExecError::Unsupported { address }),
        OpKind::Memory => Width::from_bytes(instruction.memory_size().size())
            .ok_or(ExecError::Unsupported { address }),
        _ => Err(ExecError::Unsupported { address }),
    }
}

fn read_operand(
    state: &CpuState,
    memory: &dyn GuestMemory,
    operand: Operand,
    width: Width,
) -> Result<u32, ExecError> {
    match operand {
        Operand::Register(register) => read_register(state, register, GuestVa(state.eip)),
        Operand::Memory(location) => Ok(read_memory(memory, location, width)?),
        Operand::Immediate(value) => Ok(value & width.mask()),
    }
}

fn write_operand(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    operand: Operand,
    width: Width,
    value: u32,
) -> Result<(), ExecError> {
    match operand {
        Operand::Register(register) => {
            write_register(state, register, value & width.mask(), GuestVa(state.eip))
        }
        Operand::Memory(location) => Ok(write_memory(memory, location, width, value)?),
        Operand::Immediate(_) => Err(ExecError::Unsupported { address: GuestVa(state.eip) }),
    }
}

fn read_memory(
    memory: &dyn GuestMemory,
    location: GuestVa,
    width: Width,
) -> Result<u32, MemoryError> {
    let mut bytes = [0_u8; 4];
    memory.read(location, &mut bytes[..width.bytes()])?;
    Ok(u32::from_le_bytes(bytes))
}

fn write_memory(
    memory: &dyn GuestMemory,
    location: GuestVa,
    width: Width,
    value: u32,
) -> Result<(), MemoryError> {
    memory.write(location, &value.to_le_bytes()[..width.bytes()])
}

// --- flag computation ---------------------------------------------------

/// Merges the defined flag bits and preserves the rest.
fn merge_flags(state: &mut CpuState, defined: u32, values: u32) {
    state.eflags = (state.eflags & !defined) | (values & defined);
}

fn result_bits(result: u32, width: Width) -> u32 {
    let masked = result & width.mask();
    let mut bits = 0;
    if masked == 0 {
        bits |= flags::ZERO;
    }
    if masked & width.sign_bit() != 0 {
        bits |= flags::SIGN;
    }
    if (masked as u8).count_ones().is_multiple_of(2) {
        bits |= flags::PARITY;
    }
    bits
}

fn add_bits(a: u32, b: u32, carry_in: u32, width: Width) -> (u32, u32) {
    let mask = width.mask();
    let (a, b) = (a & mask, b & mask);
    let wide = u64::from(a) + u64::from(b) + u64::from(carry_in);
    let result = (wide as u32) & mask;
    let mut bits = result_bits(result, width);
    if wide > u64::from(mask) {
        bits |= flags::CARRY;
    }
    if (a ^ b ^ result) & 0x10 != 0 {
        bits |= flags::AUXILIARY;
    }
    if (a ^ result) & (b ^ result) & width.sign_bit() != 0 {
        bits |= flags::OVERFLOW;
    }
    (result, bits)
}

fn sub_bits(a: u32, b: u32, borrow_in: u32, width: Width) -> (u32, u32) {
    let mask = width.mask();
    let (a, b) = (a & mask, b & mask);
    let result = a.wrapping_sub(b).wrapping_sub(borrow_in) & mask;
    let mut bits = result_bits(result, width);
    if u64::from(a) < u64::from(b) + u64::from(borrow_in) {
        bits |= flags::CARRY;
    }
    if (a ^ b ^ result) & 0x10 != 0 {
        bits |= flags::AUXILIARY;
    }
    if (a ^ b) & (a ^ result) & width.sign_bit() != 0 {
        bits |= flags::OVERFLOW;
    }
    (result, bits)
}

fn logic_bits(result: u32, width: Width) -> u32 {
    // Carry and overflow clear; auxiliary carry stays undefined (preserved).
    result_bits(result, width)
}

// --- instruction families -----------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AluKind {
    Add,
    Adc,
    Sub,
    Sbb,
    And,
    Or,
    Xor,
    Cmp,
    Test,
}

fn binary_alu(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
    kind: AluKind,
) -> Result<(), ExecError> {
    let width = operand_width(instruction, 0, address)?;
    let dst = operand(state, instruction, 0, address)?;
    let src = operand(state, instruction, 1, address)?;
    let a = read_operand(state, memory, dst, width)?;
    let b = read_operand(state, memory, src, width)?;
    let carry = u32::from(state.eflags & flags::CARRY != 0);

    let (result, defined, bits, writes) = match kind {
        AluKind::Add => {
            let (result, bits) = add_bits(a, b, 0, width);
            (result, flags::ARITHMETIC, bits, true)
        }
        AluKind::Adc => {
            let (result, bits) = add_bits(a, b, carry, width);
            (result, flags::ARITHMETIC, bits, true)
        }
        AluKind::Sub => {
            let (result, bits) = sub_bits(a, b, 0, width);
            (result, flags::ARITHMETIC, bits, true)
        }
        AluKind::Sbb => {
            let (result, bits) = sub_bits(a, b, carry, width);
            (result, flags::ARITHMETIC, bits, true)
        }
        AluKind::And => (a & b, flags::LOGIC_DEFINED, logic_bits(a & b, width), true),
        AluKind::Or => (a | b, flags::LOGIC_DEFINED, logic_bits(a | b, width), true),
        AluKind::Xor => (a ^ b, flags::LOGIC_DEFINED, logic_bits(a ^ b, width), true),
        AluKind::Cmp => {
            let (result, bits) = sub_bits(a, b, 0, width);
            (result, flags::ARITHMETIC, bits, false)
        }
        AluKind::Test => (a & b, flags::LOGIC_DEFINED, logic_bits(a & b, width), false),
    };

    if writes {
        write_operand(state, memory, dst, width, result)?;
    }
    merge_flags(state, defined, bits);
    Ok(())
}

fn mov(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<(), ExecError> {
    let width = operand_width(instruction, 0, address)?;
    let src = operand(state, instruction, 1, address)?;
    let value = read_operand(state, memory, src, width)?;
    let dst = operand(state, instruction, 0, address)?;
    write_operand(state, memory, dst, width, value)
}

fn widen(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
    sign_extend: bool,
) -> Result<(), ExecError> {
    let dst_width = operand_width(instruction, 0, address)?;
    let src_width = operand_width(instruction, 1, address)?;
    let src = operand(state, instruction, 1, address)?;
    let raw = read_operand(state, memory, src, src_width)? & src_width.mask();
    let value = if sign_extend {
        match src_width {
            Width::B => i32::from(raw as u8 as i8) as u32,
            Width::W => i32::from(raw as u16 as i16) as u32,
            Width::D => raw,
        }
    } else {
        raw
    };
    let dst = operand(state, instruction, 0, address)?;
    write_operand(state, memory, dst, dst_width, value)
}

fn lea(state: &mut CpuState, instruction: &Instruction, address: GuestVa) -> Result<(), ExecError> {
    if instruction.op_kind(0) != OpKind::Register || instruction.op_kind(1) != OpKind::Memory {
        return Err(ExecError::Unsupported { address });
    }
    let register = instruction.op_register(0);
    let width = Width::from_bytes(register.size()).ok_or(ExecError::Unsupported { address })?;
    let offset = effective_address(state, instruction, address, false)?.0;
    write_register(state, register, offset & width.mask(), address)
}

fn xchg(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<(), ExecError> {
    let width = operand_width(instruction, 0, address)?;
    let first = operand(state, instruction, 0, address)?;
    let second = operand(state, instruction, 1, address)?;
    let a = read_operand(state, memory, first, width)?;
    let b = read_operand(state, memory, second, width)?;
    // The memory write happens before any register mutation so a fault
    // leaves state untouched; at most one operand is memory.
    write_operand(state, memory, first, width, b)?;
    write_operand(state, memory, second, width, a)
}

fn xadd(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<(), ExecError> {
    let width = operand_width(instruction, 0, address)?;
    let dst = operand(state, instruction, 0, address)?;
    let src = operand(state, instruction, 1, address)?;
    let a = read_operand(state, memory, dst, width)?;
    let b = read_operand(state, memory, src, width)?;
    let (sum, bits) = add_bits(a, b, 0, width);
    // The destination is architecturally written last (TEMP := SRC + DEST;
    // SRC := DEST; DEST := TEMP), which matters when both operands name the
    // same register. A memory destination cannot alias the register source,
    // so it is written first to keep faults free of side effects.
    match dst {
        Operand::Memory(_) => {
            write_operand(state, memory, dst, width, sum)?;
            write_operand(state, memory, src, width, a)?;
        }
        _ => {
            write_operand(state, memory, src, width, a)?;
            write_operand(state, memory, dst, width, sum)?;
        }
    }
    merge_flags(state, flags::ARITHMETIC, bits);
    Ok(())
}

fn cmpxchg(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<(), ExecError> {
    let width = operand_width(instruction, 0, address)?;
    let accumulator_register = match width {
        Width::B => Register::AL,
        Width::W => Register::AX,
        Width::D => Register::EAX,
    };
    let dst = operand(state, instruction, 0, address)?;
    let src = operand(state, instruction, 1, address)?;
    let current = read_operand(state, memory, dst, width)?;
    let accumulator = read_register(state, accumulator_register, address)?;
    let (_, bits) = sub_bits(accumulator, current, 0, width);
    if accumulator == current {
        let replacement = read_operand(state, memory, src, width)?;
        write_operand(state, memory, dst, width, replacement)?;
    } else {
        // Hardware writes the destination in both outcomes (the mismatch
        // path stores the old value back), so a read-only destination
        // faults regardless of the comparison.
        write_operand(state, memory, dst, width, current)?;
        write_register(state, accumulator_register, current, address)?;
    }
    merge_flags(state, flags::ARITHMETIC, bits);
    Ok(())
}

fn cmpxchg8b(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<(), ExecError> {
    if instruction.op_kind(0) != OpKind::Memory {
        return Err(ExecError::Unsupported { address });
    }
    let location = effective_address(state, instruction, address, true)?;
    // The 8-byte compare-exchange is a single access in both directions so a
    // page-straddling fault commits nothing; the address space validates the
    // whole range before copying.
    let mut current = [0_u8; 8];
    memory.read(location, &mut current)?;
    let low = u32::from_le_bytes([current[0], current[1], current[2], current[3]]);
    let high = u32::from_le_bytes([current[4], current[5], current[6], current[7]]);
    if low == state.gpr[0] && high == state.gpr[2] {
        let mut replacement = [0_u8; 8];
        replacement[..4].copy_from_slice(&state.gpr[3].to_le_bytes());
        replacement[4..].copy_from_slice(&state.gpr[1].to_le_bytes());
        memory.write(location, &replacement)?;
        state.eflags |= flags::ZERO;
    } else {
        // The mismatch path also writes the destination (the old value),
        // matching the hardware requirement for a writable operand.
        memory.write(location, &current)?;
        state.gpr[0] = low;
        state.gpr[2] = high;
        state.eflags &= !flags::ZERO;
    }
    Ok(())
}

fn step_by_one(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
    increment: bool,
) -> Result<(), ExecError> {
    let width = operand_width(instruction, 0, address)?;
    let dst = operand(state, instruction, 0, address)?;
    let a = read_operand(state, memory, dst, width)?;
    let (result, bits) =
        if increment { add_bits(a, 1, 0, width) } else { sub_bits(a, 1, 0, width) };
    write_operand(state, memory, dst, width, result)?;
    // INC and DEC leave the carry flag unchanged.
    merge_flags(state, flags::ARITHMETIC & !flags::CARRY, bits);
    Ok(())
}

fn neg(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<(), ExecError> {
    let width = operand_width(instruction, 0, address)?;
    let dst = operand(state, instruction, 0, address)?;
    let a = read_operand(state, memory, dst, width)?;
    let (result, bits) = sub_bits(0, a, 0, width);
    write_operand(state, memory, dst, width, result)?;
    merge_flags(state, flags::ARITHMETIC, bits);
    Ok(())
}

fn not(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<(), ExecError> {
    let width = operand_width(instruction, 0, address)?;
    let dst = operand(state, instruction, 0, address)?;
    let a = read_operand(state, memory, dst, width)?;
    write_operand(state, memory, dst, width, !a)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShiftKind {
    Shl,
    Shr,
    Sar,
    Rol,
    Ror,
    Rcl,
    Rcr,
}

fn shift_count(
    state: &CpuState,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<u32, ExecError> {
    match instruction.op_kind(1) {
        OpKind::Immediate8 => Ok(u32::from(instruction.immediate8()) & 0x1F),
        OpKind::Register if instruction.op_register(1) == Register::CL => Ok(state.gpr[1] & 0x1F),
        _ => Err(ExecError::Unsupported { address }),
    }
}

fn shift(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
    kind: ShiftKind,
) -> Result<(), ExecError> {
    let width = operand_width(instruction, 0, address)?;
    let count = shift_count(state, instruction, address)?;
    let dst = operand(state, instruction, 0, address)?;
    let a = read_operand(state, memory, dst, width)? & width.mask();
    if count == 0 {
        // A masked count of zero changes neither the value nor the flags,
        // but the read-modify-write access still happens on hardware.
        write_operand(state, memory, dst, width, a)?;
        return Ok(());
    }
    let bits = width.bits();
    let mask = width.mask();
    let carry_in = u32::from(state.eflags & flags::CARRY != 0);

    let (result, carry_out, overflow) = match kind {
        ShiftKind::Shl => {
            let wide = u64::from(a) << count;
            let result = (wide as u32) & mask;
            let carry = ((wide >> bits) & 1) as u32;
            (result, carry, (carry ^ u32::from(result & width.sign_bit() != 0)) != 0)
        }
        ShiftKind::Shr => {
            let result = if count >= 32 { 0 } else { (a >> count) & mask };
            let carry = if count > 32 { 0 } else { (a >> (count - 1)) & 1 };
            // Count-one overflow is the original sign bit.
            (result, carry, a & width.sign_bit() != 0)
        }
        ShiftKind::Sar => {
            let extended = i64::from(sign_extend(a, width));
            let result = ((extended >> count.min(63)) as u32) & mask;
            let carry = ((extended >> (count - 1).min(63)) & 1) as u32;
            (result, carry, false)
        }
        ShiftKind::Rol => {
            let rotation = count % bits;
            let result = ((a << rotation) | a.checked_shr(bits - rotation).unwrap_or(0)) & mask;
            let carry = result & 1;
            (result, carry, (carry ^ u32::from(result & width.sign_bit() != 0)) != 0)
        }
        ShiftKind::Ror => {
            let rotation = count % bits;
            let result = ((a >> rotation) | a.checked_shl(bits - rotation).unwrap_or(0)) & mask;
            let carry = u32::from(result & width.sign_bit() != 0);
            let next = u32::from(result & (width.sign_bit() >> 1) != 0);
            (result, carry, (carry ^ next) != 0)
        }
        ShiftKind::Rcl => {
            let rotation = count % (bits + 1);
            let mut value = a;
            let mut carry = carry_in;
            for _ in 0..rotation {
                let out = u32::from(value & width.sign_bit() != 0);
                value = ((value << 1) | carry) & mask;
                carry = out;
            }
            (value, carry, (carry ^ u32::from(value & width.sign_bit() != 0)) != 0)
        }
        ShiftKind::Rcr => {
            // Count-one overflow uses the state before the rotation.
            let overflow = (carry_in ^ u32::from(a & width.sign_bit() != 0)) != 0;
            let rotation = count % (bits + 1);
            let mut value = a;
            let mut carry = carry_in;
            for _ in 0..rotation {
                let out = value & 1;
                value = (value >> 1) | (carry << (bits - 1));
                carry = out;
            }
            (value, carry, overflow)
        }
    };

    write_operand(state, memory, dst, width, result)?;
    let mut defined;
    let mut values;
    match kind {
        ShiftKind::Rol | ShiftKind::Ror | ShiftKind::Rcl | ShiftKind::Rcr => {
            // Rotates define only the carry flag (plus overflow at count one).
            defined = flags::CARRY;
            values = carry_out * flags::CARRY;
        }
        _ => {
            // Shifts define carry, sign, zero, and parity; auxiliary carry
            // stays undefined and keeps the previous guest value. Carry is
            // also undefined once the count reaches the operand width for
            // SHL and SHR, so those counts preserve it too.
            defined = flags::CARRY | flags::SIGN | flags::ZERO | flags::PARITY;
            if count >= width.bits() && matches!(kind, ShiftKind::Shl | ShiftKind::Shr) {
                defined &= !flags::CARRY;
            }
            values = result_bits(result, width) | (carry_out * flags::CARRY);
        }
    }
    if count == 1 {
        defined |= flags::OVERFLOW;
        values |= u32::from(overflow) * flags::OVERFLOW;
    }
    merge_flags(state, defined, values);
    Ok(())
}

fn double_shift(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
    left: bool,
) -> Result<(), ExecError> {
    let width = operand_width(instruction, 0, address)?;
    if width == Width::B {
        return Err(ExecError::Unsupported { address });
    }
    let count = match instruction.op_kind(2) {
        OpKind::Immediate8 => u32::from(instruction.immediate8()) & 0x1F,
        OpKind::Register if instruction.op_register(2) == Register::CL => state.gpr[1] & 0x1F,
        _ => return Err(ExecError::Unsupported { address }),
    };
    let dst = operand(state, instruction, 0, address)?;
    let src = operand(state, instruction, 1, address)?;
    let a = u128::from(read_operand(state, memory, dst, width)? & width.mask());
    let b = u128::from(read_operand(state, memory, src, width)? & width.mask());
    if count == 0 {
        // The read-modify-write access still happens for a zero count.
        write_operand(state, memory, dst, width, a as u32)?;
        return Ok(());
    }
    let bits = width.bits();
    let mask = width.mask();

    let (result, carry) = if left {
        let wide = (a << bits) | b;
        let shifted = wide << count;
        (((shifted >> bits) as u32) & mask, ((shifted >> (2 * bits)) & 1) as u32)
    } else {
        let wide = (b << bits) | a;
        (((wide >> count) as u32) & mask, ((wide >> (count - 1)) & 1) as u32)
    };

    write_operand(state, memory, dst, width, result)?;
    let mut defined = flags::CARRY | flags::SIGN | flags::ZERO | flags::PARITY;
    let mut values = result_bits(result, width) | (carry * flags::CARRY);
    if count == 1 {
        defined |= flags::OVERFLOW;
        let sign_changed = (a as u32 ^ result) & width.sign_bit() != 0;
        values |= u32::from(sign_changed) * flags::OVERFLOW;
    }
    merge_flags(state, defined, values);
    Ok(())
}

fn mul(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<(), ExecError> {
    let width = operand_width(instruction, 0, address)?;
    let src = operand(state, instruction, 0, address)?;
    let b = u64::from(read_operand(state, memory, src, width)? & width.mask());
    let overflowed;
    match width {
        Width::B => {
            let product = u64::from(state.gpr[0] & 0xFF) * b;
            state.gpr[0] = (state.gpr[0] & 0xFFFF_0000) | ((product as u32) & 0xFFFF);
            overflowed = product > 0xFF;
        }
        Width::W => {
            let product = u64::from(state.gpr[0] & 0xFFFF) * b;
            state.gpr[0] = (state.gpr[0] & 0xFFFF_0000) | ((product as u32) & 0xFFFF);
            state.gpr[2] = (state.gpr[2] & 0xFFFF_0000) | (((product >> 16) as u32) & 0xFFFF);
            overflowed = product > 0xFFFF;
        }
        Width::D => {
            let product = u64::from(state.gpr[0]) * b;
            state.gpr[0] = product as u32;
            state.gpr[2] = (product >> 32) as u32;
            overflowed = product > 0xFFFF_FFFF;
        }
    }
    let bit = u32::from(overflowed);
    merge_flags(
        state,
        flags::CARRY | flags::OVERFLOW,
        (bit * flags::CARRY) | (bit * flags::OVERFLOW),
    );
    Ok(())
}

const fn sign_extend(value: u32, width: Width) -> i32 {
    match width {
        Width::B => value as u8 as i8 as i32,
        Width::W => value as u16 as i16 as i32,
        Width::D => value as i32,
    }
}

fn imul(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<(), ExecError> {
    match instruction.code() {
        Code::Imul_rm8 | Code::Imul_rm16 | Code::Imul_rm32 => {
            let width = operand_width(instruction, 0, address)?;
            let src = operand(state, instruction, 0, address)?;
            let b = i64::from(sign_extend(read_operand(state, memory, src, width)?, width));
            let overflowed;
            match width {
                Width::B => {
                    let product = i64::from(state.gpr[0] as u8 as i8) * b;
                    state.gpr[0] = (state.gpr[0] & 0xFFFF_0000) | ((product as u32) & 0xFFFF);
                    overflowed = product != i64::from(product as i8);
                }
                Width::W => {
                    let product = i64::from(state.gpr[0] as u16 as i16) * b;
                    state.gpr[0] = (state.gpr[0] & 0xFFFF_0000) | ((product as u32) & 0xFFFF);
                    state.gpr[2] =
                        (state.gpr[2] & 0xFFFF_0000) | (((product >> 16) as u32) & 0xFFFF);
                    overflowed = product != i64::from(product as i16);
                }
                Width::D => {
                    let product = i64::from(state.gpr[0] as i32) * b;
                    state.gpr[0] = product as u32;
                    state.gpr[2] = (product >> 32) as u32;
                    overflowed = product != i64::from(product as i32);
                }
            }
            let bit = u32::from(overflowed);
            merge_flags(
                state,
                flags::CARRY | flags::OVERFLOW,
                (bit * flags::CARRY) | (bit * flags::OVERFLOW),
            );
            Ok(())
        }
        Code::Imul_r16_rm16
        | Code::Imul_r32_rm32
        | Code::Imul_r16_rm16_imm8
        | Code::Imul_r32_rm32_imm8
        | Code::Imul_r16_rm16_imm16
        | Code::Imul_r32_rm32_imm32 => {
            let width = operand_width(instruction, 0, address)?;
            let (a_index, b_index) = if instruction.op_count() == 2 { (0, 1) } else { (1, 2) };
            let a_operand = operand(state, instruction, a_index, address)?;
            let a = i64::from(sign_extend(read_operand(state, memory, a_operand, width)?, width));
            let b_operand = operand(state, instruction, b_index, address)?;
            let b = i64::from(sign_extend(read_operand(state, memory, b_operand, width)?, width));
            let product = a * b;
            let truncated = (product as u32) & width.mask();
            let overflowed = product != i64::from(sign_extend(truncated, width));
            let dst = operand(state, instruction, 0, address)?;
            write_operand(state, memory, dst, width, truncated)?;
            let bit = u32::from(overflowed);
            merge_flags(
                state,
                flags::CARRY | flags::OVERFLOW,
                (bit * flags::CARRY) | (bit * flags::OVERFLOW),
            );
            Ok(())
        }
        _ => Err(ExecError::Unsupported { address }),
    }
}

fn div(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
    signed: bool,
) -> Result<(), ExecError> {
    let width = operand_width(instruction, 0, address)?;
    let src = operand(state, instruction, 0, address)?;
    let divisor_raw = read_operand(state, memory, src, width)? & width.mask();
    if divisor_raw == 0 {
        return Err(ExecError::Divide { address });
    }

    match width {
        Width::B => {
            let dividend = state.gpr[0] & 0xFFFF;
            let (quotient, remainder) = if signed {
                let dividend = dividend as u16 as i16 as i32;
                let divisor = i32::from(divisor_raw as u8 as i8);
                let quotient = dividend.wrapping_div(divisor);
                if quotient > i32::from(i8::MAX) || quotient < i32::from(i8::MIN) {
                    return Err(ExecError::Divide { address });
                }
                ((quotient as u32) & 0xFF, (dividend.wrapping_rem(divisor) as u32) & 0xFF)
            } else {
                let quotient = dividend / divisor_raw;
                if quotient > 0xFF {
                    return Err(ExecError::Divide { address });
                }
                (quotient, dividend % divisor_raw)
            };
            state.gpr[0] = (state.gpr[0] & 0xFFFF_0000) | (remainder << 8) | quotient;
        }
        Width::W => {
            let dividend = ((state.gpr[2] & 0xFFFF) << 16) | (state.gpr[0] & 0xFFFF);
            let (quotient, remainder) = if signed {
                let dividend = dividend as i32;
                let divisor = i32::from(divisor_raw as u16 as i16);
                let quotient = dividend.wrapping_div(divisor);
                if quotient > i32::from(i16::MAX) || quotient < i32::from(i16::MIN) {
                    return Err(ExecError::Divide { address });
                }
                ((quotient as u32) & 0xFFFF, (dividend.wrapping_rem(divisor) as u32) & 0xFFFF)
            } else {
                let quotient = dividend / divisor_raw;
                if quotient > 0xFFFF {
                    return Err(ExecError::Divide { address });
                }
                (quotient, dividend % divisor_raw)
            };
            state.gpr[0] = (state.gpr[0] & 0xFFFF_0000) | quotient;
            state.gpr[2] = (state.gpr[2] & 0xFFFF_0000) | remainder;
        }
        Width::D => {
            let dividend = (u64::from(state.gpr[2]) << 32) | u64::from(state.gpr[0]);
            let (quotient, remainder) = if signed {
                let dividend = dividend as i64;
                let divisor = i64::from(divisor_raw as i32);
                let quotient = dividend.wrapping_div(divisor);
                if quotient > i64::from(i32::MAX) || quotient < i64::from(i32::MIN) {
                    return Err(ExecError::Divide { address });
                }
                (quotient as u32, dividend.wrapping_rem(divisor) as u32)
            } else {
                let divisor = u64::from(divisor_raw);
                let quotient = dividend / divisor;
                if quotient > u64::from(u32::MAX) {
                    return Err(ExecError::Divide { address });
                }
                (quotient as u32, (dividend % divisor) as u32)
            };
            state.gpr[0] = quotient;
            state.gpr[2] = remainder;
        }
    }
    // Every arithmetic flag is undefined after a divide; all are preserved.
    Ok(())
}

fn bswap(
    state: &mut CpuState,
    instruction: &Instruction,
    address: GuestVa,
) -> Result<(), ExecError> {
    let register = instruction.op_register(0);
    if register.size() != 4 {
        return Err(ExecError::Unsupported { address });
    }
    let value = read_register(state, register, address)?;
    write_register(state, register, value.swap_bytes(), address)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BitOp {
    Test,
    Set,
    Reset,
    Complement,
}

fn bit_test(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
    op: BitOp,
) -> Result<(), ExecError> {
    let width = operand_width(instruction, 0, address)?;
    if width == Width::B {
        return Err(ExecError::Unsupported { address });
    }
    let bits = width.bits();

    // Resolve the bit location. A register bit index over a memory operand
    // addresses the surrounding bit string: the effective address moves by
    // whole operands in either direction before the in-operand bit selects.
    let (target, bit) = match (instruction.op_kind(0), instruction.op_kind(1)) {
        (OpKind::Register, _) => {
            let index = operand(state, instruction, 1, address)?;
            let index = read_operand(state, memory, index, width)?;
            (operand(state, instruction, 0, address)?, index & (bits - 1))
        }
        (OpKind::Memory, OpKind::Immediate8) => {
            let base = effective_address(state, instruction, address, true)?;
            (Operand::Memory(base), u32::from(instruction.immediate8()) & (bits - 1))
        }
        (OpKind::Memory, OpKind::Register) => {
            let base = effective_address(state, instruction, address, true)?;
            let register = instruction.op_register(1);
            let raw = read_register(state, register, address)?;
            let index = i64::from(sign_extend(raw, width));
            let log2 = bits.trailing_zeros();
            let element = index >> log2;
            let byte_offset = (element * width.bytes() as i64) as u32;
            (
                Operand::Memory(GuestVa(base.0.wrapping_add(byte_offset))),
                (index as u32) & (bits - 1),
            )
        }
        _ => return Err(ExecError::Unsupported { address }),
    };

    let value = read_operand(state, memory, target, width)?;
    let carry = (value >> bit) & 1;
    let updated = match op {
        BitOp::Test => None,
        BitOp::Set => Some(value | (1 << bit)),
        BitOp::Reset => Some(value & !(1 << bit)),
        BitOp::Complement => Some(value ^ (1 << bit)),
    };
    if let Some(updated) = updated {
        write_operand(state, memory, target, width, updated)?;
    }
    // Only the carry flag is defined; the rest stay preserved.
    merge_flags(state, flags::CARRY, carry * flags::CARRY);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringKind {
    Movs,
    Stos,
    Lods,
    Scas,
    Cmps,
}

/// Executes one string instruction, honoring repeat prefixes and DF.
///
/// Repeated forms commit ESI/EDI/ECX and flags per completed iteration, so a
/// fault mid-repeat leaves the partial progress hardware would leave (the
/// instruction is restartable at the unchanged EIP).
fn string_op(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
    kind: StringKind,
    width: Width,
) -> Result<(), ExecError> {
    // Require the 32-bit address-size forms; this also excludes the SSE2
    // instructions sharing the MOVSD/CMPSD mnemonics.
    let operands_valid = match kind {
        StringKind::Movs => {
            instruction.op0_kind() == OpKind::MemoryESEDI
                && instruction.op1_kind() == OpKind::MemorySegESI
        }
        StringKind::Stos => instruction.op0_kind() == OpKind::MemoryESEDI,
        StringKind::Lods => instruction.op1_kind() == OpKind::MemorySegESI,
        StringKind::Scas => instruction.op1_kind() == OpKind::MemoryESEDI,
        StringKind::Cmps => {
            instruction.op0_kind() == OpKind::MemorySegESI
                && instruction.op1_kind() == OpKind::MemoryESEDI
        }
    };
    if !operands_valid {
        return Err(ExecError::Unsupported { address });
    }

    let repeated = instruction.has_rep_prefix() || instruction.has_repne_prefix();
    let conditional = matches!(kind, StringKind::Scas | StringKind::Cmps);
    // REPNE continues while ZF is clear; REPE/REP continues while ZF is set.
    let continue_on_zero = !instruction.has_repne_prefix();
    let source_base = segment_base(state, instruction.memory_segment(), address)?;
    let destination_base = state.segment(Segment::Es).base;
    let accumulator = match width {
        Width::B => Register::AL,
        Width::W => Register::AX,
        Width::D => Register::EAX,
    };

    // A repeated string instruction is interruptible on hardware: EIP stays
    // on the instruction while ECX/ESI/EDI record progress. Bounding the
    // iterations per `step` keeps one giant REP (a multi-megabyte memset)
    // from freezing the run loop; the instruction simply resumes on the next
    // step with its committed partial progress.
    const MAX_REP_ITERATIONS_PER_STEP: u32 = 0x1_0000;

    let mut iterations = 0_u32;
    loop {
        if repeated && state.gpr[1] == 0 {
            break;
        }
        if repeated && iterations == MAX_REP_ITERATIONS_PER_STEP {
            // Pre-compensate the caller's unconditional advance so the EIP
            // stays on this instruction and it restarts where it left off.
            state.eip = state.eip.wrapping_sub(instruction.len() as u32);
            break;
        }
        iterations += 1;

        let delta = if state.eflags & flags::DIRECTION != 0 {
            0_u32.wrapping_sub(width.bytes() as u32)
        } else {
            width.bytes() as u32
        };
        let source = GuestVa(source_base.wrapping_add(state.gpr[6]));
        let destination = GuestVa(destination_base.wrapping_add(state.gpr[7]));

        match kind {
            StringKind::Movs => {
                let value = read_memory(memory, source, width)?;
                write_memory(memory, destination, width, value)?;
                state.gpr[6] = state.gpr[6].wrapping_add(delta);
                state.gpr[7] = state.gpr[7].wrapping_add(delta);
            }
            StringKind::Stos => {
                let value = read_register(state, accumulator, address)?;
                write_memory(memory, destination, width, value)?;
                state.gpr[7] = state.gpr[7].wrapping_add(delta);
            }
            StringKind::Lods => {
                let value = read_memory(memory, source, width)?;
                write_register(state, accumulator, value, address)?;
                state.gpr[6] = state.gpr[6].wrapping_add(delta);
            }
            StringKind::Scas => {
                let value = read_memory(memory, destination, width)?;
                let a = read_register(state, accumulator, address)?;
                let (_, bits) = sub_bits(a, value, 0, width);
                merge_flags(state, flags::ARITHMETIC, bits);
                state.gpr[7] = state.gpr[7].wrapping_add(delta);
            }
            StringKind::Cmps => {
                let a = read_memory(memory, source, width)?;
                let b = read_memory(memory, destination, width)?;
                let (_, bits) = sub_bits(a, b, 0, width);
                merge_flags(state, flags::ARITHMETIC, bits);
                state.gpr[6] = state.gpr[6].wrapping_add(delta);
                state.gpr[7] = state.gpr[7].wrapping_add(delta);
            }
        }

        if !repeated {
            break;
        }
        state.gpr[1] = state.gpr[1].wrapping_sub(1);
        if conditional && (state.eflags & flags::ZERO != 0) != continue_on_zero {
            break;
        }
    }
    Ok(())
}

/// Reports a deterministic Pentium III (Coppermine) identity.
///
/// Family 6, model 8; features include MMX, FXSR, and SSE and exclude SSE2,
/// matching the Xbox CPU. Out-of-range leaves return the highest basic leaf,
/// as hardware does.
fn cpuid(state: &mut CpuState) {
    let (a, b, c, d) = match state.gpr[0] {
        0 => (2, 0x756E_6547, 0x6C65_746E, 0x4965_6E69), // "GenuineIntel"
        1 => (0x0000_068A, 0, 0, 0x0383_F9FF),
        _ => (0x0302_0101, 0, 0, 0x0C04_0843),
    };
    state.gpr[0] = a;
    state.gpr[3] = b;
    state.gpr[1] = c;
    state.gpr[2] = d;
}

fn bit_scan(
    state: &mut CpuState,
    memory: &dyn GuestMemory,
    instruction: &Instruction,
    address: GuestVa,
    forward: bool,
) -> Result<(), ExecError> {
    let width = operand_width(instruction, 0, address)?;
    let src = operand(state, instruction, 1, address)?;
    let value = read_operand(state, memory, src, width)? & width.mask();
    if value == 0 {
        // The destination is architecturally undefined for a zero source;
        // hardware leaves it unchanged and this interpreter matches that.
        merge_flags(state, flags::ZERO, flags::ZERO);
        return Ok(());
    }
    let index = if forward { value.trailing_zeros() } else { 31 - value.leading_zeros() };
    let dst = operand(state, instruction, 0, address)?;
    write_operand(state, memory, dst, width, index)?;
    merge_flags(state, flags::ZERO, 0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use exbawks_memory::SoftwareAddressSpace;
    use exbawks_types::{GuestRange, MemoryPermissions};

    use super::*;

    const CODE: u32 = 0x0001_0000;
    const DATA: u32 = 0x0002_0000;
    const READ_ONLY: u32 = 0x0003_0000;
    const UNMAPPED: u32 = 0x0009_0000;

    fn machine() -> (CpuState, SoftwareAddressSpace) {
        let memory = SoftwareAddressSpace::new(4 * 1024 * 1024).expect("memory initializes");
        let rwx = MemoryPermissions::READ | MemoryPermissions::WRITE | MemoryPermissions::EXECUTE;
        let rw = MemoryPermissions::READ | MemoryPermissions::WRITE;
        for (start, pages, permissions) in
            [(CODE, 2_u64, rwx), (DATA, 2, rw), (READ_ONLY, 1, MemoryPermissions::READ)]
        {
            let range =
                GuestRange::page_aligned(GuestVa(start), pages * u64::from(GUEST_PAGE_SIZE))
                    .expect("range is valid");
            memory.map_anonymous(range, permissions).expect("mapping succeeds");
        }
        let state = CpuState { eip: CODE, ..CpuState::default() };
        (state, memory)
    }

    fn try_exec(
        state: &mut CpuState,
        memory: &SoftwareAddressSpace,
        bytes: &[u8],
    ) -> Result<(), ExecError> {
        memory.write(GuestVa(state.eip), bytes).expect("code writes");
        step(state, memory)
    }

    fn exec(state: &mut CpuState, memory: &SoftwareAddressSpace, bytes: &[u8]) {
        try_exec(state, memory, bytes)
            .unwrap_or_else(|error| panic!("{bytes:02X?} failed: {error:?}"));
    }

    fn arith(state: &CpuState) -> u32 {
        state.eflags & flags::ARITHMETIC
    }

    /// The `f64` at a guest address, as the unit stored it.
    fn read_f64(memory: &SoftwareAddressSpace, at: u32) -> f64 {
        let mut bytes = [0_u8; 8];
        memory.read(GuestVa(at), &mut bytes).expect("data reads");
        f64::from_le_bytes(bytes)
    }

    #[test]
    fn x87_loads_and_stores_cross_the_memory_boundary() {
        let (mut state, memory) = machine();
        memory.write(GuestVa(DATA), &1.5_f32.to_le_bytes()).expect("data writes");

        // fld dword [DATA]; fstp qword [DATA+8]
        exec(&mut state, &memory, &[0xD9, 0x05, 0x00, 0x00, 0x02, 0x00]);
        exec(&mut state, &memory, &[0xDD, 0x1D, 0x08, 0x00, 0x02, 0x00]);
        assert_eq!(read_f64(&memory, DATA + 8), 1.5, "a single widens and a double stores");

        // The pop left the stack empty again, so the top is back where it
        // started; a load and a store that did not move it would leave the
        // next value one register off.
        assert_eq!(state.x87.status & 0x3800, 0, "the stack returned to its top");
    }

    #[test]
    fn x87_arithmetic_runs_on_the_stack() {
        let (mut state, memory) = machine();
        memory.write(GuestVa(DATA), &3.0_f64.to_le_bytes()).expect("data writes");
        memory.write(GuestVa(DATA + 8), &4.0_f64.to_le_bytes()).expect("data writes");

        // fld qword [DATA]; fld qword [DATA+8]; fmul st(1),st(0) style:
        // load both, multiply the top pair, and store the result.
        exec(&mut state, &memory, &[0xDD, 0x05, 0x00, 0x00, 0x02, 0x00]);
        exec(&mut state, &memory, &[0xDD, 0x05, 0x08, 0x00, 0x02, 0x00]);
        // faddp st(1),st(0) — adds the top into the one below and pops.
        exec(&mut state, &memory, &[0xDE, 0xC1]);
        exec(&mut state, &memory, &[0xDD, 0x1D, 0x10, 0x00, 0x02, 0x00]);
        assert_eq!(read_f64(&memory, DATA + 16), 7.0, "three plus four, popped into one");
    }

    #[test]
    fn x87_subtraction_keeps_its_operands_the_right_way_round() {
        let (mut state, memory) = machine();
        memory.write(GuestVa(DATA), &10.0_f64.to_le_bytes()).expect("data writes");
        memory.write(GuestVa(DATA + 8), &4.0_f64.to_le_bytes()).expect("data writes");

        // fld qword [DATA]; fsub qword [DATA+8]  ->  10 - 4
        exec(&mut state, &memory, &[0xDD, 0x05, 0x00, 0x00, 0x02, 0x00]);
        exec(&mut state, &memory, &[0xDC, 0x25, 0x08, 0x00, 0x02, 0x00]);
        exec(&mut state, &memory, &[0xDD, 0x1D, 0x10, 0x00, 0x02, 0x00]);
        assert_eq!(read_f64(&memory, DATA + 16), 6.0, "the reversed form would give -6");

        // fld qword [DATA]; fsubr qword [DATA+8]  ->  4 - 10
        exec(&mut state, &memory, &[0xDD, 0x05, 0x00, 0x00, 0x02, 0x00]);
        exec(&mut state, &memory, &[0xDC, 0x2D, 0x08, 0x00, 0x02, 0x00]);
        exec(&mut state, &memory, &[0xDD, 0x1D, 0x18, 0x00, 0x02, 0x00]);
        assert_eq!(read_f64(&memory, DATA + 24), -6.0, "and the reverse really reverses");
    }

    #[test]
    fn x87_stores_an_integer_by_rounding_it() {
        let (mut state, memory) = machine();
        memory.write(GuestVa(DATA), &2.5_f64.to_le_bytes()).expect("data writes");

        // fld qword [DATA]; fistp dword [DATA+8]
        exec(&mut state, &memory, &[0xDD, 0x05, 0x00, 0x00, 0x02, 0x00]);
        exec(&mut state, &memory, &[0xDB, 0x1D, 0x08, 0x00, 0x02, 0x00]);
        let mut bytes = [0_u8; 4];
        memory.read(GuestVa(DATA + 8), &mut bytes).expect("data reads");
        // Half rounds to even under the default control word, so 2.5 is 2.
        assert_eq!(i32::from_le_bytes(bytes), 2, "half rounds to even, not away");
    }

    #[test]
    fn x87_compares_through_the_status_word() {
        let (mut state, memory) = machine();
        memory.write(GuestVa(DATA), &1.0_f64.to_le_bytes()).expect("data writes");
        memory.write(GuestVa(DATA + 8), &2.0_f64.to_le_bytes()).expect("data writes");

        // fld qword [DATA]; fcom qword [DATA+8]  ->  1 < 2 sets C0
        exec(&mut state, &memory, &[0xDD, 0x05, 0x00, 0x00, 0x02, 0x00]);
        exec(&mut state, &memory, &[0xDC, 0x15, 0x08, 0x00, 0x02, 0x00]);
        assert_eq!(state.x87.status & 0x4500, 0x0100, "less than sets C0 alone");

        // fcom qword [DATA] against itself  ->  equal sets C3
        exec(&mut state, &memory, &[0xDC, 0x15, 0x00, 0x00, 0x02, 0x00]);
        assert_eq!(state.x87.status & 0x4500, 0x4000, "equal sets C3 alone");
    }

    #[test]
    fn x87_exchanges_two_stack_registers() {
        let (mut state, memory) = machine();
        memory.write(GuestVa(DATA), &1.0_f64.to_le_bytes()).expect("data writes");
        memory.write(GuestVa(DATA + 8), &2.0_f64.to_le_bytes()).expect("data writes");

        exec(&mut state, &memory, &[0xDD, 0x05, 0x00, 0x00, 0x02, 0x00]);
        exec(&mut state, &memory, &[0xDD, 0x05, 0x08, 0x00, 0x02, 0x00]);
        // fxch st(1), then store: the one that was underneath comes out.
        exec(&mut state, &memory, &[0xD9, 0xC9]);
        exec(&mut state, &memory, &[0xDD, 0x1D, 0x10, 0x00, 0x02, 0x00]);
        assert_eq!(read_f64(&memory, DATA + 16), 1.0, "the exchange reached the top");
    }

    #[test]
    fn an_extended_value_survives_a_round_trip() {
        let (mut state, memory) = machine();
        memory.write(GuestVa(DATA), &1.5_f64.to_le_bytes()).expect("data writes");

        // fld qword [DATA]; fstp tbyte [DATA+16]; fld tbyte [DATA+16];
        // fstp qword [DATA+32]
        exec(&mut state, &memory, &[0xDD, 0x05, 0x00, 0x00, 0x02, 0x00]);
        exec(&mut state, &memory, &[0xDB, 0x3D, 0x10, 0x00, 0x02, 0x00]);
        exec(&mut state, &memory, &[0xDB, 0x2D, 0x10, 0x00, 0x02, 0x00]);
        exec(&mut state, &memory, &[0xDD, 0x1D, 0x20, 0x00, 0x02, 0x00]);
        assert_eq!(read_f64(&memory, DATA + 32), 1.5, "the extended format round-trips");
    }

    #[test]
    fn a_fault_leaves_the_floating_point_stack_alone() {
        let (mut state, memory) = machine();
        let before = state.x87.clone();
        // fld dword [READ_ONLY - 0x1000]: an unmapped address.
        let result = try_exec(&mut state, &memory, &[0xD9, 0x05, 0x00, 0x00, 0x00, 0x00]);
        assert!(result.is_err(), "an unmapped load faults");
        assert_eq!(state.x87, before, "and the stack is exactly as it was");
    }

    #[test]
    fn mov_covers_loads_stores_and_widths() {
        let (mut state, memory) = machine();
        memory.write_u32(GuestVa(DATA), 0xAABB_CCDD).expect("data writes");

        // mov ecx, [DATA]
        exec(&mut state, &memory, &[0x8B, 0x0D, 0x00, 0x00, 0x02, 0x00]);
        assert_eq!(state.gpr[1], 0xAABB_CCDD);
        assert_eq!(state.eip, CODE + 6);

        // mov [DATA+0x10], ecx
        exec(&mut state, &memory, &[0x89, 0x0D, 0x10, 0x00, 0x02, 0x00]);
        assert_eq!(memory.read_u32(GuestVa(DATA + 0x10)).expect("read"), 0xAABB_CCDD);

        // mov al, [DATA] (moffs8)
        state.gpr[0] = 0x1111_1100;
        exec(&mut state, &memory, &[0xA0, 0x00, 0x00, 0x02, 0x00]);
        assert_eq!(state.gpr[0], 0x1111_11DD);

        // mov [DATA+0x14], ax (moffs16)
        exec(&mut state, &memory, &[0x66, 0xA3, 0x14, 0x00, 0x02, 0x00]);
        assert_eq!(memory.read_u32(GuestVa(DATA + 0x14)).expect("read") & 0xFFFF, 0x11DD);

        // mov byte ptr [DATA+4], 0xAB
        exec(&mut state, &memory, &[0xC6, 0x05, 0x04, 0x00, 0x02, 0x00, 0xAB]);
        assert_eq!(memory.read_u32(GuestVa(DATA + 4)).expect("read") & 0xFF, 0xAB);
    }

    /// The exact shape of the retail entry stub: two absolute loads, ALU on
    /// the results, and a compare against a register-indirect operand.
    #[test]
    fn retail_entry_shape_executes() {
        let (mut state, memory) = machine();
        memory.write_u32(GuestVa(DATA), DATA + 0x40).expect("data writes");
        memory.write_u32(GuestVa(DATA + 4), 0x1234).expect("data writes");
        memory.write_u32(GuestVa(DATA + 0x40), 0xFFFF_11F4).expect("data writes");

        exec(&mut state, &memory, &[0x8B, 0x0D, 0x00, 0x00, 0x02, 0x00]); // mov ecx,[DATA]
        exec(&mut state, &memory, &[0xA1, 0x04, 0x00, 0x02, 0x00]); // mov eax,[DATA+4]
        exec(&mut state, &memory, &[0x29, 0xC8]); // sub eax, ecx
        exec(&mut state, &memory, &[0x05, 0x00, 0x00, 0x01, 0x00]); // add eax, 0x10000
        exec(&mut state, &memory, &[0x3B, 0x01]); // cmp eax, [ecx]

        assert_eq!(state.gpr[0], 0xFFFF_11F4);
        assert_ne!(state.eflags & flags::ZERO, 0, "compare must observe equality");
        assert_eq!(state.eip, CODE + 20);
    }

    #[test]
    fn read_modify_write_adds_into_memory() {
        let (mut state, memory) = machine();
        memory.write_u32(GuestVa(DATA), 1).expect("data writes");
        state.gpr[3] = 0xFFFF_FFFF;

        // add [DATA], ebx
        exec(&mut state, &memory, &[0x01, 0x1D, 0x00, 0x00, 0x02, 0x00]);
        assert_eq!(memory.read_u32(GuestVa(DATA)).expect("read"), 0);
        assert_eq!(arith(&state), flags::CARRY | flags::PARITY | flags::AUXILIARY | flags::ZERO);
    }

    #[test]
    fn adc_and_sbb_consume_the_carry_flag() {
        let (mut state, memory) = machine();
        state.gpr[0] = 0xFFFF_FFFF;
        state.gpr[3] = 5;
        exec(&mut state, &memory, &[0x83, 0xC0, 0x01]); // add eax, 1 -> CF
        exec(&mut state, &memory, &[0x83, 0xD3, 0x00]); // adc ebx, 0
        assert_eq!(state.gpr[3], 6);

        state.gpr[0] = 0;
        state.gpr[3] = 5;
        exec(&mut state, &memory, &[0x83, 0xE8, 0x01]); // sub eax, 1 -> CF
        exec(&mut state, &memory, &[0x83, 0xDB, 0x00]); // sbb ebx, 0
        assert_eq!(state.gpr[3], 4);
    }

    #[test]
    fn inc_and_dec_preserve_the_carry_flag() {
        let (mut state, memory) = machine();
        exec(&mut state, &memory, &[0xF9]); // stc
        exec(&mut state, &memory, &[0x40]); // inc eax
        assert_eq!(state.gpr[0], 1);
        assert_ne!(state.eflags & flags::CARRY, 0);

        exec(&mut state, &memory, &[0x48]); // dec eax
        assert_eq!(state.gpr[0], 0);
        assert_ne!(state.eflags & flags::CARRY, 0);
        assert_ne!(state.eflags & flags::ZERO, 0);
    }

    #[test]
    fn neg_sets_carry_for_nonzero_values() {
        let (mut state, memory) = machine();
        state.gpr[0] = 1;
        exec(&mut state, &memory, &[0xF7, 0xD8]); // neg eax
        assert_eq!(state.gpr[0], 0xFFFF_FFFF);
        assert_ne!(state.eflags & flags::CARRY, 0);

        state.gpr[0] = 0;
        exec(&mut state, &memory, &[0xF7, 0xD8]);
        assert_eq!(state.gpr[0], 0);
        assert_eq!(state.eflags & flags::CARRY, 0);
    }

    #[test]
    fn multiply_family_reports_widening_overflow() {
        let (mut state, memory) = machine();
        state.gpr[0] = 0x1_0000;
        state.gpr[1] = 0x1_0000;
        exec(&mut state, &memory, &[0xF7, 0xE1]); // mul ecx
        assert_eq!(state.gpr[0], 0);
        assert_eq!(state.gpr[2], 1);
        assert_ne!(state.eflags & flags::CARRY, 0);
        assert_ne!(state.eflags & flags::OVERFLOW, 0);

        state.gpr[0] = 6;
        state.gpr[1] = 7;
        exec(&mut state, &memory, &[0x0F, 0xAF, 0xC1]); // imul eax, ecx
        assert_eq!(state.gpr[0], 42);
        assert_eq!(state.eflags & flags::CARRY, 0);

        state.gpr[1] = 0x4000_0000;
        exec(&mut state, &memory, &[0x6B, 0xC1, 0x10]); // imul eax, ecx, 16
        assert_eq!(state.gpr[0], 0);
        assert_ne!(state.eflags & flags::OVERFLOW, 0, "truncated product must overflow");
    }

    #[test]
    fn divide_produces_quotient_and_remainder() {
        let (mut state, memory) = machine();
        state.gpr[0] = 17;
        state.gpr[1] = 5;
        state.gpr[2] = 0;
        exec(&mut state, &memory, &[0xF7, 0xF1]); // div ecx
        assert_eq!(state.gpr[0], 3);
        assert_eq!(state.gpr[2], 2);

        // -17 / 5 = -3 remainder -2.
        state.gpr[0] = 0xFFFF_FFEF;
        state.gpr[2] = 0xFFFF_FFFF;
        exec(&mut state, &memory, &[0xF7, 0xF9]); // idiv ecx
        assert_eq!(state.gpr[0], 0xFFFF_FFFD);
        assert_eq!(state.gpr[2], 0xFFFF_FFFE);
    }

    #[test]
    fn divide_errors_are_typed_and_leave_state() {
        let (mut state, memory) = machine();
        state.gpr[1] = 0;
        let error = try_exec(&mut state, &memory, &[0xF7, 0xF1]); // div ecx by zero
        assert!(matches!(error, Err(ExecError::Divide { .. })));
        assert_eq!(state.eip, CODE, "a faulting divide must not advance EIP");

        state.gpr[0] = 0;
        state.gpr[1] = 1;
        state.gpr[2] = 1; // dividend 2^32, divisor 1: quotient overflows.
        let error = try_exec(&mut state, &memory, &[0xF7, 0xF1]);
        assert!(matches!(error, Err(ExecError::Divide { .. })));

        // INT_MIN / -1 overflows the signed quotient.
        state.gpr[0] = 0x8000_0000;
        state.gpr[2] = 0xFFFF_FFFF;
        state.gpr[1] = 0xFFFF_FFFF;
        let error = try_exec(&mut state, &memory, &[0xF7, 0xF9]);
        assert!(matches!(error, Err(ExecError::Divide { .. })));
    }

    #[test]
    fn shifts_define_documented_flags() {
        let (mut state, memory) = machine();
        state.gpr[0] = 0x8000_0001;
        exec(&mut state, &memory, &[0xD1, 0xE0]); // shl eax, 1
        assert_eq!(state.gpr[0], 2);
        assert_ne!(state.eflags & flags::CARRY, 0);
        assert_ne!(state.eflags & flags::OVERFLOW, 0, "count-one overflow is CF xor MSB");

        state.gpr[0] = 1;
        exec(&mut state, &memory, &[0xD1, 0xE8]); // shr eax, 1
        assert_eq!(state.gpr[0], 0);
        assert_ne!(state.eflags & flags::CARRY, 0);
        assert_ne!(state.eflags & flags::ZERO, 0);
        assert_eq!(state.eflags & flags::OVERFLOW, 0);

        state.gpr[0] = 0x8000_0000;
        exec(&mut state, &memory, &[0xD1, 0xF8]); // sar eax, 1
        assert_eq!(state.gpr[0], 0xC000_0000);
        assert_eq!(state.eflags & flags::CARRY, 0);
    }

    #[test]
    fn masked_zero_shift_counts_change_nothing() {
        let (mut state, memory) = machine();
        state.gpr[0] = 0xDEAD_BEEF;
        exec(&mut state, &memory, &[0xF9]); // stc
        let before = state.eflags;
        exec(&mut state, &memory, &[0xC1, 0xE0, 0x20]); // shl eax, 32 -> masked to 0
        assert_eq!(state.gpr[0], 0xDEAD_BEEF);
        assert_eq!(state.eflags, before);
    }

    #[test]
    fn rotates_move_bits_through_the_carry() {
        let (mut state, memory) = machine();
        state.gpr[0] = 0x8000_0000;
        exec(&mut state, &memory, &[0xD1, 0xC0]); // rol eax, 1
        assert_eq!(state.gpr[0], 1);
        assert_ne!(state.eflags & flags::CARRY, 0);

        state.gpr[0] = 0x4000_0000;
        exec(&mut state, &memory, &[0xF9]); // stc
        exec(&mut state, &memory, &[0xD1, 0xD0]); // rcl eax, 1
        assert_eq!(state.gpr[0], 0x8000_0001);
        assert_eq!(state.eflags & flags::CARRY, 0);

        state.gpr[0] = 1;
        exec(&mut state, &memory, &[0xF9]); // stc
        exec(&mut state, &memory, &[0xD1, 0xD8]); // rcr eax, 1
        assert_eq!(state.gpr[0], 0x8000_0000);
        assert_ne!(state.eflags & flags::CARRY, 0);
    }

    #[test]
    fn double_shifts_pull_bits_from_the_second_operand() {
        let (mut state, memory) = machine();
        state.gpr[0] = 0x1234_5678;
        state.gpr[3] = 0x9ABC_DEF0;
        exec(&mut state, &memory, &[0x0F, 0xA4, 0xD8, 0x08]); // shld eax, ebx, 8
        assert_eq!(state.gpr[0], 0x3456_789A);

        state.gpr[0] = 0x1234_5678;
        state.gpr[3] = 0x9ABC_DEF0;
        exec(&mut state, &memory, &[0x0F, 0xAC, 0xD8, 0x08]); // shrd eax, ebx, 8
        assert_eq!(state.gpr[0], 0xF012_3456);
    }

    #[test]
    fn widening_moves_zero_and_sign_extend() {
        let (mut state, memory) = machine();
        state.gpr[1] = 0x80;
        exec(&mut state, &memory, &[0x0F, 0xB6, 0xC1]); // movzx eax, cl
        assert_eq!(state.gpr[0], 0x80);
        exec(&mut state, &memory, &[0x0F, 0xBE, 0xC1]); // movsx eax, cl
        assert_eq!(state.gpr[0], 0xFFFF_FF80);

        memory.write_u32(GuestVa(DATA), 0x8001).expect("data writes");
        exec(&mut state, &memory, &[0x0F, 0xB7, 0x05, 0x00, 0x00, 0x02, 0x00]); // movzx eax, word
        assert_eq!(state.gpr[0], 0x8001);
        exec(&mut state, &memory, &[0x0F, 0xBF, 0x05, 0x00, 0x00, 0x02, 0x00]); // movsx eax, word
        assert_eq!(state.gpr[0], 0xFFFF_8001);
    }

    #[test]
    fn setcc_and_cmovcc_follow_conditions() {
        let (mut state, memory) = machine();
        state.gpr[0] = 7;
        exec(&mut state, &memory, &[0x83, 0xF8, 0x07]); // cmp eax, 7
        exec(&mut state, &memory, &[0x0F, 0x94, 0xC3]); // sete bl
        assert_eq!(state.gpr[3] & 0xFF, 1);

        state.gpr[1] = 0x5555;
        exec(&mut state, &memory, &[0x0F, 0x44, 0xD1]); // cmove edx, ecx (ZF set)
        assert_eq!(state.gpr[2], 0x5555);

        state.gpr[2] = 0;
        exec(&mut state, &memory, &[0x0F, 0x45, 0xD1]); // cmovne edx, ecx (ZF set)
        assert_eq!(state.gpr[2], 0, "a false condition must not move");
    }

    #[test]
    fn cmov_reads_memory_even_when_the_condition_fails() {
        let (mut state, memory) = machine();
        exec(&mut state, &memory, &[0x31, 0xC0]); // xor eax, eax -> ZF
        // cmovne eax, [UNMAPPED] with a false condition still faults.
        let error = try_exec(&mut state, &memory, &[0x0F, 0x45, 0x05, 0x00, 0x00, 0x09, 0x00]);
        assert!(matches!(error, Err(ExecError::Memory(MemoryError::Unmapped { .. }))));
        assert_eq!(state.gpr[0], 0);
    }

    #[test]
    fn bit_tests_cover_registers_and_bit_strings() {
        let (mut state, memory) = machine();
        state.gpr[0] = 0b1000;
        state.gpr[1] = 3;
        exec(&mut state, &memory, &[0x0F, 0xA3, 0xC8]); // bt eax, ecx
        assert_ne!(state.eflags & flags::CARRY, 0);

        exec(&mut state, &memory, &[0x0F, 0xBA, 0xE8, 0x04]); // bts eax, 4
        assert_eq!(state.gpr[0], 0b1_1000);
        assert_eq!(state.eflags & flags::CARRY, 0);

        // A register bit index over memory addresses the surrounding string:
        // index -1 selects bit 31 of the previous dword.
        memory.write_u32(GuestVa(DATA + 0x3C), 0x8000_0000).expect("data writes");
        state.gpr[1] = 0xFFFF_FFFF;
        exec(&mut state, &memory, &[0x0F, 0xA3, 0x0D, 0x40, 0x00, 0x02, 0x00]); // bt [DATA+0x40], ecx
        assert_ne!(state.eflags & flags::CARRY, 0);
    }

    #[test]
    fn bit_scans_handle_zero_sources() {
        let (mut state, memory) = machine();
        state.gpr[1] = 0x0010_0000;
        exec(&mut state, &memory, &[0x0F, 0xBC, 0xC1]); // bsf eax, ecx
        assert_eq!(state.gpr[0], 20);
        assert_eq!(state.eflags & flags::ZERO, 0);

        state.gpr[0] = 0xAAAA_AAAA;
        state.gpr[1] = 0;
        exec(&mut state, &memory, &[0x0F, 0xBD, 0xC1]); // bsr eax, ecx (zero source)
        assert_eq!(state.gpr[0], 0xAAAA_AAAA, "a zero source leaves the destination");
        assert_ne!(state.eflags & flags::ZERO, 0);
    }

    #[test]
    fn sign_extension_helpers_fill_the_accumulator_pair() {
        let (mut state, memory) = machine();
        state.gpr[0] = 0x8000;
        exec(&mut state, &memory, &[0x98]); // cwde
        assert_eq!(state.gpr[0], 0xFFFF_8000);
        exec(&mut state, &memory, &[0x99]); // cdq
        assert_eq!(state.gpr[2], 0xFFFF_FFFF);
    }

    #[test]
    fn flag_utilities_round_trip_through_ah() {
        let (mut state, memory) = machine();
        exec(&mut state, &memory, &[0xF9]); // stc
        exec(&mut state, &memory, &[0x9F]); // lahf
        assert_eq!((state.gpr[0] >> 8) & 0xFF, 0x03, "CF and the fixed bit land in AH");

        state.gpr[0] = 0xC5 << 8; // SF | ZF | PF | CF
        exec(&mut state, &memory, &[0x9E]); // sahf
        assert_ne!(state.eflags & flags::SIGN, 0);
        assert_ne!(state.eflags & flags::ZERO, 0);
        assert_ne!(state.eflags & flags::CARRY, 0);

        exec(&mut state, &memory, &[0xF5]); // cmc
        assert_eq!(state.eflags & flags::CARRY, 0);
    }

    #[test]
    fn exchange_family_swaps_and_compares() {
        let (mut state, memory) = machine();
        memory.write_u32(GuestVa(DATA), 0x1111).expect("data writes");
        state.gpr[1] = 0x2222;
        exec(&mut state, &memory, &[0x87, 0x0D, 0x00, 0x00, 0x02, 0x00]); // xchg [DATA], ecx
        assert_eq!(state.gpr[1], 0x1111);
        assert_eq!(memory.read_u32(GuestVa(DATA)).expect("read"), 0x2222);

        state.gpr[0] = 10;
        state.gpr[1] = 3;
        exec(&mut state, &memory, &[0x0F, 0xC1, 0xC8]); // xadd eax, ecx
        assert_eq!(state.gpr[0], 13);
        assert_eq!(state.gpr[1], 10);

        // cmpxchg: match replaces the destination and sets ZF.
        state.gpr[0] = 13;
        state.gpr[1] = 13;
        state.gpr[3] = 99;
        exec(&mut state, &memory, &[0x0F, 0xB1, 0xD9]); // cmpxchg ecx, ebx
        assert_eq!(state.gpr[1], 99);
        assert_ne!(state.eflags & flags::ZERO, 0);

        // cmpxchg8b mismatch loads EDX:EAX and clears ZF.
        memory.write_u32(GuestVa(DATA + 0x40), 0x1122_3344).expect("data writes");
        memory.write_u32(GuestVa(DATA + 0x44), 0x5566_7788).expect("data writes");
        state.gpr[0] = 0;
        state.gpr[2] = 0;
        exec(&mut state, &memory, &[0x0F, 0xC7, 0x0D, 0x40, 0x00, 0x02, 0x00]); // cmpxchg8b
        assert_eq!(state.gpr[0], 0x1122_3344);
        assert_eq!(state.gpr[2], 0x5566_7788);
        assert_eq!(state.eflags & flags::ZERO, 0);
    }

    #[test]
    fn compare_and_test_never_write() {
        let (mut state, memory) = machine();
        memory.write_u32(GuestVa(DATA), 0x77).expect("data writes");
        // cmp dword ptr [DATA], 1 and test dword ptr [DATA], 1
        exec(&mut state, &memory, &[0x83, 0x3D, 0x00, 0x00, 0x02, 0x00, 0x01]);
        exec(&mut state, &memory, &[0xF7, 0x05, 0x00, 0x00, 0x02, 0x00, 0x01, 0x00, 0x00, 0x00]);
        assert_eq!(memory.read_u32(GuestVa(DATA)).expect("read"), 0x77);
    }

    #[test]
    fn lea_computes_offsets_without_segment_bases() {
        let (mut state, memory) = machine();
        state.gpr[3] = 0x100;
        state.gpr[1] = 4;
        let mut fs = state.segment(Segment::Fs);
        fs.base = 0x5000_0000;
        state.set_segment(Segment::Fs, fs);

        // lea eax, fs:[ebx+ecx*4+8]: the base must not participate.
        exec(&mut state, &memory, &[0x64, 0x8D, 0x44, 0x8B, 0x08]);
        assert_eq!(state.gpr[0], 0x118);
    }

    #[test]
    fn segment_override_bases_join_effective_addresses() {
        let (mut state, memory) = machine();
        let mut fs = state.segment(Segment::Fs);
        fs.base = DATA;
        state.set_segment(Segment::Fs, fs);
        memory.write_u32(GuestVa(DATA + 0x1C), 0xFEED_BACC).expect("data writes");

        // mov eax, fs:[0x1C]
        exec(&mut state, &memory, &[0x64, 0xA1, 0x1C, 0x00, 0x00, 0x00]);
        assert_eq!(state.gpr[0], 0xFEED_BACC);
    }

    #[test]
    fn fetch_straddles_a_page_boundary() {
        let (mut state, memory) = machine();
        state.eip = CODE + 0xFFE;
        // mov eax, 0xCAFEBABE splits two bytes into the first page.
        exec(&mut state, &memory, &[0xB8, 0xBE, 0xBA, 0xFE, 0xCA]);
        assert_eq!(state.gpr[0], 0xCAFE_BABE);
        assert_eq!(state.eip, CODE + 0xFFE + 5);
    }

    #[test]
    fn faults_are_typed_and_leave_state_untouched() {
        let (mut state, memory) = machine();
        state.gpr[0] = 0x1234;
        state.eflags = 0x0000_0002 | flags::CARRY;
        let before = state.clone();

        let error = try_exec(&mut state, &memory, &[0x8B, 0x05, 0x00, 0x00, 0x09, 0x00]);
        assert!(matches!(error, Err(ExecError::Memory(MemoryError::Unmapped { .. }))));
        assert_eq!(state, before);

        let error = try_exec(&mut state, &memory, &[0x89, 0x05, 0x00, 0x00, 0x03, 0x00]);
        assert!(matches!(error, Err(ExecError::Memory(MemoryError::AccessDenied { .. }))));
        assert_eq!(state, before);
    }

    #[test]
    fn undecodable_bytes_and_unmapped_fetches_are_typed() {
        let (mut state, memory) = machine();
        let error = try_exec(&mut state, &memory, &[0x0F, 0x04]);
        assert!(matches!(error, Err(ExecError::InvalidInstruction { .. })));

        state.eip = UNMAPPED;
        let error = step(&mut state, &memory);
        assert!(matches!(error, Err(ExecError::Memory(MemoryError::Unmapped { .. }))));
    }

    /// Malformed input must produce typed errors, never a panic.
    #[test]
    fn random_byte_soup_never_panics() {
        let (_, memory) = machine();
        let mut seed = 0xDEAD_4EED_u32;
        let mut bytes = [0_u8; 15];
        for _ in 0..2_000 {
            for byte in &mut bytes {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *byte = (seed >> 24) as u8;
            }
            let mut state = CpuState { eip: CODE, ..CpuState::default() };
            memory.write(GuestVa(CODE), &bytes).expect("code writes");
            let _ = step(&mut state, &memory);
        }
    }

    fn stackframe(state: &mut CpuState) {
        state.gpr[4] = DATA + 0x1000; // stack grows down inside the data pages
    }

    #[test]
    fn jumps_and_branches_move_the_instruction_pointer() {
        let (mut state, memory) = machine();
        exec(&mut state, &memory, &[0xEB, 0x02]); // jmp +2
        assert_eq!(state.eip, CODE + 4);

        state.eip = CODE;
        exec(&mut state, &memory, &[0xE9, 0x10, 0x00, 0x00, 0x00]); // jmp rel32 +0x10
        assert_eq!(state.eip, CODE + 5 + 0x10);

        state.eip = CODE;
        exec(&mut state, &memory, &[0x31, 0xC0]); // xor eax, eax -> ZF
        exec(&mut state, &memory, &[0x74, 0x10]); // je +0x10 (taken)
        assert_eq!(state.eip, CODE + 4 + 0x10);

        state.eip = CODE;
        exec(&mut state, &memory, &[0x75, 0x10]); // jne +0x10 (not taken)
        assert_eq!(state.eip, CODE + 2);
    }

    #[test]
    fn call_and_ret_round_trip_through_the_stack() {
        let (mut state, memory) = machine();
        stackframe(&mut state);
        let esp = state.gpr[4];

        exec(&mut state, &memory, &[0xE8, 0x2B, 0x00, 0x00, 0x00]); // call +0x2B
        assert_eq!(state.eip, CODE + 5 + 0x2B);
        assert_eq!(state.gpr[4], esp - 4);
        assert_eq!(memory.read_u32(GuestVa(esp - 4)).expect("read"), CODE + 5);

        exec(&mut state, &memory, &[0xC3]); // ret
        assert_eq!(state.eip, CODE + 5);
        assert_eq!(state.gpr[4], esp);

        // Indirect call through a register, then ret imm16 argument cleanup.
        state.eip = CODE;
        state.gpr[0] = CODE + 0x40;
        exec(&mut state, &memory, &[0xFF, 0xD0]); // call eax
        assert_eq!(state.eip, CODE + 0x40);
        exec(&mut state, &memory, &[0xC2, 0x08, 0x00]); // ret 8
        assert_eq!(state.eip, CODE + 2);
        assert_eq!(state.gpr[4], esp + 8);

        // Indirect call through a memory slot.
        state.gpr[4] = esp;
        state.eip = CODE;
        memory.write_u32(GuestVa(DATA + 0x80), CODE + 0x60).expect("data writes");
        exec(&mut state, &memory, &[0xFF, 0x15, 0x80, 0x00, 0x02, 0x00]); // call [DATA+0x80]
        assert_eq!(state.eip, CODE + 0x60);
        assert_eq!(memory.read_u32(GuestVa(esp - 4)).expect("read"), CODE + 6);
    }

    #[test]
    fn push_and_pop_follow_stack_pointer_rules() {
        let (mut state, memory) = machine();
        stackframe(&mut state);
        let esp = state.gpr[4];

        state.gpr[0] = 0x1234_5678;
        exec(&mut state, &memory, &[0x50]); // push eax
        exec(&mut state, &memory, &[0x59]); // pop ecx
        assert_eq!(state.gpr[1], 0x1234_5678);
        assert_eq!(state.gpr[4], esp);

        // `push esp` stores the pre-decrement value.
        exec(&mut state, &memory, &[0x54]);
        assert_eq!(memory.read_u32(GuestVa(esp - 4)).expect("read"), esp);
        // `pop esp` replaces the increment with the loaded value.
        exec(&mut state, &memory, &[0x5C]);
        assert_eq!(state.gpr[4], esp);

        exec(&mut state, &memory, &[0x6A, 0xFE]); // push -2 (imm8 sign-extends)
        assert_eq!(memory.read_u32(GuestVa(esp - 4)).expect("read"), 0xFFFF_FFFE);
        exec(&mut state, &memory, &[0x66, 0x58]); // pop ax (16-bit)
        assert_eq!(state.gpr[0] & 0xFFFF, 0xFFFE);
        assert_eq!(state.gpr[4], esp - 4 + 2);
    }

    #[test]
    fn pushad_and_popad_round_trip_and_discard_esp() {
        let (mut state, memory) = machine();
        stackframe(&mut state);
        let esp = state.gpr[4];
        state.gpr = [1, 2, 3, 4, esp, 5, 6, 7];

        exec(&mut state, &memory, &[0x60]); // pushad
        assert_eq!(state.gpr[4], esp - 32);
        state.gpr = [0; 8];
        state.gpr[4] = esp - 32;
        exec(&mut state, &memory, &[0x61]); // popad
        assert_eq!(state.gpr, [1, 2, 3, 4, esp, 5, 6, 7]);
    }

    /// The classic CPUID-availability probe: toggling the ID flag through
    /// PUSHFD/POPFD must round-trip on a Pentium III class machine.
    #[test]
    fn eflags_id_bit_toggles_through_pushfd_popfd() {
        let (mut state, memory) = machine();
        stackframe(&mut state);

        exec(&mut state, &memory, &[0x9C]); // pushfd
        exec(&mut state, &memory, &[0x58]); // pop eax
        let original = state.gpr[0];
        exec(&mut state, &memory, &[0x35, 0x00, 0x00, 0x20, 0x00]); // xor eax, ID
        exec(&mut state, &memory, &[0x50]); // push eax
        exec(&mut state, &memory, &[0x9D]); // popfd
        exec(&mut state, &memory, &[0x9C]); // pushfd
        exec(&mut state, &memory, &[0x58]); // pop eax
        assert_eq!(state.gpr[0] ^ original, flags::ID, "the ID flag must be writable");
    }

    #[test]
    fn leave_unwinds_one_frame() {
        let (mut state, memory) = machine();
        stackframe(&mut state);
        let frame = state.gpr[4] - 0x20;
        memory.write_u32(GuestVa(frame), 0xBBBB_0000).expect("data writes");
        state.gpr[5] = frame;
        state.gpr[4] = frame - 0x10;

        exec(&mut state, &memory, &[0xC9]); // leave
        assert_eq!(state.gpr[4], frame + 4);
        assert_eq!(state.gpr[5], 0xBBBB_0000);
    }

    #[test]
    fn loops_count_down_and_jecxz_tests_ecx() {
        let (mut state, memory) = machine();
        state.gpr[1] = 2;
        exec(&mut state, &memory, &[0xE2, 0x10]); // loop +0x10 (taken)
        assert_eq!(state.gpr[1], 1);
        assert_eq!(state.eip, CODE + 2 + 0x10);

        state.eip = CODE;
        exec(&mut state, &memory, &[0xE2, 0x10]); // loop (counter reaches zero)
        assert_eq!(state.gpr[1], 0);
        assert_eq!(state.eip, CODE + 2);

        exec(&mut state, &memory, &[0xE3, 0x08]); // jecxz (ecx == 0, taken)
        assert_eq!(state.eip, CODE + 4 + 8);
    }

    #[test]
    fn rep_movs_copies_in_both_directions() {
        let (mut state, memory) = machine();
        memory.write(GuestVa(DATA), b"exbawks!").expect("data writes");
        state.gpr[6] = DATA; // esi
        state.gpr[7] = DATA + 0x100; // edi
        state.gpr[1] = 8; // ecx
        exec(&mut state, &memory, &[0xF3, 0xA4]); // rep movsb
        let mut copied = [0_u8; 8];
        memory.read(GuestVa(DATA + 0x100), &mut copied).expect("read");
        assert_eq!(&copied, b"exbawks!");
        assert_eq!(state.gpr[1], 0);
        assert_eq!(state.gpr[6], DATA + 8);
        assert_eq!(state.gpr[7], DATA + 0x108);

        // Downward copy with DF set: two dwords ending at the start indices.
        exec(&mut state, &memory, &[0xFD]); // std
        state.gpr[6] = DATA + 4;
        state.gpr[7] = DATA + 0x204;
        state.gpr[1] = 2;
        exec(&mut state, &memory, &[0xF3, 0xA5]); // rep movsd
        let mut copied = [0_u8; 8];
        memory.read(GuestVa(DATA + 0x200), &mut copied).expect("read");
        assert_eq!(&copied, b"exbawks!");
        exec(&mut state, &memory, &[0xFC]); // cld
    }

    #[test]
    fn rep_stos_fills_and_zero_count_is_a_no_op() {
        let (mut state, memory) = machine();
        state.gpr[0] = 0xABAB_ABAB;
        state.gpr[7] = DATA;
        state.gpr[1] = 4;
        exec(&mut state, &memory, &[0xF3, 0xAB]); // rep stosd
        assert_eq!(memory.read_u32(GuestVa(DATA + 12)).expect("read"), 0xABAB_ABAB);
        assert_eq!(state.gpr[7], DATA + 16);

        state.gpr[1] = 0;
        state.gpr[7] = UNMAPPED; // must not be touched with a zero count
        exec(&mut state, &memory, &[0xF3, 0xAB]);
        assert_eq!(state.gpr[7], UNMAPPED);
    }

    #[test]
    fn repne_scas_finds_a_byte() {
        let (mut state, memory) = machine();
        memory.write(GuestVa(DATA), b"abcXdef\0").expect("data writes");
        state.gpr[0] = u32::from(b'X');
        state.gpr[7] = DATA;
        state.gpr[1] = 8;
        exec(&mut state, &memory, &[0xF2, 0xAE]); // repne scasb
        assert_eq!(state.gpr[7], DATA + 4, "EDI stops one past the match");
        assert_eq!(state.gpr[1], 4);
        assert_ne!(state.eflags & flags::ZERO, 0);
    }

    #[test]
    fn repe_cmps_stops_at_the_first_mismatch() {
        let (mut state, memory) = machine();
        memory.write(GuestVa(DATA), b"same-same-DIFF").expect("data writes");
        memory.write(GuestVa(DATA + 0x100), b"same-same-diff").expect("data writes");
        state.gpr[6] = DATA;
        state.gpr[7] = DATA + 0x100;
        state.gpr[1] = 14;
        exec(&mut state, &memory, &[0xF3, 0xA6]); // repe cmpsb
        assert_eq!(state.gpr[6], DATA + 11);
        assert_eq!(state.gpr[1], 3);
        assert_eq!(state.eflags & flags::ZERO, 0);
    }

    #[test]
    fn a_faulting_repeat_commits_partial_progress() {
        let (mut state, memory) = machine();
        // The destination run ends where the read-only page begins... use the
        // unmapped hole instead: DATA maps two pages, so DATA+0x2000 faults.
        state.gpr[0] = 0x5A5A_5A5A;
        state.gpr[7] = DATA + 0x2000 - 8;
        state.gpr[1] = 4;
        let error = try_exec(&mut state, &memory, &[0xF3, 0xAB]); // rep stosd
        assert!(matches!(error, Err(ExecError::Memory(MemoryError::Unmapped { .. }))));
        assert_eq!(state.gpr[1], 2, "two iterations completed before the fault");
        assert_eq!(state.gpr[7], DATA + 0x2000);
        assert_eq!(state.eip, CODE, "the instruction stays restartable");
        assert_eq!(memory.read_u32(GuestVa(DATA + 0x2000 - 4)).expect("read"), 0x5A5A_5A5A);
    }

    #[test]
    fn lods_honors_segment_overrides() {
        let (mut state, memory) = machine();
        let mut fs = state.segment(Segment::Fs);
        fs.base = DATA;
        state.set_segment(Segment::Fs, fs);
        memory.write_u32(GuestVa(DATA + 0x30), 0xCAFE_D00D).expect("data writes");
        state.gpr[6] = 0x30;
        exec(&mut state, &memory, &[0x64, 0xAD]); // lodsd fs:[esi]
        assert_eq!(state.gpr[0], 0xCAFE_D00D);
        assert_eq!(state.gpr[6], 0x34);
    }

    #[test]
    fn sixteen_bit_string_forms_are_rejected() {
        let (mut state, memory) = machine();
        let error = try_exec(&mut state, &memory, &[0x67, 0xA4]); // movsb [di],[si]
        assert!(matches!(error, Err(ExecError::Unsupported { .. })));
    }

    #[test]
    fn cpuid_reports_the_pentium_iii_profile() {
        let (mut state, memory) = machine();
        state.gpr[0] = 0;
        exec(&mut state, &memory, &[0x0F, 0xA2]); // cpuid
        assert_eq!(state.gpr[0], 2);
        let vendor: [u8; 12] = {
            let mut bytes = [0_u8; 12];
            bytes[..4].copy_from_slice(&state.gpr[3].to_le_bytes());
            bytes[4..8].copy_from_slice(&state.gpr[2].to_le_bytes());
            bytes[8..].copy_from_slice(&state.gpr[1].to_le_bytes());
            bytes
        };
        assert_eq!(&vendor, b"GenuineIntel");

        state.gpr[0] = 1;
        exec(&mut state, &memory, &[0x0F, 0xA2]);
        assert_eq!(state.gpr[0], 0x0000_068A, "family 6 model 8");
        const SSE: u32 = 1 << 25;
        const SSE2: u32 = 1 << 26;
        assert_ne!(state.gpr[2] & SSE, 0, "SSE must be present");
        assert_eq!(state.gpr[2] & SSE2, 0, "SSE2 must be absent");
    }

    #[test]
    fn rdtsc_reads_the_deterministic_counter() {
        let (mut state, memory) = machine();
        exec(&mut state, &memory, &[0x0F, 0x31]); // rdtsc
        let first = state.gpr[0];
        exec(&mut state, &memory, &[0x90]); // nop
        exec(&mut state, &memory, &[0x0F, 0x31]); // rdtsc
        assert_eq!(state.gpr[0], first + 2, "one tick per retired instruction");
        assert_eq!(state.gpr[2], 0);
    }

    /// 66-prefixed and far control-flow forms must fail typed instead of
    /// silently executing with 32-bit semantics.
    #[test]
    fn narrow_and_far_control_flow_forms_are_rejected() {
        let (mut state, memory) = machine();
        stackframe(&mut state);
        memory.write_u32(GuestVa(DATA + 0x80), CODE).expect("data writes");
        let before = state.clone();

        for bytes in [
            &[0x66, 0xC3][..],                               // retnw
            &[0x66, 0xC2, 0x08, 0x00][..],                   // retnw imm16
            &[0x66, 0xC9][..],                               // leavew
            &[0x66, 0xFF, 0x2D, 0x80, 0x00, 0x02, 0x00][..], // jmp m16:16
            &[0x66, 0xFF, 0x1D, 0x80, 0x00, 0x02, 0x00][..], // call m16:16
        ] {
            let error = try_exec(&mut state, &memory, bytes);
            assert!(
                matches!(error, Err(ExecError::Unsupported { .. })),
                "{bytes:02X?} must be rejected"
            );
            assert_eq!(state, before, "{bytes:02X?} must not mutate state");
        }
    }

    #[test]
    fn stack_faults_are_typed_and_atomic() {
        let (mut state, memory) = machine();
        state.gpr[4] = UNMAPPED;
        state.gpr[0] = 7;
        let before = state.clone();

        let error = try_exec(&mut state, &memory, &[0x50]); // push eax
        assert!(matches!(error, Err(ExecError::Memory(MemoryError::Unmapped { .. }))));
        assert_eq!(state, before);

        let error = try_exec(&mut state, &memory, &[0xC3]); // ret
        assert!(matches!(error, Err(ExecError::Memory(MemoryError::Unmapped { .. }))));
        assert_eq!(state, before);

        let error = try_exec(&mut state, &memory, &[0xE8, 0x00, 0x00, 0x00, 0x00]); // call
        assert!(matches!(error, Err(ExecError::Memory(MemoryError::Unmapped { .. }))));
        assert_eq!(state, before);
    }

    #[test]
    fn xadd_on_the_same_register_doubles_it() {
        let (mut state, memory) = machine();
        state.gpr[0] = 5;
        exec(&mut state, &memory, &[0x0F, 0xC1, 0xC0]); // xadd eax, eax
        assert_eq!(state.gpr[0], 10);
    }

    #[test]
    fn compare_exchange_mismatch_still_writes_the_destination() {
        let (mut state, memory) = machine();
        state.gpr[0] = 1; // accumulator differs from the read-only cell (0)
        state.gpr[3] = 7;
        // cmpxchg [READ_ONLY], ebx must fault even though the compare fails.
        let error = try_exec(&mut state, &memory, &[0x0F, 0xB1, 0x1D, 0x00, 0x00, 0x03, 0x00]);
        assert!(matches!(error, Err(ExecError::Memory(MemoryError::AccessDenied { .. }))));
        assert_eq!(state.gpr[0], 1, "the accumulator must stay intact after the fault");
    }

    #[test]
    fn zero_count_shifts_still_access_the_operand() {
        let (mut state, memory) = machine();
        // shl dword ptr [READ_ONLY], 32: the masked count is zero, but the
        // read-modify-write access still needs write permission.
        let error = try_exec(&mut state, &memory, &[0xC1, 0x25, 0x00, 0x00, 0x03, 0x00, 0x20]);
        assert!(matches!(error, Err(ExecError::Memory(MemoryError::AccessDenied { .. }))));
    }

    #[test]
    fn oversized_narrow_shifts_preserve_the_carry_flag() {
        let (mut state, memory) = machine();
        state.gpr[0] = 0xFF;
        exec(&mut state, &memory, &[0xF9]); // stc
        exec(&mut state, &memory, &[0xC0, 0xE0, 0x09]); // shl al, 9
        assert_eq!(state.gpr[0] & 0xFF, 0);
        assert_ne!(
            state.eflags & flags::CARRY,
            0,
            "carry is undefined at counts past the width and must be preserved"
        );
    }

    #[test]
    fn sixteen_bit_addressing_is_rejected() {
        let (mut state, memory) = machine();
        let error = try_exec(&mut state, &memory, &[0x67, 0x8B, 0x07]); // mov eax, [bx]
        assert!(matches!(error, Err(ExecError::Unsupported { .. })));
    }

    #[test]
    fn pause_and_wide_nops_execute() {
        let (mut state, memory) = machine();
        state.gpr[0] = UNMAPPED; // a wide NOP operand must never be accessed
        exec(&mut state, &memory, &[0xF3, 0x90]); // pause
        exec(&mut state, &memory, &[0x0F, 0x1F, 0x40, 0x00]); // nop dword ptr [eax]
        assert_eq!(state.eip, CODE + 6);
    }

    #[test]
    fn high_byte_registers_read_and_write() {
        let (mut state, memory) = machine();
        state.gpr[0] = 0x0000_0034; // AL = 0x34
        exec(&mut state, &memory, &[0xB4, 0x12]); // mov ah, 0x12
        assert_eq!(state.gpr[0], 0x0000_1234);
        exec(&mut state, &memory, &[0x00, 0xC4]); // add ah, al
        assert_eq!(state.gpr[0], 0x0000_4634);
    }

    /// The tier-0 interpreter and the legacy register-only oracle must agree
    /// on the overlap subset for identical inputs.
    #[test]
    fn register_only_overlap_matches_the_legacy_oracle() {
        let programs: &[&[u8]] = &[
            &[0x89, 0xD8],                   // mov eax, ebx
            &[0xB9, 0x78, 0x56, 0x34, 0x12], // mov ecx, imm32
            &[0x01, 0xCB],                   // add ebx, ecx
            &[0x83, 0xE8, 0x01],             // sub eax, 1
            &[0x21, 0xD1],                   // and ecx, edx
            &[0x09, 0xF7],                   // or edi, esi
            &[0x31, 0xC0],                   // xor eax, eax
        ];

        let mut seed = 0x1357_9BDF_u32;
        for program in programs {
            for _ in 0..8 {
                let (mut state_a, memory) = machine();
                for slot in &mut state_a.gpr {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    *slot = seed;
                }
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                state_a.eflags = (seed & flags::ARITHMETIC) | 0x2;
                state_a.eip = CODE;
                let mut state_b = state_a.clone();

                exec(&mut state_a, &memory, program);
                let block = crate::BasicBlockDecoder::default()
                    .decode(GuestVa(state_b.eip), program)
                    .expect("program decodes");
                crate::step_register_only(&mut state_b, &block.instructions[0])
                    .expect("oracle executes");
                // The legacy oracle does not model the time-stamp counter.
                state_b.tsc = state_a.tsc;

                assert_eq!(state_a, state_b, "tiers diverged on {program:02X?}");
            }
        }
    }
}
