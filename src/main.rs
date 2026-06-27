use circuit::Circuit;

use crate::{
    component::{Component, GateOp, InIdx, Logic, OutIdx, PinId, Pins},
    value::Value,
};

mod circuit;
mod component;
mod net;
mod value;

fn main() {
    let mut c = Circuit::new();
    let i1 = c.add_component(Component::input(Value::Floating));
    let i2 = c.add_component(Component::input(Value::Floating));
    let o1 = c.add_component(Component::output());
    let o2 = c.add_component(Component::output());

    let and = c.add_component(Component::gate(GateOp::And, 2, 1));
    let or = c.add_component(Component::gate(GateOp::Or, 2, 1));

    c.link(i1, PinId::output(0), and, PinId::input(0));
    c.link(i2, PinId::output(0), and, PinId::input(1));
    c.link(i1, PinId::output(0), or, PinId::input(0));
    c.link(i2, PinId::output(0), or, PinId::input(1));
    c.link(and, PinId::output(0), o1, PinId::input(0));
    c.link(or, PinId::output(0), o2, PinId::input(0));

    c.set_input(i1, Value::new(1, 1));
    c.set_input(i2, Value::new(0, 1));

    c.settle();
    println!("{:?}", c.read_output(o1));
    println!("{:?}", c.read_output(o2));
}
