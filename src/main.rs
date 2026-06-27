use circuit::Circuit;

use crate::{
    component::{Component, GateOp, PinId},
    value::Value,
};

mod circuit;
mod component;
mod net;
mod value;

fn main() {
    let mut c = Circuit::new();
    let i1 = c.add_component(Component::input(Value::new(1, 1)));
    let i2 = c.add_component(Component::input(Value::new(0, 1)));
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

    // c.set_input(i1, Value::new(1, 1));
    // c.set_input(i2, Value::new(0, 1));

    c.settle();

    println!("{:?}", c.read_output(o1));
    println!("{:?}", c.read_output(o2));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_and_or() {
        let mut c = Circuit::new();
        let i1 = c.add_component(Component::input(Value::new(1, 1)));
        let i2 = c.add_component(Component::input(Value::new(0, 1)));
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

        c.settle();
        assert_eq!(c.read_output(o1), Value::new(0, 1));
        assert_eq!(c.read_output(o2), Value::new(1, 1));
    }

    #[test]
    fn test_mux() {
        let mut c = Circuit::new();
        let i1 = c.add_component(Component::input(Value::new(3, 2)));
        let i2 = c.add_component(Component::input(Value::new(2, 2)));
        let i3 = c.add_component(Component::input(Value::new(1, 2)));
        let i4 = c.add_component(Component::input(Value::new(0, 2)));
        let sel = c.add_component(Component::input(Value::new(0, 2)));

        let o1 = c.add_component(Component::output());

        let mux = c.add_component(Component::mux(2, 2));

        c.link(i1, PinId::output(0), mux, PinId::input(1));
        c.link(i2, PinId::output(0), mux, PinId::input(2));
        c.link(i3, PinId::output(0), mux, PinId::input(3));
        c.link(i4, PinId::output(0), mux, PinId::input(4));
        c.link(sel, PinId::output(0), mux, PinId::input(0));

        c.link(mux, PinId::output(0), o1, PinId::input(0));

        c.settle();
        assert_eq!(c.read_output(o1), Value::new(3, 2));
        c.set_input(sel, Value::new(1, 2));
        c.settle();
        assert_eq!(c.read_output(o1), Value::new(2, 2));
        c.set_input(sel, Value::new(2, 2));
        c.settle();
        assert_eq!(c.read_output(o1), Value::new(1, 2));
        c.set_input(sel, Value::new(3, 2));
        c.settle();
        assert_eq!(c.read_output(o1), Value::new(0, 2));
    }
}
