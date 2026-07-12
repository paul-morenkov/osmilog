use super::CombLogic;
use crate::sim::value::Value;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Adder {
    pub data_width: u8,
}

impl Adder {
    const ADDEND_1_PIN: usize = 0;
    const ADDEND_2_PIN: usize = 1;
    const CARRY_IN_PIN: usize = 2;
    const SUM_PIN: usize = 0;
    const CARRY_OUT_PIN: usize = 1;
}

impl CombLogic for Adder {
    fn n_inputs(&self) -> usize {
        // Two addends and a carry-in
        3
    }

    fn n_outputs(&self) -> usize {
        // Sum and carry-out
        2
    }

    fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        // A Floating carry-in behaves the same as a zero carry-in; anything else
        // non-Fixed, or a Fixed carry-in at the wrong width, falls through to Floating.
        let carry_in = match inputs[Self::CARRY_IN_PIN] {
            Value::Floating => Some(0u32),
            Value::Fixed { bits, width: 1 } => Some(bits),
            _ => None,
        };
        match (
            inputs[Self::ADDEND_1_PIN],
            inputs[Self::ADDEND_2_PIN],
            carry_in,
        ) {
            (Value::Fixed { bits: a, width: aw }, Value::Fixed { bits: b, width: bw }, Some(cin))
                if aw == self.data_width && bw == self.data_width =>
            {
                // Widen to u64 so a+b+cin can't overflow u32 (both addends can be up
                // to Value::mask(32) = u32::MAX) before it's split back into sum/carry.
                let sum_full = a as u64 + b as u64 + cin as u64;
                let sum = (sum_full & Value::mask(self.data_width) as u64) as u32;
                let carry = if self.data_width < 64 {
                    ((sum_full >> self.data_width) & 1) as u32
                } else {
                    0
                };
                vec![Value::new(sum, self.data_width), Value::new(carry, 1)]
            }
            _ => vec![Value::Floating, Value::Floating],
        }
    }

    fn input_width(&self, i: usize) -> Option<u8> {
        match i {
            Self::ADDEND_1_PIN | Self::ADDEND_2_PIN => Some(self.data_width),
            _ => Some(1), // carry-in
        }
    }

    fn output_width(&self, i: usize) -> Option<u8> {
        match i {
            Self::SUM_PIN => Some(self.data_width),
            Self::CARRY_OUT_PIN => Some(1),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adder(data_width: u8) -> Adder {
        Adder { data_width }
    }

    #[test]
    fn test_sum_no_carry() {
        let a = adder(4);
        assert_eq!(
            a.evaluate(&[Value::new(2, 4), Value::new(3, 4), Value::ZERO]),
            vec![Value::new(5, 4), Value::ZERO]
        );
    }

    #[test]
    fn test_carry_in_propagates() {
        let a = adder(4);
        assert_eq!(
            a.evaluate(&[Value::new(2, 4), Value::new(3, 4), Value::ONE]),
            vec![Value::new(6, 4), Value::ZERO]
        );
    }

    #[test]
    fn test_sum_overflows_to_carry_out() {
        let a = adder(4);
        // 15 + 2 = 17, which is 0b10001: wraps to 0b0001 (1) with carry-out set.
        assert_eq!(
            a.evaluate(&[Value::new(15, 4), Value::new(2, 4), Value::ZERO]),
            vec![Value::new(1, 4), Value::ONE]
        );
    }

    #[test]
    fn test_floating_carry_in_behaves_as_zero() {
        let a = adder(4);
        assert_eq!(
            a.evaluate(&[Value::new(2, 4), Value::new(3, 4), Value::Floating]),
            vec![Value::new(5, 4), Value::ZERO]
        );
    }

    #[test]
    fn test_carry_in_alone_can_trigger_carry_out() {
        let a = adder(4);
        // 15 + 0 + 1 = 16, which is 0b10000: wraps to 0 with carry-out set.
        assert_eq!(
            a.evaluate(&[Value::new(15, 4), Value::new(0, 4), Value::ONE]),
            vec![Value::new(0, 4), Value::ONE]
        );
    }

    #[test]
    fn test_full_width_addend_does_not_panic() {
        // Both addends at max u32 for a 32-bit adder: exercises the u64 widening
        // that avoids an overflow panic on plain u32 addition.
        let a = adder(32);
        // u32::MAX + u32::MAX + 1 = 2^33 - 1, which wraps to u32::MAX with carry-out set.
        assert_eq!(
            a.evaluate(&[
                Value::new(u32::MAX, 32),
                Value::new(u32::MAX, 32),
                Value::ONE
            ]),
            vec![Value::new(u32::MAX, 32), Value::ONE]
        );
    }

    #[test]
    fn test_mismatched_addend_width_yields_floating() {
        let a = adder(4);
        assert_eq!(
            a.evaluate(&[Value::new(1, 3), Value::new(1, 4), Value::ZERO]),
            vec![Value::Floating, Value::Floating]
        );
    }

    #[test]
    fn test_floating_addend_yields_floating() {
        let a = adder(4);
        assert_eq!(
            a.evaluate(&[Value::Floating, Value::new(1, 4), Value::ZERO]),
            vec![Value::Floating, Value::Floating]
        );
    }

    #[test]
    fn test_input_output_widths() {
        let a = adder(4);
        assert_eq!(a.input_width(0), Some(4));
        assert_eq!(a.input_width(1), Some(4));
        assert_eq!(a.input_width(2), Some(1));
        assert_eq!(a.output_width(0), Some(4));
        assert_eq!(a.output_width(1), Some(1));
    }
}
