use super::{SeqLogic, SeqState};
use crate::sim::value::Value;

// Pin layout depends on `parallel_load`/`num_stages`, so pin indices are
// methods rather than associated consts, except DATA_PIN which is fixed in
// both modes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ShiftRegConf {
    pub data_width: u8,
    pub num_stages: usize,
    pub parallel_load: bool,
}

impl ShiftRegConf {
    pub const DATA_PIN: usize = 0;

    pub fn shift_pin(&self) -> usize {
        if self.parallel_load {
            2
        } else {
            1
        }
    }

    pub fn load_pin(&self) -> Option<usize> {
        self.parallel_load.then_some(1)
    }

    pub fn stage_pin(&self, i: usize) -> usize {
        3 + i
    }

    pub fn reset_pin(&self) -> usize {
        if self.parallel_load {
            3 + self.num_stages
        } else {
            2
        }
    }
}

impl ShiftRegConf {
    pub fn n_inputs(&self) -> usize {
        if self.parallel_load {
            4 + self.num_stages // data, load, shift, one per stage, reset
        } else {
            3 // data, shift, reset
        }
    }

    pub fn n_outputs(&self) -> usize {
        if self.parallel_load {
            self.num_stages
        } else {
            1
        }
    }

    pub fn input_width(&self, i: usize) -> Option<u8> {
        if i == Self::DATA_PIN {
            return Some(self.data_width);
        }
        if i == self.shift_pin() || i == self.reset_pin() {
            return Some(1);
        }
        if self.parallel_load {
            if Some(i) == self.load_pin() {
                return Some(1);
            }
            if i >= self.stage_pin(0) && i < self.stage_pin(0) + self.num_stages {
                return Some(self.data_width);
            }
        }
        None
    }

    pub fn output_width(&self, i: usize) -> Option<u8> {
        if i < self.n_outputs() {
            Some(self.data_width)
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub struct ShiftReg {
    conf: ShiftRegConf,
    // stages[num_stages - 1] is the sole output in serial mode.
    stages: Vec<Value>,
}

impl ShiftReg {
    pub fn new(data_width: u8, num_stages: usize, parallel_load: bool) -> Self {
        let num_stages = num_stages.max(1);
        Self {
            conf: ShiftRegConf {
                data_width,
                num_stages,
                parallel_load,
            },
            stages: vec![Value::new(0, data_width); num_stages],
        }
    }

    fn zeroed_stages(&self) -> Vec<Value> {
        vec![Value::new(0, self.conf.data_width); self.conf.num_stages]
    }

    fn shift_in(&mut self, data: Value) {
        for i in (1..self.conf.num_stages).rev() {
            self.stages[i] = self.stages[i - 1];
        }
        self.stages[0] = data;
    }
}

impl SeqLogic for ShiftReg {
    fn n_inputs(&self) -> usize {
        self.conf.n_inputs()
    }

    fn n_outputs(&self) -> usize {
        self.conf.n_outputs()
    }

    fn tick(&mut self, inputs: &[Value]) -> Vec<Value> {
        // Async reset dominates everything else.
        if matches!(inputs[self.conf.reset_pin()], Value::ONE) {
            self.stages = self.zeroed_stages();
            return self.observe();
        }

        let shift = inputs[self.conf.shift_pin()];
        let data = inputs[ShiftRegConf::DATA_PIN];

        if let Some(load_pin) = self.conf.load_pin() {
            // Parallel load wins over a simultaneous shift.
            if matches!(inputs[load_pin], Value::ONE) {
                for i in 0..self.conf.num_stages {
                    self.stages[i] = inputs[self.conf.stage_pin(i)];
                }
            } else if matches!(shift, Value::ONE) {
                self.shift_in(data);
            }
        } else if matches!(shift, Value::ONE) {
            self.shift_in(data);
        }

        self.observe()
    }

    fn apply_async(&mut self, inputs: &[Value]) {
        // Clears immediately while held, so the reset takes effect during
        // settle() with no clock tick.
        if matches!(inputs[self.conf.reset_pin()], Value::ONE) {
            self.stages = self.zeroed_stages();
        }
    }

    fn observe(&self) -> Vec<Value> {
        if self.conf.parallel_load {
            self.stages.clone()
        } else {
            vec![*self.stages.last().expect("num_stages >= 1")]
        }
    }

    fn reset(&mut self) {
        self.stages = self.zeroed_stages();
    }

    fn snapshot(&self) -> SeqState {
        SeqState::ShiftReg(self.stages.clone())
    }

    fn input_width(&self, i: usize) -> Option<u8> {
        self.conf.input_width(i)
    }

    fn output_width(&self, i: usize) -> Option<u8> {
        self.conf.output_width(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::component::LogicSeq;

    fn new_shift_reg(data_width: u8, num_stages: usize, parallel_load: bool) -> LogicSeq {
        LogicSeq::ShiftReg(ShiftReg::new(data_width, num_stages, parallel_load))
    }

    // ── Serial mode (parallel_load = false): inputs = [data, shift, reset] ──────

    fn serial_ins(data: Value, shift: Value, reset: Value) -> Vec<Value> {
        vec![data, shift, reset]
    }

    #[test]
    fn test_initial_value_before_any_tick() {
        let sr = new_shift_reg(4, 3, false);
        assert_eq!(sr.observe(), vec![Value::new(0, 4)]);
    }

    #[test]
    fn test_serial_shift_propagates_through_stages() {
        let mut sr = new_shift_reg(4, 3, false);

        // tick 1: shift in 1 -> stages [1, 0, 0], output (last stage) = 0
        assert_eq!(
            sr.tick(&serial_ins(Value::new(1, 4), Value::ONE, Value::ZERO)),
            vec![Value::new(0, 4)]
        );
        // tick 2: shift in 2 -> stages [2, 1, 0], output = 0
        assert_eq!(
            sr.tick(&serial_ins(Value::new(2, 4), Value::ONE, Value::ZERO)),
            vec![Value::new(0, 4)]
        );
        // tick 3: shift in 3 -> stages [3, 2, 1], output = 1
        assert_eq!(
            sr.tick(&serial_ins(Value::new(3, 4), Value::ONE, Value::ZERO)),
            vec![Value::new(1, 4)]
        );
        // tick 4: shift in 4 -> stages [4, 3, 2], output = 2
        assert_eq!(
            sr.tick(&serial_ins(Value::new(4, 4), Value::ONE, Value::ZERO)),
            vec![Value::new(2, 4)]
        );
    }

    #[test]
    fn test_serial_holds_when_shift_not_asserted() {
        let mut sr = new_shift_reg(4, 2, false);
        sr.tick(&serial_ins(Value::new(9, 4), Value::ONE, Value::ZERO)); // stages [9, 0]
        sr.tick(&serial_ins(Value::new(5, 4), Value::ONE, Value::ZERO)); // stages [5, 9]
        assert_eq!(sr.observe(), vec![Value::new(9, 4)]);

        assert_eq!(
            sr.tick(&serial_ins(Value::new(1, 4), Value::ZERO, Value::ZERO)),
            vec![Value::new(9, 4)]
        );
        // Floating shift also holds; only exactly ONE shifts.
        assert_eq!(
            sr.tick(&serial_ins(Value::new(1, 4), Value::Floating, Value::ZERO)),
            vec![Value::new(9, 4)]
        );
    }

    #[test]
    fn test_serial_async_reset_clears_destructively() {
        let mut sr = new_shift_reg(4, 2, false);
        sr.tick(&serial_ins(Value::new(9, 4), Value::ONE, Value::ZERO));
        sr.tick(&serial_ins(Value::new(5, 4), Value::ONE, Value::ZERO));
        assert_eq!(sr.observe(), vec![Value::new(9, 4)]);

        sr.apply_async(&serial_ins(Value::Floating, Value::Floating, Value::ONE));
        assert_eq!(sr.observe(), vec![Value::new(0, 4)]);

        // Released: the clear is destructive, so the old contents are gone.
        sr.apply_async(&serial_ins(Value::new(9, 4), Value::ONE, Value::ZERO));
        assert_eq!(sr.observe(), vec![Value::new(0, 4)]);
    }

    #[test]
    fn test_serial_reset_dominates_on_tick() {
        let mut sr = new_shift_reg(4, 2, false);
        sr.tick(&serial_ins(Value::new(9, 4), Value::ONE, Value::ZERO));

        assert_eq!(
            sr.tick(&serial_ins(Value::new(5, 4), Value::ONE, Value::ONE)),
            vec![Value::new(0, 4)]
        );
    }

    // ── Parallel-load mode: inputs = [data, load, shift, stage0.., reset] ───────

    fn pl_ins(
        data: Value,
        load: Value,
        shift: Value,
        stages: &[Value],
        reset: Value,
    ) -> Vec<Value> {
        let mut v = vec![data, load, shift];
        v.extend_from_slice(stages);
        v.push(reset);
        v
    }

    #[test]
    fn test_parallel_load_sets_all_stages() {
        let mut sr = new_shift_reg(4, 3, true);
        let stages = [Value::new(1, 4), Value::new(2, 4), Value::new(3, 4)];
        assert_eq!(
            sr.tick(&pl_ins(
                Value::Floating,
                Value::ONE,
                Value::ZERO,
                &stages,
                Value::ZERO
            )),
            stages.to_vec()
        );
        assert_eq!(sr.observe(), stages.to_vec());
    }

    #[test]
    fn test_parallel_shift_uses_serial_data_pin() {
        let mut sr = new_shift_reg(4, 3, true);
        let stages = [Value::new(1, 4), Value::new(2, 4), Value::new(3, 4)];
        sr.tick(&pl_ins(
            Value::Floating,
            Value::ONE,
            Value::ZERO,
            &stages,
            Value::ZERO,
        ));

        // stage0 <- data; other stages shift up.
        assert_eq!(
            sr.tick(&pl_ins(
                Value::new(9, 4),
                Value::ZERO,
                Value::ONE,
                &stages,
                Value::ZERO
            )),
            vec![Value::new(9, 4), Value::new(1, 4), Value::new(2, 4)]
        );
    }

    #[test]
    fn test_parallel_load_wins_over_simultaneous_shift() {
        let mut sr = new_shift_reg(4, 2, true);
        let initial = [Value::new(1, 4), Value::new(2, 4)];
        sr.tick(&pl_ins(
            Value::Floating,
            Value::ONE,
            Value::ZERO,
            &initial,
            Value::ZERO,
        ));

        let loaded = [Value::new(7, 4), Value::new(8, 4)];
        assert_eq!(
            sr.tick(&pl_ins(
                Value::new(9, 4),
                Value::ONE,
                Value::ONE,
                &loaded,
                Value::ZERO
            )),
            loaded.to_vec()
        );
    }

    #[test]
    fn test_parallel_holds_when_neither_load_nor_shift_asserted() {
        let mut sr = new_shift_reg(4, 2, true);
        let initial = [Value::new(1, 4), Value::new(2, 4)];
        sr.tick(&pl_ins(
            Value::Floating,
            Value::ONE,
            Value::ZERO,
            &initial,
            Value::ZERO,
        ));

        assert_eq!(
            sr.tick(&pl_ins(
                Value::new(9, 4),
                Value::ZERO,
                Value::ZERO,
                &initial,
                Value::ZERO
            )),
            initial.to_vec()
        );
    }

    #[test]
    fn test_parallel_async_reset_clears_all_stages_destructively() {
        let mut sr = new_shift_reg(4, 2, true);
        let initial = [Value::new(1, 4), Value::new(2, 4)];
        sr.tick(&pl_ins(
            Value::Floating,
            Value::ONE,
            Value::ZERO,
            &initial,
            Value::ZERO,
        ));

        sr.apply_async(&pl_ins(
            Value::Floating,
            Value::Floating,
            Value::Floating,
            &initial,
            Value::ONE,
        ));
        assert_eq!(sr.observe(), vec![Value::new(0, 4), Value::new(0, 4)]);
    }

    #[test]
    fn test_single_stage_shift_behaves_like_a_plain_register() {
        let mut sr = new_shift_reg(4, 1, false);
        assert_eq!(
            sr.tick(&serial_ins(Value::new(5, 4), Value::ONE, Value::ZERO)),
            vec![Value::new(5, 4)]
        );
        assert_eq!(
            sr.tick(&serial_ins(Value::new(9, 4), Value::ZERO, Value::ZERO)),
            vec![Value::new(5, 4)]
        );
    }
}
