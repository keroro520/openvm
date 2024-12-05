use std::{borrow::BorrowMut, sync::Arc};

use ax_circuit_primitives::bitwise_op_lookup::{
    BitwiseOperationLookupBus, BitwiseOperationLookupChip,
};
use ax_stark_backend::{
    p3_air::BaseAir,
    p3_field::{AbstractField, PrimeField32},
    p3_matrix::{dense::RowMajorMatrix, Matrix},
    utils::disable_debug_builder,
    verifier::VerificationError,
    Chip, ChipUsageGetter,
};
use ax_stark_sdk::{p3_baby_bear::BabyBear, utils::create_seeded_rng};
use axvm_circuit::arch::{testing::VmChipTestBuilder, VmAdapterChip};
use axvm_instructions::{instruction::Instruction, program::PC_BITS, AxVmOpcode, UsizeOpcode};
use axvm_rv32im_transpiler::Rv32AuipcOpcode::{self, *};
use rand::{rngs::StdRng, Rng};

use super::{run_auipc, Rv32AuipcChip, Rv32AuipcCoreChip, Rv32AuipcCoreCols};
use crate::adapters::{Rv32RdWriteAdapterChip, RV32_CELL_BITS, RV32_REGISTER_NUM_LIMBS};

const IMM_BITS: usize = 24;
const BITWISE_OP_LOOKUP_BUS: usize = 9;

type F = BabyBear;

fn set_and_execute(
    tester: &mut VmChipTestBuilder<F>,
    chip: &mut Rv32AuipcChip<F>,
    rng: &mut StdRng,
    opcode: Rv32AuipcOpcode,
    imm: Option<u32>,
    initial_pc: Option<u32>,
) {
    let imm = imm.unwrap_or(rng.gen_range(0..(1 << IMM_BITS))) as usize;
    let a = rng.gen_range(0..32) << 2;

    tester.execute_with_pc(
        chip,
        Instruction::from_usize(AxVmOpcode::with_default_offset(opcode), [a, 0, imm, 1, 0]),
        initial_pc.unwrap_or(rng.gen_range(0..(1 << PC_BITS))),
    );
    let initial_pc = tester.execution.last_from_pc().as_canonical_u32();

    let rd_data = run_auipc(opcode, initial_pc, imm as u32);

    assert_eq!(rd_data.map(F::from_canonical_u32), tester.read::<4>(1, a));
}

///////////////////////////////////////////////////////////////////////////////////////
/// POSITIVE TESTS
///
/// Randomly generate computations and execute, ensuring that the generated trace
/// passes all constraints.
///////////////////////////////////////////////////////////////////////////////////////

#[test]
fn rand_auipc_test() {
    let mut rng = create_seeded_rng();
    let bitwise_bus = BitwiseOperationLookupBus::new(BITWISE_OP_LOOKUP_BUS);
    let bitwise_chip = Arc::new(BitwiseOperationLookupChip::<RV32_CELL_BITS>::new(
        bitwise_bus,
    ));

    let mut tester = VmChipTestBuilder::default();
    let adapter = Rv32RdWriteAdapterChip::<F>::new(
        tester.execution_bus(),
        tester.program_bus(),
        tester.memory_controller(),
    );
    let core = Rv32AuipcCoreChip::new(bitwise_chip.clone(), Rv32AuipcOpcode::default_offset());
    let mut chip = Rv32AuipcChip::<F>::new(adapter, core, tester.memory_controller());

    let num_tests: usize = 100;
    for _ in 0..num_tests {
        set_and_execute(&mut tester, &mut chip, &mut rng, AUIPC, None, None);
    }

    let tester = tester.build().load(chip).load(bitwise_chip).finalize();
    tester.simple_test().expect("Verification failed");
}

//////////////////////////////////////////////////////////////////////////////////////
// NEGATIVE TESTS
//
// Given a fake trace of a single operation, setup a chip and run the test. We replace
// the write part of the trace and check that the core chip throws the expected error.
// A dummy adaptor is used so memory interactions don't indirectly cause false passes.
//////////////////////////////////////////////////////////////////////////////////////

fn run_negative_auipc_test(
    opcode: Rv32AuipcOpcode,
    initial_imm: Option<u32>,
    initial_pc: Option<u32>,
    rd_data: Option<[u32; RV32_REGISTER_NUM_LIMBS]>,
    imm_limbs: Option<[u32; RV32_REGISTER_NUM_LIMBS - 1]>,
    pc_limbs: Option<[u32; RV32_REGISTER_NUM_LIMBS - 1]>,
    expected_error: VerificationError,
) {
    let mut rng = create_seeded_rng();
    let bitwise_bus = BitwiseOperationLookupBus::new(BITWISE_OP_LOOKUP_BUS);
    let bitwise_chip = Arc::new(BitwiseOperationLookupChip::<RV32_CELL_BITS>::new(
        bitwise_bus,
    ));

    let mut tester = VmChipTestBuilder::default();
    let adapter = Rv32RdWriteAdapterChip::<F>::new(
        tester.execution_bus(),
        tester.program_bus(),
        tester.memory_controller(),
    );
    let adapter_width = BaseAir::<F>::width(adapter.air());
    let core = Rv32AuipcCoreChip::new(bitwise_chip.clone(), Rv32AuipcOpcode::default_offset());
    let mut chip = Rv32AuipcChip::<F>::new(adapter, core, tester.memory_controller());

    set_and_execute(
        &mut tester,
        &mut chip,
        &mut rng,
        opcode,
        initial_imm,
        initial_pc,
    );

    let auipc_trace_width = chip.trace_width();
    let mut chip_input = chip.generate_air_proof_input();
    let auipc_trace = chip_input.raw.common_main.as_mut().unwrap();
    {
        let mut trace_row = auipc_trace.row_slice(0).to_vec();

        let (_, core_row) = trace_row.split_at_mut(adapter_width);

        let core_cols: &mut Rv32AuipcCoreCols<F> = core_row.borrow_mut();

        if let Some(data) = rd_data {
            core_cols.rd_data = data.map(F::from_canonical_u32);
        }

        if let Some(data) = imm_limbs {
            core_cols.imm_limbs = data.map(F::from_canonical_u32);
        }

        if let Some(data) = pc_limbs {
            core_cols.pc_limbs = data.map(F::from_canonical_u32);
        }

        *auipc_trace = RowMajorMatrix::new(trace_row, auipc_trace_width);
    }
    disable_debug_builder();
    let tester = tester
        .build()
        .load_air_proof_input(chip_input)
        .load(bitwise_chip)
        .finalize();
    tester.simple_test_with_expected_error(expected_error);
}

#[test]
fn invalid_limb_negative_tests() {
    run_negative_auipc_test(
        AUIPC,
        Some(9722891),
        None,
        None,
        Some([107, 46, 81]),
        None,
        VerificationError::OodEvaluationMismatch,
    );
    run_negative_auipc_test(
        AUIPC,
        None,
        None,
        None,
        None,
        Some([206, 166, 133]),
        VerificationError::OodEvaluationMismatch,
    );
    run_negative_auipc_test(
        AUIPC,
        None,
        None,
        Some([30, 92, 82, 132]),
        None,
        None,
        VerificationError::OodEvaluationMismatch,
    );

    run_negative_auipc_test(
        AUIPC,
        None,
        Some(876487877),
        Some([197, 202, 49, 70]),
        Some([166, 243, 17]),
        Some([36, 62, 52]),
        VerificationError::NonZeroCumulativeSum,
    );
}

#[test]
fn overflow_negative_tests() {
    run_negative_auipc_test(
        AUIPC,
        Some(256264),
        None,
        None,
        Some([3592, 219, 3]),
        None,
        VerificationError::OodEvaluationMismatch,
    );
    run_negative_auipc_test(
        AUIPC,
        None,
        None,
        None,
        None,
        Some([0, 0, 0]),
        VerificationError::OodEvaluationMismatch,
    );
    run_negative_auipc_test(
        AUIPC,
        Some(255),
        None,
        None,
        Some([F::NEG_ONE.as_canonical_u32(), 1, 0]),
        None,
        VerificationError::NonZeroCumulativeSum,
    );
    run_negative_auipc_test(
        AUIPC,
        Some(0),
        Some(255),
        Some([F::NEG_ONE.as_canonical_u32(), 1, 0, 0]),
        Some([0, 0, 0]),
        Some([1, 0, 0]),
        VerificationError::NonZeroCumulativeSum,
    );
}

///////////////////////////////////////////////////////////////////////////////////////
/// SANITY TESTS
///
/// Ensure that solve functions produce the correct results.
///////////////////////////////////////////////////////////////////////////////////////

#[test]
fn execute_roundtrip_sanity_test() {
    let mut rng = create_seeded_rng();
    let bitwise_bus = BitwiseOperationLookupBus::new(BITWISE_OP_LOOKUP_BUS);
    let bitwise_chip = Arc::new(BitwiseOperationLookupChip::<RV32_CELL_BITS>::new(
        bitwise_bus,
    ));

    let mut tester = VmChipTestBuilder::default();
    let adapter = Rv32RdWriteAdapterChip::<F>::new(
        tester.execution_bus(),
        tester.program_bus(),
        tester.memory_controller(),
    );
    let inner = Rv32AuipcCoreChip::new(bitwise_chip, Rv32AuipcOpcode::default_offset());
    let mut chip = Rv32AuipcChip::<F>::new(adapter, inner, tester.memory_controller());

    let num_tests: usize = 100;
    for _ in 0..num_tests {
        set_and_execute(&mut tester, &mut chip, &mut rng, AUIPC, None, None);
    }
}

#[test]
fn run_auipc_sanity_test() {
    let opcode = AUIPC;
    let initial_pc = 234567890;
    let imm = 11302451;
    let rd_data = run_auipc(opcode, initial_pc, imm);

    assert_eq!(rd_data, [210, 107, 113, 186]);
}
