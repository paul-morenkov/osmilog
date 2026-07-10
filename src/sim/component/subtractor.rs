use super::CombLogic;
use crate::sim::value::Value;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Subtractor {
    pub data_width: u8,
}

impl Subtractor {
    const MINUEND_PIN: usize = 0;
    const SUBTRAHEND_PIN: usize = 1;
    const BORROW_IN_PIN: usize = 2;
    const DIFF_PIN: usize = 0;
    const BORROW_OUT_PIN: usize = 1;
}

impl CombLogic for Subtractor {
    fn n_inputs(&self) -> usize {
        // Minuend, subtrahend, and a borrow-in
        3
    }

    fn n_outputs(&self) -> usize {
        // Difference and borrow-out
        2
    }

    fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        let width = self.data_width;
        match (
            inputs[Self::MINUEND_PIN],
            inputs[Self::SUBTRAHEND_PIN],
            inputs[Self::BORROW_IN_PIN],
        ) {
            (
                Value::Fixed { bits: a, width: aw },
                Value::Fixed { bits: b, width: bw },
                Value::Fixed {
                    bits: bin,
                    width: bw_in,
                },
            ) if aw == width && bw == width && bw_in == 1 => {
                // Wraps mod 2^32 then masks to `width` bits, which equals mod
                // 2^width since 2^width divides 2^32 - no signed/widened
                // intermediate needed, unlike the borrow-out check below.
                let diff = a.wrapping_sub(b).wrapping_sub(bin) & Value::mask(width);
                let borrow = ((a as u64) < (b as u64 + bin as u64)) as u32;
                vec![Value::new(diff, width), Value::new(borrow, 1)]
            }
            _ => vec![Value::Floating, Value::Floating],
        }
    }

    fn input_width(&self, i: usize) -> Option<u8> {
        match i {
            Self::MINUEND_PIN | Self::SUBTRAHEND_PIN => Some(self.data_width),
            _ => Some(1), // borrow-in
        }
    }

    fn output_width(&self, i: usize) -> Option<u8> {
        match i {
            Self::DIFF_PIN => Some(self.data_width),
            Self::BORROW_OUT_PIN => Some(1),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subtractor(data_width: u8) -> Subtractor {
        Subtractor { data_width }
    }

    #[test]
    fn test_diff_no_borrow() {
        let s = subtractor(4);
        assert_eq!(
            s.evaluate(&[Value::new(5, 4), Value::new(3, 4), Value::ZERO]),
            vec![Value::new(2, 4), Value::ZERO]
        );
    }

    #[test]
    fn test_borrow_in_propagates() {
        let s = subtractor(4);
        assert_eq!(
            s.evaluate(&[Value::new(5, 4), Value::new(3, 4), Value::ONE]),
            vec![Value::new(1, 4), Value::ZERO]
        );
    }

    #[test]
    fn test_diff_underflows_to_borrow_out() {
        let s = subtractor(4);
        // 2 - 3 = -1, which wraps to 0b1111 (15) with borrow-out set.
        assert_eq!(
            s.evaluate(&[Value::new(2, 4), Value::new(3, 4), Value::ZERO]),
            vec![Value::new(15, 4), Value::ONE]
        );
    }

    #[test]
    fn test_borrow_in_alone_can_trigger_borrow_out() {
        let s = subtractor(4);
        // 0 - 0 - 1 = -1, which wraps to 0b1111 (15) with borrow-out set.
        assert_eq!(
            s.evaluate(&[Value::new(0, 4), Value::new(0, 4), Value::ONE]),
            vec![Value::new(15, 4), Value::ONE]
        );
    }

    #[test]
    fn test_full_width_subtrahend_does_not_panic() {
        // Subtrahend at max u32 plus a borrow-in for a 32-bit subtractor: exercises
        // the u64 widening in the borrow-out check that avoids an overflow panic
        // on plain u32 addition (b + bin).
        let s = subtractor(32);
        assert_eq!(
            s.evaluate(&[Value::new(0, 32), Value::new(u32::MAX, 32), Value::ONE]),
            vec![Value::new(0, 32), Value::ONE]
        );
    }

    #[test]
    fn test_mismatched_operand_width_yields_floating() {
        let s = subtractor(4);
        assert_eq!(
            s.evaluate(&[Value::new(1, 3), Value::new(1, 4), Value::ZERO]),
            vec![Value::Floating, Value::Floating]
        );
    }

    #[test]
    fn test_floating_minuend_yields_floating() {
        let s = subtractor(4);
        assert_eq!(
            s.evaluate(&[Value::Floating, Value::new(1, 4), Value::ZERO]),
            vec![Value::Floating, Value::Floating]
        );
    }

    #[test]
    fn test_input_output_widths() {
        let s = subtractor(4);
        assert_eq!(s.input_width(0), Some(4));
        assert_eq!(s.input_width(1), Some(4));
        assert_eq!(s.input_width(2), Some(1));
        assert_eq!(s.output_width(0), Some(4));
        assert_eq!(s.output_width(1), Some(1));
    }
}
