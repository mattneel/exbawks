//! Guest EFLAGS bit positions and masks.

/// The carry flag.
pub const CARRY: u32 = 1 << 0;
/// The parity flag.
pub const PARITY: u32 = 1 << 2;
/// The auxiliary carry flag.
pub const AUXILIARY: u32 = 1 << 4;
/// The zero flag.
pub const ZERO: u32 = 1 << 6;
/// The sign flag.
pub const SIGN: u32 = 1 << 7;
/// The overflow flag.
pub const OVERFLOW: u32 = 1 << 11;

/// Every guest-visible arithmetic flag.
pub const ARITHMETIC: u32 = CARRY | PARITY | AUXILIARY | ZERO | SIGN | OVERFLOW;

/// The flags that logical operations define.
///
/// Hardware leaves the auxiliary carry undefined after logical operations,
/// so translated code and the interpreter preserve the previous guest value.
pub const LOGIC_DEFINED: u32 = CARRY | PARITY | ZERO | SIGN | OVERFLOW;
