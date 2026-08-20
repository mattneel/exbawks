//! The SSE unit: single-precision vectors as the interpreter models them.
//!
//! The console's processor is a Pentium III, so this is the SSE1 surface —
//! four packed or one scalar `f32` per instruction — and nothing newer
//! until a title demands it. Arithmetic is exact IEEE single precision in
//! round-to-nearest, which is bit-identical to the hardware for the
//! operations here. The two deliberate deviations are `rcpss`/`rsqrtss`
//! and their packed forms: hardware gives a 12-bit approximation whose
//! exact bits are implementation-specific, and this tier computes the true
//! reciprocal instead — deterministic for the golden tier, and closer to
//! the value the approximation approximates. Conversions honor `MXCSR`'s
//! rounding field.

use iced_x86::{Instruction, Mnemonic, OpKind, Register};

use exbawks_types::GuestVa;

use crate::exec::{ExecError, GuestMemoryRef, effective_address_for};
use crate::flags;
use crate::state::CpuState;

/// Which XMM register an operand names, when it names one.
fn xmm_index(register: Register) -> Option<usize> {
    if (Register::XMM0..=Register::XMM7).contains(&register) {
        Some(register as usize - Register::XMM0 as usize)
    } else {
        None
    }
}

/// Which 32-bit general register an operand names, in encoding order.
fn gpr32_index(register: Register) -> Option<usize> {
    if (Register::EAX..=Register::EDI).contains(&register) {
        Some(register as usize - Register::EAX as usize)
    } else {
        None
    }
}

/// A 128-bit value as its four packed lanes.
fn lanes(value: u128) -> [f32; 4] {
    core::array::from_fn(|lane| f32::from_bits((value >> (lane * 32)) as u32))
}

/// Four lanes packed back into a register value.
fn pack(lanes: [f32; 4]) -> u128 {
    lanes
        .iter()
        .enumerate()
        .fold(0_u128, |value, (lane, item)| value | u128::from(item.to_bits()) << (lane * 32))
}

/// The SSE minimum: on a NaN in either operand, or equality, the SECOND
/// operand is the answer. This asymmetry is architectural and titles
/// exploit it for clamping.
fn sse_min(first: f32, second: f32) -> f32 {
    if first.is_nan() || second.is_nan() || first >= second { second } else { first }
}

/// The SSE maximum, with the same second-operand rule.
fn sse_max(first: f32, second: f32) -> f32 {
    if first.is_nan() || second.is_nan() || first <= second { second } else { first }
}

/// Reads an operand as 16 bytes: an XMM register or a memory operand.
fn read_wide(
    state: &CpuState,
    memory: GuestMemoryRef<'_>,
    instruction: &Instruction,
    operand: u32,
    address: GuestVa,
    bytes: usize,
) -> Result<u128, ExecError> {
    match instruction.op_kind(operand) {
        OpKind::Register => xmm_index(instruction.op_register(operand))
            .map(|index| state.xmm[index])
            .ok_or(ExecError::Unsupported { address }),
        OpKind::Memory => {
            let location = effective_address_for(state, instruction, address)?;
            let mut buffer = [0_u8; 16];
            memory.read(location, &mut buffer[..bytes])?;
            Ok(u128::from_le_bytes(buffer))
        }
        _ => Err(ExecError::Unsupported { address }),
    }
}

/// Writes an operand as up to 16 bytes: an XMM register or memory.
///
/// A partial register write (`bytes < 16`) keeps the untouched high lanes,
/// which is what the scalar and low-half moves do.
fn write_wide(
    state: &mut CpuState,
    memory: GuestMemoryRef<'_>,
    instruction: &Instruction,
    operand: u32,
    address: GuestVa,
    bytes: usize,
    value: u128,
) -> Result<(), ExecError> {
    match instruction.op_kind(operand) {
        OpKind::Register => {
            let index = xmm_index(instruction.op_register(operand))
                .ok_or(ExecError::Unsupported { address })?;
            if bytes >= 16 {
                state.xmm[index] = value;
            } else {
                let bits = bytes * 8;
                let mask = (1_u128 << bits) - 1;
                state.xmm[index] = (state.xmm[index] & !mask) | (value & mask);
            }
            Ok(())
        }
        OpKind::Memory => {
            let location = effective_address_for(state, instruction, address)?;
            let buffer = value.to_le_bytes();
            memory.write(location, &buffer[..bytes])?;
            Ok(())
        }
        _ => Err(ExecError::Unsupported { address }),
    }
}

/// A lane-wise binary operation into the destination register.
fn binary_packed(
    state: &mut CpuState,
    memory: GuestMemoryRef<'_>,
    instruction: &Instruction,
    address: GuestVa,
    operation: impl Fn(f32, f32) -> f32,
) -> Result<(), ExecError> {
    let destination =
        xmm_index(instruction.op_register(0)).ok_or(ExecError::Unsupported { address })?;
    let source = lanes(read_wide(state, memory, instruction, 1, address, 16)?);
    let existing = lanes(state.xmm[destination]);
    state.xmm[destination] =
        pack(core::array::from_fn(|lane| operation(existing[lane], source[lane])));
    Ok(())
}

/// A scalar binary operation into the destination's low lane.
fn binary_scalar(
    state: &mut CpuState,
    memory: GuestMemoryRef<'_>,
    instruction: &Instruction,
    address: GuestVa,
    operation: impl Fn(f32, f32) -> f32,
) -> Result<(), ExecError> {
    let destination =
        xmm_index(instruction.op_register(0)).ok_or(ExecError::Unsupported { address })?;
    let source = f32::from_bits(read_wide(state, memory, instruction, 1, address, 4)? as u32);
    let existing = f32::from_bits(state.xmm[destination] as u32);
    let result = operation(existing, source);
    state.xmm[destination] =
        (state.xmm[destination] & !0xFFFF_FFFF_u128) | u128::from(result.to_bits());
    Ok(())
}

/// The comparison predicate a `cmpss`/`cmpps` immediate selects.
fn compare_predicate(immediate: u8, first: f32, second: f32) -> bool {
    let unordered = first.is_nan() || second.is_nan();
    match immediate & 7 {
        0 => first == second,
        1 => first < second,
        2 => first <= second,
        3 => unordered,
        4 => first != second || unordered,
        // Not-less-than and not-less-or-equal are TRUE on unordered,
        // which is why they exist as distinct predicates.
        5 => first >= second || unordered,
        6 => first > second || unordered,
        _ => !unordered,
    }
}

/// `comiss`/`ucomiss`: the ordered comparison's flag image.
fn compare_flags(state: &mut CpuState, first: f32, second: f32) {
    let (zero, parity, carry) = if first.is_nan() || second.is_nan() {
        (true, true, true)
    } else if first > second {
        (false, false, false)
    } else if first < second {
        (false, false, true)
    } else {
        (true, false, false)
    };
    let mut eflags = state.eflags
        & !(flags::ZERO
            | flags::PARITY
            | flags::CARRY
            | flags::OVERFLOW
            | flags::AUXILIARY
            | flags::SIGN);
    if zero {
        eflags |= flags::ZERO;
    }
    if parity {
        eflags |= flags::PARITY;
    }
    if carry {
        eflags |= flags::CARRY;
    }
    state.eflags = eflags;
}

/// Converts by `MXCSR`'s rounding-control field, as `cvtss2si` does.
fn round_by_mxcsr(mxcsr: u32, value: f32) -> f32 {
    match (mxcsr >> 13) & 3 {
        1 => value.floor(),
        2 => value.ceil(),
        3 => value.trunc(),
        _ => {
            let rounded = value.round();
            // Ties go to even, which `round` (away from zero) does not do.
            if (value - value.trunc()).abs() == 0.5 && rounded.abs() % 2.0 == 1.0 {
                rounded - value.signum()
            } else {
                rounded
            }
        }
    }
}

/// A float already rounded to an integer, as the i32 the conversion
/// produces: out of range is the indefinite value, as hardware reports.
fn to_i32(value: f32) -> u32 {
    if value.is_nan() || !(-2_147_483_648.0_f32..2_147_483_648.0_f32).contains(&value) {
        0x8000_0000
    } else {
        (value as i32) as u32
    }
}

/// Executes one SSE instruction; `None` when the mnemonic is not SSE.
pub(crate) fn execute(
    state: &mut CpuState,
    memory: GuestMemoryRef<'_>,
    instruction: &Instruction,
    address: GuestVa,
) -> Option<Result<(), ExecError>> {
    let mnemonic = instruction.mnemonic();
    let result = match mnemonic {
        // -- moves ---------------------------------------------------------
        Mnemonic::Movss => {
            // A load into a register zeroes the high lanes; a
            // register-to-register move keeps them. Memory stores write
            // four bytes either way.
            if instruction.op_kind(0) == OpKind::Register
                && instruction.op_kind(1) == OpKind::Memory
            {
                read_wide(state, memory, instruction, 1, address, 4)
                    .and_then(|value| write_wide(state, memory, instruction, 0, address, 16, value))
            } else {
                read_wide(state, memory, instruction, 1, address, 4)
                    .and_then(|value| write_wide(state, memory, instruction, 0, address, 4, value))
            }
        }
        Mnemonic::Movaps | Mnemonic::Movups | Mnemonic::Movntps => {
            read_wide(state, memory, instruction, 1, address, 16)
                .and_then(|value| write_wide(state, memory, instruction, 0, address, 16, value))
        }
        Mnemonic::Movlps => read_wide(state, memory, instruction, 1, address, 8)
            .and_then(|value| write_wide(state, memory, instruction, 0, address, 8, value)),
        Mnemonic::Movhps => match instruction.op_kind(0) {
            // Load: memory into the high half, low half untouched.
            OpKind::Register => {
                read_wide(state, memory, instruction, 1, address, 8).and_then(|value| {
                    let index = xmm_index(instruction.op_register(0))
                        .ok_or(ExecError::Unsupported { address })?;
                    state.xmm[index] = (state.xmm[index] & 0xFFFF_FFFF_FFFF_FFFF) | (value << 64);
                    Ok(())
                })
            }
            // Store: the high half into memory.
            _ => {
                let source = xmm_index(instruction.op_register(1))
                    .map(|index| state.xmm[index] >> 64)
                    .ok_or(ExecError::Unsupported { address });
                source
                    .and_then(|value| write_wide(state, memory, instruction, 0, address, 8, value))
            }
        },
        Mnemonic::Movhlps => {
            let (Some(destination), Some(source)) =
                (xmm_index(instruction.op_register(0)), xmm_index(instruction.op_register(1)))
            else {
                return Some(Err(ExecError::Unsupported { address }));
            };
            let high = state.xmm[source] >> 64;
            state.xmm[destination] = (state.xmm[destination] & !0xFFFF_FFFF_FFFF_FFFF) | high;
            Ok(())
        }
        Mnemonic::Movlhps => {
            let (Some(destination), Some(source)) =
                (xmm_index(instruction.op_register(0)), xmm_index(instruction.op_register(1)))
            else {
                return Some(Err(ExecError::Unsupported { address }));
            };
            let low = state.xmm[source] & 0xFFFF_FFFF_FFFF_FFFF;
            state.xmm[destination] = (state.xmm[destination] & 0xFFFF_FFFF_FFFF_FFFF) | (low << 64);
            Ok(())
        }
        Mnemonic::Movmskps => {
            let (Some(destination), Some(source)) =
                (gpr32_index(instruction.op_register(0)), xmm_index(instruction.op_register(1)))
            else {
                return Some(Err(ExecError::Unsupported { address }));
            };
            let value =
                lanes(state.xmm[source]).iter().enumerate().fold(0_u32, |mask, (lane, item)| {
                    mask | (u32::from(item.is_sign_negative()) << lane)
                });
            state.gpr[destination] = value;
            Ok(())
        }
        // -- arithmetic ----------------------------------------------------
        Mnemonic::Addss => binary_scalar(state, memory, instruction, address, |a, b| a + b),
        Mnemonic::Subss => binary_scalar(state, memory, instruction, address, |a, b| a - b),
        Mnemonic::Mulss => binary_scalar(state, memory, instruction, address, |a, b| a * b),
        Mnemonic::Divss => binary_scalar(state, memory, instruction, address, |a, b| a / b),
        Mnemonic::Minss => binary_scalar(state, memory, instruction, address, sse_min),
        Mnemonic::Maxss => binary_scalar(state, memory, instruction, address, sse_max),
        Mnemonic::Sqrtss => binary_scalar(state, memory, instruction, address, |_, b| b.sqrt()),
        Mnemonic::Rcpss => binary_scalar(state, memory, instruction, address, |_, b| 1.0 / b),
        Mnemonic::Rsqrtss => {
            binary_scalar(state, memory, instruction, address, |_, b| 1.0 / b.sqrt())
        }
        Mnemonic::Addps => binary_packed(state, memory, instruction, address, |a, b| a + b),
        Mnemonic::Subps => binary_packed(state, memory, instruction, address, |a, b| a - b),
        Mnemonic::Mulps => binary_packed(state, memory, instruction, address, |a, b| a * b),
        Mnemonic::Divps => binary_packed(state, memory, instruction, address, |a, b| a / b),
        Mnemonic::Minps => binary_packed(state, memory, instruction, address, sse_min),
        Mnemonic::Maxps => binary_packed(state, memory, instruction, address, sse_max),
        Mnemonic::Sqrtps => binary_packed(state, memory, instruction, address, |_, b| b.sqrt()),
        Mnemonic::Rcpps => binary_packed(state, memory, instruction, address, |_, b| 1.0 / b),
        Mnemonic::Rsqrtps => {
            binary_packed(state, memory, instruction, address, |_, b| 1.0 / b.sqrt())
        }
        // -- bitwise -------------------------------------------------------
        Mnemonic::Andps | Mnemonic::Andnps | Mnemonic::Orps | Mnemonic::Xorps => {
            let destination = match xmm_index(instruction.op_register(0)) {
                Some(index) => index,
                None => return Some(Err(ExecError::Unsupported { address })),
            };
            read_wide(state, memory, instruction, 1, address, 16).map(|source| {
                let existing = state.xmm[destination];
                state.xmm[destination] = match mnemonic {
                    Mnemonic::Andps => existing & source,
                    Mnemonic::Andnps => !existing & source,
                    Mnemonic::Orps => existing | source,
                    _ => existing ^ source,
                };
            })
        }
        // -- comparison ----------------------------------------------------
        Mnemonic::Comiss | Mnemonic::Ucomiss => {
            let destination = match xmm_index(instruction.op_register(0)) {
                Some(index) => index,
                None => return Some(Err(ExecError::Unsupported { address })),
            };
            read_wide(state, memory, instruction, 1, address, 4).map(|source| {
                let first = f32::from_bits(state.xmm[destination] as u32);
                let second = f32::from_bits(source as u32);
                compare_flags(state, first, second);
            })
        }
        Mnemonic::Cmpss => {
            let immediate = instruction.immediate8();
            binary_scalar(state, memory, instruction, address, move |a, b| {
                if compare_predicate(immediate, a, b) {
                    f32::from_bits(u32::MAX)
                } else {
                    f32::from_bits(0)
                }
            })
        }
        Mnemonic::Cmpps => {
            let immediate = instruction.immediate8();
            binary_packed(state, memory, instruction, address, move |a, b| {
                if compare_predicate(immediate, a, b) {
                    f32::from_bits(u32::MAX)
                } else {
                    f32::from_bits(0)
                }
            })
        }
        // -- shuffles ------------------------------------------------------
        Mnemonic::Shufps => {
            let destination = match xmm_index(instruction.op_register(0)) {
                Some(index) => index,
                None => return Some(Err(ExecError::Unsupported { address })),
            };
            let selector = instruction.immediate8() as usize;
            read_wide(state, memory, instruction, 1, address, 16).map(|source| {
                let low = lanes(state.xmm[destination]);
                let high = lanes(source);
                state.xmm[destination] = pack([
                    low[selector & 3],
                    low[(selector >> 2) & 3],
                    high[(selector >> 4) & 3],
                    high[(selector >> 6) & 3],
                ]);
            })
        }
        Mnemonic::Unpcklps => {
            let destination = match xmm_index(instruction.op_register(0)) {
                Some(index) => index,
                None => return Some(Err(ExecError::Unsupported { address })),
            };
            read_wide(state, memory, instruction, 1, address, 16).map(|source| {
                let low = lanes(state.xmm[destination]);
                let high = lanes(source);
                state.xmm[destination] = pack([low[0], high[0], low[1], high[1]]);
            })
        }
        Mnemonic::Unpckhps => {
            let destination = match xmm_index(instruction.op_register(0)) {
                Some(index) => index,
                None => return Some(Err(ExecError::Unsupported { address })),
            };
            read_wide(state, memory, instruction, 1, address, 16).map(|source| {
                let low = lanes(state.xmm[destination]);
                let high = lanes(source);
                state.xmm[destination] = pack([low[2], high[2], low[3], high[3]]);
            })
        }
        // -- conversion ----------------------------------------------------
        Mnemonic::Cvtsi2ss => {
            let destination = match xmm_index(instruction.op_register(0)) {
                Some(index) => index,
                None => return Some(Err(ExecError::Unsupported { address })),
            };
            let source = match instruction.op_kind(1) {
                OpKind::Register => gpr32_index(instruction.op_register(1))
                    .map(|index| state.gpr[index])
                    .ok_or(ExecError::Unsupported { address }),
                OpKind::Memory => {
                    read_wide(state, memory, instruction, 1, address, 4).map(|value| value as u32)
                }
                _ => Err(ExecError::Unsupported { address }),
            };
            source.map(|value| {
                let converted = value as i32 as f32;
                state.xmm[destination] =
                    (state.xmm[destination] & !0xFFFF_FFFF_u128) | u128::from(converted.to_bits());
            })
        }
        Mnemonic::Cvttss2si | Mnemonic::Cvtss2si => {
            let destination = match gpr32_index(instruction.op_register(0)) {
                Some(index) => index,
                None => return Some(Err(ExecError::Unsupported { address })),
            };
            read_wide(state, memory, instruction, 1, address, 4).map(|source| {
                let value = f32::from_bits(source as u32);
                let rounded = if mnemonic == Mnemonic::Cvttss2si {
                    value.trunc()
                } else {
                    round_by_mxcsr(state.mxcsr, value)
                };
                state.gpr[destination] = to_i32(rounded);
            })
        }
        // -- control -------------------------------------------------------
        Mnemonic::Ldmxcsr => {
            let location = match effective_address_for(state, instruction, address) {
                Ok(location) => location,
                Err(error) => return Some(Err(error)),
            };
            let mut bytes = [0_u8; 4];
            memory.read(location, &mut bytes).map_err(ExecError::from).map(|()| {
                state.mxcsr = u32::from_le_bytes(bytes);
            })
        }
        Mnemonic::Stmxcsr => {
            let location = match effective_address_for(state, instruction, address) {
                Ok(location) => location,
                Err(error) => return Some(Err(error)),
            };
            memory.write(location, &state.mxcsr.to_le_bytes()).map_err(ExecError::from)
        }
        // The prefetch and ordering hints do nothing a single-processor
        // interpreter can observe.
        Mnemonic::Prefetcht0
        | Mnemonic::Prefetcht1
        | Mnemonic::Prefetcht2
        | Mnemonic::Prefetchnta
        | Mnemonic::Sfence => Ok(()),
        _ => return None,
    };
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_and_max_return_the_second_operand_on_a_nan() {
        assert_eq!(sse_min(f32::NAN, 2.0), 2.0);
        assert_eq!(sse_min(2.0, f32::NAN).to_bits(), f32::NAN.to_bits());
        assert_eq!(sse_max(f32::NAN, -2.0), -2.0);
        assert_eq!(sse_min(1.0, 2.0), 1.0);
        assert_eq!(sse_max(1.0, 2.0), 2.0);
    }

    #[test]
    fn lane_packing_round_trips() {
        let values = [1.5_f32, -2.25, 0.0, f32::INFINITY];
        assert_eq!(lanes(pack(values)), values);
    }

    #[test]
    fn mxcsr_rounding_selects_the_mode() {
        // Nearest-even at the tie.
        assert_eq!(round_by_mxcsr(0, 2.5), 2.0);
        assert_eq!(round_by_mxcsr(0, 3.5), 4.0);
        // Floor, ceiling, truncation.
        assert_eq!(round_by_mxcsr(1 << 13, 2.7), 2.0);
        assert_eq!(round_by_mxcsr(2 << 13, 2.2), 3.0);
        assert_eq!(round_by_mxcsr(3 << 13, -2.7), -2.0);
    }

    #[test]
    fn out_of_range_conversion_is_the_indefinite_value() {
        assert_eq!(to_i32(f32::NAN), 0x8000_0000);
        assert_eq!(to_i32(3e9), 0x8000_0000);
        assert_eq!(to_i32(-3.0), 0xFFFF_FFFD);
    }

    #[test]
    fn comparison_predicates_match_the_encoding() {
        assert!(compare_predicate(0, 1.0, 1.0));
        assert!(compare_predicate(1, 1.0, 2.0));
        assert!(compare_predicate(2, 2.0, 2.0));
        assert!(compare_predicate(3, f32::NAN, 1.0));
        assert!(compare_predicate(4, 1.0, 2.0));
        assert!(compare_predicate(4, f32::NAN, 1.0), "neq is true on unordered");
        assert!(compare_predicate(5, 2.0, 1.0));
        assert!(compare_predicate(5, f32::NAN, 1.0), "nlt is true on unordered");
        assert!(compare_predicate(6, 3.0, 2.0));
        assert!(compare_predicate(7, 1.0, 2.0));
        assert!(!compare_predicate(7, f32::NAN, 2.0));
    }
}
