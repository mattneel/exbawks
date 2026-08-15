use exbawks_types::GuestVa;
use iced_x86::{Code, Instruction, Mnemonic, OpKind, Register};

use crate::Gpr;

/// An arithmetic or logical operation in the register-only subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AluOp {
    /// 32-bit addition.
    Add,
    /// 32-bit subtraction.
    Sub,
    /// Bitwise AND.
    And,
    /// Bitwise OR.
    Or,
    /// Bitwise XOR.
    Xor,
}

/// One register or immediate source operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterOperand {
    /// A 32-bit general-purpose register.
    Gpr(Gpr),
    /// A 32-bit immediate value.
    Immediate(u32),
}

/// One instruction in the approved register-only subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterOp {
    /// A single-byte no-operation.
    Nop,
    /// A 32-bit register move.
    Mov {
        /// The destination register.
        dst: Gpr,
        /// The source operand.
        src: RegisterOperand,
    },
    /// A 32-bit arithmetic or logical operation.
    Alu {
        /// The operation.
        op: AluOp,
        /// The destination register.
        dst: Gpr,
        /// The source operand.
        src: RegisterOperand,
    },
}

/// Classifies one instruction against the register-only subset.
#[must_use]
pub fn classify_register_op(instruction: &Instruction) -> Option<RegisterOp> {
    match instruction.mnemonic() {
        Mnemonic::Nop if instruction.op_count() == 0 => Some(RegisterOp::Nop),
        Mnemonic::Mov => {
            Some(RegisterOp::Mov { dst: gpr32(instruction, 0)?, src: source(instruction)? })
        }
        Mnemonic::Add => alu(AluOp::Add, instruction),
        Mnemonic::Sub => alu(AluOp::Sub, instruction),
        Mnemonic::And => alu(AluOp::And, instruction),
        Mnemonic::Or => alu(AluOp::Or, instruction),
        Mnemonic::Xor => alu(AluOp::Xor, instruction),
        _ => None,
    }
}

/// Returns the pointer slot of one absolute near indirect call.
///
/// Matches only the 32-bit near form `call dword ptr [disp32]`
/// (`Code::Call_rm32`) that patched kernel thunk calls use. The far
/// indirect form `call m16:32` (`FF /3`) shares the `Call` mnemonic but has
/// different stack semantics, so it is intentionally rejected.
#[must_use]
pub fn indirect_call_slot(instruction: &Instruction) -> Option<GuestVa> {
    if instruction.code() != Code::Call_rm32 || instruction.op0_kind() != OpKind::Memory {
        return None;
    }
    if instruction.memory_base() != Register::None || instruction.memory_index() != Register::None {
        return None;
    }

    u32::try_from(instruction.memory_displacement64()).ok().map(GuestVa)
}

fn alu(op: AluOp, instruction: &Instruction) -> Option<RegisterOp> {
    Some(RegisterOp::Alu { op, dst: gpr32(instruction, 0)?, src: source(instruction)? })
}

fn gpr32(instruction: &Instruction, operand: u32) -> Option<Gpr> {
    if instruction.op_kind(operand) != OpKind::Register {
        return None;
    }
    gpr_from_register(instruction.op_register(operand))
}

fn source(instruction: &Instruction) -> Option<RegisterOperand> {
    if instruction.op_count() != 2 {
        return None;
    }

    match instruction.op_kind(1) {
        OpKind::Register => gpr_from_register(instruction.op_register(1)).map(RegisterOperand::Gpr),
        OpKind::Immediate32 => Some(RegisterOperand::Immediate(instruction.immediate32())),
        OpKind::Immediate8to32 => {
            Some(RegisterOperand::Immediate(instruction.immediate8to32() as u32))
        }
        _ => None,
    }
}

const fn gpr_from_register(register: Register) -> Option<Gpr> {
    match register {
        Register::EAX => Some(Gpr::Eax),
        Register::ECX => Some(Gpr::Ecx),
        Register::EDX => Some(Gpr::Edx),
        Register::EBX => Some(Gpr::Ebx),
        Register::ESP => Some(Gpr::Esp),
        Register::EBP => Some(Gpr::Ebp),
        Register::ESI => Some(Gpr::Esi),
        Register::EDI => Some(Gpr::Edi),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use exbawks_types::GuestVa;

    use crate::BasicBlockDecoder;

    use super::*;

    fn classify(bytes: &[u8]) -> Option<RegisterOp> {
        let block =
            BasicBlockDecoder::default().decode(GuestVa(0x1000), bytes).expect("block decodes");
        classify_register_op(&block.instructions[0])
    }

    #[test]
    fn subset_accepts_register_and_immediate_forms() {
        assert_eq!(classify(&[0x90]), Some(RegisterOp::Nop));
        assert_eq!(
            classify(&[0x89, 0xD8]),
            Some(RegisterOp::Mov { dst: Gpr::Eax, src: RegisterOperand::Gpr(Gpr::Ebx) })
        );
        assert_eq!(
            classify(&[0xB9, 0x78, 0x56, 0x34, 0x12]),
            Some(RegisterOp::Mov { dst: Gpr::Ecx, src: RegisterOperand::Immediate(0x1234_5678) })
        );
        assert_eq!(
            classify(&[0x01, 0xCB]),
            Some(RegisterOp::Alu {
                op: AluOp::Add,
                dst: Gpr::Ebx,
                src: RegisterOperand::Gpr(Gpr::Ecx)
            })
        );
        assert_eq!(
            classify(&[0x83, 0xE8, 0x01]),
            Some(RegisterOp::Alu {
                op: AluOp::Sub,
                dst: Gpr::Eax,
                src: RegisterOperand::Immediate(1)
            })
        );
        assert_eq!(
            classify(&[0x83, 0xC6, 0xFF]),
            Some(RegisterOp::Alu {
                op: AluOp::Add,
                dst: Gpr::Esi,
                src: RegisterOperand::Immediate(0xFFFF_FFFF)
            })
        );
    }

    #[test]
    fn subset_rejects_memory_control_flow_and_narrow_forms() {
        assert_eq!(classify(&[0x8B, 0x01]), None);
        assert_eq!(classify(&[0xC3]), None);
        assert_eq!(classify(&[0x66, 0x01, 0xCB]), None);
        assert_eq!(classify(&[0x00, 0xCB]), None);
    }

    fn first(bytes: &[u8]) -> iced_x86::Instruction {
        BasicBlockDecoder::default().decode(GuestVa(0x1000), bytes).expect("decodes").instructions
            [0]
    }

    #[test]
    fn indirect_call_slot_matches_only_the_near_form() {
        // FF /2: call dword ptr [0x00011200] (near indirect).
        let near = first(&[0xFF, 0x15, 0x00, 0x12, 0x01, 0x00]);
        assert_eq!(indirect_call_slot(&near), Some(GuestVa(0x0001_1200)));

        // FF /3: call fword ptr [0x00011200] (far indirect) must not match.
        let far = first(&[0xFF, 0x1D, 0x00, 0x12, 0x01, 0x00]);
        assert_eq!(indirect_call_slot(&far), None);

        // FF /2 with a register base is not a bare [disp32] slot.
        let based = first(&[0xFF, 0x10]);
        assert_eq!(indirect_call_slot(&based), None);

        // A register-only instruction is not a call.
        let mov = first(&[0x89, 0xD8]);
        assert_eq!(indirect_call_slot(&mov), None);
    }
}
