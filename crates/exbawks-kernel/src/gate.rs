use exbawks_types::GuestVa;

/// The first guest address of the kernel dispatch gate region.
///
/// The loader patches every kernel thunk slot with one gate address. The
/// region stays unmapped, so the runtime alone can interpret gate targets,
/// and stray guest accesses fail with typed memory errors.
pub const KERNEL_GATE_BASE: u32 = 0xFF80_0000;

/// The exclusive end of the kernel dispatch gate region.
pub const KERNEL_GATE_END: u32 = KERNEL_GATE_BASE + ((u16::MAX as u32 + 1) << 2);

/// Returns the gate address for one kernel export ordinal.
#[must_use]
pub const fn gate_address(ordinal: u16) -> GuestVa {
    GuestVa(KERNEL_GATE_BASE + ((ordinal as u32) << 2))
}

/// Returns the ordinal for one gate address.
#[must_use]
pub const fn gate_ordinal(address: GuestVa) -> Option<u16> {
    if address.0 < KERNEL_GATE_BASE || address.0 >= KERNEL_GATE_END {
        return None;
    }

    let offset = address.0 - KERNEL_GATE_BASE;
    if offset & 0b11 != 0 {
        return None;
    }

    Some((offset >> 2) as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_addresses_round_trip_every_ordinal_boundary() {
        for ordinal in [0, 1, 255, 366, u16::MAX] {
            assert_eq!(gate_ordinal(gate_address(ordinal)), Some(ordinal));
        }
    }

    #[test]
    fn addresses_outside_the_region_have_no_ordinal() {
        assert_eq!(gate_ordinal(GuestVa(KERNEL_GATE_BASE - 4)), None);
        assert_eq!(gate_ordinal(GuestVa(KERNEL_GATE_END)), None);
        assert_eq!(gate_ordinal(GuestVa(KERNEL_GATE_BASE + 2)), None);
        assert_eq!(gate_ordinal(GuestVa(0)), None);
    }
}
