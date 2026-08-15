use exbawks_cpu::{DecodedBlock, format_instruction};
use exbawks_types::{BackendKind, GuestVa};
use iced_x86::{FlowControl, InstructionInfoFactory};

/// The required lowering path for one guest instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteClass {
    /// The instruction is a native emission candidate.
    NativeCandidate,
    /// The instruction accesses guest memory.
    Memory,
    /// The instruction ends or changes control flow.
    ControlFlow,
}

/// One classified instruction in a translation plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationAction {
    /// The guest instruction address.
    pub guest_ip: GuestVa,
    /// The guest instruction length.
    pub length: usize,
    /// The formatted guest instruction.
    pub text: String,
    /// The selected rewrite class.
    pub class: RewriteClass,
}

/// A backend-neutral first-pass translation plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationPlan {
    /// The selected backend.
    pub backend: BackendKind,
    /// The guest block start.
    pub guest_start: GuestVa,
    /// The classified instructions.
    pub actions: Vec<TranslationAction>,
}

/// The direct `iced-x86` rewrite planner.
#[derive(Debug, Default, Clone, Copy)]
pub struct DirectRewritePlanner;

impl DirectRewritePlanner {
    /// Classifies every instruction in one decoded block.
    #[must_use]
    pub fn plan(&self, block: &DecodedBlock) -> TranslationPlan {
        let mut information = InstructionInfoFactory::new();
        let mut actions = Vec::with_capacity(block.instructions.len());

        for instruction in &block.instructions {
            let info = information.info(instruction);
            let class = if instruction.flow_control() != FlowControl::Next {
                RewriteClass::ControlFlow
            } else if !info.used_memory().is_empty() {
                RewriteClass::Memory
            } else {
                RewriteClass::NativeCandidate
            };

            actions.push(TranslationAction {
                guest_ip: GuestVa(u32::try_from(instruction.ip()).unwrap_or(u32::MAX)),
                length: instruction.len(),
                text: format_instruction(instruction),
                class,
            });
        }

        TranslationPlan { backend: BackendKind::DirectRewrite, guest_start: block.start, actions }
    }
}

#[cfg(test)]
mod tests {
    use exbawks_cpu::BasicBlockDecoder;

    use super::*;

    #[test]
    fn planner_separates_memory_and_control_flow() {
        let block = BasicBlockDecoder::default()
            .decode(GuestVa(0x1000), &[0x8B, 0x01, 0xC3])
            .expect("block must decode");
        let plan = DirectRewritePlanner.plan(&block);

        assert_eq!(plan.actions[0].class, RewriteClass::Memory);
        assert_eq!(plan.actions[1].class, RewriteClass::ControlFlow);
    }
}
