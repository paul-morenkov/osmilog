use super::CombLogic;
use crate::sim::value::Value;
use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Comparator {
    pub data_width: u8,
}

impl Comparator {
    const LHS_PIN: usize = 0;
    const RHS_PIN: usize = 1;
    const GT_PIN: usize = 0;
    const EQ_PIN: usize = 1;
    const LT_PIN: usize = 2;
}

impl CombLogic for Comparator {
    fn n_inputs(&self) -> usize {
        // The two operands being compared
        2
    }

    fn n_outputs(&self) -> usize {
        // Greater-than, equal, less-than
        3
    }

    fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        let width = self.data_width;
        match (inputs[Self::LHS_PIN], inputs[Self::RHS_PIN]) {
            (Value::Fixed { bits: a, width: aw }, Value::Fixed { bits: b, width: bw })
                if aw == width && bw == width =>
            {
                let (gt, eq, lt) = match a.cmp(&b) {
                    Ordering::Greater => (1, 0, 0),
                    Ordering::Equal => (0, 1, 0),
                    Ordering::Less => (0, 0, 1),
                };
                vec![Value::new(gt, 1), Value::new(eq, 1), Value::new(lt, 1)]
            }
            _ => vec![Value::Floating, Value::Floating, Value::Floating],
        }
    }

    fn input_width(&self, i: usize) -> Option<u8> {
        match i {
            Self::LHS_PIN | Self::RHS_PIN => Some(self.data_width),
            _ => None,
        }
    }

    fn output_width(&self, i: usize) -> Option<u8> {
        match i {
            Self::GT_PIN | Self::EQ_PIN | Self::LT_PIN => Some(1),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comparator(data_width: u8) -> Comparator {
        Comparator { data_width }
    }

    #[test]
    fn test_greater_than() {
        let c = comparator(4);
        assert_eq!(
            c.evaluate(&[Value::new(5, 4), Value::new(3, 4)]),
            vec![Value::ONE, Value::ZERO, Value::ZERO]
        );
    }

    #[test]
    fn test_equal() {
        let c = comparator(4);
        assert_eq!(
            c.evaluate(&[Value::new(3, 4), Value::new(3, 4)]),
            vec![Value::ZERO, Value::ONE, Value::ZERO]
        );
    }

    #[test]
    fn test_less_than() {
        let c = comparator(4);
        assert_eq!(
            c.evaluate(&[Value::new(2, 4), Value::new(3, 4)]),
            vec![Value::ZERO, Value::ZERO, Value::ONE]
        );
    }

    #[test]
    fn test_mismatched_operand_width_yields_floating() {
        let c = comparator(4);
        assert_eq!(
            c.evaluate(&[Value::new(1, 3), Value::new(1, 4)]),
            vec![Value::Floating, Value::Floating, Value::Floating]
        );
    }

    #[test]
    fn test_floating_lhs_yields_floating() {
        let c = comparator(4);
        assert_eq!(
            c.evaluate(&[Value::Floating, Value::new(1, 4)]),
            vec![Value::Floating, Value::Floating, Value::Floating]
        );
    }

    #[test]
    fn test_floating_rhs_yields_floating() {
        let c = comparator(4);
        assert_eq!(
            c.evaluate(&[Value::new(1, 4), Value::Floating]),
            vec![Value::Floating, Value::Floating, Value::Floating]
        );
    }

    #[test]
    fn test_input_output_widths() {
        let c = comparator(4);
        assert_eq!(c.input_width(0), Some(4));
        assert_eq!(c.input_width(1), Some(4));
        assert_eq!(c.output_width(0), Some(1));
        assert_eq!(c.output_width(1), Some(1));
        assert_eq!(c.output_width(2), Some(1));
    }
}
