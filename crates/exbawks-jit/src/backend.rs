use exbawks_cpu::DecodedBlock;
use exbawks_platform::PlatformError;
use exbawks_types::BackendKind;

use crate::{DirectEmitter, DirectRewritePlanner, EmittedBlock, JitError, TranslationPlan};

/// The state of a compiled block artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilationState {
    /// The block contains analysis but no machine code.
    Planned,
    /// The block contains executable machine code.
    Executable,
}

/// A translation artifact stored by the code cache.
#[derive(Debug)]
pub struct CompiledBlock {
    /// The translation plan.
    pub plan: TranslationPlan,
    /// Host machine-code bytes.
    pub machine_code: Box<[u8]>,
    /// The current artifact state.
    pub state: CompilationState,
    /// The sealed executable block on supported hosts.
    pub executable: Option<EmittedBlock>,
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
    emitter: DirectEmitter,
}

impl CodegenBackend for DirectRewriteBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::DirectRewrite
    }

    fn compile(&self, block: &DecodedBlock) -> Result<CompiledBlock, JitError> {
        if block.instructions.is_empty() {
            return Err(JitError::EmptyBlock);
        }

        let plan = self.planner.plan(block);
        match self.emitter.emit(block) {
            Ok(emitted) => Ok(CompiledBlock {
                plan,
                machine_code: emitted.machine_code().into(),
                state: CompilationState::Executable,
                executable: Some(emitted),
            }),
            // Hosts without executable-memory support keep the plan.
            Err(JitError::Platform(PlatformError::Unsupported(_))) => Ok(CompiledBlock {
                plan,
                machine_code: Box::default(),
                state: CompilationState::Planned,
                executable: None,
            }),
            Err(error) => Err(error),
        }
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
