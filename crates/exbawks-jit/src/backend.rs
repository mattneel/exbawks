use exbawks_cpu::DecodedBlock;
use exbawks_types::BackendKind;

use crate::{DirectRewritePlanner, JitError, TranslationPlan};

/// The state of a compiled block artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilationState {
    /// The block contains analysis but no machine code.
    Planned,
    /// The block contains executable machine code.
    Executable,
}

/// A translation artifact stored by the code cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledBlock {
    /// The translation plan.
    pub plan: TranslationPlan,
    /// Host machine-code bytes.
    pub machine_code: Box<[u8]>,
    /// The current artifact state.
    pub state: CompilationState,
}

/// A dynamic code-generation backend.
pub trait CodegenBackend: Send + Sync {
    /// Returns the backend identifier.
    fn kind(&self) -> BackendKind;

    /// Compiles or plans one decoded block.
    fn compile(&self, block: &DecodedBlock) -> Result<CompiledBlock, JitError>;
}

/// The first direct `iced-x86` rewrite backend.
#[derive(Debug, Default, Clone, Copy)]
pub struct DirectRewriteBackend {
    planner: DirectRewritePlanner,
}

impl CodegenBackend for DirectRewriteBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::DirectRewrite
    }

    fn compile(&self, block: &DecodedBlock) -> Result<CompiledBlock, JitError> {
        if block.instructions.is_empty() {
            return Err(JitError::EmptyBlock);
        }

        Ok(CompiledBlock {
            plan: self.planner.plan(block),
            machine_code: Box::default(),
            state: CompilationState::Planned,
        })
    }
}

/// A placeholder backend for later Cranelift lowering.
#[derive(Debug, Default, Clone, Copy)]
pub struct CraneliftBackend;

impl CodegenBackend for CraneliftBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Cranelift
    }

    fn compile(&self, _block: &DecodedBlock) -> Result<CompiledBlock, JitError> {
        Err(JitError::BackendUnavailable {
            backend: BackendKind::Cranelift,
            reason: "the normalized guest IR and Cranelift emitter are not implemented",
        })
    }
}
