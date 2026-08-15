use exbawks_types::{BackendKind, BuildFlavor, GuestVa};
use serde::{Deserialize, Serialize};

use crate::EntryBlockPlan;

/// A serializable action from one entry-block translation plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationActionReport {
    /// The guest instruction address.
    pub address: GuestVa,
    /// The decoded instruction length.
    pub length: usize,
    /// The formatted instruction.
    pub instruction: String,
    /// The selected lowering class.
    pub class: String,
}

/// A serializable report for the initial XBE boot plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootPlanReport {
    /// The guest image base.
    pub image_base: GuestVa,
    /// The decoded entry point.
    pub entry_point: GuestVa,
    /// The decoded kernel thunk table address.
    pub kernel_thunk_address: GuestVa,
    /// The detected XBE flavor.
    pub build_flavor: BuildFlavor,
    /// The number of XBE sections.
    pub section_count: usize,
    /// The selected translation backend.
    pub backend: BackendKind,
    /// The block byte count.
    pub decoded_bytes: usize,
    /// The block instruction count.
    pub decoded_instructions: usize,
    /// The condition that stopped decoding.
    pub block_stop: String,
    /// The current compilation artifact state.
    pub compilation_state: String,
    /// Per-instruction translation actions.
    pub actions: Vec<TranslationActionReport>,
}

impl BootPlanReport {
    pub(crate) fn from_plan(plan: &EntryBlockPlan) -> Self {
        let image = plan.image.image();
        let actions = plan
            .compiled
            .plan
            .actions
            .iter()
            .map(|action| TranslationActionReport {
                address: action.guest_ip,
                length: action.length,
                instruction: action.text.clone(),
                class: format!("{:?}", action.class),
            })
            .collect();

        Self {
            image_base: image.header.base_address,
            entry_point: image.header.entry_point,
            kernel_thunk_address: image.header.kernel_thunk_address,
            build_flavor: image.header.build_flavor,
            section_count: image.sections.len(),
            backend: plan.compiled.plan.backend,
            decoded_bytes: plan.decoded.byte_len,
            decoded_instructions: plan.decoded.instructions.len(),
            block_stop: format!("{:?}", plan.decoded.stop),
            compilation_state: format!("{:?}", plan.compiled.state),
            actions,
        }
    }
}
