use super::CombLogic;
use crate::sim::value::Value;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Mux {
    pub data_width: u8,
    pub sel_width: u8,
}

impl Mux {
    const SEL_PIN: usize = 0;
}

impl CombLogic for Mux {
    fn n_inputs(&self) -> usize {
        (1usize << self.sel_width) + 1
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        match inputs[Self::SEL_PIN] {
            Value::Floating | Value::Invalid => vec![Value::Floating],
            Value::Fixed { bits, width } => {
                if self.sel_width == width {
                    vec![inputs[bits as usize + 1]]
                } else {
                    vec![Value::Floating]
                }
            }
        }
    }
    fn input_width(&self, i: usize) -> Option<u8> {
        if i == Self::SEL_PIN {
            Some(self.sel_width) // selector
        } else {
            Some(self.data_width) // data branch
        }
    }
    fn output_width(&self, _i: usize) -> Option<u8> {
        Some(self.data_width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_selects_branch_by_selector() {
        let mux = Mux {
            data_width: 2,
            sel_width: 2,
        };
        let branches = [
            Value::new(3, 2),
            Value::new(2, 2),
            Value::new(1, 2),
            Value::new(0, 2),
        ];
        for (sel, expected) in branches.iter().enumerate() {
            let mut inputs = vec![Value::new(sel as u32, 2)];
            inputs.extend_from_slice(&branches);
            assert_eq!(mux.evaluate(&inputs), vec![*expected]);
        }
    }

    #[test]
    fn test_floating_selector_yields_floating_output() {
        let mux = Mux {
            data_width: 1,
            sel_width: 1,
        };
        assert_eq!(
            mux.evaluate(&[Value::Floating, Value::ONE, Value::ZERO]),
            vec![Value::Floating]
        );
    }

    #[test]
    fn test_selector_width_mismatch_yields_floating() {
        let mux = Mux {
            data_width: 2,
            sel_width: 2,
        };
        // Selector is width 1, but the mux expects sel_width=2.
        let inputs = [
            Value::ZERO,
            Value::new(5, 2),
            Value::Floating,
            Value::Floating,
            Value::Floating,
        ];
        assert_eq!(mux.evaluate(&inputs), vec![Value::Floating]);
    }

    #[test]
    fn test_unconnected_data_branch_is_floating() {
        let mux = Mux {
            data_width: 1,
            sel_width: 1,
        };
        // Selector picks branch 0, which is unconnected -> Floating.
        assert_eq!(
            mux.evaluate(&[Value::ZERO, Value::Floating, Value::ZERO]),
            vec![Value::Floating]
        );
    }
}
