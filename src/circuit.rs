use crate::component::{CompKey, Component, PinId};

use slotmap::{SecondaryMap, SlotMap};
use std::collections::VecDeque;

use crate::net::{Net, NetKey};

pub struct Circuit {
    nets: SlotMap<NetKey, Net>,
    components: SlotMap<CompKey, Component>,
    dirty: VecDeque<NetKey>,
    queued: SecondaryMap<NetKey, bool>, // TODO: there might be a nicer way of organizing this
}

impl Circuit {
    const MAX_ITERATIONS: usize = 100;

    fn add_component(&mut self, comp: Component) -> CompKey {
        self.components.insert(comp)
    }

    fn net_of(&self, comp: CompKey, pin: PinId) -> Option<NetKey> {
        self.components.get(comp).and_then(|c| c.net_of(pin))
    }

    fn attach(&mut self, net: NetKey, comp: CompKey, pin: PinId) {
        // Attaches a Component pin to a net, and back-links
        match pin {
            PinId::In(i) => self.nets[net].sinks.push((comp, i)),
            PinId::Out(i) => self.nets[net].source = Some((comp, i)),
        }
    }

    fn link(&mut self, a: CompKey, a_pin: PinId, b: CompKey, b_pin: PinId) -> NetKey {
        let net_a = self.net_of(a, a_pin);
        let net_b = self.net_of(b, b_pin);

        match (net_a, net_b) {
            (None, None) => {
                // Need to create a new Net
                let net = self.nets.insert(Net::default());
                self.attach(net, a, a_pin);
                self.attach(net, b, b_pin);
                net
            }
            (Some(net), None) => {
                self.attach(net, b, b_pin);
                net
            }
            (None, Some(net)) => {
                self.attach(net, a, a_pin);
                net
            }
            (Some(a_net), Some(b_net)) if a_net == b_net => a_net,
            (Some(a_net), Some(b_net)) => self.merge(a_net, b_net),
        }
    }

    fn merge(&mut self, a: NetKey, b: NetKey) -> NetKey {
        if a == b {
            return a;
        }
        // Remove the second net
        let b_net = self.nets.remove(b).unwrap();

        // Correct backreferences on Net B, then add them into Net A
        for (comp, i) in b_net.sinks {
            self.components[comp].set_pin_net(PinId::In(i), a);
            self.nets[a].sinks.push((comp, i));
        }

        // Handle source pins
        match (self.nets[a].source, b_net.source) {
            (Some(_), Some((comp, i))) => {
                // TODO: Decide how to handle competing source
                self.components[comp].set_pin_net(PinId::Out(i), a);
            }
            (None, Some((comp, i))) => {
                // Only Net B was driven, so make that Net A's driver
                self.components[comp].set_pin_net(PinId::Out(i), a);
                self.nets[a].source = Some((comp, i));
            }
            (_, None) => {}
        }
        self.mark_dirty(a);
        a
    }

    fn mark_dirty(&mut self, net: NetKey) {
        if !self.queued.get(net).copied().unwrap_or(false) {
            self.queued.insert(net, true);
            self.dirty.push_back(net);
        }
    }

    fn settle(&mut self) {
        let mut iterations = 0;

        while let Some(net) = self.dirty.pop_front() {
            // Clear visit before eval so that it can be re-evaled in the case of a loop
            self.queued.insert(net, false);

            let sinks: Vec<_> = self.nets[net]
                .sinks
                .iter()
                .copied()
                .filter(|(comp, _)| !self.components[*comp].is_sequential())
                .collect();

            for (comp, _) in sinks {
                self.eval_component(comp);
            }

            iterations += 1;
            if iterations > Self::MAX_ITERATIONS {
                // FIXME: Handle error
                panic!("Exceeded max iterations");
                break;
            }
        }
    }

    fn eval_component(&mut self, comp: CompKey) {
        let comp = &self.components[comp];
        let new_values: Vec<_> = comp.evaluate(&self.nets);
        // filter out values where: a) output pin is disconnected, or b) new value matches previous
        // value
        let dirty_values: Vec<_> = new_values
            .into_iter()
            .enumerate()
            .filter_map(|(i, new_val)| comp.pins.outputs[i].map(|net| (net, new_val)))
            .collect();

        for (net, new_value) in dirty_values {
            if self.nets[net].value != new_value {
                self.nets[net].value = new_value;
                self.mark_dirty(net);
            }
        }
    }
}
