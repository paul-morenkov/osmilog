pub mod app;
pub mod circuit;
pub mod component;
pub mod geometry;
pub mod net;
pub mod shape;
pub mod value;

#[cfg(test)]
mod tests {
    use crate::{
        circuit::Circuit,
        component::{Component, GateOp, PinId},
        value::Value,
    };

    #[test]
    fn test_and_or() {
        let mut c = Circuit::new();
        let i1 = c.add_component(Component::input(1, 1));
        let i2 = c.add_component(Component::input(0, 1));
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
        let i1 = c.add_component(Component::input(3, 2));
        let i2 = c.add_component(Component::input(2, 2));
        let i3 = c.add_component(Component::input(1, 2));
        let i4 = c.add_component(Component::input(0, 2));
        let sel = c.add_component(Component::input(0, 2));

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
        c.set_input(sel, 1, 2);
        c.settle();
        assert_eq!(c.read_output(o1), Value::new(2, 2));
        c.set_input(sel, 2, 2);
        c.settle();
        assert_eq!(c.read_output(o1), Value::new(1, 2));
        c.set_input(sel, 3, 2);
        c.settle();
        assert_eq!(c.read_output(o1), Value::new(0, 2));
    }

    #[test]
    fn test_demux() {
        let mut c = Circuit::new();
        let i1 = c.add_component(Component::input(1, 1));
        let sel = c.add_component(Component::input(2, 2));

        let o1 = c.add_component(Component::output());
        let o2 = c.add_component(Component::output());
        let o3 = c.add_component(Component::output());
        let o4 = c.add_component(Component::output());

        let demux = c.add_component(Component::demux(1, 2));

        c.link(i1, PinId::output(0), demux, PinId::input(0));
        c.link(sel, PinId::output(0), demux, PinId::input(1));

        c.link(demux, PinId::output(0), o1, PinId::input(0));
        c.link(demux, PinId::output(1), o2, PinId::input(0));
        c.link(demux, PinId::output(2), o3, PinId::input(0));
        c.link(demux, PinId::output(3), o4, PinId::input(0));

        c.settle();
        assert_eq!(c.read_output(o1), Value::new(0, 1));
        assert_eq!(c.read_output(o2), Value::new(0, 1));
        assert_eq!(c.read_output(o3), Value::new(1, 1));
        assert_eq!(c.read_output(o4), Value::new(0, 1));
    }

    #[test]
    fn test_reg() {
        // TODO: Verify this test
        let mut c = Circuit::new();

        let data = c.add_component(Component::input(5, 4));
        let we = c.add_component(Component::input(0, 1));
        let reg = c.add_component(Component::reg(4));
        let out = c.add_component(Component::output());

        c.link(data, PinId::output(0), reg, PinId::input(0));
        c.link(we, PinId::output(0), reg, PinId::input(1));
        c.link(reg, PinId::output(0), out, PinId::input(0));

        c.settle();
        // Zero-initialized, unaffected by data already driving 5 pre-tick.
        assert_eq!(c.read_output(out), Value::new(0, 4));

        // write_enable=1, tick: latches data.
        c.set_input(we, 1, 1);
        c.settle();
        c.tick_clock();
        assert_eq!(c.read_output(out), Value::new(5, 4));

        // write_enable=0, change data, tick: holds previous value.
        c.set_input(we, 0, 1);
        c.set_input(data, 9, 4);
        c.settle();
        c.tick_clock();
        assert_eq!(c.read_output(out), Value::new(5, 4));
    }
}
