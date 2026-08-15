#![deny(unsafe_code)]
#![doc = "Translation planning, code emission, and cache services for Exbawks."]

mod backend;
mod cache;
mod dispatch;
mod emit;
mod error;
mod exit;
mod plan;

pub use backend::{
    CodegenBackend, CompilationState, CompiledBlock, CraneliftBackend, DirectRewriteBackend,
};
pub use cache::{BlockKey, CachedBlock, CodeCache, PhysicalPageDependency};
pub use dispatch::Dispatcher;
pub use emit::{DirectEmitter, EmittedBlock};
pub use error::JitError;
pub use exit::BlockExit;
pub use plan::{DirectRewritePlanner, RewriteClass, TranslationAction, TranslationPlan};
