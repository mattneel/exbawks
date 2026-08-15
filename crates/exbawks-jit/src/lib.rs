#![forbid(unsafe_code)]
#![doc = "Translation planning and code-cache services for Exbawks."]

mod backend;
mod cache;
mod error;
mod plan;

pub use backend::{
    CodegenBackend, CompilationState, CompiledBlock, CraneliftBackend, DirectRewriteBackend,
};
pub use cache::{BlockKey, CachedBlock, CodeCache, PhysicalPageDependency};
pub use error::JitError;
pub use plan::{DirectRewritePlanner, RewriteClass, TranslationAction, TranslationPlan};
