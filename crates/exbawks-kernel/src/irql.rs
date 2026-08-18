//! IRQL exports.
//!
//! Under the cooperative single-processor scheduler (ADR 0011) the IRQL is
//! pure bookkeeping: nothing preempts a running guest thread, so raising and
//! lowering only maintain the KPCR's `Irql` field (offset `0x24`, reached
//! through the thread's `fs` base) for the guest's own reads.

use exbawks_types::GuestVa;

use crate::startup::stack_argument;
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
    registry.register(HalGetInterruptVector)?;
    registry.register(KeInitializeInterrupt)?;
    registry.register(KeConnectInterrupt)?;
    registry.register(KeDisconnectInterrupt)?;
    registry.register(HalReadWritePCISpace)?;
    Ok(())
}

/// Reads or writes PCI configuration space.
///
/// Reads serve a synthetic NV2A identity (vendor `0x10DE`, device `0x02A0`)
/// at register 0 and zeros elsewhere; writes are ignored. Titles probe the
/// GPU's identity during Direct3D initialization.
#[derive(Debug, Default, Clone, Copy)]
pub struct HalReadWritePCISpace;

impl KernelExport for HalReadWritePCISpace {
    fn ordinal(&self) -> u16 {
        crate::ordinal::HAL_READ_WRITE_PCI_SPACE
    }

    fn name(&self) -> &'static str {
        "HalReadWritePCISpace"
    }

    fn stack_bytes(&self) -> u16 {
        24
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        use crate::startup::stack_argument;

        // HalReadWritePCISpace(BusNumber, SlotNumber, RegisterNumber,
        //                      Buffer, Length, WritePCISpace).
        let register = stack_argument(context, 2).unwrap_or(0);
        let buffer = stack_argument(context, 3).unwrap_or(0);
        let length = stack_argument(context, 4).unwrap_or(0);
        let write = stack_argument(context, 5).unwrap_or(0) & 0xFF != 0;
        if write || buffer == 0 {
            return KernelStatus::SUCCESS;
        }

        // A synthetic NV2A config space: identity, enabled command
        // register, and the register/framebuffer BARs a driver reads to
        // locate the device. Zeros elsewhere.
        fn config_byte(offset: u32) -> u8 {
            const NV2A_ID: u32 = 0x02A0_10DE;
            /// I/O, memory, and bus-master enable.
            const COMMAND: u32 = 0x0000_0007;
            /// BAR0: the register block (32-bit memory BAR).
            const BAR0: u32 = 0xFD00_0000;
            /// BAR1: the RAM/framebuffer aperture (prefetchable).
            const BAR1: u32 = 0xF000_0008;
            let (dword, byte) = (offset / 4, (offset % 4) as usize);
            match dword {
                0 => NV2A_ID.to_le_bytes()[byte],
                1 => COMMAND.to_le_bytes()[byte],
                4 => BAR0.to_le_bytes()[byte],
                5 => BAR1.to_le_bytes()[byte],
                _ => 0,
            }
        }
        let capped = length.min(256);
        for offset in 0..capped {
            let byte = config_byte(register.wrapping_add(offset));
            let _ = context.memory.write(GuestVa(buffer + offset), &[byte]);
        }
        KernelStatus::SUCCESS
    }
}

/// Fills a guest `KINTERRUPT` object.
///
/// The caller's service routine is recorded here, because a modelled
/// device that raises an interrupt has to call it: a title's USB stack
/// connects an interrupt and then does nothing until one arrives.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeInitializeInterrupt;

impl KernelExport for KeInitializeInterrupt {
    fn ordinal(&self) -> u16 {
        crate::ordinal::KE_INITIALIZE_INTERRUPT
    }

    fn name(&self) -> &'static str {
        "KeInitializeInterrupt"
    }

    fn stack_bytes(&self) -> u16 {
        28
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // KeInitializeInterrupt(Interrupt, ServiceRoutine, ServiceContext,
        //                       Vector, Irql, InterruptMode, ShareVector)
        // Argument zero is the first: `[esp]` holds the return address.
        let object = stack_argument(context, 0).unwrap_or(0);
        let routine = stack_argument(context, 1).unwrap_or(0);
        let service_context = stack_argument(context, 2).unwrap_or(0);
        let vector = stack_argument(context, 3).unwrap_or(0);
        if object != 0 && routine != 0 {
            context.services.set_interrupt_routine(crate::InterruptRoutine {
                object,
                routine,
                context: service_context,
                vector,
            });
        }
        KernelStatus::SUCCESS
    }
}

/// Connects a guest interrupt object, reporting success (BOOLEAN TRUE).
#[derive(Debug, Default, Clone, Copy)]
pub struct KeConnectInterrupt;

impl KernelExport for KeConnectInterrupt {
    fn ordinal(&self) -> u16 {
        crate::ordinal::KE_CONNECT_INTERRUPT
    }

    fn name(&self) -> &'static str {
        "KeConnectInterrupt"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        let object = stack_argument(context, 0).unwrap_or(0);
        if object != 0 {
            context.services.connect_interrupt(object, true);
        }
        // BOOLEAN TRUE in AL: the interrupt is connected.
        KernelStatus(1)
    }
}

/// Disconnects a guest interrupt object, reporting success (BOOLEAN TRUE).
#[derive(Debug, Default, Clone, Copy)]
pub struct KeDisconnectInterrupt;

impl KernelExport for KeDisconnectInterrupt {
    fn ordinal(&self) -> u16 {
        crate::ordinal::KE_DISCONNECT_INTERRUPT
    }

    fn name(&self) -> &'static str {
        "KeDisconnectInterrupt"
    }

    fn stack_bytes(&self) -> u16 {
        4
    }

    fn call(&self, _context: &mut KernelCallContext<'_>) -> KernelStatus {
        // BOOLEAN TRUE: the interrupt was "disconnected" (it never fired).
        KernelStatus(1)
    }
}

/// Maps a bus interrupt level to a vector and its IRQL.
///
/// No interrupts are ever delivered (devices are HLE), so the mapping only
/// has to be self-consistent: the Xbox HAL returns `0x30 + level` with IRQL
/// descending from `DISPATCH_LEVEL + …` — drivers store both and proceed.
#[derive(Debug, Default, Clone, Copy)]
pub struct HalGetInterruptVector;

impl KernelExport for HalGetInterruptVector {
    fn ordinal(&self) -> u16 {
        crate::ordinal::HAL_GET_INTERRUPT_VECTOR
    }

    fn name(&self) -> &'static str {
        "HalGetInterruptVector"
    }

    fn stack_bytes(&self) -> u16 {
        8
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        use crate::startup::stack_argument;

        let level = stack_argument(context, 0).unwrap_or(0) & 0xFF;
        let irql_out = stack_argument(context, 1).unwrap_or(0);
        if irql_out != 0 {
            // IRQL above DISPATCH_LEVEL, descending with the level as on
            // the console's PIC layout.
            let irql = 26_u32.saturating_sub(level).max(DISPATCH_LEVEL + 1);
            let _ = context.memory.write_u32(GuestVa(irql_out), irql);
        }
        KernelStatus(0x30 + level)
    }
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
