use std::collections::BTreeMap;

use cycle_tracker::CycleTracker;
use metrics::counter;
use openvm_instructions::{
    exe::{FnBound, FnBounds},
    VmOpcode,
};
use openvm_stark_backend::p3_field::PrimeField32;

use crate::arch::{ExecutionSegment, InstructionExecutor, VmConfig};

pub mod cycle_tracker;

#[derive(Clone, Debug, Default)]
pub struct VmMetrics {
    pub chip_heights: Vec<(String, usize)>,
    /// Maps (dsl_ir, opcode) to number of times opcode was executed
    pub counts: BTreeMap<(Option<String>, String), usize>,
    /// Maps (dsl_ir, opcode, air_name) to number of trace cells generated by opcode
    pub trace_cells: BTreeMap<(Option<String>, String, String), usize>,
    /// Metric collection tools. Only collected when `config.profiling` is true.
    pub cycle_tracker: CycleTracker,
    #[allow(dead_code)]
    pub(crate) fn_bounds: FnBounds,
    /// Cycle span by function if function start/end addresses are available
    #[allow(dead_code)]
    pub(crate) current_fn: FnBound,
    pub(crate) current_trace_cells: Vec<usize>,
}

impl<F, VC> ExecutionSegment<F, VC>
where
    F: PrimeField32,
    VC: VmConfig<F>,
{
    /// Update metrics that increment per instruction
    #[allow(unused_variables)]
    pub fn update_instruction_metrics(
        &mut self,
        pc: u32,
        opcode: VmOpcode,
        dsl_instr: Option<String>,
    ) {
        counter!("total_cycles").increment(1u64);

        if self.system_config().profiling {
            let executor = self.chip_complex.inventory.get_executor(opcode).unwrap();
            let opcode_name = executor.get_opcode_name(opcode.as_usize());
            self.metrics.update_trace_cells(
                &self.air_names,
                self.current_trace_cells(),
                opcode_name,
                dsl_instr,
            );

            #[cfg(feature = "function-span")]
            self.metrics.update_current_fn(pc);
        }
    }

    pub fn finalize_metrics(&mut self) {
        counter!("total_cells_used")
            .absolute(self.current_trace_cells().into_iter().sum::<usize>() as u64);

        if self.system_config().profiling {
            self.metrics.chip_heights =
                itertools::izip!(self.air_names.clone(), self.current_trace_heights()).collect();
            self.metrics.emit();
        }
    }
}

impl VmMetrics {
    fn update_trace_cells(
        &mut self,
        air_names: &[String],
        now_trace_cells: Vec<usize>,
        opcode_name: String,
        dsl_instr: Option<String>,
    ) {
        let key = (dsl_instr, opcode_name);
        self.cycle_tracker.increment_opcode(&key);
        *self.counts.entry(key.clone()).or_insert(0) += 1;

        for (air_name, now_value, prev_value) in
            itertools::izip!(air_names, &now_trace_cells, &self.current_trace_cells)
        {
            if prev_value != now_value {
                let key = (key.0.clone(), key.1.clone(), air_name.to_owned());
                self.cycle_tracker
                    .increment_cells_used(&key, now_value - prev_value);
                *self.trace_cells.entry(key).or_insert(0) += now_value - prev_value;
            }
        }
        self.current_trace_cells = now_trace_cells;
    }

    #[cfg(feature = "function-span")]
    fn update_current_fn(&mut self, pc: u32) {
        if !self.fn_bounds.is_empty() && (pc < self.current_fn.start || pc > self.current_fn.end) {
            self.current_fn = self
                .fn_bounds
                .range(..=pc)
                .next_back()
                .map(|(_, func)| (*func).clone())
                .unwrap();
            if pc == self.current_fn.start {
                self.cycle_tracker.start(self.current_fn.name.clone());
            } else {
                self.cycle_tracker.force_end();
            }
        };
    }
    pub fn emit(&self) {
        for (name, value) in self.chip_heights.iter() {
            let labels = [("chip_name", name.clone())];
            counter!("rows_used", &labels).absolute(*value as u64);
        }

        for ((dsl_ir, opcode), value) in self.counts.iter() {
            let labels = [
                ("dsl_ir", dsl_ir.clone().unwrap_or_else(String::new)),
                ("opcode", opcode.clone()),
            ];
            counter!("frequency", &labels).absolute(*value as u64);
        }

        for ((dsl_ir, opcode, air_name), value) in self.trace_cells.iter() {
            let labels = [
                ("dsl_ir", dsl_ir.clone().unwrap_or_else(String::new)),
                ("opcode", opcode.clone()),
                ("air_name", air_name.clone()),
            ];
            counter!("cells_used", &labels).absolute(*value as u64);
        }
    }
}
