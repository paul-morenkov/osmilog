use super::CombLogic;
use crate::sim::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FanDirection {
    Right,
    Left,
}

#[derive(Debug)]
pub struct Splitter {
    arms: u8,
    direction: FanDirection,
    // routing[i] = Some((arm, slot)) => the i'th data bit is arm `arm`'s
    // `slot`-th bit; None => unrouted (dropped in Right mode, always 0 in
    // the merged Left-mode trunk). Only constructible via Splitter::new(),
    // which builds this from an arm-major Vec<Vec<u8>> so every arm index
    // here is guaranteed valid.
    routing: Vec<Option<(u8, u8)>>,
    // arm_width[j] = number of data bits owned by arm j. In Right mode this
    // is the width evaluate() gives that arm's output; in Left mode it's
    // the width evaluate() requires on that arm's input.
    arm_width: Vec<u8>,
}

impl Splitter {
    // arm_bits[j] lists the data-bit indices routed to arm j, e.g.
    // arm_bits = [[0, 2], [1, 3]] sends bits 0,2 to arm0 and bits 1,3 to arm1.
    // Arm indices are just positions in `arm_bits`, so an out-of-range arm
    // reference isn't representable. If a bit index is listed in more than one
    // arm, the later arm (by position in `arm_bits`) wins. `direction` picks
    // which side of this bit-to-arm mapping is the input: Right keeps the
    // classic splitter shape (1 input bus -> arms outputs); Left inverts it
    // into a combiner (arms inputs -> 1 output bus), using the same mapping.
    pub fn new(arm_bits: Vec<Vec<u8>>, direction: FanDirection) -> Self {
        let arms = arm_bits.len() as u8;
        let width = arm_bits
            .iter()
            .flatten()
            .map(|&bit| bit + 1)
            .max()
            .unwrap_or(0);
        let mut owner = vec![None; width as usize];
        for (arm, bits) in arm_bits.into_iter().enumerate() {
            for bit in bits {
                owner[bit as usize] = Some(arm as u8);
            }
        }
        // routing[i] = Some((arm, slot)) => data bit i is arm `arm`'s `slot`-th
        // bit (0 = LSB of that arm's Value), in ascending data-bit order per
        // arm. Precomputed once here (rather than recounted on every
        // evaluate() call) since it depends only on arm_bits' structure.
        let mut arm_width = vec![0u8; arms as usize];
        let routing: Vec<Option<(u8, u8)>> = owner
            .into_iter()
            .map(|maybe_arm| {
                maybe_arm.map(|arm| {
                    let slot = arm_width[arm as usize];
                    arm_width[arm as usize] += 1;
                    (arm, slot)
                })
            })
            .collect();

        Self {
            arms,
            direction,
            routing,
            arm_width,
        }
    }

    pub fn data_width(&self) -> u8 {
        self.routing.len() as u8
    }
    pub fn direction(&self) -> FanDirection {
        self.direction
    }

    // Reconstructs an arm-major arm_bits from routing/arm_width, inverting
    // the mapping built in new(). Bit-claim collisions were already resolved
    // at construction time, so this round-trips through Splitter::new() to
    // an identical routing/arm_width even though it isn't necessarily the
    // exact Vec<Vec<u8>> originally passed in (e.g. a bit claimed by two
    // arms only appears under the winning arm here, same as it does in the
    // live routing table).
    pub(crate) fn arm_bits(&self) -> Vec<Vec<u8>> {
        let mut arm_bits: Vec<Vec<u8>> =
            self.arm_width.iter().map(|&w| vec![0u8; w as usize]).collect();
        for (bit, routed) in self.routing.iter().enumerate() {
            if let Some((arm, slot)) = routed {
                arm_bits[*arm as usize][*slot as usize] = bit as u8;
            }
        }
        arm_bits
    }
}

impl CombLogic for Splitter {
    fn n_inputs(&self) -> usize {
        match self.direction {
            FanDirection::Right => 1,
            FanDirection::Left => self.arms as usize,
        }
    }
    fn n_outputs(&self) -> usize {
        match self.direction {
            FanDirection::Right => self.arms as usize,
            FanDirection::Left => 1,
        }
    }
    fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        match self.direction {
            FanDirection::Right => match inputs[0] {
                Value::Fixed { bits, .. } => {
                    let mut arm_bits_out = vec![0u32; self.arms as usize];
                    for (data_i, route) in self.routing.iter().enumerate() {
                        if let Some((arm, slot)) = *route {
                            if bits & (1 << data_i) != 0 {
                                arm_bits_out[arm as usize] |= 1 << slot;
                            }
                        }
                    }
                    (0..self.arms as usize)
                        .map(|arm| Value::new(arm_bits_out[arm], self.arm_width[arm]))
                        .collect()
                }
                Value::Floating | Value::Invalid => vec![Value::Floating; self.arms as usize],
            },
            FanDirection::Left => {
                let arm_vals: Vec<Value> = (0..self.arms as usize).map(|i| inputs[i]).collect();
                // Every arm that owns >=1 bit must be driven at exactly the
                // width it owns; Floating or a width mismatch poisons the
                // whole merged output, mirroring Value's own bitwise-op
                // width-mismatch convention rather than silently
                // truncating/zero-extending.
                let widths_ok = (0..self.arms as usize).all(|arm| {
                    self.arm_width[arm] == 0
                        || matches!(arm_vals[arm], Value::Fixed { width, .. } if width == self.arm_width[arm])
                });
                if !widths_ok {
                    vec![Value::Floating]
                } else {
                    let mut out_bits = 0u32;
                    for (data_i, route) in self.routing.iter().enumerate() {
                        if let Some((arm, slot)) = *route {
                            if let Value::Fixed { bits, .. } = arm_vals[arm as usize] {
                                if bits & (1 << slot) != 0 {
                                    out_bits |= 1 << data_i;
                                }
                            }
                        }
                    }
                    vec![Value::new(out_bits, self.data_width())]
                }
            }
        }
    }
    fn input_width(&self, i: usize) -> Option<u8> {
        match self.direction {
            FanDirection::Right => Some(self.data_width()), // trunk bus
            // An arm owning zero bits accepts any width, mirroring evaluate()'s own
            // `arm_width == 0` "accepts anything" special case.
            FanDirection::Left => match self.arm_width[i] {
                0 => None,
                w => Some(w),
            },
        }
    }
    fn output_width(&self, i: usize) -> Option<u8> {
        match self.direction {
            FanDirection::Right => match self.arm_width[i] {
                0 => None,
                w => Some(w),
            },
            FanDirection::Left => Some(self.data_width()), // trunk bus
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_contiguous_halves() {
        // 4-bit bus split into two 2-bit arms: bits [0,1] -> arm0, bits [2,3] -> arm1.
        let s = Splitter::new(vec![vec![0, 1], vec![2, 3]], FanDirection::Right);
        assert_eq!(
            s.evaluate(&[Value::new(0, 4)]),
            vec![Value::new(0, 2), Value::new(0, 2)]
        );
        assert_eq!(
            s.evaluate(&[Value::new(0b0001, 4)]),
            vec![Value::new(1, 2), Value::new(0, 2)]
        );
        assert_eq!(
            s.evaluate(&[Value::new(0b0100, 4)]),
            vec![Value::new(0, 2), Value::new(1, 2)]
        );
        assert_eq!(
            s.evaluate(&[Value::new(0b1111, 4)]),
            vec![Value::new(3, 2), Value::new(3, 2)]
        );
    }

    #[test]
    fn test_interleaved() {
        // 4-bit bus, even bits (0,2) -> arm0, odd bits (1,3) -> arm1.
        let s = Splitter::new(vec![vec![0, 2], vec![1, 3]], FanDirection::Right);
        assert_eq!(
            s.evaluate(&[Value::new(0, 4)]),
            vec![Value::new(0, 2), Value::new(0, 2)]
        );
        assert_eq!(
            s.evaluate(&[Value::new(0b0100, 4)]), // bit2 (even) -> arm0 pos1
            vec![Value::new(2, 2), Value::new(0, 2)]
        );
        assert_eq!(
            s.evaluate(&[Value::new(0b1000, 4)]), // bit3 (odd) -> arm1 pos1
            vec![Value::new(0, 2), Value::new(2, 2)]
        );
        assert_eq!(
            s.evaluate(&[Value::new(0b1010, 4)]), // bits 1,3 -> arm1 full
            vec![Value::new(0, 2), Value::new(3, 2)]
        );
        assert_eq!(
            s.evaluate(&[Value::new(0b1111, 4)]),
            vec![Value::new(3, 2), Value::new(3, 2)]
        );
    }

    #[test]
    fn test_full_spread() {
        // Each of the 4 bits fans out to its own dedicated 1-bit arm.
        let s = Splitter::new(
            vec![vec![0], vec![1], vec![2], vec![3]],
            FanDirection::Right,
        );
        assert_eq!(
            s.evaluate(&[Value::new(0, 4)]),
            vec![Value::ZERO, Value::ZERO, Value::ZERO, Value::ZERO]
        );
        assert_eq!(
            s.evaluate(&[Value::new(0b0100, 4)]),
            vec![Value::ZERO, Value::ZERO, Value::ONE, Value::ZERO]
        );
        assert_eq!(
            s.evaluate(&[Value::new(0b1111, 4)]),
            vec![Value::ONE, Value::ONE, Value::ONE, Value::ONE]
        );
    }

    #[test]
    fn test_floating_input_propagates_to_all_arms() {
        let s = Splitter::new(vec![vec![0], vec![1], vec![2]], FanDirection::Right);
        assert_eq!(s.evaluate(&[Value::Floating]), vec![Value::Floating; 3]);
    }

    #[test]
    fn test_zero_arms_produces_empty_output() {
        let s = Splitter::new(vec![], FanDirection::Right);
        assert_eq!(s.n_outputs(), 0);
        assert!(s.evaluate(&[Value::Floating]).is_empty());
    }

    #[test]
    fn test_arm_with_no_mapped_bits_is_zero_width() {
        // arm 2 is listed with no bits, so it should receive nothing.
        let s = Splitter::new(vec![vec![0], vec![1], vec![]], FanDirection::Right);
        let out = s.evaluate(&[Value::new(0b11, 2)]);
        assert_eq!(out[2], Value::new(0, 0));
    }

    #[test]
    fn test_unrouted_high_bits_of_wider_input_are_ignored() {
        // arm_bits only covers the low 2 bits of a 4-bit input value; the
        // upper bits (2,3) are unrouted and should have no effect on any arm.
        let s = Splitter::new(vec![vec![0], vec![1]], FanDirection::Right);
        assert_eq!(
            s.evaluate(&[Value::new(0b1101, 4)]),
            vec![Value::ONE, Value::ZERO]
        );
    }

    #[test]
    fn test_bit_claimed_by_multiple_arms_last_arm_wins() {
        // bit1 is listed under both arm0 and arm1; arm_bits is processed in
        // order, so the later arm (arm1) should end up owning it, not arm0.
        let s = Splitter::new(vec![vec![0, 1], vec![1]], FanDirection::Right);
        assert_eq!(
            s.evaluate(&[Value::new(0b01, 2)]), // bit0 set, bit1 clear
            vec![Value::ONE, Value::ZERO]
        );
        assert_eq!(
            s.evaluate(&[Value::new(0b10, 2)]), // bit0 clear, bit1 set
            vec![Value::ZERO, Value::ONE]
        );
    }

    #[test]
    fn test_data_width_derived_from_arm_bits() {
        let s = Splitter::new(vec![vec![0, 2], vec![1]], FanDirection::Right);
        assert_eq!(s.data_width(), 3);
        assert_eq!(s.direction(), FanDirection::Right);

        let s = Splitter::new(vec![vec![0, 2], vec![1]], FanDirection::Left);
        assert_eq!(s.direction(), FanDirection::Left);
    }

    #[test]
    fn test_combine_contiguous_halves() {
        // Inverse of test_contiguous_halves: two 2-bit arms merge into
        // a single 4-bit output, bits [0,1] from arm0, bits [2,3] from arm1.
        let s = Splitter::new(vec![vec![0, 1], vec![2, 3]], FanDirection::Left);
        assert_eq!(
            s.evaluate(&[Value::new(0, 2), Value::new(0, 2)]),
            vec![Value::new(0, 4)]
        );
        assert_eq!(
            s.evaluate(&[Value::new(0b01, 2), Value::new(0, 2)]),
            vec![Value::new(0b0001, 4)]
        );
        assert_eq!(
            s.evaluate(&[Value::new(0b01, 2), Value::new(0b10, 2)]),
            vec![Value::new(0b1001, 4)]
        );
    }

    #[test]
    fn test_combine_floating_arm_propagates() {
        // One owning arm left unconnected (Floating) poisons the whole merged
        // output, regardless of the other arm's value.
        let s = Splitter::new(vec![vec![0], vec![1]], FanDirection::Left);
        assert_eq!(
            s.evaluate(&[Value::new(0b1, 1), Value::Floating]),
            vec![Value::Floating]
        );
    }

    #[test]
    fn test_combine_width_mismatch_yields_floating() {
        // arm0 owns 2 bits but is driven by a 3-bit source -> Floating.
        let s = Splitter::new(vec![vec![0, 1], vec![2]], FanDirection::Left);
        assert_eq!(
            s.evaluate(&[Value::new(0, 3), Value::ZERO]),
            vec![Value::Floating]
        );
    }

    #[test]
    fn test_combine_unrouted_bit_defaults_zero() {
        // arm_bits only covers bits 0 and 2 of a 3-bit merged output; bit 1 has
        // no owning arm and should always read as 0, regardless of other arms.
        let s = Splitter::new(vec![vec![0], vec![2]], FanDirection::Left);
        assert_eq!(
            s.evaluate(&[Value::new(0b1, 1), Value::new(0b1, 1)]),
            vec![Value::new(0b101, 3)]
        );
    }

    #[test]
    fn test_combine_zero_arms_produces_single_zero_output() {
        // Deliberately asymmetric with test_zero_arms_produces_empty_output:
        // Left mode always has exactly one trunk output pin, even with zero arms,
        // whereas Right mode with zero arms has zero output pins.
        let s = Splitter::new(vec![], FanDirection::Left);
        assert_eq!(s.n_inputs(), 0);
        assert_eq!(s.evaluate(&[]), vec![Value::new(0, 0)]);
    }

    #[test]
    fn test_arm_bits_round_trips_through_new() {
        // Interleaved mapping (not a trivial contiguous split) exercises
        // reconstruction across non-adjacent bit indices per arm.
        let s1 = Splitter::new(vec![vec![0, 2], vec![1, 3]], FanDirection::Right);
        let reconstructed = s1.arm_bits();
        let s2 = Splitter::new(reconstructed, FanDirection::Right);
        assert_eq!(s1.data_width(), s2.data_width());
        assert_eq!(
            s1.evaluate(&[Value::new(0b1011, 4)]),
            s2.evaluate(&[Value::new(0b1011, 4)])
        );
    }

    #[test]
    fn test_arm_bits_round_trips_after_collision_resolution() {
        // Bit 0 is claimed by both arms; the later arm (arm1) wins per
        // Splitter::new's documented precedence. arm_bits() must reflect the
        // already-resolved winner, not the original ambiguous input.
        let s1 = Splitter::new(vec![vec![0, 1], vec![0]], FanDirection::Right);
        let reconstructed = s1.arm_bits();
        let s2 = Splitter::new(reconstructed, FanDirection::Right);
        assert_eq!(
            s1.evaluate(&[Value::new(0b11, 2)]),
            s2.evaluate(&[Value::new(0b11, 2)])
        );
    }
}
