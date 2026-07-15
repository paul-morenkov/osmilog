use crate::sim::component::{CompKey, Component, Input, Logic, LogicComb, LogicSeq, PinId};
use crate::sim::net::{Net, NetKey};
use crate::sim::value::Value;

use slotmap::{SecondaryMap, SlotMap};
use std::collections::{HashMap, VecDeque};

/// Stable, app-assigned identifier for a `Tunnel`. Like [`CompKey`], it survives
/// a remove + re-insert so undo can restore a deleted tunnel under its original
/// key; ids come from a monotonic counter and are never reused.
///
/// [`CompKey`]: crate::sim::component::CompKey
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct TunnelKey(pub(crate) u64);

// Ties all Tunnels sharing a Label into one virtual net without a drawn wire
// (a schematic "net label" / off-page connector). Feed tunnels drive their
// net FROM the group's resolved value; Pull tunnels contribute their net's
// value TO the group. See Circuit::settle()/resolve_net().
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
    // Monotonic id allocators for the two stable-key maps above. Never reused,
    // so a removed key is never handed to a different entity (no ABA); undo
    // re-inserts a deleted entity under its original key with a plain
    // `HashMap::insert`.
    next_comp: u64,
    next_tunnel: u64,
}

impl Circuit {
    // How many times a single net may change value within one settle() call
    // before it's a combinational oscillation - bounded by reconvergent
    // fan-in depth, not circuit size.
    const REVISIT_THRESHOLD: usize = 16;
    // Defensive backstop on total net-pops, in case many nets oscillate at
    // once. Scaled to circuit size to avoid false positives; should rarely
    // trigger if the per-net check above is doing its job.
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

    /// Re-inserts a previously-removed component under its original key - the
    /// undo of `remove_component`. Re-evaluates it so its `out_cache` refreshes;
    /// the caller re-establishes nets (via rebuild) and settles. A moved-in
    /// `Reg`'s latched state comes back exactly as it was at removal.
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

    /// Injects a Value directly onto a component's output pin 0, as if the
    /// component had produced it, and dirties that output net. Used to feed a
    /// subcircuit's boundary Input components from the enclosing circuit: unlike
    /// set_input it can deliver any Value (including Floating), and because an
    /// Input has no input nets, settle() never re-runs its evaluate() to
    /// overwrite the injected value. Marks the net dirty only when the value
    /// changes, so re-driving identical inputs is a no-op (keeps settle
    /// convergent). No-op if `comp` is stale.
    pub(crate) fn drive_input(&mut self, comp: CompKey, value: Value) {
        if self.components.contains_key(&comp) {
            self.apply_output_values(comp, vec![value]);
        }
    }

    /// Writes one word into a ROM component's contents in place, masked to its
    /// data_width, and re-evaluates it so the change propagates on the next
    /// settle(). No-op if `comp` isn't a ROM or `index` is out of range. Unlike
    /// structural edits this bypasses the Command/undo layer entirely (like
    /// set_input / clock ticks): ROM contents are mutated live, not undoable.
    pub fn write_rom(&mut self, comp: CompKey, index: usize, value: u32) {
        // set_word takes &self (interior mutability), so this needs only a shared
        // borrow to write; re-evaluate afterward, once that borrow has ended, to
        // dirty the output net.
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

    /// Writes one word into a RAM component's contents in place, masked to
    /// its data_width. Unlike write_rom this never changes data_out (RAM's
    /// output is a *registered* read, only updated by tick_clock - see
    /// RamCell), so there is nothing to re-evaluate; the write is purely a
    /// debug-time direct memory edit. No-op if `comp` isn't a RAM or `index`
    /// is out of range. Like write_rom, bypasses the Command/undo layer
    /// entirely - RAM contents are mutated live, not undoable.
    pub fn write_ram(&mut self, comp: CompKey, index: usize, value: u32) {
        if let Logic::Seq(LogicSeq::Ram(ram)) = &self.components[&comp].logic {
            let contents = ram.contents();
            if index < contents.len() {
                contents.set_word(index, value);
            }
        }
    }

    /// The current value on `comp`'s input, if it's an Output component;
    /// `Value::Floating` otherwise.
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

        // Every NetKey is about to become invalid; tunnels must not keep
        // pointing at one, or they'd hold a dangling key.
        for t in self.tunnels.values_mut() {
            t.net = None;
        }

        self.nets.clear();
        self.dirty.clear();
        self.queued.clear();

        // Every input pin now reads Floating, so re-evaluate each component to
        // refresh its out_cache - otherwise a component would keep a stale
        // output that a subsequent relink could read back as live.
        let keys: Vec<CompKey> = self.components.keys().copied().collect();
        for key in keys {
            self.eval_component(key);
        }
    }

    fn net_of(&self, comp: CompKey, pin: PinId) -> Option<NetKey> {
        self.components.get(&comp).and_then(|c| c.net_of(pin))
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
            PinId::Out(i) => self.nets[net].sources.push((comp, i)),
        }
        self.components.get_mut(&comp).unwrap().set_pin_net(pin, net);
        // If attaching a sink pin, immediately evaluate the component since no Net's have changed
        // so nothing will call eval_component automatically.
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
            self.components.get_mut(&comp).unwrap().set_pin_net(PinId::In(i), a);
            self.nets[a].sinks.push((comp, i));
        }

        // Fold B's drivers into A. If both nets were driven, A ends up with
        // more than one source and resolve_net will surface it as Invalid
        // (a driver conflict), rather than silently dropping one.
        for (comp, i) in b_net.sources {
            self.components.get_mut(&comp).unwrap().set_pin_net(PinId::Out(i), a);
            self.nets[a].sources.push((comp, i));
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

    // Finds or creates the net at (comp, pin) and attaches the tunnel to it.
    pub fn link_tunnel(&mut self, tunnel: TunnelKey, comp: CompKey, pin: PinId) -> NetKey {
        let net = self.net_or_create(comp, pin);
        self.attach_tunnel(tunnel, net);
        net
    }

    fn attach_tunnel(&mut self, tunnel: TunnelKey, net: NetKey) {
        let old_net = self.tunnels[&tunnel].net;
        let label = self.tunnels[&tunnel].label.clone();
        self.tunnels.get_mut(&tunnel).unwrap().net = Some(net);
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
        let label = self.tunnels[&tunnel].label.clone();
        if let Some(old) = self.tunnels.get_mut(&tunnel).unwrap().net.take() {
            self.mark_dirty(old);
        }
        self.dirty_label_feed_nets(&label);
    }

    /// Removes a tunnel outright and returns it (the caller moves it into the
    /// undo entry). Dropped from its label group with its net binding cleared,
    /// so it contributes nothing once gone. `None` if the key is already gone.
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

    /// Re-inserts a previously-removed tunnel under its original key - the undo
    /// of `remove_tunnel` - rejoining its label group. Its net binding is
    /// re-established by the caller's relink.
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
                let t = &self.tunnels[&tk];
                (t.role == TunnelRole::Feed).then_some(t.net).flatten()
            })
            .collect();
        for n in nets {
            self.mark_dirty(n);
        }
    }

    // Aggregates a label group's value from its Pull-role tunnels' net
    // values. `strict` controls disagreement handling: lenient (false) takes
    // the last differing value and never errors (safe mid-convergence in
    // resolve_net()); strict (true) returns TunnelConflict, meant to be
    // called once settle()'s dirty-queue loop has fully drained.
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

                // Re-evaluate every sink, sequential ones included: a
                // sequential component's observe() can depend on its live
                // inputs (e.g. an async reset pin), so an input change must
                // refresh its output within the same settle(), without a clock
                // tick. This never advances clocked state - eval_component
                // dispatches Logic::Seq to observe(), not tick() - so it stays
                // a pure function of (latched state, inputs) and can't
                // oscillate any more than combinational logic can.
                let sinks: Vec<_> = self.nets[net].sinks.to_vec();

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

    // True if this net's attached pins declare conflicting expected bit
    // widths (Component::input_width/output_width) - independent of the
    // net's current Value, so a mismatch is flagged even while Floating.
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

    // Recomputes the Net's Value from its source(s). Returns whether the value changed.
    // A net with two or more drivers is a conflict (a short) and resolves to
    // Value::Invalid, the same structural signal used for a width mismatch and
    // handled identically downstream (Invalid stays local, never propagates).
    fn resolve_net(&mut self, net: NetKey) -> bool {
        puffin::profile_function!();
        let old = self.nets[net].value;

        let new = if self.net_width_conflict(net) || self.nets[net].sources.len() > 1 {
            Value::Invalid
        } else {
            match self.nets[net].sources.first() {
                // Net takes value from pins.out_cache, which is updated in eval_component
                Some(&(comp, i)) => self.components[&comp].pins.out_cache[i.0 as usize],
                // A component driver always takes priority; only fall back to a
                // Feed tunnel's group value when this net has no real driver.
                None => self.tunnel_feed_value(net),
            }
        };
        self.nets[net].value = new;
        new != old
    }

    // Evaluates component logic, storing the Value in pins.out_cache and marking the net as dirty
    // if necessary.
    fn eval_component(&mut self, comp: CompKey) {
        puffin::profile_function!();
        // A sequential component may apply asynchronous, level-sensitive
        // effects to its own latched state here (e.g. an async reset that
        // clears the value the instant its pin is held) before its output is
        // read - so an input change takes effect within this same settle(),
        // with no clock tick. This is the one place besides tick_clock() where
        // settle() mutates latched state; it's sound because apply_async is
        // idempotent, so re-evaluating a component any number of times before
        // the queue drains converges to the same state.
        if self.components[&comp].is_stateful() {
            let inputs = self.components[&comp].read_inputs(&self.nets);
            self.components.get_mut(&comp).unwrap().apply_async(&inputs);
        }
        let new_values = self.components[&comp].evaluate(&self.nets);
        self.apply_output_values(comp, new_values);
    }

    // Diffs new_values against a component's current out_cache, updates out_cache in place,
    // and marks any changed output net dirty. Shared by eval_component (combinational path)
    // and tick_clock (sequential path).
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

    // Advances the clock by one tick: snapshot every sequential component's
    // current inputs, compute each one's next state via Component::tick
    // (updating out_cache/persisted state and dirtying changed nets), then
    // settle() to propagate through the combinational circuit. Generic over
    // LogicSeq variants - a new sequential type only needs new match arms in
    // Component::evaluate/tick, not changes here.
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

    // Restores every active sequential component's latched state to its
    // power-on initial value, dirties the changed output nets, and settles to
    // propagate the reset through the combinational circuit. Drives the GUI's
    // clock "Stop" (see gui::app). Like tick_clock, generic over LogicSeq
    // variants - a new sequential type only needs a SeqLogic::reset impl.
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

        // Collected now, acted on after the dirty/queued reset below (so the
        // fresh marks survive): tunnels whose net is about to vanish
        // (detached, sibling nets re-dirtied), sink components that lose
        // their driver (re-evaluated so Floating propagates), and surviving
        // nets whose value may change (re-resolved).
        let mut affected_labels: Vec<String> = Vec::new();
        let mut affected_sinks: Vec<CompKey> = Vec::new();
        let mut retained_nets: Vec<NetKey> = Vec::new();

        // Drop this component's driver entry from each net it feeds. A net
        // with another driver survives; one left driverless is torn down,
        // freeing each sink's input pin slot.
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

        // Detach from nets this component receives; remove it from each net's sinks list
        for net_key in input_nets {
            if let Some(net) = self.nets.get_mut(net_key) {
                net.sinks.retain(|&(ck, _)| ck != key);
            }
        }

        // Clear propagation state; the caller is expected to call settle() after
        self.dirty.clear();
        self.queued.clear();

        // Remove outright and hand the owned Component back to the caller,
        // which moves it into the undo entry; undo re-inserts it under this
        // same key (see insert_component). Its pins are nulled so the moved-out
        // copy holds no dangling NetKeys - the caller rebuilds nets on re-insert.
        // A Reg's latched state (kept apart from pins) rides along untouched, so
        // an undone deletion restores it.
        let mut removed = self.components.remove(&key);
        if let Some(c) = &mut removed {
            c.clear_pins();
        }

        // Re-evaluate sinks that lost their driver so their now-Floating input
        // propagates: eval_component recomputes out_cache and marks any changed
        // output net dirty for the caller's settle() to carry downstream.
        for sink in affected_sinks {
            if self.components.contains_key(&sink) {
                self.eval_component(sink);
            }
        }

        // Re-dirty sibling Feed-tunnel nets now that propagation state has
        // already been reset above.
        for label in affected_labels {
            self.dirty_label_feed_nets(&label);
        }

        // Re-resolve nets that kept another driver (e.g. a two-driver conflict
        // that just became single-driver).
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
        // add_component eagerly evaluates, before any link() or settle().
        assert_eq!(c.components[&i].pins.out_cache[0], Value::new(5, 3));
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
        assert_eq!(c.read_output(o1), Value::ZERO);
        assert_eq!(c.read_output(o2), Value::ZERO);
    }

    #[test]
    fn test_link_second_source_yields_invalid() {
        let mut c = Circuit::new();
        let i1 = c.add_component(Component::input(1, 1));
        let i2 = c.add_component(Component::input(0, 1));
        let g1 = c.add_component(Component::gate(GateOp::Not, 1, 1)); // NOT(1) = 0
        let g2 = c.add_component(Component::gate(GateOp::Not, 1, 1)); // NOT(0) = 1
        let o = c.add_component(Component::output());

        c.link(i1, PinId::output(0), g1, PinId::input(0));
        c.link(i2, PinId::output(0), g2, PinId::input(0));

        c.link(g1, PinId::output(0), o, PinId::input(0));
        // o's input pin already has a net driven by g1; adding g2 gives the net
        // two drivers, which resolve_net reports as a conflict (Invalid) rather
        // than silently picking one.
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
        // Merging two already-driven nets folds both drivers onto the surviving
        // net, giving it two sources. resolve_net reports that as a conflict
        // (Invalid) rather than silently keeping one driver.
        let mut c = Circuit::new();
        let driver1 = c.add_component(Component::input(1, 1));
        let driver2 = c.add_component(Component::input(0, 1));
        let sink1 = c.add_component(Component::output());
        let sink2 = c.add_component(Component::output());

        c.link(driver1, PinId::output(0), sink1, PinId::input(0)); // net1, source = driver1
        c.link(driver2, PinId::output(0), sink2, PinId::input(0)); // net2, source = driver2
                                                                   // Resolve both nets before merging: see
                                                                   // test_link_merge_of_still_dirty_nets_removes_stale_key for what
                                                                   // happens if a merged-away net is still pending in the dirty queue.
        c.settle().unwrap();
        assert_eq!(c.read_output(sink1), Value::ONE);
        assert_eq!(c.read_output(sink2), Value::ZERO);

        // Merge net1 and net2 by linking their already-attached input pins.
        c.link(sink1, PinId::input(0), sink2, PinId::input(0));

        c.settle().unwrap();
        // Both sinks now share one net driven by both driver1 and driver2 -> Invalid.
        assert_eq!(c.read_output(sink1), Value::Invalid);
        assert_eq!(c.read_output(sink2), Value::Invalid);
    }

    #[test]
    fn test_remove_one_of_two_drivers_clears_conflict() {
        // A net with two drivers is Invalid; removing one driver leaves a
        // single-driver net that resolves to the survivor's value (the net is
        // kept, not torn down).
        let mut c = Circuit::new();
        let d1 = c.add_component(Component::input(1, 1));
        let d2 = c.add_component(Component::input(0, 1));
        let o = c.add_component(Component::output());

        c.link(d1, PinId::output(0), o, PinId::input(0));
        c.link(d2, PinId::output(0), o, PinId::input(0)); // two drivers -> Invalid
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::Invalid);

        c.remove_component(d2);
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::ONE); // only d1 (=1) left
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
        assert_eq!(c.read_output(o), Value::ZERO); // NOT(1 AND 1) = 0
    }

    #[test]
    fn test_settle_idempotent_when_no_dirty_nets() {
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), o, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::ONE);
        c.settle().unwrap(); // nothing dirty; must be a no-op
        assert_eq!(c.read_output(o), Value::ONE);
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
    fn test_settle_second_driver_into_loop_is_invalid_not_oscillation() {
        // A NOT-gate ring seeded by a concrete Input used to be forced into a
        // toggling feedback loop by closing it with a *second* driver on n1's
        // input net (last-link-wins injecting a stale concrete value). Now that
        // two drivers resolve to Invalid, that net short-circuits to Invalid
        // and stays there: settle() converges cleanly instead of oscillating.
        // (With strict single-driver nets a purely combinational loop can never
        // bootstrap a concrete value - Floating is absorbing through every gate
        // - so REVISIT_THRESHOLD is now a defensive backstop, not reachable via
        // legitimate wiring.)
        let mut c = Circuit::new();
        let seed = c.add_component(Component::input(0, 1));
        let n1 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let n2 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let n3 = c.add_component(Component::gate(GateOp::Not, 1, 1));

        c.link(seed, PinId::output(0), n1, PinId::input(0));
        c.link(n1, PinId::output(0), n2, PinId::input(0));
        c.link(n2, PinId::output(0), n3, PinId::input(0));
        c.settle().unwrap(); // seeds n1/n2/n3 with concrete alternating values, no loop yet

        // Close the loop: n3 becomes a second driver of n1's input net.
        c.link(n3, PinId::output(0), n1, PinId::input(0));
        assert!(c.settle().is_ok());
        // n1's input net has two drivers (seed + n3) -> Invalid, which NOT reads
        // as Floating, so n1's *output* net (a normal single-driver net) settles
        // to Floating.
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
            assert_eq!(c.read_output(out), Value::new(0, 4)); // settle() never ticks
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
        assert_eq!(c.read_output(out), Value::ONE); // NOT(0) = 1

        c.set_input(data, 1, 1);
        c.settle().unwrap();
        c.tick_clock().unwrap(); // latches 1; trailing settle() propagates through not_g
        assert_eq!(c.read_output(out), Value::ZERO); // NOT(1) = 0
    }

    #[test]
    fn test_reg_async_reset_destroys_state_during_settle_without_a_tick() {
        // The whole point of async-reset support: driving the reset pin clears
        // the register within settle() alone (no tick_clock()), destructively -
        // once cleared it stays zero even after the reset pin is released.
        let mut c = Circuit::new();
        let data = c.add_component(Component::input(9, 4));
        let we = c.add_component(Component::input(1, 1));
        let rst = c.add_component(Component::input(0, 1)); // reset deasserted
        let reg = c.add_component(Component::reg(4));
        let out = c.add_component(Component::output());
        c.link(data, PinId::output(0), reg, PinId::input(RegConf::DATA_PIN as u8));
        c.link(we, PinId::output(0), reg, PinId::input(RegConf::WRITE_EN_PIN as u8));
        c.link(rst, PinId::output(0), reg, PinId::input(RegConf::RESET_PIN as u8));
        c.link(reg, PinId::output(0), out, PinId::input(0));

        // Latch 9 into the register.
        c.settle().unwrap();
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out), Value::new(9, 4));

        // Assert reset and merely settle() - no tick. State clears immediately.
        c.set_input(rst, 1, 1);
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::new(0, 4));

        // Release reset, still no tick: the clear was destructive, so it stays
        // 0 - the old 9 is gone.
        c.set_input(rst, 0, 1);
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::new(0, 4));
    }

    #[test]
    fn test_t_flip_flop_async_reset_gui_flow() {
        // The exact interactive flow: a T input and the async "0" pin wired to
        // a T flip-flop. Toggle it high over a few clocks, pulse "0" to clear
        // it mid-run (no tick), then resume ticking from zero.
        use crate::sim::component::TFlipFlopConf as T;
        let mut c = Circuit::new();
        let toggle = c.add_component(Component::input(1, 1)); // T held high
        let rst = c.add_component(Component::input(0, 1)); // "0" pin, deasserted
        let ff = c.add_component(Component::t_flip_flop());
        let out = c.add_component(Component::output());
        c.link(toggle, PinId::output(0), ff, PinId::input(T::TOGGLE_PIN as u8));
        c.link(rst, PinId::output(0), ff, PinId::input(T::RESET_PIN as u8));
        c.link(ff, PinId::output(0), out, PinId::input(0));
        c.settle().unwrap();

        // Three toggles: 0 -> 1 -> 0 -> 1, ending high.
        c.tick_clock().unwrap();
        c.tick_clock().unwrap();
        c.tick_clock().unwrap();
        assert_eq!(c.read_output(out), Value::ONE);

        // Toggle "0" on: clears to 0 at once, no tick.
        c.set_input(rst, 1, 1);
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ZERO);

        // Toggle "0" back off: stays 0 (destroyed, not restored).
        c.set_input(rst, 0, 1);
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ZERO);

        // Resume clocking: toggles from 0 -> 1 as normal.
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

        // A counter counting up, with its own latched value.
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

        // Reset returns both to their power-on initial value and propagates it.
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

        c.remove_component(g1); // g1 only reads from a's net
        c.settle().unwrap();
        assert_eq!(c.read_output(o2), Value::ZERO);
    }

    #[test]
    fn test_remove_component_refreshes_downstream_sinks() {
        // Removing a driver re-evaluates the sinks that lose it and re-dirties
        // their output nets, so a following settle() refreshes values more than
        // one hop downstream rather than leaving stale out_cache behind.
        let mut c = Circuit::new();
        let a = c.add_component(Component::input(1, 1));
        let g1 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let g2 = c.add_component(Component::gate(GateOp::Not, 1, 1));
        let o = c.add_component(Component::output());
        c.link(a, PinId::output(0), g1, PinId::input(0));
        c.link(g1, PinId::output(0), g2, PinId::input(0));
        c.link(g2, PinId::output(0), o, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::ONE); // NOT(NOT(1)) = 1

        c.remove_component(g1);
        c.settle().unwrap();

        // g2's input pin was nulled -> it now reads Floating, so g2 =
        // NOT(Floating) = Floating, and that refresh propagates through to o.
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
        assert_eq!(c.read_output(o), Value::ONE); // unaffected
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
        c.link_tunnel(pull, sink2, PinId::input(0)); // creates + attaches to sink2's net

        // sink1's net (driven by driver1) and sink2's net (undriven, only the
        // tunnel) both already exist; linking their already-attached input pins
        // together forces a merge() with a single surviving driver.
        c.link(sink1, PinId::input(0), sink2, PinId::input(0));
        c.settle().unwrap();

        // The tunnel must have followed the merge (repointed from the
        // removed net to the surviving one), not been left dangling.
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

        // Rename the Pull tunnel from "OLD" to "NEW".
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

        // driver's net has a concrete Fixed value, but two sinks disagree on
        // expected width (4 vs 8), so it resolves to Invalid rather than
        // whatever driver's out_cache happens to hold.
        assert_eq!(c.read_output(out), Value::Invalid);
    }

    #[test]
    fn test_width_conflict_detected_even_with_no_driver() {
        let mut c = Circuit::new();
        // No component drives this net at all (both endpoints below are input
        // pins) - it should still be flagged Invalid purely from the two
        // sinks' conflicting declared widths, not from any concrete Value.
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
        // Output declares no expected width (input_width returns None), so a
        // single width-declaring participant (g's output) has nothing to
        // conflict with - this must stay an ordinary Floating, not Invalid.
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

        assert_eq!(c.read_output(probe), Value::Invalid); // the mismatched net itself
                                                          // One hop downstream: g4 read an Invalid input and produced Floating
                                                          // (per Value::Not), and that net's own widths agree - so it resolves
                                                          // as ordinary Floating rather than carrying Invalid any further.
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

        // Reads the pre-loaded word at address 2.
        assert_eq!(c.read_output(out), Value::new(0x5A, 8));

        // An in-place content write at the addressed cell propagates on settle.
        c.write_rom(rom_key, 2, 0xFF);
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::new(0xFF, 8));

        // Writing a *different* address leaves the current output untouched.
        c.write_rom(rom_key, 5, 0x11);
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::new(0xFF, 8));
    }

    // ── Subcircuits (Logic::Sub) ──────────────────────────────────────────────

    // A fresh subcircuit component wrapping AND(a, b): two 1-bit inputs, one
    // 1-bit output. Built anew each call (Circuit isn't Clone), so two calls
    // yield independent instances.
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

    // A fresh subcircuit component wrapping a 4-bit register: inputs [D, WE],
    // output [Q].
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
        assert_eq!(c.read_output(out), Value::ONE); // 1 AND 1

        // An input change settles through the boundary with no clock tick.
        c.set_input(x, 0, 1);
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ZERO); // 0 AND 1
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

        // An outer clock tick drives the inner register one step.
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
        // A subcircuit whose inner circuit itself contains a subcircuit:
        // outer -> mid(sub) -> and(sub).
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
