use crate::sim::component::{CompKey, Component, Input, Logic, LogicComb, LogicSeq, PinId};
use crate::sim::net::{Net, NetKey};
use crate::sim::value::Value;

use slotmap::{SecondaryMap, SlotMap};
use std::collections::{HashMap, VecDeque};

/// Stable, app-assigned id for a `Tunnel`; survives remove + re-insert (ids never reused).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct TunnelKey(pub(crate) u64);

// Ties tunnels sharing a label into one virtual net, with no drawn wire.
// Feed drives FROM the group's value; Pull contributes TO it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    pub(crate) components: HashMap<CompKey, Component>,
    pub(crate) dirty: VecDeque<NetKey>,
    queued: SecondaryMap<NetKey, bool>,
    pub(crate) tunnels: HashMap<TunnelKey, Tunnel>,
    tunnel_labels: HashMap<String, Vec<TunnelKey>>,
    // Monotonic, never reused - avoids ABA when undo re-inserts a deleted entity under its original key.
    next_comp: u64,
    next_tunnel: u64,
}

impl Circuit {
    // Max value changes for one net per settle() before it flags oscillation.
    const REVISIT_THRESHOLD: usize = 16;
    // Backstop on total net-pops if many nets oscillate at once; scaled to circuit size.
    const ITERATION_BUDGET_PER_NET: usize = 64;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_component(&mut self, comp: Component) -> CompKey {
        let key = CompKey(self.next_comp);
        self.next_comp += 1;
        self.components.insert(key, comp);
        self.eval_component(key);
        key
    }

    /// Undo of `remove_component`. The caller rebuilds nets afterward. A
    /// `Reg`'s latched state returns intact.
    pub(crate) fn insert_component(&mut self, key: CompKey, comp: Component) {
        self.components.insert(key, comp);
        self.eval_component(key);
    }

    pub fn set_input(&mut self, comp: CompKey, bits: u32, width: u8) {
        // TODO: Make this return a result
        if let Logic::Comb(LogicComb::Input(Input { bits: b, width: w })) =
            &mut self.components.get_mut(&comp).unwrap().logic
        {
            *b = bits;
            *w = width;
            self.eval_component(comp);
        }
    }

    /// Injects `value` directly onto `comp`'s output pin 0, bypassing
    /// `evaluate()`. Unlike `set_input`, this accepts `Value::Floating`. Used
    /// to drive a subcircuit's boundary `Input` components. No-op if `comp`
    /// is stale.
    pub(crate) fn drive_input(&mut self, comp: CompKey, value: Value) {
        if self.components.contains_key(&comp) {
            self.apply_output_values(comp, vec![value]);
        }
    }

    /// Writes one word into a ROM's contents. Not undoable. No-op if `comp`
    /// isn't a ROM or `index` is out of range.
    pub fn write_rom(&mut self, comp: CompKey, index: usize, value: u32) {
        // set_word takes &self (interior mutability); re-evaluate after the borrow ends to dirty the output net.
        let wrote = if let Logic::Comb(LogicComb::Rom(rom)) = &self.components[&comp].logic {
            if index < rom.len() {
                rom.set_word(index, value);
                true
            } else {
                false
            }
        } else {
            false
        };
        if wrote {
            self.eval_component(comp);
        }
    }

    /// Writes one word into a RAM's contents. Not undoable. Unlike
    /// `write_rom`, this never changes `data_out` (a registered read,
    /// updated only by `tick_clock`). No-op if `comp` isn't a RAM or `index`
    /// is out of range.
    pub fn write_ram(&mut self, comp: CompKey, index: usize, value: u32) {
        if let Logic::Seq(LogicSeq::Ram(ram)) = &self.components[&comp].logic {
            let contents = ram.contents();
            if index < contents.len() {
                contents.set_word(index, value);
            }
        }
    }

    /// The value on `comp`'s input if it is an Output component, else `Value::Floating`.
    pub fn read_output(&self, comp: CompKey) -> Value {
        let comp = &self.components[&comp];

        match comp.logic {
            Logic::Comb(LogicComb::Output) => match comp.pins.inputs[0] {
                Some(net) => self.nets[net].value,
                None => Value::Floating,
            },
            _ => Value::Floating,
        }
    }

    pub fn clear_nets(&mut self) {
        puffin::profile_function!();
        for comp in self.components.values_mut() {
            comp.clear_pins();
        }

        // Every NetKey is about to become invalid; clear tunnel bindings so none dangle.
        for t in self.tunnels.values_mut() {
            t.net = None;
        }

        self.nets.clear();
        self.dirty.clear();
        self.queued.clear();

        // Every input now reads Floating; re-evaluate each component so a later relink can't read back a stale output.
        let keys: Vec<CompKey> = self.components.keys().copied().collect();
        for key in keys {
            self.eval_component(key);
        }
    }

    fn net_of(&self, comp: CompKey, pin: PinId) -> Option<NetKey> {
        self.components.get(&comp).and_then(|c| c.net_of(pin))
    }

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
        match pin {
            PinId::In(i) => self.nets[net].sinks.push((comp, i)),
            PinId::Out(i) => self.nets[net].sources.push((comp, i)),
        }
        self.components
            .get_mut(&comp)
            .unwrap()
            .set_pin_net(pin, net);
        // A sink pin needs an immediate eval here; nothing else will trigger eval_component for it.
        if let PinId::In(_) = pin {
            self.eval_component(comp);
        }
    }

    pub fn link(&mut self, a: CompKey, a_pin: PinId, b: CompKey, b_pin: PinId) -> NetKey {
        puffin::profile_function!();
        let net_a = self.net_of(a, a_pin);
        let net_b = self.net_of(b, b_pin);

        match (net_a, net_b) {
            (None, None) => {
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
        let b_net = self.nets.remove(b).unwrap();

        for (comp, i) in b_net.sinks {
            self.components
                .get_mut(&comp)
                .unwrap()
                .set_pin_net(PinId::In(i), a);
            self.nets[a].sinks.push((comp, i));
        }

        // A may end up with two sources here; resolve_net reports that as Invalid rather than dropping one.
        for (comp, i) in b_net.sources {
            self.components
                .get_mut(&comp)
                .unwrap()
                .set_pin_net(PinId::Out(i), a);
            self.nets[a].sources.push((comp, i));
        }

        // Repoint tunnels off the removed net; mark_dirty(a) below covers the dirtying.
        for t in self.tunnels.values_mut() {
            if t.net == Some(b) {
                t.net = Some(a);
            }
        }

        self.mark_dirty(a);
        a
    }

    pub fn add_tunnel(&mut self, label: String, role: TunnelRole) -> TunnelKey {
        let key = TunnelKey(self.next_tunnel);
        self.next_tunnel += 1;
        self.tunnels.insert(
            key,
            Tunnel {
                label: label.clone(),
                role,
                net: None,
            },
        );
        self.tunnel_labels.entry(label).or_default().push(key);
        key
    }

    pub fn link_tunnel(&mut self, tunnel: TunnelKey, comp: CompKey, pin: PinId) -> NetKey {
        let net = self.net_or_create(comp, pin);
        self.attach_tunnel(tunnel, net);
        net
    }

    fn attach_tunnel(&mut self, tunnel: TunnelKey, net: NetKey) {
        let old_net = self.tunnels[&tunnel].net;
        let label = self.tunnels[&tunnel].label.clone();
        self.tunnels.get_mut(&tunnel).unwrap().net = Some(net);
        // The old net must also be re-resolved, or it keeps a stale tunnel-contributed value.
        if let Some(old) = old_net {
            if old != net {
                self.mark_dirty(old);
            }
        }
        self.mark_dirty(net);
        // Group membership changed regardless of whether this net's value did; settle()'s
        // change-only dirtying can't catch that, so dirty Feed siblings explicitly.
        self.dirty_label_feed_nets(&label);
    }

    pub fn detach_tunnel(&mut self, tunnel: TunnelKey) {
        let label = self.tunnels[&tunnel].label.clone();
        if let Some(old) = self.tunnels.get_mut(&tunnel).unwrap().net.take() {
            self.mark_dirty(old);
        }
        self.dirty_label_feed_nets(&label);
    }

    /// Removes and returns a tunnel, dropped from its label group. `None` if
    /// the key is already gone.
    pub fn remove_tunnel(&mut self, tunnel: TunnelKey) -> Option<Tunnel> {
        let mut t = self.tunnels.remove(&tunnel)?;
        let label = t.label.clone();
        let net = t.net.take();
        if let Some(keys) = self.tunnel_labels.get_mut(&label) {
            keys.retain(|&k| k != tunnel);
            if keys.is_empty() {
                self.tunnel_labels.remove(&label);
            }
        }
        if let Some(net) = net {
            self.mark_dirty(net);
        }
        self.dirty_label_feed_nets(&label);
        Some(t)
    }

    /// Undo of `remove_tunnel`. The caller re-establishes the net binding via relink.
    pub(crate) fn insert_tunnel(&mut self, key: TunnelKey, tunnel: Tunnel) {
        let label = tunnel.label.clone();
        self.tunnels.insert(key, tunnel);
        self.tunnel_labels
            .entry(label.clone())
            .or_default()
            .push(key);
        self.dirty_label_feed_nets(&label);
    }

    /// The current label of a tunnel, or `None` if the key is stale.
    pub fn tunnel_label(&self, tunnel: TunnelKey) -> Option<&str> {
        self.tunnels.get(&tunnel).map(|t| t.label.as_str())
    }

    pub fn rename_tunnel(&mut self, tunnel: TunnelKey, new_label: String) {
        let Some(t) = self.tunnels.get_mut(&tunnel) else {
            return;
        };
        if t.label == new_label {
            return; // no-op: label unchanged, don't churn the label group
        }
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

    // Dirties every Feed tunnel's net in the group so it re-resolves after a value or membership change.
    fn dirty_label_feed_nets(&mut self, label: &str) {
        let Some(keys) = self.tunnel_labels.get(label) else {
            return;
        };
        let nets: Vec<NetKey> = keys
            .iter()
            .filter_map(|&tk| {
                let t = &self.tunnels[&tk];
                (t.role == TunnelRole::Feed).then_some(t.net).flatten()
            })
            .collect();
        for n in nets {
            self.mark_dirty(n);
        }
    }

    // Aggregates Pull tunnels' net values. `strict=false` is lenient (safe mid-convergence);
    // `strict=true` errors on disagreement, meant to be called once settle() has drained.
    fn tunnel_group_value(&self, label: &str, strict: bool) -> Result<Value, SettleError> {
        let Some(keys) = self.tunnel_labels.get(label) else {
            return Ok(Value::Floating);
        };
        let mut result = Value::Floating;
        for &tk in keys {
            let t = &self.tunnels[&tk];
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
        puffin::profile_function!();
        let mut revisits: SecondaryMap<NetKey, usize> = SecondaryMap::new();
        let iteration_budget = self
            .nets
            .len()
            .saturating_mul(Self::ITERATION_BUDGET_PER_NET)
            .max(1024);
        let mut total_iterations = 0;

        while let Some(net) = self.dirty.pop_front() {
            // Net may have been merged away and become stale.
            if !self.nets.contains_key(net) {
                continue;
            }

            // Clear the visited flag before eval so a loop can re-queue this net.
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

                // Re-evaluate every sink, including sequential ones: an async reset pin
                // must take effect within this settle(), with no clock tick.
                // eval_component calls observe(), never tick(), so latched state never advances here.
                let sinks: Vec<_> = self.nets[net].sinks.to_vec();

                for (comp, _) in sinks {
                    self.eval_component(comp);
                }

                // If a Pull tunnel reads this net, its group's value may have changed; re-dirty sibling Feed nets.
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
                // Defensive backstop; should be unreachable if the per-net revisit check above works.
                return Err(SettleError::Oscillation {
                    net,
                    revisits: revisits.get(net).copied().unwrap_or(0),
                });
            }
        }

        // All nets have converged; any tunnel-group disagreement found now is genuine, not a mid-convergence artifact.
        let labels: Vec<String> = self.tunnel_labels.keys().cloned().collect();
        for label in &labels {
            self.tunnel_group_value(label, true)?;
        }
        Ok(())
    }

    // True if attached pins declare conflicting widths, independent of the net's current Value.
    fn net_width_conflict(&self, net: NetKey) -> bool {
        let n = &self.nets[net];
        let mut widths = n
            .sources
            .iter()
            .filter_map(|&(comp, i)| self.components[&comp].output_width(i))
            .chain(
                n.sinks
                    .iter()
                    .filter_map(|&(comp, i)| self.components[&comp].input_width(i)),
            );
        let Some(first) = widths.next() else {
            return false;
        };
        widths.any(|w| w != first)
    }

    // Two or more drivers is a conflict (a short); resolves to Value::Invalid, same as a width mismatch.
    fn resolve_net(&mut self, net: NetKey) -> bool {
        puffin::profile_function!();
        let old = self.nets[net].value;

        let new = if self.net_width_conflict(net) || self.nets[net].sources.len() > 1 {
            Value::Invalid
        } else {
            match self.nets[net].sources.first() {
                // Net takes value from pins.out_cache, which is updated in eval_component
                Some(&(comp, i)) => self.components[&comp].pins.out_cache[i.0 as usize],
                // A component driver always takes priority over a Feed tunnel's group value.
                None => self.tunnel_feed_value(net),
            }
        };
        self.nets[net].value = new;
        new != old
    }

    fn eval_component(&mut self, comp: CompKey) {
        puffin::profile_function!();
        // A sequential component may apply async, level-sensitive effects here (e.g. an
        // async reset) before its output is read, with no clock tick. This is the only
        // other place settle() mutates latched state; apply_async is idempotent, so repeat calls converge.
        if self.components[&comp].is_stateful() {
            let inputs = self.components[&comp].read_inputs(&self.nets);
            self.components.get_mut(&comp).unwrap().apply_async(&inputs);
        }
        let new_values = self.components[&comp].evaluate(&self.nets);
        self.apply_output_values(comp, new_values);
    }

    // Shared by eval_component (combinational path) and tick_clock (sequential path).
    fn apply_output_values(&mut self, comp: CompKey, new_values: Vec<Value>) {
        puffin::profile_function!();
        let c = self.components.get_mut(&comp).unwrap();
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

    // Generic over LogicSeq variants; a new sequential type needs no changes here.
    pub fn tick_clock(&mut self) -> Result<(), SettleError> {
        puffin::profile_function!();
        let seq_comps: Vec<CompKey> = self
            .components
            .iter()
            .filter(|(_, c)| c.is_stateful())
            .map(|(key, _)| *key)
            .collect();

        let collected_inputs: Vec<(CompKey, Vec<Value>)> = seq_comps
            .into_iter()
            .map(|key| {
                let inputs = self.components[&key].read_inputs(&self.nets);
                (key, inputs)
            })
            .collect();

        for (key, inputs) in collected_inputs {
            let new_values = self.components.get_mut(&key).unwrap().tick(&inputs);
            self.apply_output_values(key, new_values);
        }

        self.settle()
    }

    // Drives the GUI's clock "Stop". Generic over LogicSeq; a new sequential type needs only a SeqLogic::reset impl.
    pub fn reset_sequential(&mut self) -> Result<(), SettleError> {
        puffin::profile_function!();
        let seq_comps: Vec<CompKey> = self
            .components
            .iter()
            .filter(|(_, c)| c.is_stateful())
            .map(|(key, _)| *key)
            .collect();

        for key in seq_comps {
            self.components.get_mut(&key).unwrap().reset();
            let values = self.components[&key].observe();
            self.apply_output_values(key, values);
        }

        self.settle()
    }

    pub fn remove_component(&mut self, key: CompKey) -> Option<Component> {
        let comp = self.components.get(&key)?;
        let output_nets: Vec<NetKey> = comp.pins.outputs.iter().filter_map(|&n| n).collect();
        let input_nets: Vec<NetKey> = comp.pins.inputs.iter().filter_map(|&n| n).collect();

        // Collected now, applied after the dirty/queued reset below so the fresh marks survive.
        let mut affected_labels: Vec<String> = Vec::new();
        let mut affected_sinks: Vec<CompKey> = Vec::new();
        let mut retained_nets: Vec<NetKey> = Vec::new();

        // A net with another driver survives; a driverless one is torn down, freeing sink pin slots.
        for net_key in output_nets {
            let Some(net) = self.nets.get_mut(net_key) else {
                continue;
            };
            net.sources.retain(|&(ck, _)| ck != key);
            if !net.sources.is_empty() {
                retained_nets.push(net_key);
                continue;
            }
            let sinks = net.sinks.clone();
            for (sink_comp, sink_pin) in sinks {
                if let Some(sc) = self.components.get_mut(&sink_comp) {
                    sc.pins.inputs[sink_pin.0 as usize] = None;
                }
                if sink_comp != key {
                    affected_sinks.push(sink_comp);
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

        for net_key in input_nets {
            if let Some(net) = self.nets.get_mut(net_key) {
                net.sinks.retain(|&(ck, _)| ck != key);
            }
        }

        // Clear propagation state; the caller is expected to call settle() after
        self.dirty.clear();
        self.queued.clear();

        // Hand the owned Component back for the undo entry (see insert_component); pins are
        // nulled so it holds no dangling NetKeys. A Reg's latched state rides along untouched.
        let mut removed = self.components.remove(&key);
        if let Some(c) = &mut removed {
            c.clear_pins();
        }

        // Re-evaluate sinks that lost their driver so the new Floating input propagates on settle().
        for sink in affected_sinks {
            if self.components.contains_key(&sink) {
                self.eval_component(sink);
            }
        }

        // Re-dirty sibling Feed-tunnel nets now that propagation state is reset above.
        for label in affected_labels {
            self.dirty_label_feed_nets(&label);
        }

        // Re-resolve nets that kept another driver, e.g. a conflict that just became single-driver.
        for net_key in retained_nets {
            self.mark_dirty(net_key);
        }

        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::component::{GateOp, RegConf};

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
        assert_eq!(c.read_output(o1), Value::ZERO);
        assert_eq!(c.read_output(o2), Value::ONE);
    }

    #[test]
    fn test_add_component_input_out_cache_populated_immediately() {
        let mut c = Circuit::new();
        let i = c.add_component(Component::input(5, 3));
        // add_component eagerly evaluates, before link() or settle().
        assert_eq!(c.components[&i].pins.out_cache[0], Value::new(5, 3));
    }

    #[test]
    fn test_link_before_settle_net_value_still_floating() {
        let mut c = Circuit::new();
        let i = c.add_component(Component::input(5, 3));
        let o = c.add_component(Component::output());
        c.link(i, PinId::output(0), o, PinId::input(0));
        // out_cache is populated, but the net's value isn't resolved until settle() runs resolve_net.
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
        assert_eq!(c.read_output(o1), Value::ZERO);
        assert_eq!(c.read_output(o2), Value::ZERO);
    }

    #[test]
    fn test_link_second_source_yields_invalid() {
        let mut c = Circuit::new();
        let i1 = c.add_component(Component::input(1, 1));
        let i2 = c.add_component(Component::input(0, 1));
        let g1 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let g2 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let o = c.add_component(Component::output());

        c.link(i1, PinId::output(0), g1, PinId::input(0));
        c.link(i2, PinId::output(0), g2, PinId::input(0));

        c.link(g1, PinId::output(0), o, PinId::input(0));
        // o already has a net driven by g1; adding g2 gives it two drivers -> Invalid, not silently picked.
        c.link(g2, PinId::output(0), o, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::Invalid);
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
    fn test_link_merge_two_drivers_yields_invalid() {
        // Merging two driven nets folds both drivers onto the survivor -> Invalid, not silently kept.
        let mut c = Circuit::new();
        let driver1 = c.add_component(Component::input(1, 1));
        let driver2 = c.add_component(Component::input(0, 1));
        let sink1 = c.add_component(Component::output());
        let sink2 = c.add_component(Component::output());

        c.link(driver1, PinId::output(0), sink1, PinId::input(0));
        c.link(driver2, PinId::output(0), sink2, PinId::input(0));
        // Resolve both nets before merging; see
        // test_link_merge_of_still_dirty_nets_removes_stale_key for the dirty-queue case.
        c.settle().unwrap();
        assert_eq!(c.read_output(sink1), Value::ONE);
        assert_eq!(c.read_output(sink2), Value::ZERO);

        c.link(sink1, PinId::input(0), sink2, PinId::input(0));

        c.settle().unwrap();
        // Both sinks now share one net driven by both driver1 and driver2 -> Invalid.
        assert_eq!(c.read_output(sink1), Value::Invalid);
        assert_eq!(c.read_output(sink2), Value::Invalid);
    }

    #[test]
    fn test_remove_one_of_two_drivers_clears_conflict() {
        // A two-driver net is Invalid; removing one driver leaves it single-driver, not torn down.
        let mut c = Circuit::new();
        let d1 = c.add_component(Component::input(1, 1));
        let d2 = c.add_component(Component::input(0, 1));
        let o = c.add_component(Component::output());

        c.link(d1, PinId::output(0), o, PinId::input(0));
        c.link(d2, PinId::output(0), o, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::Invalid);

        c.remove_component(d2);
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::ONE);
    }

    #[test]
    fn test_link_merge_of_still_dirty_nets_removes_stale_key() {
        let mut c = Circuit::new();
        let driver1 = c.add_component(Component::input(1, 1));
        let driver2 = c.add_component(Component::input(0, 1));
        let sink1 = c.add_component(Component::output());
        let sink2 = c.add_component(Component::output());

        c.link(driver1, PinId::output(0), sink1, PinId::input(0));
        c.link(driver2, PinId::output(0), sink2, PinId::input(0));
        // Merging while both nets are still dirty removes net2 from the slotmap while queued.
        c.link(sink1, PinId::input(0), sink2, PinId::input(0));

        c.settle().unwrap();
    }

    // ---- Group 2: propagation / settle behavior ----

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
        assert_eq!(c.read_output(o), Value::ZERO);
    }

    #[test]
    fn test_settle_idempotent_when_no_dirty_nets() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), o, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::ONE);
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::ONE);
    }

    #[test]
    fn test_settle_self_loop_stabilizes_to_floating() {
        let mut c = Circuit::new();
        let g = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let o = c.add_component(Component::output());
        c.link(g, PinId::output(0), g, PinId::input(0));
        c.link(g, PinId::output(0), o, PinId::input(0));

        c.settle().unwrap(); // must not panic
        assert_eq!(c.read_output(o), Value::Floating);
    }

    #[test]
    fn test_settle_long_acyclic_chain_settles_successfully() {
        // Regression test: settle() used to count total net-pops, so a deep acyclic chain could
        // falsely trip the old iteration cap. Per-net revisit tracking fixes this.
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
    fn test_settle_second_driver_into_loop_is_invalid_not_oscillation() {
        // Regression test: a NOT-gate ring used to oscillate when a second driver closed the
        // loop. Two drivers now resolve to Invalid and stay there, so settle() converges instead.
        let mut c = Circuit::new();
        let seed = c.add_component(Component::input(0, 1));
        let n1 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let n2 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let n3 = c.add_component(Component::gate(GateOp::Not, 1, 1));

        c.link(seed, PinId::output(0), n1, PinId::input(0));
        c.link(n1, PinId::output(0), n2, PinId::input(0));
        c.link(n2, PinId::output(0), n3, PinId::input(0));
        c.settle().unwrap(); // seeds n1/n2/n3 with concrete alternating values, no loop yet

        c.link(n3, PinId::output(0), n1, PinId::input(0));
        assert!(c.settle().is_ok());
        // n1's input net is Invalid (two drivers); NOT reads Invalid as Floating,
        // so n1's single-driver output net settles Floating.
        let o = c.add_component(Component::output());
        c.link(n1, PinId::output(0), o, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::Floating);
    }

    // ---- Group 3: register / clock behavior ----

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
            assert_eq!(c.read_output(out), Value::new(0, 4)); // settle() never latches
        }
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
        assert_eq!(c.read_output(out), Value::ONE);

        c.set_input(data, 1, 1);
        c.settle().unwrap();
        c.tick_clock().unwrap(); // latches 1; trailing settle() propagates through not_g
        assert_eq!(c.read_output(out), Value::ZERO);
    }

    #[test]
    fn test_reg_async_reset_destroys_state_during_settle_without_a_tick() {
        // Driving reset clears the register within settle() alone, destructively - it stays zero after release.
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(9, 4));
        let we = c.add_component(Component::input(1, 1));
        let rst = c.add_component(Component::input(0, 1)); // deasserted
        let reg = c.add_component(Component::reg(4));
        let out = c.add_component(Component::output());
        c.link(
            data,
            PinId::output(0),
            reg,
            PinId::input(RegConf::DATA_PIN as u8),
        );
        c.link(
            we,
            PinId::output(0),
            reg,
            PinId::input(RegConf::WRITE_EN_PIN as u8),
        );
        c.link(
            rst,
            PinId::output(0),
            reg,
            PinId::input(RegConf::RESET_PIN as u8),
        );
        c.link(reg, PinId::output(0), out, PinId::input(0));

        c.settle().unwrap();
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out), Value::new(9, 4));

        // Assert reset with settle() only, no tick: state clears immediately.
        c.set_input(rst, 1, 1);
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::new(0, 4));

        // Release reset, still no tick: the clear was destructive, so it stays 0.
        c.set_input(rst, 0, 1);
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::new(0, 4));
    }

    #[test]
    fn test_t_flip_flop_async_reset_gui_flow() {
        // The interactive flow: toggle a T flip-flop over a few clocks, pulse the async reset mid-run (no tick), then resume.
        use crate::sim::component::TFlipFlopConf as T;
        let mut c = Circuit::new();
        let toggle = c.add_component(Component::input(1, 1));
        let rst = c.add_component(Component::input(0, 1));
        let ff = c.add_component(Component::t_flip_flop());
        let out = c.add_component(Component::output());
        c.link(
            toggle,
            PinId::output(0),
            ff,
            PinId::input(T::TOGGLE_PIN as u8),
        );
        c.link(rst, PinId::output(0), ff, PinId::input(T::RESET_PIN as u8));
        c.link(ff, PinId::output(0), out, PinId::input(0));
        c.settle().unwrap();

        // Three toggles: 0 -> 1 -> 0 -> 1, ending high.
        c.tick_clock().unwrap();
        c.tick_clock().unwrap();
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out), Value::ONE);

        c.set_input(rst, 1, 1);
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ZERO);

        // Stays 0 after release (destroyed, not restored).
        c.set_input(rst, 0, 1);
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ZERO);

        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out), Value::ONE);
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

        // tick 1: reg2 sees reg1's OLD value (0) - tick_clock snapshots inputs before mutating.
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out2), Value::new(0, 4));

        // tick 2: reg2 latches what reg1 captured during tick 1.
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
        assert_eq!(c.read_output(o), Value::ONE);
    }

    #[test]
    fn test_reset_sequential_restores_initial_state() {
        use crate::sim::component::OverflowAction;
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(5, 4));
        let we = c.add_component(Component::input(1, 1));
        let reg = c.add_component(Component::reg(4));
        let out_reg = c.add_component(Component::output());

        let load = c.add_component(Component::input(0, 1));
        let count = c.add_component(Component::input(1, 1));
        let counter = c.add_component(Component::counter(4, 15, OverflowAction::Wrap));
        let out_ctr = c.add_component(Component::output());

        c.link(data, PinId::output(0), reg, PinId::input(0));
        c.link(we, PinId::output(0), reg, PinId::input(1));
        c.link(reg, PinId::output(0), out_reg, PinId::input(0));
        c.link(load, PinId::output(0), counter, PinId::input(1)); // LOAD_PIN = 0 (count up)
        c.link(count, PinId::output(0), counter, PinId::input(2)); // COUNT_PIN
        c.link(counter, PinId::output(0), out_ctr, PinId::input(0)); // Q_PIN
        c.settle().unwrap();

        // Advance a few ticks so both hold non-initial state.
        c.tick_clock().unwrap();
        c.tick_clock().unwrap();
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out_reg), Value::new(5, 4));
        assert_eq!(c.read_output(out_ctr), Value::new(3, 4));

        c.reset_sequential().unwrap();
        assert_eq!(c.read_output(out_reg), Value::new(0, 4));
        assert_eq!(c.read_output(out_ctr), Value::new(0, 4));
    }

    #[test]
    fn test_reset_sequential_noop_with_no_sequential_components() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), o, PinId::input(0));
        c.settle().unwrap();
        c.reset_sequential().unwrap(); // behaves like settle()
        assert_eq!(c.read_output(o), Value::ONE);
    }

    // ---- Group 4: structural operations ----

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
        assert_eq!(c.read_output(o1), Value::ZERO);
        assert_eq!(c.read_output(o2), Value::ONE);

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
        assert_eq!(c.read_output(o), Value::ONE);

        c.clear_nets();
        c.set_input(a, 0, 1);
        c.link(a, PinId::output(0), g, PinId::input(0));
        c.link(b, PinId::output(0), g, PinId::input(1));
        c.link(g, PinId::output(0), o, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::ZERO);
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
        assert_eq!(c.read_output(o), Value::ZERO);

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
        assert_eq!(c.read_output(o2), Value::ZERO);

        c.remove_component(g1);
        c.settle().unwrap();
        assert_eq!(c.read_output(o2), Value::ZERO);
    }

    #[test]
    fn test_remove_component_refreshes_downstream_sinks() {
        // Removing a driver re-evaluates lost sinks and re-dirties their nets,
        // refreshing values more than one hop downstream.
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let g1 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let g2 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), g1, PinId::input(0));
        c.link(g1, PinId::output(0), g2, PinId::input(0));
        c.link(g2, PinId::output(0), o, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::ONE);

        c.remove_component(g1);
        c.settle().unwrap();

        // g2's input was nulled -> Floating; NOT(Floating) = Floating propagates through to o.
        assert_eq!(c.read_output(o), Value::Floating);
    }

    // ---- Group 5: error / edge-case behavior ----

    #[test]
    fn test_read_output_on_non_output_floating() {
        let mut c = Circuit::new();
        let i = c.add_component(Component::input(1, 1));
        assert_eq!(c.read_output(i), Value::Floating)
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
        assert_eq!(c.read_output(o), Value::ONE);

        c.set_input(g, 99, 4); // g is a Gate, not an Input; silently no-ops
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::ONE);
    }

    // ---- Group 6: tunnels ----

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
        assert_eq!(c.read_output(out), Value::ONE);
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
        let sink1 = c.add_component(Component::output());
        let sink2 = c.add_component(Component::output());
        c.link(driver1, PinId::output(0), sink1, PinId::input(0));

        let pull = c.add_tunnel("X".to_string(), TunnelRole::Pull);
        c.link_tunnel(pull, sink2, PinId::input(0));

        // sink1's net (driven) and sink2's net (tunnel only) both exist; linking their pins forces a merge().
        c.link(sink1, PinId::input(0), sink2, PinId::input(0));
        c.settle().unwrap();

        // The tunnel must follow the merge, repointed to the surviving net, not left dangling.
        let feed = c.add_tunnel("X".to_string(), TunnelRole::Feed);
        let out = c.add_component(Component::output());
        c.link_tunnel(feed, out, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ONE);
    }

    #[test]
    fn test_tunnel_detached_when_driving_component_removed() {
        let mut c = Circuit::new();
        let driver = c.add_component(Component::input(1, 1));
        let pull = c.add_tunnel("Y".to_string(), TunnelRole::Pull);
        c.link_tunnel(pull, driver, PinId::output(0));
        c.settle().unwrap();

        c.remove_component(driver);
        // Must not panic (no dangling NetKey held by the tunnel).
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

        let feed = c.add_tunnel("Z".to_string(), TunnelRole::Feed);
        let out = c.add_component(Component::output());
        c.link_tunnel(pull, driver, PinId::output(0));
        c.link_tunnel(feed, out, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ONE);
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
        assert_eq!(c.read_output(out_old), Value::ONE); // still "OLD" group
        assert_eq!(c.read_output(out_new), Value::Floating); // "NEW" has no Pull yet

        c.rename_tunnel(pull, "NEW".to_string());
        c.settle().unwrap();

        assert_eq!(c.read_output(out_new), Value::ONE); // now follows "NEW"
        assert_eq!(c.read_output(out_old), Value::Floating); // "OLD" lost its only Pull
    }

    // ---- Group 7: net width-conflict detection (Value::Invalid) ----

    #[test]
    fn test_width_conflict_between_fanned_out_sinks_is_invalid() {
        let mut c = Circuit::new();
        let driver = c.add_component(Component::input(1, 4));
        let g4 = c.add_component(Component::gate(GateOp::Not, 1, 4));
        let g8 = c.add_component(Component::gate(GateOp::Not, 1, 8)); // wrong width sink
        let out = c.add_component(Component::output());

        c.link(driver, PinId::output(0), g4, PinId::input(0));
        c.link(driver, PinId::output(0), g8, PinId::input(0));
        c.link(driver, PinId::output(0), out, PinId::input(0));
        c.settle().unwrap();

        // driver's net has a concrete value, but sinks disagree on width (4 vs 8) -> Invalid, not driver's out_cache.
        assert_eq!(c.read_output(out), Value::Invalid);
    }

    #[test]
    fn test_width_conflict_detected_even_with_no_driver() {
        let mut c = Circuit::new();
        // No component drives this net; it must still flag Invalid purely from the sinks' conflicting widths.
        let g4 = c.add_component(Component::gate(GateOp::Not, 1, 4));
        let g8 = c.add_component(Component::gate(GateOp::Not, 1, 8));
        let out = c.add_component(Component::output());

        c.link(g4, PinId::input(0), g8, PinId::input(0));
        c.link(out, PinId::input(0), g4, PinId::input(0));
        c.settle().unwrap();

        assert_eq!(c.read_output(out), Value::Invalid);
    }

    #[test]
    fn test_lone_widthed_pin_with_unconstrained_sink_is_not_invalid() {
        let mut c = Circuit::new();
        // Output declares no expected width, so g's output has nothing to conflict with -> Floating, not Invalid.
        let g = c.add_component(Component::gate(GateOp::Not, 1, 4));
        let out = c.add_component(Component::output());
        c.link(g, PinId::output(0), out, PinId::input(0));
        c.settle().unwrap();

        assert_eq!(c.read_output(out), Value::Floating);
    }

    #[test]
    fn test_width_mismatch_local_only_does_not_propagate_downstream() {
        let mut c = Circuit::new();
        let driver = c.add_component(Component::input(1, 4));
        let g4 = c.add_component(Component::gate(GateOp::Not, 1, 4));
        let g8 = c.add_component(Component::gate(GateOp::Not, 1, 8)); // conflicts with g4 on driver's net
        let probe = c.add_component(Component::output()); // reads the conflicted net directly
        let downstream = c.add_component(Component::gate(GateOp::Not, 1, 4));
        let out = c.add_component(Component::output());

        c.link(driver, PinId::output(0), g4, PinId::input(0));
        c.link(driver, PinId::output(0), g8, PinId::input(0));
        c.link(driver, PinId::output(0), probe, PinId::input(0));
        c.link(g4, PinId::output(0), downstream, PinId::input(0));
        c.link(downstream, PinId::output(0), out, PinId::input(0));
        c.settle().unwrap();

        assert_eq!(c.read_output(probe), Value::Invalid);
        // One hop downstream: g4 reads Invalid, produces Floating (Value::Not);
        // that net's widths agree, so Invalid doesn't propagate further.
        assert_eq!(c.read_output(out), Value::Floating);
    }

    #[test]
    fn test_rom_reads_and_write_rom_propagates() {
        let mut c = Circuit::new();
        // Address input (width 3) -> ROM (data_width 8, 8 words) -> Output.
        let addr = c.add_component(Component::input(2, 3));
        let rom = crate::sim::component::Rom::new(8, 3);
        rom.set_word(2, 0x5A);
        let rom_key = c.add_component(Component::rom(rom));
        let out = c.add_component(Component::output());
        c.link(addr, PinId::output(0), rom_key, PinId::input(0));
        c.link(rom_key, PinId::output(0), out, PinId::input(0));
        c.settle().unwrap();

        assert_eq!(c.read_output(out), Value::new(0x5A, 8));

        c.write_rom(rom_key, 2, 0xFF);
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::new(0xFF, 8));

        c.write_rom(rom_key, 5, 0x11);
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::new(0xFF, 8));
    }

    // ── Subcircuits (Logic::Sub) ──────────────────────────────────────────────

    // Wraps AND(a, b). Built anew each call (Circuit isn't Clone) so calls yield independent instances.
    fn and_subcircuit() -> Component {
        let mut inner = Circuit::new();
        let a = inner.add_component(Component::input(0, 1));
        let b = inner.add_component(Component::input(0, 1));
        let g = inner.add_component(Component::gate(GateOp::And, 2, 1));
        let o = inner.add_component(Component::output());
        inner.link(a, PinId::output(0), g, PinId::input(0));
        inner.link(b, PinId::output(0), g, PinId::input(1));
        inner.link(g, PinId::output(0), o, PinId::input(0));
        inner.settle().unwrap();
        Component::subcircuit(inner, vec![a, b], vec![o])
    }

    // Wraps a 4-bit register: inputs [D, WE], output [Q].
    fn reg_subcircuit() -> Component {
        let mut inner = Circuit::new();
        let d = inner.add_component(Component::input(0, 4));
        let we = inner.add_component(Component::input(0, 1));
        let reg = inner.add_component(Component::reg(4));
        let o = inner.add_component(Component::output());
        inner.link(d, PinId::output(0), reg, PinId::input(0));
        inner.link(we, PinId::output(0), reg, PinId::input(1));
        inner.link(reg, PinId::output(0), o, PinId::input(0));
        inner.settle().unwrap();
        Component::subcircuit(inner, vec![d, we], vec![o])
    }

    #[test]
    fn subcircuit_propagates_combinationally() {
        let mut c = Circuit::new();
        let x = c.add_component(Component::input(1, 1));
        let y = c.add_component(Component::input(1, 1));
        let sub = c.add_component(and_subcircuit());
        let out = c.add_component(Component::output());
        c.link(x, PinId::output(0), sub, PinId::input(0));
        c.link(y, PinId::output(0), sub, PinId::input(1));
        c.link(sub, PinId::output(0), out, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ONE);

        // An input change settles through the boundary with no clock tick.
        c.set_input(x, 0, 1);
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ZERO);
    }

    #[test]
    fn subcircuit_forwards_clock_tick() {
        let mut c = Circuit::new();
        let d = c.add_component(Component::input(7, 4));
        let we = c.add_component(Component::input(1, 1));
        let sub = c.add_component(reg_subcircuit());
        let out = c.add_component(Component::output());
        c.link(d, PinId::output(0), sub, PinId::input(0));
        c.link(we, PinId::output(0), sub, PinId::input(1));
        c.link(sub, PinId::output(0), out, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::new(0, 4)); // register power-on 0

        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out), Value::new(7, 4));
    }

    #[test]
    fn subcircuit_instances_have_independent_state() {
        let mut c = Circuit::new();
        // Instance 1: WE = 1 (latches its data).
        let d1 = c.add_component(Component::input(5, 4));
        let we1 = c.add_component(Component::input(1, 1));
        let sub1 = c.add_component(reg_subcircuit());
        let out1 = c.add_component(Component::output());
        c.link(d1, PinId::output(0), sub1, PinId::input(0));
        c.link(we1, PinId::output(0), sub1, PinId::input(1));
        c.link(sub1, PinId::output(0), out1, PinId::input(0));

        // Instance 2: WE = 0 (holds its power-on value).
        let d2 = c.add_component(Component::input(9, 4));
        let we2 = c.add_component(Component::input(0, 1));
        let sub2 = c.add_component(reg_subcircuit());
        let out2 = c.add_component(Component::output());
        c.link(d2, PinId::output(0), sub2, PinId::input(0));
        c.link(we2, PinId::output(0), sub2, PinId::input(1));
        c.link(sub2, PinId::output(0), out2, PinId::input(0));

        c.settle().unwrap();
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out1), Value::new(5, 4)); // latched
        assert_eq!(c.read_output(out2), Value::new(0, 4)); // held initial
    }

    #[test]
    fn nested_subcircuit_settles() {
        // Nested: outer -> mid(sub) -> and(sub).
        let mut mid = Circuit::new();
        let ma = mid.add_component(Component::input(0, 1));
        let mb = mid.add_component(Component::input(0, 1));
        let inner_and = mid.add_component(and_subcircuit());
        let mo = mid.add_component(Component::output());
        mid.link(ma, PinId::output(0), inner_and, PinId::input(0));
        mid.link(mb, PinId::output(0), inner_and, PinId::input(1));
        mid.link(inner_and, PinId::output(0), mo, PinId::input(0));
        mid.settle().unwrap();
        let mid_sub = Component::subcircuit(mid, vec![ma, mb], vec![mo]);

        let mut c = Circuit::new();
        let x = c.add_component(Component::input(1, 1));
        let y = c.add_component(Component::input(1, 1));
        let sub = c.add_component(mid_sub);
        let out = c.add_component(Component::output());
        c.link(x, PinId::output(0), sub, PinId::input(0));
        c.link(y, PinId::output(0), sub, PinId::input(1));
        c.link(sub, PinId::output(0), out, PinId::input(0));

        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ONE); // 1 AND 1 through two levels

        c.set_input(x, 0, 1);
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ZERO);
    }
}
