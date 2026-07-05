use super::CombLogic;
use crate::sim::value::Value;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Divider {
    pub data_width: u8,
}

impl Divider {
    const DIVIDEND_PIN: usize = 0;
    const DIVISOR_PIN: usize = 1;
    const CARRY_IN_PIN: usize = 2;
    const QUOTIENT_PIN: usize = 0;
    const REMAINDER_PIN: usize = 1;
}

impl CombLogic for Divider {
    fn n_inputs(&self) -> usize {
        // Dividend (lower half), divisor, and a carry-in (upper half of the dividend)
        3
    }

    fn n_outputs(&self) -> usize {
        // Quotient and remainder
        2
    }

    fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        let width = self.data_width;
        // A Floating carry-in behaves the same as a zero carry-in; anything else
        // non-Fixed, or a Fixed carry-in at the wrong width, falls through to Floating.
        let carry_in = match inputs[Self::CARRY_IN_PIN] {
            Value::Floating => Some(0u64),
            Value::Fixed { bits, width: cw } if cw == width => Some(bits as u64),
            _ => None,
        };
        match (
            inputs[Self::DIVIDEND_PIN],
            inputs[Self::DIVISOR_PIN],
            carry_in,
        ) {
            (
                Value::Fixed {
                    bits: dividend,
                    width: dw,
                },
                Value::Fixed {
                    bits: divisor,
                    width: sw,
                },
                Some(cin),
            ) if dw == width && sw == width => {
                // A zero divisor is treated as 1 rather than dividing by zero.
                let divisor = if divisor == 0 { 1 } else { divisor } as u64;
                // Widen to u64: the carry-in occupies the upper `width` bits, the
                // dividend the lower `width` bits, so the full dividend always fits
                // within 2*width <= 64 bits.
                let full_dividend = if width < 64 {
                    (cin << width) | dividend as u64
                } else {
                    dividend as u64
                };
                let quotient = ((full_dividend / divisor) & Value::mask(width) as u64) as u32;
                let remainder = ((full_dividend % divisor) & Value::mask(width) as u64) as u32;
                vec![Value::new(quotient, width), Value::new(remainder, width)]
            }
            _ => vec![Value::Floating, Value::Floating],
        }
    }

    fn input_width(&self, i: usize) -> Option<u8> {
        match i {
            Self::DIVIDEND_PIN | Self::DIVISOR_PIN | Self::CARRY_IN_PIN => Some(self.data_width),
            _ => None,
        }
    }

    fn output_width(&self, i: usize) -> Option<u8> {
        match i {
            Self::QUOTIENT_PIN | Self::REMAINDER_PIN => Some(self.data_width),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn divider(data_width: u8) -> Divider {
        Divider { data_width }
    }

    #[test]
    fn test_quotient_and_remainder_no_carry() {
        let d = divider(4);
        // 7 / 2 = 3 remainder 1
        assert_eq!(
            d.evaluate(&[Value::new(7, 4), Value::new(2, 4), Value::new(0, 4)]),
            vec![Value::new(3, 4), Value::new(1, 4)]
        );
    }

    #[test]
    fn test_floating_carry_in_behaves_as_zero() {
        let d = divider(4);
        assert_eq!(
            d.evaluate(&[Value::new(7, 4), Value::new(2, 4), Value::Floating]),
            vec![Value::new(3, 4), Value::new(1, 4)]
        );
    }

    #[test]
    fn test_carry_in_extends_dividend() {
        let d = divider(4);
        // Full dividend = (1 << 4) | 0 = 16, divided by 5 = 3 remainder 1.
        assert_eq!(
            d.evaluate(&[Value::new(0, 4), Value::new(5, 4), Value::new(1, 4)]),
            vec![Value::new(3, 4), Value::new(1, 4)]
        );
    }

    #[test]
    fn test_zero_divisor_treated_as_one() {
        let d = divider(4);
        assert_eq!(
            d.evaluate(&[Value::new(7, 4), Value::new(0, 4), Value::new(0, 4)]),
            vec![Value::new(7, 4), Value::new(0, 4)]
        );
    }

    #[test]
    fn test_exact_division_has_zero_remainder() {
        let d = divider(4);
        assert_eq!(
            d.evaluate(&[Value::new(8, 4), Value::new(4, 4), Value::new(0, 4)]),
            vec![Value::new(2, 4), Value::new(0, 4)]
        );
    }

    #[test]
    fn test_quotient_overflow_wraps() {
        let d = divider(4);
        // Full dividend = (15 << 4) | 15 = 255, divided by 1 = 255, which wraps to
        // 0b1111 (15) once masked to 4 bits.
        assert_eq!(
            d.evaluate(&[Value::new(15, 4), Value::new(1, 4), Value::new(15, 4)]),
            vec![Value::new(15, 4), Value::new(0, 4)]
        );
    }

    #[test]
    fn test_full_width_operands_do_not_panic() {
        // Max dividend/carry-in for a 32-bit divider: exercises the u64 widening
        // that avoids an overflow panic on the carry-in shift.
        let d = divider(32);
        let full_dividend = ((u32::MAX as u64) << 32) | u32::MAX as u64;
        let divisor = 3u64;
        let expected_quotient = (full_dividend / divisor) as u32;
        let expected_remainder = (full_dividend % divisor) as u32;
        assert_eq!(
            d.evaluate(&[
                Value::new(u32::MAX, 32),
                Value::new(3, 32),
                Value::new(u32::MAX, 32)
            ]),
            vec![
                Value::new(expected_quotient, 32),
                Value::new(expected_remainder, 32)
            ]
        );
    }

    #[test]
    fn test_mismatched_operand_width_yields_floating() {
        let d = divider(4);
        assert_eq!(
            d.evaluate(&[Value::new(1, 3), Value::new(1, 4), Value::new(0, 4)]),
            vec![Value::Floating, Value::Floating]
        );
    }

    #[test]
    fn test_mismatched_carry_in_width_yields_floating() {
        let d = divider(4);
        assert_eq!(
            d.evaluate(&[Value::new(1, 4), Value::new(1, 4), Value::new(0, 3)]),
            vec![Value::Floating, Value::Floating]
        );
    }

    #[test]
    fn test_floating_dividend_yields_floating() {
        let d = divider(4);
        assert_eq!(
            d.evaluate(&[Value::Floating, Value::new(1, 4), Value::new(0, 4)]),
            vec![Value::Floating, Value::Floating]
        );
    }

    #[test]
    fn test_input_output_widths() {
        let d = divider(4);
        assert_eq!(d.input_width(0), Some(4));
        assert_eq!(d.input_width(1), Some(4));
        assert_eq!(d.input_width(2), Some(4));
        assert_eq!(d.output_width(0), Some(4));
        assert_eq!(d.output_width(1), Some(4));
    }
}
