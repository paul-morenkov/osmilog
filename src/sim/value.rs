use std::ops::{Add, BitAnd, BitOr, BitXor, Not, Sub};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    #[default]
    Floating,
    Fixed {
        bits: u32,
        width: u8, // TODO: Verification of nonzero width
    },
    // A Net whose attached pins declare conflicting expected bit widths (e.g. a driver's
    // output width doesn't match a sink's expected input width). Distinct from Floating,
    // which just means "no concrete value yet" - Invalid means the wiring itself is wrong,
    // and holds regardless of whether any attached pin currently carries a real value. Set
    // only by Circuit::resolve_net(); CombLogic::evaluate() never produces it directly, and
    // an Invalid operand falls through the same catch-all arms as any other non-Fixed value
    // below, so it does not propagate past the one component reading it.
    Invalid,
}

impl Value {
    pub const ZERO: Value = Value::Fixed { bits: 0, width: 1 };
    pub const ONE: Value = Value::Fixed { bits: 1, width: 1 };

    pub fn new(bits: u32, width: u8) -> Self {
        Value::Fixed { bits, width }
    }
    pub fn mask(width: u8) -> u32 {
        if width >= 32 {
            u32::MAX
        } else {
            (1 << width) - 1
        }
    }
}

impl BitAnd for Value {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        match (self, rhs) {
            (Self::Fixed { bits: a, width: n }, Self::Fixed { bits: b, width: m }) if n == m => {
                Self::Fixed {
                    bits: a & b,
                    width: n,
                }
            }
            _ => Self::Floating,
        }
    }
}

impl BitOr for Value {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        match (self, rhs) {
            (Self::Fixed { bits: a, width: n }, Self::Fixed { bits: b, width: m }) if n == m => {
                Self::Fixed {
                    bits: a | b,
                    width: n,
                }
            }
            _ => Self::Floating,
        }
    }
}

impl BitXor for Value {
    type Output = Self;

    fn bitxor(self, rhs: Self) -> Self::Output {
        match (self, rhs) {
            (Self::Fixed { bits: a, width: n }, Self::Fixed { bits: b, width: m }) if n == m => {
                Self::Fixed {
                    bits: a ^ b,
                    width: n,
                }
            }
            _ => Self::Floating,
        }
    }
}

// TODO: Verification of overflow behavior on Add
impl Add for Value {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        match (self, rhs) {
            (Self::Fixed { bits: a, width: n }, Self::Fixed { bits: b, width: m }) if n == m => {
                Self::Fixed {
                    bits: a + b,
                    width: n,
                }
            }
            _ => Self::Floating,
        }
    }
}

// TODO: Verification of overflow behavior on Sub
impl Sub for Value {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        match (self, rhs) {
            (Self::Fixed { bits: a, width: n }, Self::Fixed { bits: b, width: m }) if n == m => {
                Self::Fixed {
                    bits: a - b,
                    width: n,
                }
            }
            _ => Self::Floating,
        }
    }
}

impl Not for Value {
    type Output = Self;

    fn not(self) -> Self::Output {
        match self {
            Self::Fixed { bits, width } => Self::Fixed {
                bits: !bits & Self::mask(width),
                width,
            },
            Self::Floating => Self::Floating,
            Self::Invalid => Self::Floating,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_or() {
        assert_eq!(
            (Value::new(0b110, 3) | Value::new(0b011, 3)),
            Value::new(0b111, 3)
        )
    }

    #[test]
    fn test_xor() {
        assert_eq!(
            (Value::new(0b110, 3) ^ Value::new(0b011, 3)),
            Value::new(0b101, 3)
        )
    }
    #[test]
    fn test_and() {
        assert_eq!(
            (Value::new(0b110, 3) & Value::new(0b011, 3)),
            Value::new(0b010, 3)
        )
    }

    #[test]
    fn test_not() {
        assert_eq!(!Value::new(0b010, 3), Value::new(0b101, 3))
    }

    #[test]
    fn test_not_floating() {
        assert_eq!(!Value::Floating, Value::Floating)
    }

    #[test]
    fn test_add() {
        assert_eq!((Value::new(2, 4) + Value::new(3, 4)), Value::new(5, 4))
    }

    #[test]
    fn test_sub() {
        assert_eq!((Value::new(5, 4) - Value::new(3, 4)), Value::new(2, 4))
    }

    #[test]
    fn test_mismatched_width_is_floating() {
        assert_eq!(Value::new(0b11, 2) & Value::new(0b11, 3), Value::Floating);
        assert_eq!(Value::new(0b11, 2) | Value::new(0b11, 3), Value::Floating);
        assert_eq!(Value::new(0b11, 2) ^ Value::new(0b11, 3), Value::Floating);
        assert_eq!(Value::new(0b11, 2) + Value::new(0b11, 3), Value::Floating);
        assert_eq!(Value::new(0b11, 2) - Value::new(0b11, 3), Value::Floating);
    }

    #[test]
    fn test_floating_operand_is_floating() {
        assert_eq!(Value::Floating & Value::new(0b1, 1), Value::Floating);
        assert_eq!(Value::new(0b1, 1) | Value::Floating, Value::Floating);
        assert_eq!(Value::Floating ^ Value::Floating, Value::Floating);
        assert_eq!(Value::Floating + Value::new(0b1, 1), Value::Floating);
        assert_eq!(Value::new(0b1, 1) - Value::Floating, Value::Floating);
    }

    #[test]
    fn test_default_is_floating() {
        assert_eq!(Value::default(), Value::Floating)
    }

    #[test]
    fn test_mask() {
        assert_eq!(Value::mask(0), 0);
        assert_eq!(Value::mask(1), 0b1);
        assert_eq!(Value::mask(3), 0b111);
        assert_eq!(Value::mask(31), u32::MAX >> 1);
        assert_eq!(Value::mask(32), u32::MAX);
        assert_eq!(Value::mask(33), u32::MAX);
    }
}
