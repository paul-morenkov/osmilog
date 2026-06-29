use crate::{
    component::{CompKey, Component, Logic, PinId},
    value::Value,
};

use slotmap::{SecondaryMap, SlotMap};
use std::collections::VecDeque;

use crate::net::{Net, NetKey};

#[derive(Debug, Default)]
pub struct Circuit {
    pub(crate) nets: SlotMap<NetKey, Net>,
    pub(crate) components: SlotMap<CompKey, Component>,
    pub(crate) dirty: VecDeque<NetKey>,
    queued: SecondaryMap<NetKey, bool>, // TODO: there might be a nicer way of organizing this
}

impl Circuit {
    const MAX_ITERATIONS: usize = 100;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_component(&mut self, comp: Component) -> CompKey {
        let key = self.components.insert(comp);
        self.eval_component(key);
        key
    }

    pub fn set_input(&mut self, comp: CompKey, value: Value) {
        // TODO: Make this return a result
        if let Logic::Input(v) = &mut self.components[comp].logic {
            *v = value
        } else {
            return;
        }
        self.eval_component(comp);
    }

    pub fn read_output(&self, comp: CompKey) -> Value {
        // TODO: Handle the component not being Logic::Output
        match self.components[comp].pins.inputs[0] {
            Some(net) => self.nets[net].value,
            None => Value::Floating,
        }
    }

    pub fn clear_nets(&mut self) {
        for comp in self.components.values_mut() {
            comp.clear_pins();
        }

        self.nets.clear();
        self.dirty.clear();
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
        self.components[comp].set_pin_net(pin, net);
        // If attaching a sink pin, immediately evaluate the component since no Net's have changed
        // so nothing will call eval_component automatically.
        if let PinId::In(_) = pin {
            self.eval_component(comp);
        }
    }

    pub fn link(&mut self, a: CompKey, a_pin: PinId, b: CompKey, b_pin: PinId) -> NetKey {
        let net_a = self.net_of(a, a_pin);
        let net_b = self.net_of(b, b_pin);

        match (net_a, net_b) {
            (None, None) => {
                // Need to create a new Net
                let net = self.nets.insert(Net::default());
                self.attach(net, a, a_pin);
                self.attach(net, b, b_pin);
                self.mark_dirty(net);
                net
            }
            (Some(net), None) => {
                self.attach(net, b, b_pin);
                self.mark_dirty(net);
                net
            }
            (None, Some(net)) => {
                self.attach(net, a, a_pin);
                self.mark_dirty(net);
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

    pub fn settle(&mut self) {
        let mut iterations = 0;

        while let Some(net) = self.dirty.pop_front() {
            // Clear visit before eval so that it can be re-evaled in the case of a loop
            self.queued.insert(net, false);
            let changed = self.resolve_net(net);

            if changed {
                let sinks: Vec<_> = self.nets[net]
                    .sinks
                    .iter()
                    .copied()
                    .filter(|(comp, _)| !self.components[*comp].is_sequential())
                    .collect();

                for (comp, _) in sinks {
                    self.eval_component(comp);
                }
            }
            iterations += 1;
            if iterations > Self::MAX_ITERATIONS {
                // FIXME: Handle error
                panic!("Exceeded max iterations");
            }
        }
    }

    // Recomputes the Net's Value from it's source. Returns whether the value changed.
    // TODO: Add functionality for multiple sources and conflict detection.
    fn resolve_net(&mut self, net: NetKey) -> bool {
        let old = self.nets[net].value;
        let source = self.nets[net].source;

        let new = match source {
            // Net takes value from pins.out_cache, which is updated in eval_component
            Some((comp, i)) => self.components[comp].pins.out_cache[i.0 as usize],
            None => Value::Floating,
        };
        self.nets[net].value = new;
        new != old
    }

    // Evaluates component logic, storing the Value in pins.out_cache and marking the net as dirty
    // if necessary.
    fn eval_component(&mut self, comp: CompKey) {
        let new_values: Vec<_> = self.components[comp].evaluate(&self.nets);
        let c = &mut self.components[comp];

        let mut dirty_nets = Vec::new();

        for (i, new_val) in new_values.into_iter().enumerate() {
            let cache_slot = &mut c.pins.out_cache[i];
            if *cache_slot != new_val {
                *cache_slot = new_val;
                if let Some(net) = c.pins.outputs[i] {
                    dirty_nets.push(net);
                }
            }
        }

        for net in dirty_nets {
            self.mark_dirty(net);
        }
    }
}
