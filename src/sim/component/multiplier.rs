use super::CombLogic;
use crate::sim::value::Value;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Multiplier {
    pub data_width: u8,
}

impl Multiplier {
    const MULTIPLICAND_PIN: usize = 0;
    const MULTIPLIER_PIN: usize = 1;
    const CARRY_IN_PIN: usize = 2;
    const PRODUCT_PIN: usize = 0;
    const CARRY_OUT_PIN: usize = 1;
}

impl CombLogic for Multiplier {
    fn n_inputs(&self) -> usize {
        // Multiplicand, multiplier, and a carry-in
        3
    }

    fn n_outputs(&self) -> usize {
        // Product and carry-out (the top half of the product)
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
            inputs[Self::MULTIPLICAND_PIN],
            inputs[Self::MULTIPLIER_PIN],
            carry_in,
        ) {
            (
                Value::Fixed { bits: a, width: aw },
                Value::Fixed { bits: b, width: bw },
                Some(cin),
            ) if aw == width && bw == width => {
                // Widen to u64: the product of two `width`-bit values plus a
                // `width`-bit carry-in always fits within 2*width <= 64 bits.
                let full = a as u64 * b as u64 + cin;
                let product = (full & Value::mask(width) as u64) as u32;
                let carry_out = if width < 64 {
                    ((full >> width) & Value::mask(width) as u64) as u32
                } else {
                    0
                };
                vec![Value::new(product, width), Value::new(carry_out, width)]
            }
            _ => vec![Value::Floating, Value::Floating],
        }
    }

    fn input_width(&self, i: usize) -> Option<u8> {
        match i {
            Self::MULTIPLICAND_PIN | Self::MULTIPLIER_PIN | Self::CARRY_IN_PIN => {
                Some(self.data_width)
            }
            _ => None,
        }
    }

    fn output_width(&self, i: usize) -> Option<u8> {
        match i {
            Self::PRODUCT_PIN | Self::CARRY_OUT_PIN => Some(self.data_width),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn multiplier(data_width: u8) -> Multiplier {
        Multiplier { data_width }
    }

    #[test]
    fn test_product_no_carry() {
        let m = multiplier(4);
        assert_eq!(
            m.evaluate(&[Value::new(2, 4), Value::new(3, 4), Value::new(0, 4)]),
            vec![Value::new(6, 4), Value::new(0, 4)]
        );
    }

    #[test]
    fn test_floating_carry_in_behaves_as_zero() {
        let m = multiplier(4);
        assert_eq!(
            m.evaluate(&[Value::new(2, 4), Value::new(3, 4), Value::Floating]),
            vec![Value::new(6, 4), Value::new(0, 4)]
        );
    }

    #[test]
    fn test_carry_in_propagates() {
        let m = multiplier(4);
        // 2 * 3 + 5 = 11, all within the low 4 bits, no overflow into carry-out.
        assert_eq!(
            m.evaluate(&[Value::new(2, 4), Value::new(3, 4), Value::new(5, 4)]),
            vec![Value::new(11, 4), Value::new(0, 4)]
        );
    }

    #[test]
    fn test_product_overflows_to_carry_out() {
        let m = multiplier(4);
        // 15 * 15 = 225 = 0b1110_0001: low 4 bits = 1, high 4 bits = 14.
        assert_eq!(
            m.evaluate(&[Value::new(15, 4), Value::new(15, 4), Value::new(0, 4)]),
            vec![Value::new(1, 4), Value::new(14, 4)]
        );
    }

    #[test]
    fn test_carry_in_alone_can_trigger_carry_out() {
        let m = multiplier(4);
        // 0 * 0 + 0, then carry-in near the top: 1 * 1 + 15 = 16 = 0b1_0000.
        assert_eq!(
            m.evaluate(&[Value::new(1, 4), Value::new(1, 4), Value::new(15, 4)]),
            vec![Value::new(0, 4), Value::new(1, 4)]
        );
    }

    #[test]
    fn test_full_width_operands_do_not_panic() {
        // Both operands at max u32 plus a max carry-in for a 32-bit multiplier:
        // exercises the u64 widening that avoids an overflow panic on plain u32
        // multiplication.
        let m = multiplier(32);
        let full = u32::MAX as u64 * u32::MAX as u64 + u32::MAX as u64;
        let expected_product = (full & Value::mask(32) as u64) as u32;
        let expected_carry = (full >> 32) as u32;
        assert_eq!(
            m.evaluate(&[
                Value::new(u32::MAX, 32),
                Value::new(u32::MAX, 32),
                Value::new(u32::MAX, 32)
            ]),
            vec![
                Value::new(expected_product, 32),
                Value::new(expected_carry, 32)
            ]
        );
    }

    #[test]
    fn test_mismatched_operand_width_yields_floating() {
        let m = multiplier(4);
        assert_eq!(
            m.evaluate(&[Value::new(1, 3), Value::new(1, 4), Value::new(0, 4)]),
            vec![Value::Floating, Value::Floating]
        );
    }

    #[test]
    fn test_mismatched_carry_in_width_yields_floating() {
        let m = multiplier(4);
        assert_eq!(
            m.evaluate(&[Value::new(1, 4), Value::new(1, 4), Value::new(0, 3)]),
            vec![Value::Floating, Value::Floating]
        );
    }

    #[test]
    fn test_floating_multiplicand_yields_floating() {
        let m = multiplier(4);
        assert_eq!(
            m.evaluate(&[Value::Floating, Value::new(1, 4), Value::new(0, 4)]),
            vec![Value::Floating, Value::Floating]
        );
    }

    #[test]
    fn test_input_output_widths() {
        let m = multiplier(4);
        assert_eq!(m.input_width(0), Some(4));
        assert_eq!(m.input_width(1), Some(4));
        assert_eq!(m.input_width(2), Some(4));
        assert_eq!(m.output_width(0), Some(4));
        assert_eq!(m.output_width(1), Some(4));
    }
}
