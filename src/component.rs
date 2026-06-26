use crate::net::{Net, NetKey};
use crate::value::Value;
use slotmap::{new_key_type, SlotMap};

new_key_type! {
    pub struct CompKey;
}

pub struct Component {
    pub pins: Pins,
    pub logic: Logic,
}

impl Component {
    // TODO: Consider returning pin index information
    pub fn evaluate(&self, nets: &SlotMap<NetKey, Net>) -> Vec<Value> {
        let read_pin = |i: usize| -> Value {
            match self.pins.inputs[i] {
                Some(net) => nets[net].value,
                None => Value::Floating,
            }
        };

        match &self.logic {
            Logic::Gate(op) => match op {
                GateOp::And | GateOp::Nand => {}
                GateOp::Or | GateOp::Nor => {}
                GateOp::Xor | GateOp::Xnor => {}
                GateOp::Not => {}
            },
            Logic::Mux => todo!(),
            Logic::Demux => todo!(),
            Logic::Reg => todo!(),
        }
    }

    pub fn net_of(&self, pin: PinId) -> Option<NetKey> {
        match pin {
            // TODO: will panic on out of bounds, fix this
            PinId::In(i) => self.pins.inputs[i.0 as usize],
            PinId::Out(i) => self.pins.outputs[i.0 as usize],
        }
    }
    pub fn set_pin_net(&mut self, pin: PinId, net: NetKey) {
        match pin {
            PinId::In(i) => self.pins.inputs[i.0 as usize] = Some(net),
            PinId::Out(i) => self.pins.outputs[i.0 as usize] = Some(net),
        };
    }

    pub fn is_sequential(&self) -> bool {
        match self.logic {
            Logic::Gate(_) | Logic::Mux | Logic::Demux => false,
            Logic::Reg => true,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct InIdx(u8);

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct OutIdx(u8);

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PinId {
    In(InIdx),
    Out(OutIdx),
}

#[derive(Debug)]
pub struct Pins {
    pub inputs: Vec<Option<NetKey>>,
    pub outputs: Vec<Option<NetKey>>,
}

pub enum Logic {
    Gate(GateOp),
    Mux,
    Demux,
    Reg,
}

pub enum GateOp {
    And,
    Or,
    Xor,
    Xnor,
    Nand,
    Nor,
    Not,
}
