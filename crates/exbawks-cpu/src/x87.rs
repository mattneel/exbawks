//! The x87 floating-point unit, as the interpreter models it.
//!
//! ADR 0018 settles the representation: the register stack holds `f64`, and
//! conversion happens where a value crosses the memory boundary. That is
//! exact for everything a title can observe — it stores single or double
//! precision, and both survive a round trip — and gives up the eleven extra
//! mantissa bits of the hardware's 80-bit format, which only a long
//! register-resident computation could notice.
//!
//! The stack is a stack, not eight independent registers: `st(0)` is
//! whichever register the status word's top field points at, and a load
//! pushes by decrementing it. Getting that wrong shows up as a title
//! reading its own values back shifted by one register.

use iced_x86::{Instruction, MemorySize, Mnemonic, OpKind, Register};

use exbawks_types::GuestVa;

use crate::exec::{ExecError, GuestMemoryRef, effective_address_for};
use crate::state::CpuState;

/// The status word's stack-top field.
const TOP_SHIFT: u16 = 11;
const TOP_MASK: u16 = 0x3800;
/// The status word's condition-code bits, which a comparison writes.
const C0: u16 = 0x0100;
const C1: u16 = 0x0200;
const C2: u16 = 0x0400;
const C3: u16 = 0x4000;
/// The status word's invalid-operation and stack-fault flags.
const INVALID: u16 = 0x0001;
const STACK_FAULT: u16 = 0x0040;

/// The tag word's code for a register holding nothing.
const TAG_EMPTY: u16 = 0x3;

impl CpuState {
    /// Which physical register `st(index)` names.
    fn physical(&self, index: usize) -> usize {
        let top = ((self.x87.status & TOP_MASK) >> TOP_SHIFT) as usize;
        (top + index) & 7
    }

    /// Reads one stack register.
    fn read_st(&self, index: usize) -> f64 {
        let slot = self.x87.registers[self.physical(index)];
        f64::from_le_bytes([slot[0], slot[1], slot[2], slot[3], slot[4], slot[5], slot[6], slot[7]])
    }

    /// Writes one stack register and marks it as holding a value.
    fn write_st(&mut self, index: usize, value: f64) {
        let physical = self.physical(index);
        let bytes = value.to_le_bytes();
        self.x87.registers[physical][..8].copy_from_slice(&bytes);
        // Two bits of tag per register, and zero means a value is there.
        self.x87.tag &= !(TAG_EMPTY << (physical * 2));
    }

    /// Pushes a value, as a load does.
    fn push_st(&mut self, value: f64) {
        let top = (self.x87.status & TOP_MASK) >> TOP_SHIFT;
        let next = (top.wrapping_sub(1)) & 7;
        self.x87.status = (self.x87.status & !TOP_MASK) | (next << TOP_SHIFT);
        self.write_st(0, value);
    }

    /// Pops the top register, marking it empty.
    fn pop_st(&mut self) {
        let physical = self.physical(0);
        self.x87.tag |= TAG_EMPTY << (physical * 2);
        let top = (self.x87.status & TOP_MASK) >> TOP_SHIFT;
        let next = (top.wrapping_add(1)) & 7;
        self.x87.status = (self.x87.status & !TOP_MASK) | (next << TOP_SHIFT);
    }

    /// Records a comparison's result in the condition-code bits.
    fn set_compare(&mut self, ordering: Option<std::cmp::Ordering>) {
        self.x87.status &= !(C0 | C1 | C2 | C3);
        match ordering {
            Some(std::cmp::Ordering::Less) => self.x87.status |= C0,
            Some(std::cmp::Ordering::Equal) => self.x87.status |= C3,
            Some(std::cmp::Ordering::Greater) => {}
            // Unordered: a comparison involving a NaN sets all three.
            None => self.x87.status |= C0 | C2 | C3,
        }
    }
}

/// The operand that is not the implicit top of the stack.
///
/// Some forms decode with the top written out — `fxch st(1)` arrives as
/// two operands, `st(0)` and `st(1)` — so taking the first would name the
/// top and make the instruction a no-op.
fn other_operand(instruction: &Instruction) -> Option<u32> {
    match instruction.op_count() {
        0 => None,
        count => Some(count - 1),
    }
}

/// The stack index a register operand names, if it names one.
fn stack_index(register: Register) -> Option<usize> {
    let index = match register {
        Register::ST0 => 0,
        Register::ST1 => 1,
        Register::ST2 => 2,
        Register::ST3 => 3,
        Register::ST4 => 4,
        Register::ST5 => 5,
        Register::ST6 => 6,
        Register::ST7 => 7,
        _ => return None,
    };
    Some(index)
}

/// Reads a floating-point or integer value from a memory operand.
fn read_memory_value(
    memory: GuestMemoryRef<'_>,
    location: GuestVa,
    size: MemorySize,
    address: GuestVa,
) -> Result<f64, ExecError> {
    let mut bytes = [0_u8; 10];
    let take = match size {
        MemorySize::Float32 | MemorySize::Int32 => 4,
        MemorySize::Float64 | MemorySize::Int64 => 8,
        MemorySize::Float80 => 10,
        MemorySize::Int16 => 2,
        _ => return Err(ExecError::Unsupported { address }),
    };
    memory.read(location, &mut bytes[..take])?;
    Ok(match size {
        MemorySize::Float32 => {
            f64::from(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        }
        MemorySize::Float64 => f64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]),
        MemorySize::Float80 => extended_to_f64(&bytes),
        MemorySize::Int16 => f64::from(i16::from_le_bytes([bytes[0], bytes[1]])),
        MemorySize::Int32 => {
            f64::from(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        }
        MemorySize::Int64 => i64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]) as f64,
        _ => return Err(ExecError::Unsupported { address }),
    })
}

/// Writes a value to a memory operand in that operand's own format.
fn write_memory_value(
    memory: GuestMemoryRef<'_>,
    location: GuestVa,
    size: MemorySize,
    value: f64,
    address: GuestVa,
) -> Result<(), ExecError> {
    match size {
        MemorySize::Float32 => memory.write(location, &(value as f32).to_le_bytes())?,
        MemorySize::Float64 => memory.write(location, &value.to_le_bytes())?,
        MemorySize::Float80 => memory.write(location, &f64_to_extended(value))?,
        // An integer store rounds to nearest, as the default control word
        // asks, and saturates rather than wrapping on overflow.
        MemorySize::Int16 => {
            let rounded = round_to_nearest_even(value);
            let clamped = rounded.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
            memory.write(location, &clamped.to_le_bytes())?;
        }
        MemorySize::Int32 => {
            let rounded = round_to_nearest_even(value);
            let clamped = rounded.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
            memory.write(location, &clamped.to_le_bytes())?;
        }
        MemorySize::Int64 => {
            let rounded = round_to_nearest_even(value);
            let clamped = rounded.clamp(i64::MIN as f64, i64::MAX as f64) as i64;
            memory.write(location, &clamped.to_le_bytes())?;
        }
        _ => return Err(ExecError::Unsupported { address }),
    }
    Ok(())
}

/// Rounds half to even, which is what the default control word selects.
fn round_to_nearest_even(value: f64) -> f64 {
    let nearest = value.round();
    if (value - value.trunc()).abs() == 0.5 && nearest % 2.0 != 0.0 {
        nearest - value.signum()
    } else {
        nearest
    }
}

/// Widens an 80-bit extended value to `f64`.
fn extended_to_f64(bytes: &[u8; 10]) -> f64 {
    let mantissa = u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);
    let sign_exponent = u16::from_le_bytes([bytes[8], bytes[9]]);
    let sign = if sign_exponent & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from(sign_exponent & 0x7FFF);
    if exponent == 0 && mantissa == 0 {
        return sign * 0.0;
    }
    if exponent == 0x7FFF {
        // The explicit integer bit distinguishes an infinity from a NaN.
        return if mantissa & 0x7FFF_FFFF_FFFF_FFFF == 0 { sign * f64::INFINITY } else { f64::NAN };
    }
    // The extended format carries its leading bit explicitly, so the
    // mantissa is a whole number scaled by the exponent's bias.
    sign * (mantissa as f64) * exp2(exponent - 16383 - 63)
}

/// Narrows an `f64` to the 80-bit extended layout.
fn f64_to_extended(value: f64) -> [u8; 10] {
    let mut bytes = [0_u8; 10];
    let sign = if value.is_sign_negative() { 0x8000_u16 } else { 0 };
    if value == 0.0 {
        bytes[8..].copy_from_slice(&sign.to_le_bytes());
        return bytes;
    }
    if value.is_nan() {
        bytes[..8].copy_from_slice(&0xC000_0000_0000_0000_u64.to_le_bytes());
        bytes[8..].copy_from_slice(&(sign | 0x7FFF).to_le_bytes());
        return bytes;
    }
    if value.is_infinite() {
        bytes[..8].copy_from_slice(&0x8000_0000_0000_0000_u64.to_le_bytes());
        bytes[8..].copy_from_slice(&(sign | 0x7FFF).to_le_bytes());
        return bytes;
    }
    let bits = value.abs().to_bits();
    let raw_exponent = ((bits >> 52) & 0x7FF) as i32;
    let fraction = bits & 0x000F_FFFF_FFFF_FFFF;
    let (exponent, mantissa) = if raw_exponent == 0 {
        // A subnormal has no implicit leading bit; normalize it up.
        let shift = fraction.leading_zeros() - 11;
        (-1022 - i32::from(shift as u16) + 16383, (fraction << (shift + 1)) << 11)
    } else {
        (raw_exponent - 1023 + 16383, (1_u64 << 63) | (fraction << 11))
    };
    bytes[..8].copy_from_slice(&mantissa.to_le_bytes());
    let exponent = exponent.clamp(0, 0x7FFE) as u16;
    bytes[8..].copy_from_slice(&(sign | exponent).to_le_bytes());
    bytes
}

/// Two raised to an integer power, without a `powi` rounding surprise.
fn exp2(exponent: i32) -> f64 {
    if exponent >= 1024 {
        return f64::INFINITY;
    }
    if exponent <= -1075 {
        return 0.0;
    }
    // Split the shift so an extreme exponent still lands on a real value.
    let mut result = 1.0_f64;
    let mut remaining = exponent;
    while remaining > 1000 {
        result *= f64::from(1_u32 << 30) * f64::from(1_u32 << 30);
        remaining -= 60;
    }
    while remaining < -1000 {
        result /= f64::from(1_u32 << 30) * f64::from(1_u32 << 30);
        remaining += 60;
    }
    result * (remaining as f64).exp2()
}

/// Executes one x87 instruction.
///
/// Returns `None` when the mnemonic is not one this unit owns, so the
/// caller can carry on with the integer decode.
pub(crate) fn execute(
    state: &mut CpuState,
    memory: GuestMemoryRef<'_>,
    instruction: &Instruction,
    address: GuestVa,
) -> Option<Result<(), ExecError>> {
    let mnemonic = instruction.mnemonic();
    if !owns(mnemonic) {
        return None;
    }
    Some(run(state, memory, instruction, address, mnemonic))
}

/// Whether this unit executes a mnemonic.
fn owns(mnemonic: Mnemonic) -> bool {
    use Mnemonic::{
        F2xm1, Fabs, Fadd, Faddp, Fchs, Fcom, Fcomi, Fcomip, Fcomp, Fcompp, Fcos, Fdiv, Fdivp,
        Fdivr, Fdivrp, Fiadd, Ficom, Ficomp, Fidiv, Fidivr, Fild, Fimul, Fist, Fistp, Fisub,
        Fisubr, Fld, Fld1, Fldl2e, Fldl2t, Fldlg2, Fldln2, Fldpi, Fldz, Fmul, Fmulp, Fnclex,
        Fninit, Fnstcw, Fnstsw, Fpatan, Fprem, Fptan, Frndint, Fscale, Fsin, Fsincos, Fsqrt, Fst,
        Fstp, Fsub, Fsubp, Fsubr, Fsubrp, Ftst, Fucom, Fucomi, Fucomip, Fucomp, Fucompp, Fxam,
        Fxch, Fyl2x,
    };
    matches!(
        mnemonic,
        Fsincos
            | Fld
            | Fst
            | Fstp
            | Fild
            | Fist
            | Fistp
            | Fld1
            | Fldz
            | Fldpi
            | Fldl2e
            | Fldl2t
            | Fldlg2
            | Fldln2
            | Fadd
            | Faddp
            | Fiadd
            | Fsub
            | Fsubp
            | Fsubr
            | Fsubrp
            | Fisub
            | Fisubr
            | Fmul
            | Fmulp
            | Fimul
            | Fdiv
            | Fdivp
            | Fdivr
            | Fdivrp
            | Fidiv
            | Fidivr
            | Fchs
            | Fabs
            | Fsqrt
            | Frndint
            | Fscale
            | Fprem
            | Fcom
            | Fcomp
            | Fcompp
            | Ficom
            | Ficomp
            | Fucom
            | Fucomp
            | Fucompp
            | Fcomi
            | Fcomip
            | Fucomi
            | Fucomip
            | Ftst
            | Fxam
            | Fxch
            | Fsin
            | Fcos
            | Fptan
            | Fpatan
            | Fyl2x
            | F2xm1
            | Fninit
            | Fnclex
            | Fnstsw
            | Fnstcw
    )
}

/// The value an arithmetic instruction's source operand holds.
fn source_value(
    state: &CpuState,
    memory: GuestMemoryRef<'_>,
    instruction: &Instruction,
    address: GuestVa,
    index: u32,
) -> Result<f64, ExecError> {
    match instruction.op_kind(index) {
        OpKind::Register => {
            let register = instruction.op_register(index);
            let stack = stack_index(register).ok_or(ExecError::Unsupported { address })?;
            Ok(state.read_st(stack))
        }
        OpKind::Memory => {
            let location = effective_address_for(state, instruction, address)?;
            read_memory_value(memory, location, instruction.memory_size(), address)
        }
        _ => Err(ExecError::Unsupported { address }),
    }
}

#[allow(clippy::too_many_lines)]
fn run(
    state: &mut CpuState,
    memory: GuestMemoryRef<'_>,
    instruction: &Instruction,
    address: GuestVa,
    mnemonic: Mnemonic,
) -> Result<(), ExecError> {
    use Mnemonic as M;

    // The control instructions touch no stack register.
    match mnemonic {
        M::Fninit => {
            state.x87 = crate::X87State::default();
            return Ok(());
        }
        M::Fnclex => {
            state.x87.status &= !0x80FF;
            return Ok(());
        }
        M::Fnstsw => {
            if instruction.op0_kind() == OpKind::Register {
                state.gpr[0] = (state.gpr[0] & 0xFFFF_0000) | u32::from(state.x87.status);
            } else {
                let location = effective_address_for(state, instruction, address)?;
                memory.write(location, &state.x87.status.to_le_bytes())?;
            }
            return Ok(());
        }
        M::Fnstcw => {
            let location = effective_address_for(state, instruction, address)?;
            memory.write(location, &state.x87.control.to_le_bytes())?;
            return Ok(());
        }
        _ => {}
    }

    // Every fallible memory access happens before the stack moves, so a
    // fault leaves the unit exactly as it was and the instruction can be
    // restarted.
    match mnemonic {
        M::Fld | M::Fild => {
            let value = source_value(state, memory, instruction, address, 0)?;
            state.push_st(value);
        }
        M::Fld1 => state.push_st(1.0),
        M::Fldz => state.push_st(0.0),
        M::Fldpi => state.push_st(std::f64::consts::PI),
        M::Fldl2e => state.push_st(std::f64::consts::LOG2_E),
        M::Fldl2t => state.push_st(std::f64::consts::LOG2_10),
        M::Fldlg2 => state.push_st(std::f64::consts::LOG10_2),
        M::Fldln2 => state.push_st(std::f64::consts::LN_2),

        M::Fst | M::Fstp | M::Fist | M::Fistp => {
            let value = state.read_st(0);
            match instruction.op0_kind() {
                OpKind::Memory => {
                    let location = effective_address_for(state, instruction, address)?;
                    write_memory_value(
                        memory,
                        location,
                        instruction.memory_size(),
                        value,
                        address,
                    )?;
                }
                OpKind::Register => {
                    let stack = stack_index(instruction.op0_register())
                        .ok_or(ExecError::Unsupported { address })?;
                    state.write_st(stack, value);
                }
                _ => return Err(ExecError::Unsupported { address }),
            }
            if matches!(mnemonic, M::Fstp | M::Fistp) {
                state.pop_st();
            }
        }

        M::Fadd | M::Fsub | M::Fsubr | M::Fmul | M::Fdiv | M::Fdivr => {
            // With two operands the first names the destination; with one
            // the destination is the top of the stack.
            let (destination, left, right) = if instruction.op_count() == 2 {
                let destination = stack_index(instruction.op0_register())
                    .ok_or(ExecError::Unsupported { address })?;
                let other = source_value(state, memory, instruction, address, 1)?;
                (destination, state.read_st(destination), other)
            } else {
                let other = source_value(state, memory, instruction, address, 0)?;
                (0, state.read_st(0), other)
            };
            state.write_st(destination, arithmetic(mnemonic, left, right));
        }
        M::Fiadd | M::Fisub | M::Fisubr | M::Fimul | M::Fidiv | M::Fidivr => {
            let other = source_value(state, memory, instruction, address, 0)?;
            let result = arithmetic(integer_form(mnemonic), state.read_st(0), other);
            state.write_st(0, result);
        }
        M::Faddp | M::Fsubp | M::Fsubrp | M::Fmulp | M::Fdivp | M::Fdivrp => {
            let destination = if instruction.op_count() == 2 {
                stack_index(instruction.op0_register()).ok_or(ExecError::Unsupported { address })?
            } else {
                1
            };
            let result =
                arithmetic(popping_form(mnemonic), state.read_st(destination), state.read_st(0));
            state.write_st(destination, result);
            state.pop_st();
        }

        M::Fchs => {
            let value = state.read_st(0);
            state.write_st(0, -value);
        }
        M::Fabs => {
            let value = state.read_st(0);
            state.write_st(0, value.abs());
        }
        M::Fsqrt => {
            let value = state.read_st(0);
            state.write_st(0, value.sqrt());
        }
        M::Frndint => {
            let value = state.read_st(0);
            state.write_st(0, round_to_nearest_even(value));
        }
        M::Fscale => {
            let scale = state.read_st(1);
            let value = state.read_st(0);
            state.write_st(0, value * exp2(scale.trunc() as i32));
        }
        M::Fprem => {
            let divisor = state.read_st(1);
            let value = state.read_st(0);
            let result = if divisor == 0.0 { f64::NAN } else { value % divisor };
            state.x87.status &= !C2;
            state.write_st(0, result);
        }
        M::Fsin => {
            let value = state.read_st(0);
            state.write_st(0, value.sin());
            state.x87.status &= !C2;
        }
        M::Fsincos => {
            // The sine replaces st(0) and the cosine is pushed above it,
            // leaving cos in st(0) and sin in st(1).
            let value = state.read_st(0);
            state.write_st(0, value.sin());
            state.push_st(value.cos());
            state.x87.status &= !C2;
        }
        M::Fcos => {
            let value = state.read_st(0);
            state.write_st(0, value.cos());
            state.x87.status &= !C2;
        }
        M::Fptan => {
            let value = state.read_st(0);
            state.write_st(0, value.tan());
            state.push_st(1.0);
            state.x87.status &= !C2;
        }
        M::Fpatan => {
            let numerator = state.read_st(1);
            let denominator = state.read_st(0);
            state.write_st(1, numerator.atan2(denominator));
            state.pop_st();
        }
        M::Fyl2x => {
            let multiplier = state.read_st(1);
            let value = state.read_st(0);
            state.write_st(1, multiplier * value.log2());
            state.pop_st();
        }
        M::F2xm1 => {
            let value = state.read_st(0);
            state.write_st(0, value.exp2() - 1.0);
        }

        M::Fcom | M::Fcomp | M::Fucom | M::Fucomp | M::Ficom | M::Ficomp => {
            let other = match other_operand(instruction) {
                None => state.read_st(1),
                Some(index) => source_value(state, memory, instruction, address, index)?,
            };
            let ordering = state.read_st(0).partial_cmp(&other);
            state.set_compare(ordering);
            if matches!(mnemonic, M::Fcomp | M::Fucomp | M::Ficomp) {
                state.pop_st();
            }
        }
        M::Fcompp | M::Fucompp => {
            let ordering = state.read_st(0).partial_cmp(&state.read_st(1));
            state.set_compare(ordering);
            state.pop_st();
            state.pop_st();
        }
        M::Ftst => {
            let ordering = state.read_st(0).partial_cmp(&0.0);
            state.set_compare(ordering);
        }
        M::Fxam => {
            // Report the class of the top register: the interpreter tracks
            // enough of the tag word to tell empty from a real value.
            let value = state.read_st(0);
            state.x87.status &= !(C0 | C1 | C2 | C3);
            if value.is_sign_negative() {
                state.x87.status |= C1;
            }
            if value.is_nan() {
                state.x87.status |= C0;
            } else if value.is_infinite() {
                state.x87.status |= C0 | C2;
            } else if value == 0.0 {
                state.x87.status |= C3;
            } else {
                state.x87.status |= C2;
            }
        }

        // The comparing forms that write the integer flags instead.
        M::Fcomi | M::Fcomip | M::Fucomi | M::Fucomip => {
            let index = other_operand(instruction).ok_or(ExecError::Unsupported { address })?;
            let other = source_value(state, memory, instruction, address, index)?;
            let value = state.read_st(0);
            let (zero, parity, carry) = match value.partial_cmp(&other) {
                Some(std::cmp::Ordering::Equal) => (true, false, false),
                Some(std::cmp::Ordering::Less) => (false, false, true),
                Some(std::cmp::Ordering::Greater) => (false, false, false),
                None => (true, true, true),
            };
            let mut flags =
                state.eflags & !(crate::flags::ZERO | crate::flags::PARITY | crate::flags::CARRY);
            if zero {
                flags |= crate::flags::ZERO;
            }
            if parity {
                flags |= crate::flags::PARITY;
            }
            if carry {
                flags |= crate::flags::CARRY;
            }
            // These clear the overflow, adjust, and sign flags outright.
            state.eflags =
                flags & !(crate::flags::OVERFLOW | crate::flags::AUXILIARY | crate::flags::SIGN);
            if matches!(mnemonic, M::Fcomip | M::Fucomip) {
                state.pop_st();
            }
        }

        M::Fxch => {
            let other = match other_operand(instruction) {
                None => 1,
                Some(index) => stack_index(instruction.op_register(index))
                    .ok_or(ExecError::Unsupported { address })?,
            };
            let top = state.read_st(0);
            let swapped = state.read_st(other);
            state.write_st(0, swapped);
            state.write_st(other, top);
        }

        _ => return Err(ExecError::Unsupported { address }),
    }

    // A stack that has overflowed or underflowed is a guest error, not an
    // emulator one; the flags say so and execution continues.
    if state.x87.status & STACK_FAULT != 0 {
        state.x87.status |= INVALID;
    }
    Ok(())
}

/// The operation an arithmetic mnemonic performs.
fn arithmetic(mnemonic: Mnemonic, left: f64, right: f64) -> f64 {
    match mnemonic {
        Mnemonic::Fadd => left + right,
        Mnemonic::Fsub => left - right,
        Mnemonic::Fsubr => right - left,
        Mnemonic::Fmul => left * right,
        Mnemonic::Fdiv => left / right,
        Mnemonic::Fdivr => right / left,
        _ => f64::NAN,
    }
}

/// The plain form an integer-operand mnemonic corresponds to.
fn integer_form(mnemonic: Mnemonic) -> Mnemonic {
    match mnemonic {
        Mnemonic::Fiadd => Mnemonic::Fadd,
        Mnemonic::Fisub => Mnemonic::Fsub,
        Mnemonic::Fisubr => Mnemonic::Fsubr,
        Mnemonic::Fimul => Mnemonic::Fmul,
        Mnemonic::Fidiv => Mnemonic::Fdiv,
        _ => Mnemonic::Fdivr,
    }
}

/// The plain form a popping mnemonic corresponds to.
fn popping_form(mnemonic: Mnemonic) -> Mnemonic {
    match mnemonic {
        Mnemonic::Faddp => Mnemonic::Fadd,
        Mnemonic::Fsubp => Mnemonic::Fsub,
        Mnemonic::Fsubrp => Mnemonic::Fsubr,
        Mnemonic::Fmulp => Mnemonic::Fmul,
        Mnemonic::Fdivp => Mnemonic::Fdiv,
        _ => Mnemonic::Fdivr,
    }
}
