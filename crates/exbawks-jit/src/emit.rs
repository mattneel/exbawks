use exbawks_cpu::{
    AluOp, CpuState, DecodedBlock, Gpr, RegisterOp, RegisterOperand, classify_register_op, flags,
};
use exbawks_debug::{BlockSourceMap, SourceRange};
use exbawks_platform::{ExecutableCodeBuffer, WritableCodeBuffer};
use exbawks_types::GuestVa;

use crate::{BlockExit, JitError};

// The emitter encodes every CpuState field with one-byte displacements.
const _: () = assert!(CpuState::gpr_offset(Gpr::Edi) < 0x80);
const _: () = assert!(CpuState::EIP_OFFSET < 0x80);
const _: () = assert!(CpuState::EFLAGS_OFFSET < 0x80);

/// One sealed executable block under the ADR 0006 ABI.
#[derive(Debug)]
pub struct EmittedBlock {
    code: ExecutableCodeBuffer,
    bytes: Box<[u8]>,
    guest_start: GuestVa,
    exit_eip: GuestVa,
    static_exit: BlockExit,
    translated_instructions: usize,
    source_map: BlockSourceMap,
}

impl EmittedBlock {
    /// Returns the emitted machine-code bytes.
    #[must_use]
    pub fn machine_code(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the guest block start.
    #[must_use]
    pub const fn guest_start(&self) -> GuestVa {
        self.guest_start
    }

    /// Returns the guest EIP value the epilogue writes.
    #[must_use]
    pub const fn exit_eip(&self) -> GuestVa {
        self.exit_eip
    }

    /// Returns the exit kind the epilogue returns.
    #[must_use]
    pub const fn static_exit(&self) -> BlockExit {
        self.static_exit
    }

    /// Returns the number of translated guest instructions.
    #[must_use]
    pub const fn translated_instructions(&self) -> usize {
        self.translated_instructions
    }

    /// Returns the sorted source and fault metadata.
    #[must_use]
    pub const fn source_map(&self) -> &BlockSourceMap {
        &self.source_map
    }

    pub(crate) const fn code(&self) -> &ExecutableCodeBuffer {
        &self.code
    }
}

/// Emits register-only blocks under the ADR 0006 ABI.
#[derive(Debug, Default, Clone, Copy)]
pub struct DirectEmitter;

impl DirectEmitter {
    /// Emits and seals one decoded block.
    ///
    /// The emitter translates the register-only prefix of the block. The
    /// epilogue reports the first untranslated instruction, or the direct
    /// successor when every instruction translated.
    pub fn emit(&self, block: &DecodedBlock) -> Result<EmittedBlock, JitError> {
        if block.instructions.is_empty() {
            return Err(JitError::EmptyBlock);
        }

        let mut assembler = BlockAssembler::default();
        let mut ranges = Vec::new();
        let mut translated_instructions = 0_usize;
        let mut static_exit = BlockExit::DirectSuccessor;
        let mut exit_eip = block.end().ok_or(JitError::BlockEndOverflow { start: block.start })?;

        for instruction in &block.instructions {
            let Some(op) = classify_register_op(instruction) else {
                static_exit = BlockExit::UnsupportedInstruction;
                exit_eip = GuestVa(u32::try_from(instruction.ip()).unwrap_or(u32::MAX));
                break;
            };

            let host_start = host_offset(block.start, assembler.len())?;
            assembler.register_op(op);
            ranges.push(SourceRange {
                host_start,
                host_end: host_offset(block.start, assembler.len())?,
                guest_ip: GuestVa(u32::try_from(instruction.ip()).unwrap_or(u32::MAX)),
                guest_len: host_offset(block.start, instruction.len())?,
            });
            translated_instructions += 1;
        }

        assembler.epilogue(exit_eip, static_exit);
        let bytes = assembler.finish();
        let source_map = BlockSourceMap::new(ranges, Vec::new())?;

        let mut buffer = WritableCodeBuffer::new(bytes.len())?;
        buffer.push(&bytes)?;
        let code = buffer.seal()?;

        Ok(EmittedBlock {
            code,
            bytes: bytes.into_boxed_slice(),
            guest_start: block.start,
            exit_eip,
            static_exit,
            translated_instructions,
            source_map,
        })
    }
}

/// Converts one host offset into the 32-bit metadata range.
fn host_offset(start: GuestVa, value: usize) -> Result<u32, JitError> {
    u32::try_from(value).map_err(|_| JitError::MetadataOverflow { start })
}

/// Assembles host x86-64 bytes with `RCX` as the `CpuState` pointer.
#[derive(Debug, Default)]
struct BlockAssembler {
    bytes: Vec<u8>,
}

impl BlockAssembler {
    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn register_op(&mut self, op: RegisterOp) {
        match op {
            // One host nop keeps a non-empty source range for the guest nop.
            RegisterOp::Nop => self.bytes.push(0x90),
            RegisterOp::Mov { dst, src } => {
                self.load_eax(src);
                self.store_gpr_eax(dst);
            }
            RegisterOp::Alu { op, dst, src } => {
                self.load_gpr_eax(dst);
                match src {
                    RegisterOperand::Gpr(source) => {
                        self.load_gpr_edx(source);
                        // ALU eax, edx.
                        self.bytes.extend([alu_register_opcode(op), 0xD0]);
                    }
                    RegisterOperand::Immediate(value) => {
                        // ALU eax, imm32 through the 0x81 group.
                        self.bytes.extend([0x81, alu_immediate_modrm(op)]);
                        self.bytes.extend(value.to_le_bytes());
                    }
                }
                // pushfq captures the host flags of the ALU result.
                self.bytes.push(0x9C);
                self.store_gpr_eax(dst);
                self.merge_flags(defined_flags(op));
            }
        }
    }

    fn load_eax(&mut self, src: RegisterOperand) {
        match src {
            RegisterOperand::Gpr(source) => self.load_gpr_eax(source),
            RegisterOperand::Immediate(value) => {
                // mov eax, imm32.
                self.bytes.push(0xB8);
                self.bytes.extend(value.to_le_bytes());
            }
        }
    }

    fn load_gpr_eax(&mut self, register: Gpr) {
        // mov eax, [rcx + disp8].
        self.bytes.extend([0x8B, 0x41, gpr_disp(register)]);
    }

    fn load_gpr_edx(&mut self, register: Gpr) {
        // mov edx, [rcx + disp8].
        self.bytes.extend([0x8B, 0x51, gpr_disp(register)]);
    }

    fn store_gpr_eax(&mut self, register: Gpr) {
        // mov [rcx + disp8], eax.
        self.bytes.extend([0x89, 0x41, gpr_disp(register)]);
    }

    /// Merges the pushed host flags into the guest EFLAGS under one mask.
    fn merge_flags(&mut self, mask: u32) {
        // pop rdx.
        self.bytes.push(0x5A);
        // and edx, mask.
        self.bytes.extend([0x81, 0xE2]);
        self.bytes.extend(mask.to_le_bytes());
        // mov eax, [rcx + eflags].
        self.bytes.extend([0x8B, 0x41, eflags_disp()]);
        // and eax, !mask.
        self.bytes.push(0x25);
        self.bytes.extend((!mask).to_le_bytes());
        // or eax, edx.
        self.bytes.extend([0x09, 0xD0]);
        // mov [rcx + eflags], eax.
        self.bytes.extend([0x89, 0x41, eflags_disp()]);
    }

    fn epilogue(&mut self, exit_eip: GuestVa, exit: BlockExit) {
        // mov dword [rcx + eip], exit_eip.
        self.bytes.extend([0xC7, 0x41, eip_disp()]);
        self.bytes.extend(exit_eip.0.to_le_bytes());
        // mov eax, exit code.
        self.bytes.push(0xB8);
        self.bytes.extend(exit.to_raw().to_le_bytes());
        // ret.
        self.bytes.push(0xC3);
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

const fn alu_register_opcode(op: AluOp) -> u8 {
    match op {
        AluOp::Add => 0x01,
        AluOp::Or => 0x09,
        AluOp::And => 0x21,
        AluOp::Sub => 0x29,
        AluOp::Xor => 0x31,
    }
}

const fn alu_immediate_modrm(op: AluOp) -> u8 {
    match op {
        AluOp::Add => 0xC0,
        AluOp::Or => 0xC8,
        AluOp::And => 0xE0,
        AluOp::Sub => 0xE8,
        AluOp::Xor => 0xF0,
    }
}

const fn defined_flags(op: AluOp) -> u32 {
    match op {
        AluOp::Add | AluOp::Sub => flags::ARITHMETIC,
        AluOp::And | AluOp::Or | AluOp::Xor => flags::LOGIC_DEFINED,
    }
}

#[expect(clippy::cast_possible_truncation, reason = "offsets are asserted below 0x80")]
const fn gpr_disp(register: Gpr) -> u8 {
    CpuState::gpr_offset(register) as u8
}

#[expect(clippy::cast_possible_truncation, reason = "offsets are asserted below 0x80")]
const fn eip_disp() -> u8 {
    CpuState::EIP_OFFSET as u8
}

#[expect(clippy::cast_possible_truncation, reason = "offsets are asserted below 0x80")]
const fn eflags_disp() -> u8 {
    CpuState::EFLAGS_OFFSET as u8
}
