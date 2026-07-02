use crate::sim::component::{CompKey, Component, Logic, LogicComb, PinId};
use crate::sim::net::{Net, NetKey};
use crate::sim::value::Value;

use slotmap::{new_key_type, SecondaryMap, SlotMap};
use std::collections::{HashMap, VecDeque};

new_key_type! {
    pub struct TunnelKey;
}

// A Tunnel ties together all Tunnels sharing the same Label into one virtual
// net, without a drawn wire between them (a schematic "net label" / off-page
// connector). Feed tunnels drive their attached net FROM the shared label
// group's resolved value; Pull tunnels read their attached net's value and
// contribute it TO the group. See Circuit::settle()/resolve_net().
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelRole {
    Feed,
    Pull,
}

#[derive(Debug, Clone)]
pub struct Tunnel {
    pub label: String,
    pub role: TunnelRole,
    pub net: Option<NetKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettleError {
    Oscillation { net: NetKey, revisits: usize },
    TunnelConflict { label: String },
}

impl std::fmt::Display for SettleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettleError::Oscillation { net, revisits } => write!(
                f,
                "net {:?} did not converge after {} revisits (possible combinational oscillation)",
                net, revisits
            ),
            SettleError::TunnelConflict { label } => write!(
                f,
                "tunnel label {:?} has conflicting driven values from multiple Pull tunnels",
                label
            ),
        }
    }
}

impl std::error::Error for SettleError {}

#[derive(Debug, Default)]
pub struct Circuit {
    pub(crate) nets: SlotMap<NetKey, Net>,
    pub(crate) components: SlotMap<CompKey, Component>,
    pub(crate) dirty: VecDeque<NetKey>,
    queued: SecondaryMap<NetKey, bool>, // TODO: there might be a nicer way of organizing this
    pub(crate) tunnels: SlotMap<TunnelKey, Tunnel>,
    tunnel_labels: HashMap<String, Vec<TunnelKey>>,
}

impl Circuit {
    // How many times a single net may change value within one settle() call
    // before it's considered a combinational oscillation. Bounded by
    // reconvergent fan-in depth in legitimate circuits (small, independent
    // of circuit size), not by circuit size itself.
    const REVISIT_THRESHOLD: usize = 16;
    // Defensive backstop on total net-pops across the whole call, in case
    // many different nets are oscillating simultaneously. Scaled to circuit
    // size so it doesn't false-positive on large-but-legitimate circuits;
    // should essentially never trigger if the per-net check above is doing
    // its job.
    const ITERATION_BUDGET_PER_NET: usize = 64;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_component(&mut self, comp: Component) -> CompKey {
        let key = self.components.insert(comp);
        self.eval_component(key);
        key
    }

    pub fn set_input(&mut self, comp: CompKey, bits: u32, width: u8) {
        // TODO: Should you be able to change width via this function?
        // TODO: Make this return a result
        if let Logic::Comb(LogicComb::Input { bits: b, width: w }) =
            &mut self.components[comp].logic
        {
            *b = bits;
            *w = width;
            self.eval_component(comp);
        }
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

        // Every NetKey is about to become invalid; tunnels must not keep
        // pointing at one, or they'd hold a dangling key.
        for t in self.tunnels.values_mut() {
            t.net = None;
        }

        self.nets.clear();
        self.dirty.clear();
    }

    fn net_of(&self, comp: CompKey, pin: PinId) -> Option<NetKey> {
        self.components.get(comp).and_then(|c| c.net_of(pin))
    }

    // Returns the net already attached to (comp, pin), or creates and
    // attaches a fresh one. Shared by link()'s (None, None) case and
    // link_tunnel().
    fn net_or_create(&mut self, comp: CompKey, pin: PinId) -> NetKey {
        match self.net_of(comp, pin) {
            Some(net) => net,
            None => {
                let net = self.nets.insert(Net::default());
                self.attach(net, comp, pin);
                net
            }
        }
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
                let net = self.net_or_create(a, a_pin);
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

        // Repoint any tunnels attached to the removed net. Same net, new
        // key, so no extra dirtying beyond mark_dirty(a) below is needed.
        for t in self.tunnels.values_mut() {
            if t.net == Some(b) {
                t.net = Some(a);
            }
        }

        self.mark_dirty(a);
        a
    }

    pub fn add_tunnel(&mut self, label: String, role: TunnelRole) -> TunnelKey {
        let key = self.tunnels.insert(Tunnel {
            label: label.clone(),
            role,
            net: None,
        });
        self.tunnel_labels.entry(label).or_default().push(key);
        key
    }

    // Finds or creates the net at (comp, pin) and attaches the tunnel to it.
    pub fn link_tunnel(&mut self, tunnel: TunnelKey, comp: CompKey, pin: PinId) -> NetKey {
        let net = self.net_or_create(comp, pin);
        self.attach_tunnel(tunnel, net);
        net
    }

    fn attach_tunnel(&mut self, tunnel: TunnelKey, net: NetKey) {
        let old_net = self.tunnels[tunnel].net;
        let label = self.tunnels[tunnel].label.clone();
        self.tunnels[tunnel].net = Some(net);
        // Rewiring away from a previous net: that net must be re-resolved
        // too, or it keeps showing a stale tunnel-contributed value forever.
        if let Some(old) = old_net {
            if old != net {
                self.mark_dirty(old);
            }
        }
        self.mark_dirty(net);
        // Group membership changed (a new net now contributes to/reads from
        // this label), independent of whether this net's own value happens
        // to change - settle()'s "if changed" cross-dirty step alone can't
        // catch that, so dirty Feed siblings explicitly.
        self.dirty_label_feed_nets(&label);
    }

    pub fn detach_tunnel(&mut self, tunnel: TunnelKey) {
        let label = self.tunnels[tunnel].label.clone();
        if let Some(old) = self.tunnels[tunnel].net.take() {
            self.mark_dirty(old);
        }
        self.dirty_label_feed_nets(&label);
    }

    pub fn remove_tunnel(&mut self, tunnel: TunnelKey) {
        let Some(t) = self.tunnels.remove(tunnel) else {
            return;
        };
        if let Some(keys) = self.tunnel_labels.get_mut(&t.label) {
            keys.retain(|&k| k != tunnel);
            if keys.is_empty() {
                self.tunnel_labels.remove(&t.label);
            }
        }
        if let Some(net) = t.net {
            self.mark_dirty(net);
        }
        self.dirty_label_feed_nets(&t.label);
    }

    pub fn rename_tunnel(&mut self, tunnel: TunnelKey, new_label: String) {
        let Some(t) = self.tunnels.get_mut(tunnel) else {
            return;
        };
        let old_label = std::mem::replace(&mut t.label, new_label.clone());
        let net = t.net;

        if let Some(keys) = self.tunnel_labels.get_mut(&old_label) {
            keys.retain(|&k| k != tunnel);
            if keys.is_empty() {
                self.tunnel_labels.remove(&old_label);
            }
        }
        self.tunnel_labels
            .entry(new_label.clone())
            .or_default()
            .push(tunnel);

        if let Some(net) = net {
            self.mark_dirty(net);
        }
        // Both the old group (lost a member) and the new group (gained one)
        // may need their Feed nets re-resolved, even though this tunnel's
        // own net value is unaffected by a relabel.
        self.dirty_label_feed_nets(&old_label);
        self.dirty_label_feed_nets(&new_label);
    }

    fn tunnels_on_net(&self, net: NetKey) -> impl Iterator<Item = &Tunnel> {
        self.tunnels.values().filter(move |t| t.net == Some(net))
    }

    // Marks the net of every Feed-role tunnel in `label`'s group dirty, so
    // they re-resolve against the group's (possibly just-changed) value or
    // membership.
    fn dirty_label_feed_nets(&mut self, label: &str) {
        let Some(keys) = self.tunnel_labels.get(label) else {
            return;
        };
        let nets: Vec<NetKey> = keys
            .iter()
            .filter_map(|&tk| {
                let t = &self.tunnels[tk];
                (t.role == TunnelRole::Feed).then_some(t.net).flatten()
            })
            .collect();
        for n in nets {
            self.mark_dirty(n);
        }
    }

    // Aggregates a label group's value from its Pull-role tunnels' net
    // values. `strict` controls what happens when two Pull tunnels disagree
    // (non-Floating, differing values): lenient (false) deterministically
    // takes the last such value in tunnel_labels order and never errors,
    // safe to call mid-convergence in resolve_net(); strict (true) returns
    // TunnelConflict, meant to be called exactly once after settle()'s
    // dirty-queue loop has fully drained, when disagreement is genuine
    // rather than an evaluation-order artifact.
    fn tunnel_group_value(&self, label: &str, strict: bool) -> Result<Value, SettleError> {
        let Some(keys) = self.tunnel_labels.get(label) else {
            return Ok(Value::Floating);
        };
        let mut result = Value::Floating;
        for &tk in keys {
            let t = &self.tunnels[tk];
            if t.role != TunnelRole::Pull {
                continue;
            }
            let Some(net) = t.net else { continue };
            let v = self.nets[net].value;
            if matches!(v, Value::Floating) {
                continue;
            }
            match result {
                Value::Floating => result = v,
                _ if result == v => {}
                _ if strict => {
                    return Err(SettleError::TunnelConflict {
                        label: label.to_string(),
                    })
                }
                _ => result = v,
            }
        }
        Ok(result)
    }

    fn tunnel_feed_value(&self, net: NetKey) -> Value {
        let Some(t) = self
            .tunnels_on_net(net)
            .find(|t| t.role == TunnelRole::Feed)
        else {
            return Value::Floating;
        };
        // strict=false never returns Err.
        self.tunnel_group_value(&t.label, false)
            .unwrap_or(Value::Floating)
    }

    fn mark_dirty(&mut self, net: NetKey) {
        if !self.queued.get(net).copied().unwrap_or(false) {
            self.queued.insert(net, true);
            self.dirty.push_back(net);
        }
    }

    pub fn settle(&mut self) -> Result<(), SettleError> {
        let mut revisits: SecondaryMap<NetKey, usize> = SecondaryMap::new();
        let iteration_budget = self
            .nets
            .len()
            .saturating_mul(Self::ITERATION_BUDGET_PER_NET)
            .max(1024);
        let mut total_iterations = 0;

        while let Some(net) = self.dirty.pop_front() {
            // Check if net key is valid (could have been merged away and now be stale)
            if !self.nets.contains_key(net) {
                continue;
            }

            // Clear visit before eval so that it can be re-evaled in the case of a loop
            self.queued.insert(net, false);
            let changed = self.resolve_net(net);

            if changed {
                let revisit_count = revisits.get(net).copied().unwrap_or(0) + 1;
                revisits.insert(net, revisit_count);
                if revisit_count > Self::REVISIT_THRESHOLD {
                    return Err(SettleError::Oscillation {
                        net,
                        revisits: revisit_count,
                    });
                }

                let sinks: Vec<_> = self.nets[net]
                    .sinks
                    .iter()
                    .copied()
                    .filter(|(comp, _)| !self.components[*comp].is_sequential())
                    .collect();

                for (comp, _) in sinks {
                    self.eval_component(comp);
                }

                // If a Pull tunnel reads this net, the label group's value
                // may have changed; re-dirty sibling Feed nets so they pick
                // it up on a later pass of this same settle() call.
                let pull_label: Option<String> = self
                    .tunnels_on_net(net)
                    .find(|t| t.role == TunnelRole::Pull)
                    .map(|t| t.label.clone());
                if let Some(label) = pull_label {
                    self.dirty_label_feed_nets(&label);
                }
            }
            total_iterations += 1;
            if total_iterations > iteration_budget {
                // Extremely defensive backstop (e.g. many nets oscillating
                // simultaneously); should be unreachable if the per-net
                // revisit check above is doing its job.
                return Err(SettleError::Oscillation {
                    net,
                    revisits: revisits.get(net).copied().unwrap_or(0),
                });
            }
        }

        // All nets have fully converged at this point (the loop only exits
        // when dirty is empty). Any tunnel-group disagreement found now is
        // genuine, not a mid-convergence evaluation-order artifact.
        let labels: Vec<String> = self.tunnel_labels.keys().cloned().collect();
        for label in &labels {
            self.tunnel_group_value(label, true)?;
        }
        Ok(())
    }

    // Recomputes the Net's Value from it's source. Returns whether the value changed.
    // TODO: Add functionality for multiple sources and conflict detection.
    fn resolve_net(&mut self, net: NetKey) -> bool {
        let old = self.nets[net].value;
        let source = self.nets[net].source;

        let new = match source {
            // Net takes value from pins.out_cache, which is updated in eval_component
            Some((comp, i)) => self.components[comp].pins.out_cache[i.0 as usize],
            // A component driver always takes priority; only fall back to a
            // Feed tunnel's group value when this net has no real driver.
            None => self.tunnel_feed_value(net),
        };
        self.nets[net].value = new;
        new != old
    }

    // Evaluates component logic, storing the Value in pins.out_cache and marking the net as dirty
    // if necessary.
    fn eval_component(&mut self, comp: CompKey) {
        let new_values = self.components[comp].evaluate(&self.nets);
        self.apply_output_values(comp, new_values);
    }

    // Diffs new_values against a component's current out_cache, updates out_cache in place,
    // and marks any changed output net dirty. Shared by eval_component (combinational path)
    // and tick_clock (sequential path).
    fn apply_output_values(&mut self, comp: CompKey, new_values: Vec<Value>) {
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

    // Advances the clock by one tick for all sequential components:
    //   1. Collect every sequential component's current input Values from net state
    //      (a snapshot, taken before any state mutation).
    //   2. Compute each sequential component's next state via Component::tick, updating
    //      out_cache and persisted state, and marking changed output nets dirty.
    //   3. Call settle() to propagate the changes through the combinational circuit.
    // Generic over LogicSeq variants: adding a new sequential component type only needs
    // new match arms in Component::evaluate/Component::tick, not changes here.
    pub fn tick_clock(&mut self) -> Result<(), SettleError> {
        let seq_comps: Vec<CompKey> = self
            .components
            .iter()
            .filter(|(_, c)| c.is_sequential())
            .map(|(key, _)| key)
            .collect();

        let collected_inputs: Vec<(CompKey, Vec<Value>)> = seq_comps
            .into_iter()
            .map(|key| {
                let inputs = self.components[key].read_inputs(&self.nets);
                (key, inputs)
            })
            .collect();

        for (key, inputs) in collected_inputs {
            let new_values = self.components[key].tick(&inputs);
            self.apply_output_values(key, new_values);
        }

        self.settle()
    }

    pub fn remove_component(&mut self, key: CompKey) {
        let Some(comp) = self.components.get(key) else {
            return;
        };
        let output_nets: Vec<NetKey> = comp.pins.outputs.iter().filter_map(|&n| n).collect();
        let input_nets: Vec<NetKey> = comp.pins.inputs.iter().filter_map(|&n| n).collect();

        // Tunnels attached to nets that are about to be deleted must be
        // detached (their NetKey would otherwise dangle). Their sibling
        // nets need re-dirtying too, but only after the trailing
        // dirty/queued clear below, or the marks would be wiped.
        let mut affected_labels: Vec<String> = Vec::new();

        // Remove nets driven by this component; clear each sink's input pin slot
        for net_key in output_nets {
            if let Some(net) = self.nets.get(net_key) {
                let sinks = net.sinks.clone();
                for (sink_comp, sink_pin) in sinks {
                    if let Some(sc) = self.components.get_mut(sink_comp) {
                        sc.pins.inputs[sink_pin.0 as usize] = None;
                    }
                }
            }
            for t in self.tunnels.values_mut() {
                if t.net == Some(net_key) {
                    t.net = None;
                    affected_labels.push(t.label.clone());
                }
            }
            self.nets.remove(net_key);
        }

        // Detach from nets this component receives; remove it from each net's sinks list
        for net_key in input_nets {
            if let Some(net) = self.nets.get_mut(net_key) {
                net.sinks.retain(|&(ck, _)| ck != key);
            }
        }

        // Clear propagation state; the caller is expected to call settle() after
        self.dirty.clear();
        self.queued.clear();

        self.components.remove(key);

        // Re-dirty sibling Feed-tunnel nets now that propagation state has
        // already been reset above.
        for label in affected_labels {
            self.dirty_label_feed_nets(&label);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::component::GateOp;
    use test_case::test_case;

    // ---- Group 1: construction / basic wiring ----

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

        c.settle().unwrap();
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

        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(3, 2));
        c.set_input(sel, 1, 2);
        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(2, 2));
        c.set_input(sel, 2, 2);
        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(1, 2));
        c.set_input(sel, 3, 2);
        c.settle().unwrap();
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

        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(0, 1));
        assert_eq!(c.read_output(o2), Value::new(0, 1));
        assert_eq!(c.read_output(o3), Value::new(1, 1));
        assert_eq!(c.read_output(o4), Value::new(0, 1));
    }

    #[test]
    fn test_splitter_contiguous_halves() {
        // 4-bit bus split into two 2-bit arms: bits [0,1] -> arm0, bits [2,3] -> arm1.
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(0, 4));
        let splitter = c.add_component(Component::splitter(vec![vec![0, 1], vec![2, 3]]));
        let o1 = c.add_component(Component::output());
        let o2 = c.add_component(Component::output());

        c.link(data, PinId::output(0), splitter, PinId::input(0));
        c.link(splitter, PinId::output(0), o1, PinId::input(0));
        c.link(splitter, PinId::output(1), o2, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(0, 2));
        assert_eq!(c.read_output(o2), Value::new(0, 2));

        c.set_input(data, 0b0001, 4);
        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(1, 2));
        assert_eq!(c.read_output(o2), Value::new(0, 2));

        c.set_input(data, 0b0100, 4);
        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(0, 2));
        assert_eq!(c.read_output(o2), Value::new(1, 2));

        c.set_input(data, 0b1111, 4);
        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(3, 2));
        assert_eq!(c.read_output(o2), Value::new(3, 2));
    }

    #[test]
    fn test_splitter_interleaved() {
        // 4-bit bus, even bits (0,2) -> arm0, odd bits (1,3) -> arm1.
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(0, 4));
        let splitter = c.add_component(Component::splitter(vec![vec![0, 2], vec![1, 3]]));
        let o1 = c.add_component(Component::output());
        let o2 = c.add_component(Component::output());

        c.link(data, PinId::output(0), splitter, PinId::input(0));
        c.link(splitter, PinId::output(0), o1, PinId::input(0));
        c.link(splitter, PinId::output(1), o2, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(0, 2));
        assert_eq!(c.read_output(o2), Value::new(0, 2));

        c.set_input(data, 0b0100, 4); // bit2 (even) -> arm0 pos1
        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(2, 2));
        assert_eq!(c.read_output(o2), Value::new(0, 2));

        c.set_input(data, 0b1000, 4); // bit3 (odd) -> arm1 pos1
        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(0, 2));
        assert_eq!(c.read_output(o2), Value::new(2, 2));

        c.set_input(data, 0b1010, 4); // bits 1,3 -> arm1 full
        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(0, 2));
        assert_eq!(c.read_output(o2), Value::new(3, 2));

        c.set_input(data, 0b1111, 4);
        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(3, 2));
        assert_eq!(c.read_output(o2), Value::new(3, 2));
    }

    #[test]
    fn test_splitter_full_spread() {
        // Each of the 4 bits fans out to its own dedicated 1-bit arm.
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(0, 4));
        let splitter = c.add_component(Component::splitter(vec![
            vec![0],
            vec![1],
            vec![2],
            vec![3],
        ]));
        let o1 = c.add_component(Component::output());
        let o2 = c.add_component(Component::output());
        let o3 = c.add_component(Component::output());
        let o4 = c.add_component(Component::output());

        c.link(data, PinId::output(0), splitter, PinId::input(0));
        c.link(splitter, PinId::output(0), o1, PinId::input(0));
        c.link(splitter, PinId::output(1), o2, PinId::input(0));
        c.link(splitter, PinId::output(2), o3, PinId::input(0));
        c.link(splitter, PinId::output(3), o4, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(0, 1));
        assert_eq!(c.read_output(o2), Value::new(0, 1));
        assert_eq!(c.read_output(o3), Value::new(0, 1));
        assert_eq!(c.read_output(o4), Value::new(0, 1));

        c.set_input(data, 0b0100, 4);
        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(0, 1));
        assert_eq!(c.read_output(o2), Value::new(0, 1));
        assert_eq!(c.read_output(o3), Value::new(1, 1));
        assert_eq!(c.read_output(o4), Value::new(0, 1));

        c.set_input(data, 0b1111, 4);
        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(1, 1));
        assert_eq!(c.read_output(o2), Value::new(1, 1));
        assert_eq!(c.read_output(o3), Value::new(1, 1));
        assert_eq!(c.read_output(o4), Value::new(1, 1));
    }

    #[test]
    fn test_splitter_floating_input_propagates_to_all_arms() {
        let mut c = Circuit::new();
        // Splitter's input pin is left unconnected, so it reads as Floating.
        let splitter = c.add_component(Component::splitter(vec![vec![0], vec![1], vec![2]]));
        let o1 = c.add_component(Component::output());
        let o2 = c.add_component(Component::output());
        let o3 = c.add_component(Component::output());

        c.link(splitter, PinId::output(0), o1, PinId::input(0));
        c.link(splitter, PinId::output(1), o2, PinId::input(0));
        c.link(splitter, PinId::output(2), o3, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::Floating);
        assert_eq!(c.read_output(o2), Value::Floating);
        assert_eq!(c.read_output(o3), Value::Floating);
    }

    #[test]
    fn test_splitter_zero_arms_produces_empty_output() {
        let mut c = Circuit::new();
        let splitter = c.add_component(Component::splitter(vec![]));
        assert!(c.components[splitter].pins.out_cache.is_empty());
    }

    #[test]
    fn test_splitter_arm_with_no_mapped_bits_is_zero_width() {
        // arm 2 is listed with no bits, so it should receive nothing.
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(0b11, 2));
        let splitter = c.add_component(Component::splitter(vec![vec![0], vec![1], vec![]]));
        let o3 = c.add_component(Component::output());

        c.link(data, PinId::output(0), splitter, PinId::input(0));
        c.link(splitter, PinId::output(2), o3, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o3), Value::new(0, 0));
    }

    #[test]
    fn test_splitter_unrouted_high_bits_of_wider_input_are_ignored() {
        // arm_bits only covers the low 2 bits of a 4-bit input value; the
        // upper bits (2,3) are unrouted and should have no effect on any arm.
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(0b1101, 4));
        let splitter = c.add_component(Component::splitter(vec![vec![0], vec![1]]));
        let o1 = c.add_component(Component::output());
        let o2 = c.add_component(Component::output());

        c.link(data, PinId::output(0), splitter, PinId::input(0));
        c.link(splitter, PinId::output(0), o1, PinId::input(0));
        c.link(splitter, PinId::output(1), o2, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(1, 1));
        assert_eq!(c.read_output(o2), Value::new(0, 1));
    }

    #[test]
    fn test_splitter_bit_claimed_by_multiple_arms_last_arm_wins() {
        // bit1 is listed under both arm0 and arm1; arm_bits is processed in
        // order, so the later arm (arm1) should end up owning it, not arm0.
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(0, 2));
        let splitter = c.add_component(Component::splitter(vec![vec![0, 1], vec![1]]));
        let o1 = c.add_component(Component::output());
        let o2 = c.add_component(Component::output());

        c.link(data, PinId::output(0), splitter, PinId::input(0));
        c.link(splitter, PinId::output(0), o1, PinId::input(0));
        c.link(splitter, PinId::output(1), o2, PinId::input(0));

        c.set_input(data, 0b01, 2); // bit0 set, bit1 clear
        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(1, 1));
        assert_eq!(c.read_output(o2), Value::new(0, 1));

        c.set_input(data, 0b10, 2); // bit0 clear, bit1 set
        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(0, 1));
        assert_eq!(c.read_output(o2), Value::new(1, 1));
    }

    #[test]
    fn test_splitter_data_width_derived_from_arm_bits() {
        let comp = Component::splitter(vec![vec![0, 2], vec![1]]);
        match &comp.logic {
            Logic::Comb(LogicComb::Splitter(s)) => assert_eq!(s.data_width(), 3),
            _ => panic!("expected Splitter logic"),
        }
    }

    #[test]
    fn test_reg() {
        let mut c = Circuit::new();

        let data = c.add_component(Component::input(5, 4));
        let we = c.add_component(Component::input(0, 1));
        let reg = c.add_component(Component::reg(4));
        let out = c.add_component(Component::output());

        c.link(data, PinId::output(0), reg, PinId::input(0));
        c.link(we, PinId::output(0), reg, PinId::input(1));
        c.link(reg, PinId::output(0), out, PinId::input(0));

        c.settle().unwrap();
        // Zero-initialized, unaffected by data already driving 5 pre-tick.
        assert_eq!(c.read_output(out), Value::new(0, 4));

        // write_enable=1, tick: latches data.
        c.set_input(we, 1, 1);
        c.settle().unwrap();
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out), Value::new(5, 4));

        // write_enable=0, change data, tick: holds previous value.
        c.set_input(we, 0, 1);
        c.set_input(data, 9, 4);
        c.settle().unwrap();
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out), Value::new(5, 4));
    }

    #[test]
    fn test_add_component_input_out_cache_populated_immediately() {
        let mut c = Circuit::new();
        let i = c.add_component(Component::input(5, 3));
        // add_component eagerly evaluates, before any link() or settle().
        assert_eq!(c.components[i].pins.out_cache[0], Value::new(5, 3));
    }

    #[test]
    fn test_link_before_settle_net_value_still_floating() {
        let mut c = Circuit::new();
        let i = c.add_component(Component::input(5, 3));
        let o = c.add_component(Component::output());
        c.link(i, PinId::output(0), o, PinId::input(0));
        // out_cache is populated, but the net's own value isn't resolved
        // until settle() runs resolve_net.
        assert_eq!(c.read_output(o), Value::Floating);
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(5, 3));
    }

    #[test]
    fn test_link_extends_existing_net_fan_out() {
        let mut c = Circuit::new();
        let i = c.add_component(Component::input(1, 1));
        let g1 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let g2 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let o1 = c.add_component(Component::output());
        let o2 = c.add_component(Component::output());

        c.link(i, PinId::output(0), g1, PinId::input(0));
        c.link(i, PinId::output(0), g2, PinId::input(0));
        c.link(g1, PinId::output(0), o1, PinId::input(0));
        c.link(g2, PinId::output(0), o2, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(0, 1));
        assert_eq!(c.read_output(o2), Value::new(0, 1));
    }

    #[test]
    fn test_link_second_source_overwrites_first() {
        let mut c = Circuit::new();
        let i1 = c.add_component(Component::input(1, 1));
        let i2 = c.add_component(Component::input(0, 1));
        let g1 = c.add_component(Component::gate(GateOp::Not, 1, 1)); // NOT(1) = 0
        let g2 = c.add_component(Component::gate(GateOp::Not, 1, 1)); // NOT(0) = 1
        let o = c.add_component(Component::output());

        c.link(i1, PinId::output(0), g1, PinId::input(0));
        c.link(i2, PinId::output(0), g2, PinId::input(0));

        c.link(g1, PinId::output(0), o, PinId::input(0));
        // o's input pin already has a net; this attaches g2 as its source,
        // silently overwriting g1.
        c.link(g2, PinId::output(0), o, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(1, 1));
    }

    #[test]
    fn test_link_idempotent_returns_same_net() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let b = c.add_component(Component::output());
        let net1 = c.link(a, PinId::output(0), b, PinId::input(0));
        let net2 = c.link(a, PinId::output(0), b, PinId::input(0));
        assert_eq!(net1, net2);
    }

    #[test]
    fn test_link_merge_keeps_original_source_documents_bug() {
        // Documents current (unfinished) merge() behavior: when two
        // already-driven nets are merged, the ORIGINAL net's source silently
        // wins; the second driver's component is repointed at the merged net
        // but its value is never read. See merge()'s "TODO: Decide how to
        // handle competing source". Not necessarily correct, just current.
        let mut c = Circuit::new();
        let driver1 = c.add_component(Component::input(1, 1));
        let driver2 = c.add_component(Component::input(0, 1));
        let sink1 = c.add_component(Component::output());
        let sink2 = c.add_component(Component::output());

        c.link(driver1, PinId::output(0), sink1, PinId::input(0)); // net1, source = driver1
        c.link(driver2, PinId::output(0), sink2, PinId::input(0)); // net2, source = driver2
                                                                   // Resolve both nets before merging: see
                                                                   // test_link_merge_of_still_dirty_nets_panics_documents_bug for what
                                                                   // happens if a merged-away net is still pending in the dirty queue.
        c.settle().unwrap();
        assert_eq!(c.read_output(sink1), Value::new(1, 1));
        assert_eq!(c.read_output(sink2), Value::new(0, 1));

        // Merge net1 and net2 by linking their already-attached input pins.
        c.link(sink1, PinId::input(0), sink2, PinId::input(0));

        c.settle().unwrap();
        // Both sinks now share one net; merge() keeps net1's original source
        // (driver1), even though driver2's output pin was repointed at it.
        assert_eq!(c.read_output(sink1), Value::new(1, 1));
        assert_eq!(c.read_output(sink2), Value::new(1, 1));
    }

    #[test]
    fn test_link_merge_of_still_dirty_nets_removes_stale_key() {
        let mut c = Circuit::new();
        let driver1 = c.add_component(Component::input(1, 1));
        let driver2 = c.add_component(Component::input(0, 1));
        let sink1 = c.add_component(Component::output());
        let sink2 = c.add_component(Component::output());

        c.link(driver1, PinId::output(0), sink1, PinId::input(0)); // net1, still dirty
        c.link(driver2, PinId::output(0), sink2, PinId::input(0)); // net2, still dirty
                                                                   // Merging while both nets are still unresolved/dirty removes net2
                                                                   // from the slotmap while it's still queued.
        c.link(sink1, PinId::input(0), sink2, PinId::input(0));

        c.settle().unwrap(); // stale NetKey should get removed
    }

    // ---- Group 2: gate truth tables ----

    #[test_case(GateOp::And,  0, 0, 0 ; "and 0 0")]
    #[test_case(GateOp::And,  0, 1, 0 ; "and 0 1")]
    #[test_case(GateOp::And,  1, 0, 0 ; "and 1 0")]
    #[test_case(GateOp::And,  1, 1, 1 ; "and 1 1")]
    #[test_case(GateOp::Or,   0, 0, 0 ; "or 0 0")]
    #[test_case(GateOp::Or,   0, 1, 1 ; "or 0 1")]
    #[test_case(GateOp::Or,   1, 0, 1 ; "or 1 0")]
    #[test_case(GateOp::Or,   1, 1, 1 ; "or 1 1")]
    #[test_case(GateOp::Xor,  0, 0, 0 ; "xor 0 0")]
    #[test_case(GateOp::Xor,  0, 1, 1 ; "xor 0 1")]
    #[test_case(GateOp::Xor,  1, 0, 1 ; "xor 1 0")]
    #[test_case(GateOp::Xor,  1, 1, 0 ; "xor 1 1")]
    #[test_case(GateOp::Xnor, 0, 0, 1 ; "xnor 0 0")]
    #[test_case(GateOp::Xnor, 0, 1, 0 ; "xnor 0 1")]
    #[test_case(GateOp::Xnor, 1, 0, 0 ; "xnor 1 0")]
    #[test_case(GateOp::Xnor, 1, 1, 1 ; "xnor 1 1")]
    #[test_case(GateOp::Nand, 0, 0, 1 ; "nand 0 0")]
    #[test_case(GateOp::Nand, 0, 1, 1 ; "nand 0 1")]
    #[test_case(GateOp::Nand, 1, 0, 1 ; "nand 1 0")]
    #[test_case(GateOp::Nand, 1, 1, 0 ; "nand 1 1")]
    #[test_case(GateOp::Nor,  0, 0, 1 ; "nor 0 0")]
    #[test_case(GateOp::Nor,  0, 1, 0 ; "nor 0 1")]
    #[test_case(GateOp::Nor,  1, 0, 0 ; "nor 1 0")]
    #[test_case(GateOp::Nor,  1, 1, 0 ; "nor 1 1")]
    fn test_gate_binary_truth_table(op: GateOp, av: u32, bv: u32, expected: u32) {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(av, 1));
        let b = c.add_component(Component::input(bv, 1));
        let g = c.add_component(Component::gate(op, 2, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), g, PinId::input(0));
        c.link(b, PinId::output(0), g, PinId::input(1));
        c.link(g, PinId::output(0), o, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(expected, 1));
    }

    #[test_case(0, 1 ; "not 0")]
    #[test_case(1, 0 ; "not 1")]
    fn test_gate_not_truth_table(av: u32, expected: u32) {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(av, 1));
        let g = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), g, PinId::input(0));
        c.link(g, PinId::output(0), o, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(expected, 1));
    }

    #[test]
    fn test_gate_and_multi_input_fold() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let b = c.add_component(Component::input(1, 1));
        let d = c.add_component(Component::input(1, 1));
        let g = c.add_component(Component::gate(GateOp::And, 3, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), g, PinId::input(0));
        c.link(b, PinId::output(0), g, PinId::input(1));
        c.link(d, PinId::output(0), g, PinId::input(2));
        c.link(g, PinId::output(0), o, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(1, 1));

        c.set_input(d, 0, 1);
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(0, 1));
    }

    #[test]
    fn test_gate_or_multi_input_fold() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(0, 1));
        let b = c.add_component(Component::input(0, 1));
        let d = c.add_component(Component::input(0, 1));
        let g = c.add_component(Component::gate(GateOp::Or, 3, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), g, PinId::input(0));
        c.link(b, PinId::output(0), g, PinId::input(1));
        c.link(d, PinId::output(0), g, PinId::input(2));
        c.link(g, PinId::output(0), o, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(0, 1));

        c.set_input(d, 1, 1);
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(1, 1));
    }

    #[test]
    fn test_gate_multibit_width() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(0b1100, 4));
        let b = c.add_component(Component::input(0b1010, 4));
        let and_g = c.add_component(Component::gate(GateOp::And, 2, 4));
        let xor_g = c.add_component(Component::gate(GateOp::Xor, 2, 4));
        let o1 = c.add_component(Component::output());
        let o2 = c.add_component(Component::output());
        c.link(a, PinId::output(0), and_g, PinId::input(0));
        c.link(b, PinId::output(0), and_g, PinId::input(1));
        c.link(a, PinId::output(0), xor_g, PinId::input(0));
        c.link(b, PinId::output(0), xor_g, PinId::input(1));
        c.link(and_g, PinId::output(0), o1, PinId::input(0));
        c.link(xor_g, PinId::output(0), o2, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(0b1000, 4));
        assert_eq!(c.read_output(o2), Value::new(0b0110, 4));
    }

    #[test]
    fn test_gate_floating_input_yields_floating_output() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let g = c.add_component(Component::gate(GateOp::And, 2, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), g, PinId::input(0));
        // g's second input pin is left unconnected.
        c.link(g, PinId::output(0), o, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::Floating);
    }

    // ---- Group 3: mux / demux edge cases ----

    #[test]
    fn test_mux_floating_selector_yields_floating_output() {
        let mut c = Circuit::new();
        let d0 = c.add_component(Component::input(1, 1));
        let d1 = c.add_component(Component::input(0, 1));
        let mux = c.add_component(Component::mux(1, 1));
        let o = c.add_component(Component::output());
        // selector (input 0) left unconnected.
        c.link(d0, PinId::output(0), mux, PinId::input(1));
        c.link(d1, PinId::output(0), mux, PinId::input(2));
        c.link(mux, PinId::output(0), o, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::Floating);
    }

    #[test]
    fn test_mux_selector_width_mismatch_yields_floating() {
        let mut c = Circuit::new();
        let sel = c.add_component(Component::input(0, 1)); // width 1, mux expects sel_width=2
        let d0 = c.add_component(Component::input(5, 2));
        let mux = c.add_component(Component::mux(2, 2));
        let o = c.add_component(Component::output());
        c.link(sel, PinId::output(0), mux, PinId::input(0));
        c.link(d0, PinId::output(0), mux, PinId::input(1));
        c.link(mux, PinId::output(0), o, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::Floating);
    }

    #[test]
    fn test_mux_unconnected_data_branch_is_floating() {
        let mut c = Circuit::new();
        let sel = c.add_component(Component::input(0, 1)); // selects branch 0
        let mux = c.add_component(Component::mux(1, 1));
        let o = c.add_component(Component::output());
        c.link(sel, PinId::output(0), mux, PinId::input(0));
        // data branch 0 (input 1) left unconnected.
        c.link(mux, PinId::output(0), o, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::Floating);
    }

    #[test]
    fn test_demux_floating_selector_all_outputs_floating() {
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(1, 1));
        let demux = c.add_component(Component::demux(1, 2));
        let o1 = c.add_component(Component::output());
        let o2 = c.add_component(Component::output());
        let o3 = c.add_component(Component::output());
        let o4 = c.add_component(Component::output());
        c.link(data, PinId::output(0), demux, PinId::input(0));
        // selector (input 1) left unconnected.
        c.link(demux, PinId::output(0), o1, PinId::input(0));
        c.link(demux, PinId::output(1), o2, PinId::input(0));
        c.link(demux, PinId::output(2), o3, PinId::input(0));
        c.link(demux, PinId::output(3), o4, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::Floating);
        assert_eq!(c.read_output(o2), Value::Floating);
        assert_eq!(c.read_output(o3), Value::Floating);
        assert_eq!(c.read_output(o4), Value::Floating);
    }

    #[test]
    fn test_demux_selector_width_mismatch_all_outputs_floating() {
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(1, 1));
        let sel = c.add_component(Component::input(0, 1)); // width 1, demux expects sel_width=2
        let demux = c.add_component(Component::demux(1, 2));
        let o1 = c.add_component(Component::output());
        c.link(data, PinId::output(0), demux, PinId::input(0));
        c.link(sel, PinId::output(0), demux, PinId::input(1));
        c.link(demux, PinId::output(0), o1, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::Floating);
    }

    #[test]
    fn test_demux_unselected_branches_are_zero_not_floating() {
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(0b1111, 4));
        let sel = c.add_component(Component::input(1, 2));
        let demux = c.add_component(Component::demux(4, 2));
        let o0 = c.add_component(Component::output());
        let o1 = c.add_component(Component::output());
        c.link(data, PinId::output(0), demux, PinId::input(0));
        c.link(sel, PinId::output(0), demux, PinId::input(1));
        c.link(demux, PinId::output(0), o0, PinId::input(0));
        c.link(demux, PinId::output(1), o1, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o0), Value::new(0, 4)); // unselected: zero, not Floating
        assert_eq!(c.read_output(o1), Value::new(0b1111, 4)); // selected: data verbatim
    }

    #[test]
    fn test_demux_data_width_mismatch_passes_through_verbatim() {
        // Documents current lenient/unvalidated behavior: demux does not
        // check that the data input's width matches data_width (see the
        // "TODO: check data_width?" in Component::evaluate).
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(3, 2)); // width 2, demux built with data_width=1
        let sel = c.add_component(Component::input(0, 1));
        let demux = c.add_component(Component::demux(1, 1));
        let o0 = c.add_component(Component::output());
        c.link(data, PinId::output(0), demux, PinId::input(0));
        c.link(sel, PinId::output(0), demux, PinId::input(1));
        c.link(demux, PinId::output(0), o0, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o0), Value::new(3, 2));
    }

    // ---- Group 4: propagation / settle behavior ----

    #[test]
    fn test_settle_layered_propagation() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let b = c.add_component(Component::input(1, 1));
        let and_g = c.add_component(Component::gate(GateOp::And, 2, 1));
        let not_g = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), and_g, PinId::input(0));
        c.link(b, PinId::output(0), and_g, PinId::input(1));
        c.link(and_g, PinId::output(0), not_g, PinId::input(0));
        c.link(not_g, PinId::output(0), o, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(0, 1)); // NOT(1 AND 1) = 0
    }

    #[test]
    fn test_settle_idempotent_when_no_dirty_nets() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), o, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(1, 1));
        c.settle().unwrap(); // nothing dirty; must be a no-op
        assert_eq!(c.read_output(o), Value::new(1, 1));
    }

    #[test]
    fn test_settle_self_loop_stabilizes_to_floating() {
        let mut c = Circuit::new();
        let g = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let o = c.add_component(Component::output());
        c.link(g, PinId::output(0), g, PinId::input(0)); // self-feedback
        c.link(g, PinId::output(0), o, PinId::input(0));

        c.settle().unwrap(); // must not panic
        assert_eq!(c.read_output(o), Value::Floating);
    }

    #[test]
    fn test_settle_long_acyclic_chain_settles_successfully() {
        // Regression test: settle() used to count total net-pops within one
        // call rather than per-net revisits, so a sufficiently deep but
        // fully acyclic chain would falsely trip the old MAX_ITERATIONS
        // panic. With per-net revisit tracking, no net here is ever
        // revisited (each resolves exactly once), so this succeeds
        // regardless of chain length.
        let mut c = Circuit::new();
        let input = c.add_component(Component::input(1, 1));
        let mut prev = input;
        for _ in 0..105 {
            let g = c.add_component(Component::gate(GateOp::Not, 1, 1));
            c.link(prev, PinId::output(0), g, PinId::input(0));
            prev = g;
        }
        assert!(c.settle().is_ok());
    }

    #[test]
    fn test_settle_ring_oscillator_reports_non_convergence() {
        let mut c = Circuit::new();
        let seed = c.add_component(Component::input(0, 1));
        let n1 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let n2 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let n3 = c.add_component(Component::gate(GateOp::Not, 1, 1));

        c.link(seed, PinId::output(0), n1, PinId::input(0));
        c.link(n1, PinId::output(0), n2, PinId::input(0));
        c.link(n2, PinId::output(0), n3, PinId::input(0));
        c.settle().unwrap(); // seeds n1/n2/n3 with concrete alternating values, no loop yet

        // Close the loop: n3's output overwrites n1's input net's source
        // (last-link-wins, see test_link_second_source_overwrites_first),
        // injecting a stale concrete value into what is now a genuine
        // feedback cycle.
        c.link(n3, PinId::output(0), n1, PinId::input(0));
        // Toggles forever: the same net keeps crossing REVISIT_THRESHOLD,
        // reported as a clean Err instead of a panic.
        assert!(c.settle().is_err());
    }

    // ---- Group 5: register / clock behavior ----

    #[test]
    fn test_reg_initial_value_before_any_tick() {
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(9, 4));
        let we = c.add_component(Component::input(1, 1));
        let reg = c.add_component(Component::reg(4));
        let out = c.add_component(Component::output());
        c.link(data, PinId::output(0), reg, PinId::input(0));
        c.link(we, PinId::output(0), reg, PinId::input(1));
        c.link(reg, PinId::output(0), out, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::new(0, 4));
    }

    #[test]
    fn test_reg_settle_never_latches_only_tick_does() {
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(0, 4));
        let we = c.add_component(Component::input(1, 1));
        let reg = c.add_component(Component::reg(4));
        let out = c.add_component(Component::output());
        c.link(data, PinId::output(0), reg, PinId::input(0));
        c.link(we, PinId::output(0), reg, PinId::input(1));
        c.link(reg, PinId::output(0), out, PinId::input(0));
        c.settle().unwrap();

        for v in [1, 2, 3, 4] {
            c.set_input(data, v, 4);
            c.settle().unwrap();
            assert_eq!(c.read_output(out), Value::new(0, 4)); // settle() never ticks
        }
    }

    #[test_case(None ; "write_enable floating (unconnected)")]
    #[test_case(Some((1, 2)) ; "write_enable wrong width (bits=1, width=2)")]
    #[test_case(Some((0, 1)) ; "write_enable exactly zero")]
    fn test_reg_write_enable_non_latching_cases(we_input: Option<(u32, u8)>) {
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(7, 4));
        let reg = c.add_component(Component::reg(4));
        let out = c.add_component(Component::output());
        c.link(data, PinId::output(0), reg, PinId::input(0));
        if let Some((bits, width)) = we_input {
            let we = c.add_component(Component::input(bits, width));
            c.link(we, PinId::output(0), reg, PinId::input(1));
        }
        // Otherwise write_enable (input 1) is left unconnected -> Floating.
        c.link(reg, PinId::output(0), out, PinId::input(0));

        c.settle().unwrap();
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out), Value::new(0, 4));
    }

    #[test]
    fn test_reg_multi_tick_sequence() {
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(0, 4));
        let we = c.add_component(Component::input(0, 1));
        let reg = c.add_component(Component::reg(4));
        let out = c.add_component(Component::output());
        c.link(data, PinId::output(0), reg, PinId::input(0));
        c.link(we, PinId::output(0), reg, PinId::input(1));
        c.link(reg, PinId::output(0), out, PinId::input(0));
        c.settle().unwrap();

        // tick 1: we=1, data=3 -> latches 3.
        c.set_input(we, 1, 1);
        c.set_input(data, 3, 4);
        c.settle().unwrap();
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out), Value::new(3, 4));

        // tick 2: we=0, data=9 -> holds 3.
        c.set_input(we, 0, 1);
        c.set_input(data, 9, 4);
        c.settle().unwrap();
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out), Value::new(3, 4));

        // tick 3: we=1, data=9 -> latches 9.
        c.set_input(we, 1, 1);
        c.settle().unwrap();
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out), Value::new(9, 4));
    }

    #[test]
    fn test_reg_output_feeds_combinational_logic_after_tick() {
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(0, 1));
        let we = c.add_component(Component::input(1, 1));
        let reg = c.add_component(Component::reg(1));
        let not_g = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let out = c.add_component(Component::output());
        c.link(data, PinId::output(0), reg, PinId::input(0));
        c.link(we, PinId::output(0), reg, PinId::input(1));
        c.link(reg, PinId::output(0), not_g, PinId::input(0));
        c.link(not_g, PinId::output(0), out, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::new(1, 1)); // NOT(0) = 1

        c.set_input(data, 1, 1);
        c.settle().unwrap();
        c.tick_clock().unwrap(); // latches 1; trailing settle() propagates through not_g
        assert_eq!(c.read_output(out), Value::new(0, 1)); // NOT(1) = 0
    }

    #[test]
    fn test_tick_clock_multiple_independent_regs() {
        let mut c = Circuit::new();
        let data1 = c.add_component(Component::input(5, 4));
        let we1 = c.add_component(Component::input(1, 1));
        let reg1 = c.add_component(Component::reg(4));
        let out1 = c.add_component(Component::output());

        let data2 = c.add_component(Component::input(9, 4));
        let we2 = c.add_component(Component::input(0, 1));
        let reg2 = c.add_component(Component::reg(4));
        let out2 = c.add_component(Component::output());

        c.link(data1, PinId::output(0), reg1, PinId::input(0));
        c.link(we1, PinId::output(0), reg1, PinId::input(1));
        c.link(reg1, PinId::output(0), out1, PinId::input(0));

        c.link(data2, PinId::output(0), reg2, PinId::input(0));
        c.link(we2, PinId::output(0), reg2, PinId::input(1));
        c.link(reg2, PinId::output(0), out2, PinId::input(0));

        c.settle().unwrap();
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out1), Value::new(5, 4)); // we1=1, latches
        assert_eq!(c.read_output(out2), Value::new(0, 4)); // we2=0, holds initial
    }

    #[test]
    fn test_tick_clock_snapshot_semantics_chained_registers() {
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(5, 4));
        let we1 = c.add_component(Component::input(1, 1));
        let we2 = c.add_component(Component::input(1, 1));
        let reg1 = c.add_component(Component::reg(4));
        let reg2 = c.add_component(Component::reg(4));
        let out2 = c.add_component(Component::output());

        c.link(data, PinId::output(0), reg1, PinId::input(0));
        c.link(we1, PinId::output(0), reg1, PinId::input(1));
        c.link(reg1, PinId::output(0), reg2, PinId::input(0));
        c.link(we2, PinId::output(0), reg2, PinId::input(1));
        c.link(reg2, PinId::output(0), out2, PinId::input(0));
        c.settle().unwrap();

        // tick 1: reg1 latches 5, but reg2 sees reg1's OLD value (0), since
        // tick_clock snapshots all sequential inputs before mutating.
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out2), Value::new(0, 4));

        // tick 2: reg2 now latches what reg1 captured during tick 1.
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out2), Value::new(5, 4));
    }

    #[test]
    fn test_tick_clock_noop_with_no_sequential_components() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let b = c.add_component(Component::input(0, 1));
        let g = c.add_component(Component::gate(GateOp::Or, 2, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), g, PinId::input(0));
        c.link(b, PinId::output(0), g, PinId::input(1));
        c.link(g, PinId::output(0), o, PinId::input(0));

        c.settle().unwrap();
        c.tick_clock().unwrap(); // no sequential components; behaves like settle()
        assert_eq!(c.read_output(o), Value::new(1, 1));
    }

    // ---- Group 6: structural operations ----

    #[test]
    fn test_clear_nets_resets_all_wiring() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let b = c.add_component(Component::input(0, 1));
        let and_g = c.add_component(Component::gate(GateOp::And, 2, 1));
        let or_g = c.add_component(Component::gate(GateOp::Or, 2, 1));
        let o1 = c.add_component(Component::output());
        let o2 = c.add_component(Component::output());
        c.link(a, PinId::output(0), and_g, PinId::input(0));
        c.link(b, PinId::output(0), and_g, PinId::input(1));
        c.link(a, PinId::output(0), or_g, PinId::input(0));
        c.link(b, PinId::output(0), or_g, PinId::input(1));
        c.link(and_g, PinId::output(0), o1, PinId::input(0));
        c.link(or_g, PinId::output(0), o2, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(o1), Value::new(0, 1));
        assert_eq!(c.read_output(o2), Value::new(1, 1));

        c.clear_nets();
        assert_eq!(c.read_output(o1), Value::Floating);
        assert_eq!(c.read_output(o2), Value::Floating);
    }

    #[test]
    fn test_clear_nets_then_relink_same_components_works() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let b = c.add_component(Component::input(1, 1));
        let g = c.add_component(Component::gate(GateOp::And, 2, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), g, PinId::input(0));
        c.link(b, PinId::output(0), g, PinId::input(1));
        c.link(g, PinId::output(0), o, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(1, 1));

        c.clear_nets();
        c.set_input(a, 0, 1);
        c.link(a, PinId::output(0), g, PinId::input(0));
        c.link(b, PinId::output(0), g, PinId::input(1));
        c.link(g, PinId::output(0), o, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(0, 1));
    }

    #[test]
    fn test_remove_component_nulls_direct_sink_input_pins() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let g = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), g, PinId::input(0));
        c.link(g, PinId::output(0), o, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(0, 1));

        c.remove_component(g);
        assert_eq!(c.read_output(o), Value::Floating);
    }

    #[test]
    fn test_remove_component_preserves_net_for_other_sinks() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let g1 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let g2 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let o2 = c.add_component(Component::output());
        c.link(a, PinId::output(0), g1, PinId::input(0));
        c.link(a, PinId::output(0), g2, PinId::input(0));
        c.link(g2, PinId::output(0), o2, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(o2), Value::new(0, 1));

        c.remove_component(g1); // g1 only reads from a's net
        c.settle().unwrap();
        assert_eq!(c.read_output(o2), Value::new(0, 1));
    }

    #[test]
    fn test_remove_component_leaves_downstream_out_cache_stale() {
        // Documents a gap between remove_component's doc comment ("the
        // caller is expected to call settle() after") and actual behavior:
        // only the directly-adjacent sink's input pin is nulled
        // synchronously; nothing is marked dirty, and dirty/queued are
        // unconditionally cleared. A subsequent settle() call is therefore a
        // no-op and cannot refresh out_cache/output values more than one hop
        // downstream of the removal.
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let g1 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let g2 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), g1, PinId::input(0));
        c.link(g1, PinId::output(0), g2, PinId::input(0));
        c.link(g2, PinId::output(0), o, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(1, 1)); // NOT(NOT(1)) = 1

        c.remove_component(g1);
        c.settle().unwrap(); // documented as sufficient, but is actually a no-op here

        // g2's input pin was nulled (would read Floating if re-evaluated),
        // but g2's out_cache still holds its stale pre-removal value, and
        // settle() never re-dirtied anything to refresh it.
        assert_eq!(c.read_output(o), Value::new(1, 1)); // stale: unchanged
    }

    // ---- Group 7: error / edge-case behavior ----

    #[test]
    #[should_panic]
    fn test_read_output_on_input_component_panics() {
        let mut c = Circuit::new();
        let i = c.add_component(Component::input(1, 1));
        let _ = c.read_output(i); // Input has 0 input pins; indexes out of bounds
    }

    #[test]
    fn test_set_input_on_gate_is_silent_noop() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let b = c.add_component(Component::input(1, 1));
        let g = c.add_component(Component::gate(GateOp::And, 2, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), g, PinId::input(0));
        c.link(b, PinId::output(0), g, PinId::input(1));
        c.link(g, PinId::output(0), o, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(1, 1));

        c.set_input(g, 99, 4); // g is a Gate, not an Input; silently no-ops
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(1, 1)); // unaffected
    }

    // ---- Group 8: tunnels ----

    #[test]
    fn test_tunnel_feed_pull_propagates_without_wire() {
        let mut c = Circuit::new();
        let driver = c.add_component(Component::input(1, 1));
        let pull = c.add_tunnel("CLK".to_string(), TunnelRole::Pull);
        c.link_tunnel(pull, driver, PinId::output(0));

        let feed = c.add_tunnel("CLK".to_string(), TunnelRole::Feed);
        let out = c.add_component(Component::output());
        c.link_tunnel(feed, out, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::new(1, 1));
    }

    #[test]
    fn test_tunnel_conflict_detected_after_convergence() {
        let mut c = Circuit::new();
        let driver1 = c.add_component(Component::input(1, 1));
        let driver2 = c.add_component(Component::input(0, 1));
        let pull1 = c.add_tunnel("BUS".to_string(), TunnelRole::Pull);
        c.link_tunnel(pull1, driver1, PinId::output(0));
        let pull2 = c.add_tunnel("BUS".to_string(), TunnelRole::Pull);
        c.link_tunnel(pull2, driver2, PinId::output(0));

        let result = c.settle();
        assert_eq!(
            result,
            Err(SettleError::TunnelConflict {
                label: "BUS".to_string()
            })
        );
    }

    #[test]
    fn test_tunnel_net_repointed_on_merge() {
        let mut c = Circuit::new();
        let driver1 = c.add_component(Component::input(1, 1));
        let driver2 = c.add_component(Component::input(1, 1));
        let sink1 = c.add_component(Component::output());
        let sink2 = c.add_component(Component::output());
        c.link(driver1, PinId::output(0), sink1, PinId::input(0));
        c.link(driver2, PinId::output(0), sink2, PinId::input(0));

        let pull = c.add_tunnel("X".to_string(), TunnelRole::Pull);
        c.link_tunnel(pull, sink2, PinId::input(0)); // attaches to sink2's net

        // sink1's net and sink2's net both already exist; linking their
        // already-attached input pins together forces a merge().
        c.link(sink1, PinId::input(0), sink2, PinId::input(0));
        c.settle().unwrap();

        // The tunnel must have followed the merge (repointed from the
        // removed net to the surviving one), not been left dangling.
        let feed = c.add_tunnel("X".to_string(), TunnelRole::Feed);
        let out = c.add_component(Component::output());
        c.link_tunnel(feed, out, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::new(1, 1));
    }

    #[test]
    fn test_tunnel_detached_when_driving_component_removed() {
        let mut c = Circuit::new();
        let driver = c.add_component(Component::input(1, 1));
        let pull = c.add_tunnel("Y".to_string(), TunnelRole::Pull);
        c.link_tunnel(pull, driver, PinId::output(0));
        c.settle().unwrap();

        c.remove_component(driver);
        // Must not panic (no dangling NetKey held by the tunnel), and a
        // subsequent settle() must succeed.
        c.settle().unwrap();
    }

    #[test]
    fn test_tunnel_detached_on_clear_nets() {
        let mut c = Circuit::new();
        let driver = c.add_component(Component::input(1, 1));
        let pull = c.add_tunnel("Z".to_string(), TunnelRole::Pull);
        c.link_tunnel(pull, driver, PinId::output(0));
        c.settle().unwrap();

        c.clear_nets();
        // Must not panic; the tunnel's net should have been reset to None.
        c.settle().unwrap();

        // Re-linking the same components/tunnel afterward must still work.
        let feed = c.add_tunnel("Z".to_string(), TunnelRole::Feed);
        let out = c.add_component(Component::output());
        c.link_tunnel(pull, driver, PinId::output(0));
        c.link_tunnel(feed, out, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::new(1, 1));
    }

    #[test]
    fn test_tunnel_rename_moves_label_group() {
        let mut c = Circuit::new();
        let driver = c.add_component(Component::input(1, 1));
        let pull = c.add_tunnel("OLD".to_string(), TunnelRole::Pull);
        c.link_tunnel(pull, driver, PinId::output(0));

        let feed_old = c.add_tunnel("OLD".to_string(), TunnelRole::Feed);
        let out_old = c.add_component(Component::output());
        c.link_tunnel(feed_old, out_old, PinId::input(0));

        let feed_new = c.add_tunnel("NEW".to_string(), TunnelRole::Feed);
        let out_new = c.add_component(Component::output());
        c.link_tunnel(feed_new, out_new, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(out_old), Value::new(1, 1)); // still "OLD" group
        assert_eq!(c.read_output(out_new), Value::Floating); // "NEW" has no Pull yet

        // Rename the Pull tunnel from "OLD" to "NEW".
        c.rename_tunnel(pull, "NEW".to_string());
        c.settle().unwrap();

        assert_eq!(c.read_output(out_new), Value::new(1, 1)); // now follows "NEW"
        assert_eq!(c.read_output(out_old), Value::Floating); // "OLD" lost its only Pull
    }
}
