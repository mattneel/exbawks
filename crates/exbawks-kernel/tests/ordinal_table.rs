//! Anchors for the generated kernel export ordinal table.
//!
//! The table is machine-generated from the vendored CC0 nxdk def file; these
//! tests pin the decode rules and the anchors the runtime depends on, so a
//! generator or def-file regression fails loudly instead of corrupting the
//! guest stack through a wrong `stack_bytes` value.

use exbawks_kernel::{
    CallingConvention, ExportKind, KERNEL_ORDINALS, KernelExport, kernel_ordinal_info, ordinal,
};

/// One entry per export in the def file; re-pin when the def updates.
#[test]
fn table_holds_every_def_export() {
    assert_eq!(KERNEL_ORDINALS.len(), 371);
}

#[test]
fn table_is_strictly_sorted_by_ordinal() {
    for pair in KERNEL_ORDINALS.windows(2) {
        assert!(pair[0].ordinal < pair[1].ordinal, "unsorted at ordinal {}", pair[1].ordinal);
    }
}

#[test]
fn boundary_anchors_match_public_sources() {
    for (ordinal, name) in [
        (1, "AvGetSavedDataAddress"),
        (8, "DbgPrint"),
        (49, "HalReturnToFirmware"),
        (161, "KfLowerIrql"),
        (255, "PsCreateSystemThreadEx"),
        (328, "XeUnloadSection"),
        (360, "HalInitiateShutdown"),
    ] {
        let info = kernel_ordinal_info(ordinal).expect("anchor ordinal must exist");
        assert_eq!(info.name, name, "ordinal {ordinal}");
    }
}

/// Fastcall `@Name@N` counts ECX/EDX bytes first; N <= 8 pops nothing.
#[test]
fn fastcall_exports_pop_only_stack_arguments() {
    for ordinal in [87, 160, 161, 250] {
        let info = kernel_ordinal_info(ordinal).expect("fastcall ordinal must exist");
        assert_eq!(info.convention, Some(CallingConvention::Fastcall), "ordinal {ordinal}");
        assert_eq!(info.stack_bytes, 0, "ordinal {ordinal}");
    }
}

#[test]
fn data_exports_carry_no_convention() {
    for ordinal in [16, 22, 30, 31, 156, 164, 326] {
        let info = kernel_ordinal_info(ordinal).expect("data ordinal must exist");
        assert_eq!(info.kind, ExportKind::Data, "ordinal {ordinal}");
        assert_eq!(info.convention, None, "ordinal {ordinal}");
        assert_eq!(info.stack_bytes, 0, "ordinal {ordinal}");
    }
}

#[test]
fn cdecl_exports_pop_nothing() {
    let info = kernel_ordinal_info(ordinal::DBG_PRINT).expect("DbgPrint must exist");
    assert_eq!(info.convention, Some(CallingConvention::Cdecl));
    assert_eq!(info.stack_bytes, 0);
}

/// Every ordinal the startup registry serves must agree with the table.
#[test]
fn startup_registrations_match_the_table() {
    for (ordinal, name) in [
        (ordinal::DBG_PRINT, "DbgPrint"),
        (ordinal::HAL_RETURN_TO_FIRMWARE, "HalReturnToFirmware"),
        (ordinal::KE_DELAY_EXECUTION_THREAD, "KeDelayExecutionThread"),
        (ordinal::KE_SET_TIMER, "KeSetTimer"),
        (ordinal::NT_ALLOCATE_VIRTUAL_MEMORY, "NtAllocateVirtualMemory"),
        (ordinal::NT_CLOSE, "NtClose"),
        (ordinal::NT_CREATE_EVENT, "NtCreateEvent"),
        (ordinal::NT_CREATE_FILE, "NtCreateFile"),
        (ordinal::NT_FREE_VIRTUAL_MEMORY, "NtFreeVirtualMemory"),
        (ordinal::NT_SET_EVENT, "NtSetEvent"),
        (ordinal::PS_CREATE_SYSTEM_THREAD_EX, "PsCreateSystemThreadEx"),
        (ordinal::PS_TERMINATE_SYSTEM_THREAD, "PsTerminateSystemThread"),
    ] {
        let info = kernel_ordinal_info(ordinal).expect("registered ordinal must exist");
        assert_eq!(info.name, name, "ordinal {ordinal}");
    }
}

/// The implemented exports declare the stack bytes the table records.
#[test]
fn implemented_exports_agree_on_stack_bytes() {
    let hal = exbawks_kernel::HalReturnToFirmware;
    let info = kernel_ordinal_info(hal.ordinal()).expect("HalReturnToFirmware must exist");
    assert_eq!(info.stack_bytes, hal.stack_bytes());

    let dbg = exbawks_kernel::DbgPrint;
    let info = kernel_ordinal_info(dbg.ordinal()).expect("DbgPrint must exist");
    assert_eq!(info.stack_bytes, dbg.stack_bytes());
}
