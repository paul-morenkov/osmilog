use super::CombLogic;
use crate::sim::value::Value;

// Inputs: 0 -> enable_in; 1.. -> arms (2^sel_width of them)
// Outputs: 0 -> selector; 1 -> enable_out; 2 -> group_out
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Encoder {
    pub sel_width: u8,
}

impl CombLogic for Encoder {
    fn n_inputs(&self) -> usize {
        (1usize << self.sel_width) + 1
    }
    fn n_outputs(&self) -> usize {
        3
    }
    fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        let enable_in = inputs[0];
        let n_arms = 1 << self.sel_width;

        match enable_in {
            // Malformed enable_in (wrong bit width): the whole component goes Floating.
            Value::Fixed { width, .. } if width != 1 => {
                vec![Value::Floating, Value::Floating, Value::Floating]
            }
            // Explicitly disabled: selector = Floating, EN_OUT/GRP_OUT = 0.
            Value::Fixed { bits: 0, width: 1 } => {
                vec![Value::Floating, Value::new(0, 1), Value::new(0, 1)]
            }
            // Enabled: Floating or Fixed{width:1, bits != 0}.
            _ => {
                let highest_set = (1..n_arms + 1)
                    .map(|i| inputs[i])
                    .rposition(|v| v == Value::new(1, 1));

                if let Some(i) = highest_set {
                    // If an input is 1: selector = i, EN_OUT = 0, GRP_OUT = 1.
                    vec![
                        Value::new(i as u32, self.sel_width),
                        Value::new(0, 1),
                        Value::new(1, 1),
                    ]
                } else {
                    // If enabled but no inputs 1: selector = Floating, EN_OUT = 1, GRP_OUT = 0.
                    vec![Value::Floating, Value::new(1, 1), Value::new(0, 1)]
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    // Builds the evaluate() input vector: [enable, arm0, arm1, ...], with each
    // arm set to bit i of `mask`.
    fn make_inputs(sel_width: u8, enable: Value, mask: u32) -> Vec<Value> {
        let n_arms = 1usize << sel_width;
        let mut inputs = vec![enable];
        inputs.extend((0..n_arms).map(|i| Value::new((mask >> i) & 1, 1)));
        inputs
    }

    #[test_case(0b0000, None,    1, 0 ; "no arms set")]
    #[test_case(0b0001, Some(0), 0, 1 ; "arm 0 only")]
    #[test_case(0b0010, Some(1), 0, 1 ; "arm 1 only")]
    #[test_case(0b0100, Some(2), 0, 1 ; "arm 2 only")]
    #[test_case(0b1000, Some(3), 0, 1 ; "arm 3 only")]
    #[test_case(0b0011, Some(1), 0, 1 ; "arms 0 and 1 set, highest wins")]
    #[test_case(0b1010, Some(3), 0, 1 ; "arms 1 and 3 set, highest wins")]
    #[test_case(0b1111, Some(3), 0, 1 ; "all arms set, highest wins")]
    fn test_truth_table_priority_selects_highest_set_arm(
        mask: u32,
        expected_sel: Option<u32>,
        expected_en: u32,
        expected_grp: u32,
    ) {
        let enc = Encoder { sel_width: 2 };
        let expected_sel_value = match expected_sel {
            Some(i) => Value::new(i, 2),
            None => Value::Floating,
        };
        assert_eq!(
            enc.evaluate(&make_inputs(2, Value::new(1, 1), mask)),
            vec![
                expected_sel_value,
                Value::new(expected_en, 1),
                Value::new(expected_grp, 1),
            ]
        );
    }

    #[test_case(0b0000 ; "disabled, no arms set")]
    #[test_case(0b0001 ; "disabled, arm 0 set")]
    #[test_case(0b1111 ; "disabled, all arms set")]
    fn test_disabled_forces_floating_selector_and_zero_flags(mask: u32) {
        let enc = Encoder { sel_width: 2 };
        assert_eq!(
            enc.evaluate(&make_inputs(2, Value::new(0, 1), mask)),
            vec![Value::Floating, Value::new(0, 1), Value::new(0, 1)]
        );
    }

    #[test]
    fn test_enable_dynamic_toggle_updates_outputs() {
        let enc = Encoder { sel_width: 2 };
        // Disabled: arm 1 hot but ignored.
        assert_eq!(
            enc.evaluate(&make_inputs(2, Value::new(0, 1), 0b0010)),
            vec![Value::Floating, Value::new(0, 1), Value::new(0, 1)]
        );
        // Enabled: arm 1 is still hot, so it should now fire.
        assert_eq!(
            enc.evaluate(&make_inputs(2, Value::new(1, 1), 0b0010)),
            vec![Value::new(1, 2), Value::new(0, 1), Value::new(1, 1)]
        );
    }

    #[test]
    fn test_enable_in_floating_passes_through() {
        let enc = Encoder { sel_width: 2 };
        // A Floating enable_in is not treated as disabled - it falls through to the
        // normal arm scan, same as an explicitly-enabled encoder.
        assert_eq!(
            enc.evaluate(&make_inputs(2, Value::Floating, 0b0010)),
            vec![Value::new(1, 2), Value::new(0, 1), Value::new(1, 1)]
        );
    }

    #[test_case(2 ; "enable width 2")]
    #[test_case(3 ; "enable width 3")]
    fn test_enable_in_wrong_width_forces_all_outputs_floating(enable_width: u8) {
        let enc = Encoder { sel_width: 2 };
        // arm 0 hot; would fire if enable_in were well-formed.
        assert_eq!(
            enc.evaluate(&make_inputs(2, Value::new(0, enable_width), 0b0001)),
            vec![Value::Floating, Value::Floating, Value::Floating]
        );
    }

    #[test]
    fn test_unconnected_arm_treated_as_unset() {
        let enc = Encoder { sel_width: 2 };
        // Arm 3 (input pin 4) is unconnected -> Floating.
        assert_eq!(
            enc.evaluate(&[
                Value::new(1, 1),
                Value::new(0, 1),
                Value::new(0, 1),
                Value::new(0, 1),
                Value::Floating,
            ]),
            vec![Value::Floating, Value::new(1, 1), Value::new(0, 1)]
        );
    }

    #[test]
    fn test_wide_arm_value_never_registers_as_set() {
        let enc = Encoder { sel_width: 2 };
        // Arm 2 never registers as "set" despite bits=1, because its width (2) != 1:
        // the encoder's arm comparison requires an exact Value::Fixed{bits:1,width:1}.
        assert_eq!(
            enc.evaluate(&[
                Value::new(1, 1),
                Value::new(0, 1),
                Value::new(0, 1),
                Value::new(1, 2),
                Value::new(0, 1),
            ]),
            vec![Value::Floating, Value::new(1, 1), Value::new(0, 1)]
        );
    }

    #[test]
    fn test_sel_width_zero_degenerate_single_arm() {
        let enc = Encoder { sel_width: 0 };
        assert_eq!(enc.n_inputs(), 2); // enable_in + 1 arm

        assert_eq!(
            enc.evaluate(&[Value::new(1, 1), Value::new(0, 1)]),
            vec![Value::Floating, Value::new(1, 1), Value::new(0, 1)]
        );

        // A 0-bit-wide selector: the only possible index (0) is still a well-formed
        // Value::Fixed{bits:0,width:0} rather than Floating.
        assert_eq!(
            enc.evaluate(&[Value::new(1, 1), Value::new(1, 1)]),
            vec![Value::new(0, 0), Value::new(0, 1), Value::new(1, 1)]
        );
    }

    // Cascades two 4-arm priority encoders: enc1's enable_out feeds enc2's enable_in, so
    // enc2 only ever gets a chance to fire when enc1 found nothing. Only the top-level
    // enable_in (enc1's) is externally driven.
    #[test_case(0, 0b0000, 0b0000, false, false ; "top disabled, no arms hot anywhere")]
    #[test_case(0, 0b0101, 0b1111, false, false ; "top disabled, arms hot on both sides, still fully quiet")]
    #[test_case(1, 0b0101, 0b1111, true,  false ; "enc1 fires, suppresses enc2 despite its arms being hot")]
    #[test_case(1, 0b0000, 0b0010, false, true  ; "enc1 empty, enc2 fires")]
    #[test_case(1, 0b0000, 0b0000, false, false ; "enc1 empty, enc2 empty, both quiet but chain enabled")]
    fn test_chain_group_priority_and_activation(
        top_enable: u32,
        arms1_mask: u32,
        arms2_mask: u32,
        expect_enc1_fires: bool,
        expect_enc2_fires: bool,
    ) {
        let sel_width = 2u8;
        let enc1 = Encoder { sel_width };
        let enc2 = Encoder { sel_width };

        let out1 = enc1.evaluate(&make_inputs(
            sel_width,
            Value::new(top_enable, 1),
            arms1_mask,
        ));
        let (sel1, en1_out, grp1_out) = (out1[0], out1[1], out1[2]);

        // Cascade: enc1's enable_out feeds enc2's enable_in.
        let out2 = enc2.evaluate(&make_inputs(sel_width, en1_out, arms2_mask));
        let (sel2, en2_out, grp2_out) = (out2[0], out2[1], out2[2]);

        assert_eq!(grp1_out, Value::new(expect_enc1_fires as u32, 1));
        assert_eq!(grp2_out, Value::new(expect_enc2_fires as u32, 1));
        // The two encoders never both claim to have fired.
        assert!(!(expect_enc1_fires && expect_enc2_fires));

        if expect_enc1_fires {
            let expected_i = 31 - arms1_mask.leading_zeros(); // highest set bit index
            assert_eq!(sel1, Value::new(expected_i, sel_width));
            assert_eq!(en1_out, Value::new(0, 1));
            // enc2 never got an enabled enable_in, so it stays fully quiet even
            // though arms2_mask may be hot.
            assert_eq!(sel2, Value::Floating);
            assert_eq!(en2_out, Value::new(0, 1));
        } else if expect_enc2_fires {
            let expected_i = 31 - arms2_mask.leading_zeros();
            assert_eq!(sel1, Value::Floating);
            assert_eq!(en1_out, Value::new(1, 1));
            assert_eq!(sel2, Value::new(expected_i, sel_width));
            assert_eq!(en2_out, Value::new(0, 1));
        } else if top_enable == 1 {
            // Chain fully enabled but nothing anywhere is set.
            assert_eq!(sel1, Value::Floating);
            assert_eq!(en1_out, Value::new(1, 1));
            assert_eq!(sel2, Value::Floating);
            assert_eq!(en2_out, Value::new(1, 1));
        } else {
            // Top disabled: enc1 is disabled outright (en1_out forced to 0), so enc2's
            // enable_in sees an explicit 0 too and is disabled regardless of its arms.
            assert_eq!(sel1, Value::Floating);
            assert_eq!(en1_out, Value::new(0, 1));
            assert_eq!(sel2, Value::Floating);
            assert_eq!(en2_out, Value::new(0, 1));
        }
    }
}
