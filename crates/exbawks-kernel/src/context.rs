use exbawks_cpu::CpuState;
use exbawks_memory::GuestMemory;
use exbawks_types::StopReason;

/// Mutable state that one kernel HLE export can access.
pub struct KernelCallContext<'a> {
    /// The guest CPU state at the HLE boundary.
    pub cpu: &'a mut CpuState,
    /// Checked access to the active guest address space.
    pub memory: &'a dyn GuestMemory,
    /// A controlled stop that the runtime honors after the call.
    pub stop_request: Option<StopReason>,
}
