use super::CombLogic;
use crate::sim::value::Value;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Demux {
    pub data_width: u8,
    pub sel_width: u8,
}

impl CombLogic for Demux {
    fn n_inputs(&self) -> usize {
        2
    }
    fn n_outputs(&self) -> usize {
        1usize << self.sel_width
    }
    // inputs[0] => data, inputs[1] => selector
    fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        let branches = 1 << self.sel_width;
        match inputs[1] {
            Value::Fixed { bits: sel, width } if width == self.sel_width => {
                let mut values = vec![Value::new(0, self.data_width); branches];
                // TODO: check data_width?
                values[sel as usize] = inputs[0];
                values
            }
            _ => vec![Value::Floating; branches],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_routes_data_to_selected_branch() {
        let demux = Demux {
            data_width: 1,
            sel_width: 2,
        };
        assert_eq!(
            demux.evaluate(&[Value::new(1, 1), Value::new(2, 2)]),
            vec![
                Value::new(0, 1),
                Value::new(0, 1),
                Value::new(1, 1),
                Value::new(0, 1),
            ]
        );
    }

    #[test]
    fn test_floating_selector_all_outputs_floating() {
        let demux = Demux {
            data_width: 1,
            sel_width: 2,
        };
        assert_eq!(
            demux.evaluate(&[Value::new(1, 1), Value::Floating]),
            vec![Value::Floating; 4]
        );
    }

    #[test]
    fn test_selector_width_mismatch_all_outputs_floating() {
        let demux = Demux {
            data_width: 1,
            sel_width: 2,
        };
        // Selector is width 1, but the demux expects sel_width=2.
        assert_eq!(
            demux.evaluate(&[Value::new(1, 1), Value::new(0, 1)]),
            vec![Value::Floating; 4]
        );
    }

    #[test]
    fn test_unselected_branches_are_zero_not_floating() {
        let demux = Demux {
            data_width: 4,
            sel_width: 2,
        };
        assert_eq!(
            demux.evaluate(&[Value::new(0b1111, 4), Value::new(1, 2)]),
            vec![
                Value::new(0, 4),      // unselected: zero, not Floating
                Value::new(0b1111, 4), // selected: data verbatim
                Value::new(0, 4),
                Value::new(0, 4),
            ]
        );
    }

    #[test]
    fn test_data_width_mismatch_passes_through_verbatim() {
        // Documents current lenient/unvalidated behavior: demux does not
        // check that the data input's width matches data_width (see the
        // "TODO: check data_width?" above).
        let demux = Demux {
            data_width: 1,
            sel_width: 1,
        };
        let out = demux.evaluate(&[Value::new(3, 2), Value::new(0, 1)]);
        assert_eq!(out[0], Value::new(3, 2));
    }
}
