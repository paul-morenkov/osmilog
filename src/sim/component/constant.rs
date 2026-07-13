use super::CombLogic;
use crate::sim::value::Value;

// Behaves exactly like Input (a fixed source value, no inputs) but is a
// distinct sim type so the GUI can tell them apart when deriving a
// subcircuit's boundary interface: an Input becomes a subcircuit input pin,
// a Constant does not (it just supplies a hardcoded value inside the
// subcircuit). See gui::app::build_doc_circuit.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Constant {
    pub bits: u32,
    pub width: u8,
}

impl CombLogic for Constant {
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
        unreachable!("Constant has no input pins")
    }
    fn output_width(&self, _i: usize) -> Option<u8> {
        Some(self.width)
    }
}
