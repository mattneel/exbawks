//! IRQL exports.
//!
//! Under the cooperative single-processor scheduler (ADR 0011) the IRQL is
//! pure bookkeeping: nothing preempts a running guest thread, so raising and
//! lowering only maintain the KPCR's `Irql` field (offset `0x24`, reached
//! through the thread's `fs` base) for the guest's own reads.

use exbawks_types::GuestVa;

use crate::{KernelCallContext, KernelError, KernelExport, KernelRegistry, KernelStatus};

/// `KPCR.Irql` (Xbox layout).
const KPCR_IRQL_OFFSET: u32 = 0x24;
/// `DISPATCH_LEVEL`.
const DISPATCH_LEVEL: u32 = 2;

/// Registers the IRQL exports.
pub(crate) fn register_irql_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(KfRaiseIrql)?;
    registry.register(KfLowerIrql)?;
    registry.register(KeGetCurrentIrql)?;
    registry.register(KeRaiseIrqlToDpcLevel)?;
    Ok(())
}

/// The active thread's `KPCR.Irql` cell, through the `fs` base.
fn irql_cell(context: &KernelCallContext<'_>) -> GuestVa {
    let kpcr = context.cpu.segments[exbawks_cpu::Segment::Fs as usize].base;
    GuestVa(kpcr.wrapping_add(KPCR_IRQL_OFFSET))
}

/// Sets the IRQL and returns the previous level.
fn exchange_irql(context: &mut KernelCallContext<'_>, new_level: u32) -> KernelStatus {
    let cell = irql_cell(context);
    let previous = context.memory.read_u32(cell).unwrap_or(0) & 0xFF;
    let _ = context.memory.write_u32(cell, new_level & 0xFF);
    // The previous IRQL returns in AL.
    KernelStatus(previous)
}

/// Raises the IRQL (fastcall: the new level arrives in `cl`).
#[derive(Debug, Default, Clone, Copy)]
pub struct KfRaiseIrql;

impl KernelExport for KfRaiseIrql {
    fn ordinal(&self) -> u16 {
        crate::ordinal::KF_RAISE_IRQL
    }

    fn name(&self) -> &'static str {
        "KfRaiseIrql"
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let new_level = context.cpu.gpr[1] & 0xFF;
        exchange_irql(context, new_level)
    }
}

/// Lowers the IRQL (fastcall: the new level arrives in `cl`).
#[derive(Debug, Default, Clone, Copy)]
pub struct KfLowerIrql;

impl KernelExport for KfLowerIrql {
    fn ordinal(&self) -> u16 {
        crate::ordinal::KF_LOWER_IRQL
    }

    fn name(&self) -> &'static str {
        "KfLowerIrql"
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let new_level = context.cpu.gpr[1] & 0xFF;
        let cell = irql_cell(context);
        let _ = context.memory.write_u32(cell, new_level);
        KernelStatus::SUCCESS
    }
}

/// Reads the current IRQL into AL.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeGetCurrentIrql;

impl KernelExport for KeGetCurrentIrql {
    fn ordinal(&self) -> u16 {
        crate::ordinal::KE_GET_CURRENT_IRQL
    }

    fn name(&self) -> &'static str {
        "KeGetCurrentIrql"
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let level = context.memory.read_u32(irql_cell(context)).unwrap_or(0) & 0xFF;
        KernelStatus(level)
    }
}

/// Raises the IRQL to `DISPATCH_LEVEL`, returning the previous level.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeRaiseIrqlToDpcLevel;

impl KernelExport for KeRaiseIrqlToDpcLevel {
    fn ordinal(&self) -> u16 {
        crate::ordinal::KE_RAISE_IRQL_TO_DPC_LEVEL
    }

    fn name(&self) -> &'static str {
        "KeRaiseIrqlToDpcLevel"
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        exchange_irql(context, DISPATCH_LEVEL)
    }
}
