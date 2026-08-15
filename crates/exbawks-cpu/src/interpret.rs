use exbawks_types::GuestVa;
use iced_x86::Instruction;
use thiserror::Error;

use crate::{AluOp, CpuState, RegisterOp, RegisterOperand, classify_register_op, flags};

/// A register-only interpretation failure.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum InterpretError {
    /// The instruction is outside the register-only subset.
    #[error("instruction at {address} is outside the register-only subset")]
    Unsupported {
        /// The guest instruction address.
        address: GuestVa,
    },
}

/// Executes one register-only instruction and advances the guest EIP.
///
/// This interpreter is the flag oracle for the direct emitter.
pub fn step_register_only(
    state: &mut CpuState,
    instruction: &Instruction,
) -> Result<(), InterpretError> {
    let op = classify_register_op(instruction).ok_or(InterpretError::Unsupported {
        address: GuestVa(u32::try_from(instruction.ip()).unwrap_or(u32::MAX)),
    })?;

    apply(state, op);
    state.eip = state.eip.wrapping_add(instruction.len() as u32);
    Ok(())
}

fn apply(state: &mut CpuState, op: RegisterOp) {
    match op {
        RegisterOp::Nop => {}
        RegisterOp::Mov { dst, src } => {
            let value = read(state, src);
            state.set(dst, value);
        }
        RegisterOp::Alu { op, dst, src } => {
            let a = state.get(dst);
            let b = read(state, src);
            let (result, defined, value_flags) = match op {
                AluOp::Add => {
                    let result = a.wrapping_add(b);
                    (result, flags::ARITHMETIC, add_flags(a, b, result))
                }
                AluOp::Sub => {
                    let result = a.wrapping_sub(b);
                    (result, flags::ARITHMETIC, sub_flags(a, b, result))
                }
                AluOp::And => logic(a & b),
                AluOp::Or => logic(a | b),
                AluOp::Xor => logic(a ^ b),
            };

            state.set(dst, result);
            state.eflags = (state.eflags & !defined) | (value_flags & defined);
        }
    }
}

const fn read(state: &CpuState, operand: RegisterOperand) -> u32 {
    match operand {
        RegisterOperand::Gpr(register) => state.get(register),
        RegisterOperand::Immediate(value) => value,
    }
}

const fn logic(result: u32) -> (u32, u32, u32) {
    (result, flags::LOGIC_DEFINED, result_flags(result))
}

const fn result_flags(result: u32) -> u32 {
    let mut value = 0;
    if result == 0 {
        value |= flags::ZERO;
    }
    if result & 0x8000_0000 != 0 {
        value |= flags::SIGN;
    }
    if (result as u8).count_ones().is_multiple_of(2) {
        value |= flags::PARITY;
    }
    value
}

const fn add_flags(a: u32, b: u32, result: u32) -> u32 {
    let mut value = result_flags(result);
    if result < a {
        value |= flags::CARRY;
    }
    if (a ^ b ^ result) & 0x10 != 0 {
        value |= flags::AUXILIARY;
    }
    if (a ^ result) & (b ^ result) & 0x8000_0000 != 0 {
        value |= flags::OVERFLOW;
    }
    value
}

const fn sub_flags(a: u32, b: u32, result: u32) -> u32 {
    let mut value = result_flags(result);
    if a < b {
        value |= flags::CARRY;
    }
    if (a ^ b ^ result) & 0x10 != 0 {
        value |= flags::AUXILIARY;
    }
    if (a ^ b) & (a ^ result) & 0x8000_0000 != 0 {
        value |= flags::OVERFLOW;
    }
    value
}

#[cfg(test)]
mod tests {
    use crate::BasicBlockDecoder;

    use super::*;

    fn step(state: &mut CpuState, bytes: &[u8]) {
        let block =
            BasicBlockDecoder::default().decode(GuestVa(state.eip), bytes).expect("block decodes");
        step_register_only(state, &block.instructions[0]).expect("instruction is supported");
    }

    #[test]
    fn add_carry_produces_zero_carry_auxiliary_and_parity() {
        let mut state = CpuState { gpr: [0xFFFF_FFFF, 1, 0, 0, 0, 0, 0, 0], ..CpuState::default() };
        // add eax, ecx
        step(&mut state, &[0x01, 0xC8]);

        assert_eq!(state.gpr[0], 0);
        assert_eq!(
            state.eflags & flags::ARITHMETIC,
            flags::CARRY | flags::PARITY | flags::AUXILIARY | flags::ZERO
        );
        assert_eq!(state.eip, 2);
    }

    #[test]
    fn signed_overflow_sets_overflow_and_sign() {
        let mut state = CpuState { gpr: [0x7FFF_FFFF, 0, 0, 0, 0, 0, 0, 0], ..CpuState::default() };
        // add eax, 1
        step(&mut state, &[0x83, 0xC0, 0x01]);

        assert_eq!(state.gpr[0], 0x8000_0000);
        assert_eq!(
            state.eflags & flags::ARITHMETIC,
            flags::PARITY | flags::AUXILIARY | flags::SIGN | flags::OVERFLOW
        );
    }

    #[test]
    fn subtraction_borrow_sets_carry_and_sign() {
        let mut state = CpuState::default();
        // sub eax, 1
        step(&mut state, &[0x83, 0xE8, 0x01]);

        assert_eq!(state.gpr[0], 0xFFFF_FFFF);
        assert_eq!(
            state.eflags & flags::ARITHMETIC,
            flags::CARRY | flags::PARITY | flags::AUXILIARY | flags::SIGN
        );
    }

    #[test]
    fn logical_operations_preserve_the_auxiliary_flag() {
        let mut state = CpuState::default();
        state.eflags |= flags::AUXILIARY | flags::CARRY;
        // xor eax, eax
        step(&mut state, &[0x31, 0xC0]);

        assert_eq!(state.gpr[0], 0);
        assert_eq!(
            state.eflags & flags::ARITHMETIC,
            flags::AUXILIARY | flags::PARITY | flags::ZERO
        );
    }
}
