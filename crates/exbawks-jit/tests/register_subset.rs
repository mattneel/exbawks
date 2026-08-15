//! Oracle tests for the register-only direct emitter.
//!
//! Every emitted block runs against the interpreter oracle from identical
//! initial CPU state, and the complete architectural state must match.

#![cfg(windows)]

use exbawks_cpu::{BasicBlockDecoder, CpuState, Gpr, flags, step_register_only};
use exbawks_jit::{BlockExit, DirectEmitter, Dispatcher, JitError};
use exbawks_types::GuestVa;

const START: u32 = 0x0001_0000;

/// Encodes `ALU dst, src` through the register form.
fn alu_reg(opcode: u8, dst: Gpr, src: Gpr) -> Vec<u8> {
    vec![opcode, 0xC0 | ((src as u8) << 3) | dst as u8]
}

/// Encodes `ALU dst, imm32` through the 0x81 group.
fn alu_imm(modrm_extension: u8, dst: Gpr, value: u32) -> Vec<u8> {
    let mut bytes = vec![0x81, 0xC0 | (modrm_extension << 3) | dst as u8];
    bytes.extend(value.to_le_bytes());
    bytes
}

/// Encodes `mov dst, src`.
fn mov_reg(dst: Gpr, src: Gpr) -> Vec<u8> {
    vec![0x89, 0xC0 | ((src as u8) << 3) | dst as u8]
}

/// Encodes `mov dst, imm32`.
fn mov_imm(dst: Gpr, value: u32) -> Vec<u8> {
    let mut bytes = vec![0xB8 + dst as u8];
    bytes.extend(value.to_le_bytes());
    bytes
}

/// Runs one guest byte sequence through the emitter and the oracle.
fn run_against_oracle(code: &[u8], initial: &CpuState) -> (CpuState, BlockExit) {
    let block =
        BasicBlockDecoder::default().decode(GuestVa(START), code).expect("guest bytes must decode");
    let emitted = DirectEmitter.emit(&block).expect("emission succeeds");

    let mut translated = initial.clone();
    translated.eip = START;
    let exit = Dispatcher.run(&emitted, &mut translated).expect("dispatch succeeds");

    let mut oracle = initial.clone();
    oracle.eip = START;
    let mut oracle_count = 0_usize;
    for instruction in &block.instructions {
        if step_register_only(&mut oracle, instruction).is_err() {
            break;
        }
        oracle_count += 1;
    }

    assert_eq!(oracle_count, emitted.translated_instructions(), "translated count must match");
    assert_eq!(translated, oracle, "translated state must match the oracle for {code:02X?}");
    (translated, exit)
}

fn state_with(values: [u32; 8], eflags: u32) -> CpuState {
    CpuState { gpr: values, eflags, ..CpuState::default() }
}

const OPERAND_PAIRS: [(u32, u32); 7] = [
    (0, 0),
    (1, 0xFFFF_FFFF),
    (0xFFFF_FFFF, 1),
    (0x7FFF_FFFF, 1),
    (0x8000_0000, 0x8000_0000),
    (0x1234_5678, 0x9ABC_DEF0),
    (0x0000_00FF, 0x0000_0F0F),
];

#[test]
fn every_alu_operation_matches_the_oracle() {
    let operations: [(u8, u8); 5] = [(0x01, 0), (0x29, 5), (0x21, 4), (0x09, 1), (0x31, 6)];

    for (register_opcode, immediate_extension) in operations {
        for (a, b) in OPERAND_PAIRS {
            for initial_flags in [0x2, 0x2 | flags::ARITHMETIC] {
                let initial = state_with([0, 0, 0, a, 0, 0, b, 0], initial_flags);
                run_against_oracle(&alu_reg(register_opcode, Gpr::Ebx, Gpr::Esi), &initial);
                run_against_oracle(&alu_imm(immediate_extension, Gpr::Ebx, b), &initial);
            }
        }
    }
}

#[test]
fn register_and_immediate_moves_match_the_oracle() {
    let initial = state_with([1, 2, 3, 4, 5, 6, 7, 8], 0x2 | flags::ARITHMETIC);
    run_against_oracle(&mov_reg(Gpr::Edi, Gpr::Eax), &initial);
    run_against_oracle(&mov_imm(Gpr::Ebp, 0xDEAD_BEEF), &initial);
    run_against_oracle(&[0x90], &initial);
}

#[test]
fn multi_instruction_blocks_match_the_oracle() {
    let mut code = Vec::new();
    code.extend(mov_imm(Gpr::Eax, 5));
    code.extend(alu_imm(0, Gpr::Eax, 0xFFFF_FFFB));
    code.push(0x90);
    code.extend(mov_reg(Gpr::Ecx, Gpr::Eax));
    code.extend(alu_reg(0x31, Gpr::Edx, Gpr::Edx));
    code.extend(alu_reg(0x29, Gpr::Ebx, Gpr::Ecx));
    code.extend(alu_imm(1, Gpr::Esi, 0x8000_0001));

    let initial = state_with([9, 8, 7, 6, 0x100, 4, 3, 2], 0x2);
    let (_, exit) = run_against_oracle(&code, &initial);
    assert_eq!(exit, BlockExit::DirectSuccessor);
}

#[test]
fn addition_carry_sets_the_expected_flags() {
    let initial = state_with([0xFFFF_FFFF, 0, 0, 1, 0, 0, 0, 0], 0x2);
    // add eax, ebx
    let (state, _) = run_against_oracle(&alu_reg(0x01, Gpr::Eax, Gpr::Ebx), &initial);
    assert_eq!(state.gpr[0], 0);
    assert_eq!(
        state.eflags & flags::ARITHMETIC,
        flags::CARRY | flags::PARITY | flags::AUXILIARY | flags::ZERO
    );
}

#[test]
fn signed_overflow_sets_the_expected_flags() {
    let initial = state_with([0x7FFF_FFFF, 0, 0, 0, 0, 0, 0, 0], 0x2);
    // add eax, 1
    let (state, _) = run_against_oracle(&alu_imm(0, Gpr::Eax, 1), &initial);
    assert_eq!(state.gpr[0], 0x8000_0000);
    assert_eq!(
        state.eflags & flags::ARITHMETIC,
        flags::PARITY | flags::AUXILIARY | flags::SIGN | flags::OVERFLOW
    );
}

#[test]
fn subtraction_borrow_sets_the_expected_flags() {
    let initial = state_with([0, 0, 0, 0, 0, 0, 0, 0], 0x2);
    // sub eax, 1
    let (state, _) = run_against_oracle(&alu_imm(5, Gpr::Eax, 1), &initial);
    assert_eq!(state.gpr[0], 0xFFFF_FFFF);
    assert_eq!(
        state.eflags & flags::ARITHMETIC,
        flags::CARRY | flags::PARITY | flags::AUXILIARY | flags::SIGN
    );
}

#[test]
fn logical_zeroing_preserves_the_auxiliary_flag() {
    let initial = state_with([7, 0, 0, 0, 0, 0, 0, 0], 0x2 | flags::AUXILIARY | flags::CARRY);
    // xor eax, eax
    let (state, _) = run_against_oracle(&alu_reg(0x31, Gpr::Eax, Gpr::Eax), &initial);
    assert_eq!(state.gpr[0], 0);
    assert_eq!(state.eflags & flags::ARITHMETIC, flags::AUXILIARY | flags::PARITY | flags::ZERO);
}

#[test]
fn blocks_return_through_one_dispatcher_exit() {
    // A fully supported block exits with the direct successor.
    let block = BasicBlockDecoder::default()
        .decode(GuestVa(START), &mov_imm(Gpr::Eax, 1))
        .expect("guest bytes must decode");
    let emitted = DirectEmitter.emit(&block).expect("emission succeeds");
    let mut state = CpuState::default();
    let exit = Dispatcher.run(&emitted, &mut state).expect("dispatch succeeds");
    assert_eq!(exit, BlockExit::DirectSuccessor);
    assert_eq!(state.eip, START + 5);
    assert_eq!(emitted.exit_eip(), GuestVa(START + 5));

    // A block with an untranslated instruction reports it through EIP.
    let block = BasicBlockDecoder::default()
        .decode(GuestVa(START), &[0x90, 0xC3])
        .expect("guest bytes must decode");
    let emitted = DirectEmitter.emit(&block).expect("emission succeeds");
    assert_eq!(emitted.translated_instructions(), 1);
    let mut state = CpuState::default();
    let exit = Dispatcher.run(&emitted, &mut state).expect("dispatch succeeds");
    assert_eq!(exit, BlockExit::UnsupportedInstruction);
    assert_eq!(state.eip, START + 1);
}

#[test]
fn every_translated_instruction_has_one_source_range() {
    let mut code = Vec::new();
    code.extend(mov_imm(Gpr::Eax, 5));
    code.push(0x90);
    code.extend(alu_reg(0x01, Gpr::Eax, Gpr::Ebx));
    code.push(0xC3);

    let block = BasicBlockDecoder::default()
        .decode(GuestVa(START), &code)
        .expect("guest bytes must decode");
    let emitted = DirectEmitter.emit(&block).expect("emission succeeds");
    let map = emitted.source_map();

    assert_eq!(map.ranges().len(), emitted.translated_instructions());
    assert_eq!(map.ranges().len(), 3);

    // The translated prefix is completely covered, in guest order, with no
    // gap before the epilogue.
    let mut expected_host = 0;
    let expected_guest = [(START, 5), (START + 5, 1), (START + 6, 2)];
    for (range, (guest_ip, guest_len)) in map.ranges().iter().zip(expected_guest) {
        assert_eq!(range.host_start, expected_host);
        assert!(range.host_end > range.host_start);
        assert_eq!(range.guest_ip, GuestVa(guest_ip));
        assert_eq!(range.guest_len, guest_len);
        expected_host = range.host_end;

        let middle = range.host_start + (range.host_end - range.host_start) / 2;
        let found = map.source_for_host_offset(middle).expect("lookup succeeds");
        assert_eq!(found.guest_ip, range.guest_ip);
    }

    // Epilogue offsets have no guest source.
    assert!(map.source_for_host_offset(expected_host).is_none());
    let last = u32::try_from(emitted.machine_code().len() - 1).expect("offset fits");
    assert!(map.source_for_host_offset(last).is_none());

    // The register-only subset emits no faultable host instructions.
    assert!(map.faults().is_empty());
}

#[test]
fn empty_blocks_are_rejected() {
    let block =
        BasicBlockDecoder::default().decode(GuestVa(START), &[]).expect("empty input decodes");
    let error = DirectEmitter.emit(&block).expect_err("empty block must fail");
    assert!(matches!(error, JitError::EmptyBlock));
}
