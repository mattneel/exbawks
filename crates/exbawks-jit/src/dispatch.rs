//! The single unsafe entry into sealed code buffers.
#![allow(unsafe_code)]

use exbawks_cpu::CpuState;

use crate::{BlockExit, EmittedBlock, JitError};

/// The block entry signature fixed by ADR 0006.
type BlockEntry = unsafe extern "C" fn(*mut CpuState) -> u64;

/// Enters emitted blocks and decodes their structured exits.
#[derive(Debug, Default, Clone, Copy)]
pub struct Dispatcher;

impl Dispatcher {
    /// Runs one emitted block against guest CPU state.
    pub fn run(&self, block: &EmittedBlock, state: &mut CpuState) -> Result<BlockExit, JitError> {
        let code = block.code();
        if code.is_empty() {
            return Err(JitError::EmptyBlock);
        }

        // SAFETY: The sealed buffer holds one complete block emitted by
        // DirectEmitter under the ADR 0006 ABI at offset zero, aligned for
        // execution, and it stays mapped execute-read while `block` lives.
        let entry: BlockEntry = unsafe { core::mem::transmute(code.base()) };
        // SAFETY: The emitted block only reads and writes the passed CpuState
        // value, preserves nonvolatile host registers, makes no calls, and
        // returns through its epilogue on this thread.
        let raw = unsafe { entry(core::ptr::from_mut(state)) };

        BlockExit::from_raw(raw).ok_or(JitError::MalformedExit { value: raw })
    }
}
