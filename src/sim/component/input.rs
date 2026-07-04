use super::CombLogic;
use crate::sim::value::Value;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    pub bits: u32,
    pub width: u8,
}

impl CombLogic for Input {
    fn n_inputs(&self) -> usize {
        0
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn evaluate(&self, _inputs: &[Value]) -> Vec<Value> {
        vec![Value::Fixed {
            bits: self.bits,
            width: self.width,
        }]
    }
    fn input_width(&self, _i: usize) -> Option<u8> {
        unreachable!("Input has no input pins")
    }
    fn output_width(&self, _i: usize) -> Option<u8> {
        Some(self.width)
    }
}
