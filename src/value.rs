use std::ops::{Add, BitAnd, BitOr, BitXor, Not, Sub};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    #[default]
    Floating,
    Fixed {
        bits: u32,
        width: u8, // TODO: Verification of nonzero width
    },
}

impl Value {
    pub fn new(bits: u32, width: u8) -> Self {
        Value::Fixed { bits, width }
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
                bits: !bits & ((1 << width) - 1),
                width,
            },
            Self::Floating => Self::Floating,
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
}
