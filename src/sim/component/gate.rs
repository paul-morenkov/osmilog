use super::CombLogic;
use crate::sim::value::Value;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Gate {
    pub op: GateOp,
    pub n_inputs: usize,
    pub width: u8,
}

impl CombLogic for Gate {
    fn n_inputs(&self) -> usize {
        self.n_inputs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        let width = self.width;
        let val = match self.op {
            GateOp::And | GateOp::Nand => {
                let mut acc = Value::Fixed {
                    bits: Value::mask(width),
                    width,
                };
                for &x in inputs {
                    acc = acc & x;
                }
                if matches!(self.op, GateOp::Nand) {
                    !acc
                } else {
                    acc
                }
            }
            GateOp::Or | GateOp::Nor => {
                let mut acc = Value::Fixed { bits: 0, width };
                for &x in inputs {
                    acc = acc | x;
                }
                if matches!(self.op, GateOp::Nor) {
                    !acc
                } else {
                    acc
                }
            }
            GateOp::Xor | GateOp::Xnor => {
                let mut acc = Value::Fixed { bits: 0, width };
                for &x in inputs {
                    acc = acc ^ x;
                }
                if matches!(self.op, GateOp::Xnor) {
                    !acc
                } else {
                    acc
                }
            }
            GateOp::Not => !inputs[0],
        };
        vec![val] // Assumes single output
    }
    fn input_width(&self, _i: usize) -> Option<u8> {
        Some(self.width)
    }
    fn output_width(&self, _i: usize) -> Option<u8> {
        Some(self.width)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GateOp {
    And,
    Or,
    Xor,
    Xnor,
    Nand,
    Nor,
    Not,
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(GateOp::And,  0, 0, 0 ; "and 0 0")]
    #[test_case(GateOp::And,  0, 1, 0 ; "and 0 1")]
    #[test_case(GateOp::And,  1, 0, 0 ; "and 1 0")]
    #[test_case(GateOp::And,  1, 1, 1 ; "and 1 1")]
    #[test_case(GateOp::Or,   0, 0, 0 ; "or 0 0")]
    #[test_case(GateOp::Or,   0, 1, 1 ; "or 0 1")]
    #[test_case(GateOp::Or,   1, 0, 1 ; "or 1 0")]
    #[test_case(GateOp::Or,   1, 1, 1 ; "or 1 1")]
    #[test_case(GateOp::Xor,  0, 0, 0 ; "xor 0 0")]
    #[test_case(GateOp::Xor,  0, 1, 1 ; "xor 0 1")]
    #[test_case(GateOp::Xor,  1, 0, 1 ; "xor 1 0")]
    #[test_case(GateOp::Xor,  1, 1, 0 ; "xor 1 1")]
    #[test_case(GateOp::Xnor, 0, 0, 1 ; "xnor 0 0")]
    #[test_case(GateOp::Xnor, 0, 1, 0 ; "xnor 0 1")]
    #[test_case(GateOp::Xnor, 1, 0, 0 ; "xnor 1 0")]
    #[test_case(GateOp::Xnor, 1, 1, 1 ; "xnor 1 1")]
    #[test_case(GateOp::Nand, 0, 0, 1 ; "nand 0 0")]
    #[test_case(GateOp::Nand, 0, 1, 1 ; "nand 0 1")]
    #[test_case(GateOp::Nand, 1, 0, 1 ; "nand 1 0")]
    #[test_case(GateOp::Nand, 1, 1, 0 ; "nand 1 1")]
    #[test_case(GateOp::Nor,  0, 0, 1 ; "nor 0 0")]
    #[test_case(GateOp::Nor,  0, 1, 0 ; "nor 0 1")]
    #[test_case(GateOp::Nor,  1, 0, 0 ; "nor 1 0")]
    #[test_case(GateOp::Nor,  1, 1, 0 ; "nor 1 1")]
    fn test_binary_truth_table(op: GateOp, av: u32, bv: u32, expected: u32) {
        let gate = Gate {
            op,
            n_inputs: 2,
            width: 1,
        };
        assert_eq!(
            gate.evaluate(&[Value::new(av, 1), Value::new(bv, 1)]),
            vec![Value::new(expected, 1)]
        );
    }

    #[test_case(0, 1 ; "not 0")]
    #[test_case(1, 0 ; "not 1")]
    fn test_not_truth_table(av: u32, expected: u32) {
        let gate = Gate {
            op: GateOp::Not,
            n_inputs: 1,
            width: 1,
        };
        assert_eq!(
            gate.evaluate(&[Value::new(av, 1)]),
            vec![Value::new(expected, 1)]
        );
    }

    #[test]
    fn test_and_multi_input_fold() {
        let gate = Gate {
            op: GateOp::And,
            n_inputs: 3,
            width: 1,
        };
        assert_eq!(
            gate.evaluate(&[Value::ONE, Value::ONE, Value::ONE]),
            vec![Value::ONE]
        );
        assert_eq!(
            gate.evaluate(&[Value::ONE, Value::ONE, Value::ZERO]),
            vec![Value::ZERO]
        );
    }

    #[test]
    fn test_or_multi_input_fold() {
        let gate = Gate {
            op: GateOp::Or,
            n_inputs: 3,
            width: 1,
        };
        assert_eq!(
            gate.evaluate(&[Value::ZERO, Value::ZERO, Value::ZERO]),
            vec![Value::ZERO]
        );
        assert_eq!(
            gate.evaluate(&[Value::ZERO, Value::ZERO, Value::ONE]),
            vec![Value::ONE]
        );
    }

    #[test]
    fn test_multibit_width() {
        let a = Value::new(0b1100, 4);
        let b = Value::new(0b1010, 4);
        let and_gate = Gate {
            op: GateOp::And,
            n_inputs: 2,
            width: 4,
        };
        let xor_gate = Gate {
            op: GateOp::Xor,
            n_inputs: 2,
            width: 4,
        };
        assert_eq!(and_gate.evaluate(&[a, b]), vec![Value::new(0b1000, 4)]);
        assert_eq!(xor_gate.evaluate(&[a, b]), vec![Value::new(0b0110, 4)]);
    }

    #[test]
    fn test_floating_input_yields_floating_output() {
        let gate = Gate {
            op: GateOp::And,
            n_inputs: 2,
            width: 1,
        };
        // Second operand unconnected -> Floating.
        assert_eq!(
            gate.evaluate(&[Value::ONE, Value::Floating]),
            vec![Value::Floating]
        );
    }
}
