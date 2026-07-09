use crate::sim::circuit::{Circuit, SettleError, TunnelKey, TunnelRole};
use crate::sim::component::{
    CompKey, Component, ComponentSpec, InIdx, Input, Logic, LogicComb, OutIdx, PinId, SeqState,
};
use crate::sim::net::NetKey;

// A single structural mutation to a Circuit, expressed as data rather than a
// direct method call. This is the seam a future undo/redo stack will hook
// into (recording/inverting Commands); this step only builds the seam - no
// snapshotting, inverse-command capture, or Command::Batch yet.
#[derive(Debug)]
pub enum Command {
    AddComponent(Component),
    SetInput { comp: CompKey, bits: u32, width: u8 },
    ClearNets,
    Link {
        a: CompKey,
        a_pin: PinId,
        b: CompKey,
        b_pin: PinId,
    },
    AddTunnel { label: String, role: TunnelRole },
    LinkTunnel {
        tunnel: TunnelKey,
        comp: CompKey,
        pin: PinId,
    },
    DetachTunnel(TunnelKey),
    RemoveTunnel(TunnelKey),
    RenameTunnel { tunnel: TunnelKey, new_label: String },
    TickClock,
    RemoveComponent(CompKey),
}

// The value produced by Circuit::apply(). Which variant comes back is fully
// determined by which Command variant went in - see each unwrap_* accessor
// for its matching Command variant(s).
#[derive(Debug)]
pub enum CommandOutput {
    Comp(CompKey),
    Tunnel(TunnelKey),
    Net(NetKey),
    Settle(Result<(), SettleError>),
    None,
}

impl CommandOutput {
    /// Panics unless this came from `Command::AddComponent`.
    pub fn unwrap_comp(self) -> CompKey {
        match self {
            Self::Comp(k) => k,
            other => panic!("expected CommandOutput::Comp, got {other:?}"),
        }
    }

    /// Panics unless this came from `Command::AddTunnel`.
    pub fn unwrap_tunnel(self) -> TunnelKey {
        match self {
            Self::Tunnel(k) => k,
            other => panic!("expected CommandOutput::Tunnel, got {other:?}"),
        }
    }

    /// Panics unless this came from `Command::Link` or `Command::LinkTunnel`.
    pub fn unwrap_net(self) -> NetKey {
        match self {
            Self::Net(k) => k,
            other => panic!("expected CommandOutput::Net, got {other:?}"),
        }
    }

    /// Panics unless this came from `Command::TickClock`.
    pub fn unwrap_settle(self) -> Result<(), SettleError> {
        match self {
            Self::Settle(r) => r,
            other => panic!("expected CommandOutput::Settle, got {other:?}"),
        }
    }
}

// Enough pre-state to reverse a single applied Command later. Captured
// alongside CommandOutput by Circuit::apply_tracked() - see that method for
// how each variant is produced. This step only captures UndoActions; nothing
// yet consumes one to actually replay it as Circuit mutations (that's a
// later step). A few variants (Link's Split case, RestoreTunnelNet,
// RestoreTunnel, RestoreSeqState) will need new non-public Circuit/Component
// primitives to replay - documented on those variants below - since nothing
// today can split a merged net, detach a single pin, set a tunnel's net to
// an arbitrary already-known NetKey, or restore persisted sequential state
// directly.
#[derive(Debug)]
pub enum UndoAction {
    /// Nothing to undo (the forward Command was itself a no-op, e.g.
    /// SetInput on a non-Input component, or a rename to an identical label).
    Noop,
    /// Several UndoActions that together undo one logical GUI-level edit;
    /// undoing must replay them in reverse order. Never produced by
    /// `apply_tracked` itself - assembled by whatever groups a batch of
    /// `apply_tracked` calls together (see gui::history::History).
    Batch(Vec<UndoAction>),

    /// Undoes `Command::AddComponent`: just remove the component that was
    /// added (nothing else was wired to it yet).
    RemoveComponent(CompKey),
    /// Undoes `Command::SetInput`.
    SetInput {
        comp: CompKey,
        old_bits: u32,
        old_width: u8,
    },
    /// Undoes `Command::ClearNets`: replay recipe to re-establish every net
    /// that existed before clearing, in the same "anchor + others" shape
    /// `gui::app::rebuild_circuit` already replays wiring in - pick any one
    /// (CompKey,PinId) per net as an anchor, `link()` every other pin on
    /// that net to it, then `link_tunnel()` every tunnel that was on it.
    RelinkAll {
        links: Vec<(CompKey, PinId, CompKey, PinId)>,
        tunnel_links: Vec<(TunnelKey, CompKey, PinId)>,
    },
    /// Undoes `Command::Link`.
    Link(LinkUndo),
    /// Undoes `Command::AddTunnel`: just remove the tunnel that was added
    /// (nothing else was attached to it yet).
    RemoveTunnel(TunnelKey),
    /// Undoes `Command::LinkTunnel` / `Command::DetachTunnel`: restore the
    /// tunnel's `.net` to what it was before. Replaying this needs a new
    /// primitive to set a tunnel's net to an arbitrary already-known
    /// `NetKey` (or `None`) directly - `link_tunnel` always finds-or-creates
    /// from a `(comp,pin)` pair, it can't target a specific existing net.
    RestoreTunnelNet {
        tunnel: TunnelKey,
        net: Option<NetKey>,
    },
    /// Undoes `Command::RemoveTunnel`: re-add a tunnel with the same
    /// label/role and restore its net (via the same new primitive
    /// `RestoreTunnelNet` needs). Note the re-added tunnel gets a *new*
    /// `TunnelKey` - any later history entry referencing the removed
    /// tunnel's old key would need remapping, an already-acknowledged
    /// deferred problem (see the Command/apply() step's own notes).
    RestoreTunnel {
        label: String,
        role: TunnelRole,
        net: Option<NetKey>,
    },
    /// Undoes `Command::RenameTunnel`.
    RenameTunnel { tunnel: TunnelKey, old_label: String },
    /// Undoes `Command::TickClock`: restore every sequential component's
    /// pre-tick persisted state directly. Replaying this needs a new
    /// `LogicSeq::restore` (the write-side counterpart of `snapshot`) to set
    /// state without going through `tick()`'s write-enable gating.
    RestoreSeqState { snapshots: Vec<(CompKey, SeqState)> },
    /// Undoes `Command::RemoveComponent`: recreate an equivalent component
    /// from `spec` (getting a *new* CompKey - same key-remapping caveat as
    /// `RestoreTunnel`), then replay `links`/`tunnel_links` as `link()`/
    /// `link_tunnel()` calls against the new key, substituted in place of
    /// each pin's own PinId. No new primitive needed to replay this one -
    /// it composes entirely from existing public `link`/`link_tunnel`.
    RestoreComponent {
        spec: ComponentSpec,
        links: Vec<(PinId, CompKey, PinId)>,
        tunnel_links: Vec<(PinId, TunnelKey)>,
    },
}

/// The four outcomes `Command::Link` can produce, and what undoes each -
/// see `UndoAction::Link`.
#[derive(Debug)]
pub enum LinkUndo {
    /// The two pins were already on the same net (link is idempotent).
    Noop,
    /// A brand-new net was created for both pins (neither had one before):
    /// undo by detaching both. Replaying this needs a new primitive to
    /// detach a single pin without removing the whole component.
    DetachBoth {
        a: CompKey,
        a_pin: PinId,
        b: CompKey,
        b_pin: PinId,
    },
    /// One pin already had a net, the other was newly attached to it: undo
    /// by detaching just the newly-attached pin (same new primitive as
    /// `DetachBoth`).
    DetachOne { comp: CompKey, pin: PinId },
    /// Both pins already had different nets, so `link()`'s merge() folded
    /// `removed_net` into `surviving_net`. Undo by splitting `removed_net`
    /// back out with its original sources/sinks/tunnels. Replaying this
    /// needs a new primitive that reverses merge() - nothing today can
    /// split a net.
    Split {
        surviving_net: NetKey,
        removed_net: NetKey,
        removed_sources: Vec<(CompKey, OutIdx)>,
        removed_sinks: Vec<(CompKey, InIdx)>,
        removed_net_tunnels: Vec<TunnelKey>,
    },
}

impl Circuit {
    /// Applies a single structural `Command` to this circuit, delegating to
    /// the same methods a caller would otherwise call directly. Does NOT
    /// call settle() - callers remain responsible for that afterward,
    /// exactly as before this method existed (`TickClock` excepted, which
    /// still settles internally).
    pub fn apply(&mut self, command: Command) -> CommandOutput {
        self.apply_tracked(command).0
    }

    /// Like `apply`, but also captures enough pre-state to undo the command
    /// later (see `UndoAction`). Pre-state is always read *before* the
    /// delegating call below, since several of these mutators (`link`'s
    /// merge branch, `remove_component`) remove the very state a naive
    /// after-the-fact read would need.
    pub fn apply_tracked(&mut self, command: Command) -> (CommandOutput, UndoAction) {
        match command {
            Command::AddComponent(comp) => {
                let key = self.add_component(comp);
                (CommandOutput::Comp(key), UndoAction::RemoveComponent(key))
            }
            Command::SetInput { comp, bits, width } => {
                let old = match &self.components[comp].logic {
                    Logic::Comb(LogicComb::Input(Input {
                        bits: b,
                        width: w,
                    })) => Some((*b, *w)),
                    _ => None,
                };
                self.set_input(comp, bits, width);
                let undo = match old {
                    Some((old_bits, old_width)) => UndoAction::SetInput {
                        comp,
                        old_bits,
                        old_width,
                    },
                    None => UndoAction::Noop,
                };
                (CommandOutput::None, undo)
            }
            Command::ClearNets => {
                let mut links: Vec<(CompKey, PinId, CompKey, PinId)> = Vec::new();
                let mut tunnel_links: Vec<(TunnelKey, CompKey, PinId)> = Vec::new();
                for (net_key, net) in self.nets.iter() {
                    let participants: Vec<(CompKey, PinId)> = net
                        .sources
                        .iter()
                        .map(|&(c, i)| (c, PinId::Out(i)))
                        .chain(net.sinks.iter().map(|&(c, i)| (c, PinId::In(i))))
                        .collect();
                    let Some(&(anchor_comp, anchor_pin)) = participants.first() else {
                        continue;
                    };
                    for &(c, p) in &participants[1..] {
                        links.push((anchor_comp, anchor_pin, c, p));
                    }
                    for (tk, t) in self.tunnels.iter() {
                        if t.net == Some(net_key) {
                            tunnel_links.push((tk, anchor_comp, anchor_pin));
                        }
                    }
                }
                self.clear_nets();
                (
                    CommandOutput::None,
                    UndoAction::RelinkAll {
                        links,
                        tunnel_links,
                    },
                )
            }
            Command::Link { a, a_pin, b, b_pin } => {
                let net_a = self.components[a].net_of(a_pin);
                let net_b = self.components[b].net_of(b_pin);
                // If both sides already have different nets, link() is about
                // to merge() them and remove net_b entirely - snapshot it
                // now, or a post-call read would hit a stale/absent key.
                let split_capture = match (net_a, net_b) {
                    (Some(na), Some(nb)) if na != nb => Some((
                        na,
                        nb,
                        self.nets[nb].sources.clone(),
                        self.nets[nb].sinks.clone(),
                        self.tunnels
                            .iter()
                            .filter(|(_, t)| t.net == Some(nb))
                            .map(|(k, _)| k)
                            .collect::<Vec<_>>(),
                    )),
                    _ => None,
                };
                let net = self.link(a, a_pin, b, b_pin);
                let link_undo = match (net_a, net_b) {
                    (None, None) => LinkUndo::DetachBoth { a, a_pin, b, b_pin },
                    (Some(_), None) => LinkUndo::DetachOne { comp: b, pin: b_pin },
                    (None, Some(_)) => LinkUndo::DetachOne { comp: a, pin: a_pin },
                    (Some(na), Some(nb)) if na == nb => LinkUndo::Noop,
                    (Some(_), Some(_)) => {
                        let (
                            surviving_net,
                            removed_net,
                            removed_sources,
                            removed_sinks,
                            removed_net_tunnels,
                        ) = split_capture.unwrap();
                        LinkUndo::Split {
                            surviving_net,
                            removed_net,
                            removed_sources,
                            removed_sinks,
                            removed_net_tunnels,
                        }
                    }
                };
                (CommandOutput::Net(net), UndoAction::Link(link_undo))
            }
            Command::AddTunnel { label, role } => {
                let key = self.add_tunnel(label, role);
                (CommandOutput::Tunnel(key), UndoAction::RemoveTunnel(key))
            }
            Command::LinkTunnel { tunnel, comp, pin } => {
                let old_net = self.tunnels.get(tunnel).and_then(|t| t.net);
                let net = self.link_tunnel(tunnel, comp, pin);
                (
                    CommandOutput::Net(net),
                    UndoAction::RestoreTunnelNet {
                        tunnel,
                        net: old_net,
                    },
                )
            }
            Command::DetachTunnel(tunnel) => {
                let old_net = self.tunnels.get(tunnel).and_then(|t| t.net);
                self.detach_tunnel(tunnel);
                (
                    CommandOutput::None,
                    UndoAction::RestoreTunnelNet {
                        tunnel,
                        net: old_net,
                    },
                )
            }
            Command::RemoveTunnel(tunnel) => {
                let snapshot = self.tunnels.get(tunnel).cloned();
                self.remove_tunnel(tunnel);
                let undo = match snapshot {
                    Some(t) => UndoAction::RestoreTunnel {
                        label: t.label,
                        role: t.role,
                        net: t.net,
                    },
                    None => UndoAction::Noop,
                };
                (CommandOutput::None, undo)
            }
            Command::RenameTunnel { tunnel, new_label } => {
                let old_label = self.tunnels.get(tunnel).map(|t| t.label.clone());
                self.rename_tunnel(tunnel, new_label.clone());
                let undo = match old_label {
                    Some(old) if old != new_label => {
                        UndoAction::RenameTunnel { tunnel, old_label: old }
                    }
                    _ => UndoAction::Noop,
                };
                (CommandOutput::None, undo)
            }
            Command::TickClock => {
                let snapshots: Vec<(CompKey, SeqState)> = self
                    .components
                    .iter()
                    .filter_map(|(k, c)| match &c.logic {
                        Logic::Seq(seq) => Some((k, seq.snapshot())),
                        Logic::Comb(_) => None,
                    })
                    .collect();
                let result = self.tick_clock();
                (
                    CommandOutput::Settle(result),
                    UndoAction::RestoreSeqState { snapshots },
                )
            }
            Command::RemoveComponent(key) => {
                let spec = self.components[key].spec();

                let output_nets: Vec<(PinId, NetKey)> = self.components[key]
                    .pins
                    .outputs
                    .iter()
                    .enumerate()
                    .filter_map(|(i, n)| n.map(|net| (PinId::output(i as u8), net)))
                    .collect();
                let input_nets: Vec<(PinId, NetKey)> = self.components[key]
                    .pins
                    .inputs
                    .iter()
                    .enumerate()
                    .filter_map(|(i, n)| n.map(|net| (PinId::input(i as u8), net)))
                    .collect();

                let mut links: Vec<(PinId, CompKey, PinId)> = Vec::new();
                for &(pin, net) in output_nets.iter().chain(input_nets.iter()) {
                    let n = &self.nets[net];
                    // Exclude by exact (CompKey,PinId) identity, not by
                    // CompKey alone - a self-loop (this component wired to
                    // its own other pin) has the same CompKey but a
                    // different PinId, and must be kept.
                    for &(c, i) in &n.sources {
                        let p = PinId::Out(i);
                        if (c, p) != (key, pin) {
                            links.push((pin, c, p));
                        }
                    }
                    for &(c, i) in &n.sinks {
                        let p = PinId::In(i);
                        if (c, p) != (key, pin) {
                            links.push((pin, c, p));
                        }
                    }
                }

                // Tunnels are only detached by remove_component for output
                // nets this component solely drives (the "net torn down"
                // branch) - never for retained nets or input nets.
                let mut tunnel_links: Vec<(PinId, TunnelKey)> = Vec::new();
                for &(pin, net) in &output_nets {
                    if self.nets[net].sources.len() == 1 {
                        for (tk, t) in self.tunnels.iter() {
                            if t.net == Some(net) {
                                tunnel_links.push((pin, tk));
                            }
                        }
                    }
                }

                self.remove_component(key);
                (
                    CommandOutput::None,
                    UndoAction::RestoreComponent {
                        spec,
                        links,
                        tunnel_links,
                    },
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::component::GateOp;
    use crate::sim::value::Value;

    #[test]
    fn test_apply_add_component_returns_comp_key_and_registers() {
        let mut c = Circuit::new();
        let key = c
            .apply(Command::AddComponent(Component::input(5, 3)))
            .unwrap_comp();
        // add_component eagerly evaluates, before any link()/settle().
        assert_eq!(c.components[key].pins.out_cache[0], Value::new(5, 3));
    }

    #[test]
    fn test_apply_set_input_updates_value() {
        let mut c = Circuit::new();
        let i = c.apply(Command::AddComponent(Component::input(0, 4))).unwrap_comp();
        let o = c.apply(Command::AddComponent(Component::output())).unwrap_comp();
        c.apply(Command::Link {
            a: i,
            a_pin: PinId::output(0),
            b: o,
            b_pin: PinId::input(0),
        });
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(0, 4));

        c.apply(Command::SetInput { comp: i, bits: 7, width: 4 });
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(7, 4));
    }

    #[test]
    fn test_apply_link_returns_net_key_and_wires_components() {
        let mut c = Circuit::new();
        let i = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let o = c.apply(Command::AddComponent(Component::output())).unwrap_comp();
        let net = c
            .apply(Command::Link {
                a: i,
                a_pin: PinId::output(0),
                b: o,
                b_pin: PinId::input(0),
            })
            .unwrap_net();
        assert!(c.nets.contains_key(net));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::ONE);
    }

    #[test]
    fn test_apply_link_idempotent_returns_same_net() {
        let mut c = Circuit::new();
        let a = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let b = c.apply(Command::AddComponent(Component::output())).unwrap_comp();
        let net1 = c
            .apply(Command::Link { a, a_pin: PinId::output(0), b, b_pin: PinId::input(0) })
            .unwrap_net();
        let net2 = c
            .apply(Command::Link { a, a_pin: PinId::output(0), b, b_pin: PinId::input(0) })
            .unwrap_net();
        assert_eq!(net1, net2);
    }

    #[test]
    fn test_apply_add_tunnel_and_link_tunnel_return_correct_keys() {
        let mut c = Circuit::new();
        let driver = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let tunnel = c
            .apply(Command::AddTunnel { label: "CLK".to_string(), role: TunnelRole::Pull })
            .unwrap_tunnel();
        assert_eq!(c.tunnel_label(tunnel), Some("CLK"));

        let net = c
            .apply(Command::LinkTunnel { tunnel, comp: driver, pin: PinId::output(0) })
            .unwrap_net();
        assert!(c.nets.contains_key(net));
    }

    #[test]
    fn test_apply_detach_tunnel_clears_net() {
        let mut c = Circuit::new();
        let driver = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let pull = c
            .apply(Command::AddTunnel { label: "Y".to_string(), role: TunnelRole::Pull })
            .unwrap_tunnel();
        c.apply(Command::LinkTunnel { tunnel: pull, comp: driver, pin: PinId::output(0) });
        c.settle().unwrap();

        c.apply(Command::DetachTunnel(pull));
        // Must not panic, and a subsequent settle() must succeed with the
        // group now empty of Pull contributions.
        c.settle().unwrap();
        let feed = c
            .apply(Command::AddTunnel { label: "Y".to_string(), role: TunnelRole::Feed })
            .unwrap_tunnel();
        let out = c.apply(Command::AddComponent(Component::output())).unwrap_comp();
        c.apply(Command::LinkTunnel { tunnel: feed, comp: out, pin: PinId::input(0) });
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::Floating);
    }

    #[test]
    fn test_apply_rename_tunnel_updates_label() {
        let mut c = Circuit::new();
        let tunnel = c
            .apply(Command::AddTunnel { label: "OLD".to_string(), role: TunnelRole::Pull })
            .unwrap_tunnel();
        c.apply(Command::RenameTunnel { tunnel, new_label: "NEW".to_string() });
        assert_eq!(c.tunnel_label(tunnel), Some("NEW"));
    }

    #[test]
    fn test_apply_remove_tunnel() {
        let mut c = Circuit::new();
        let tunnel = c
            .apply(Command::AddTunnel { label: "Z".to_string(), role: TunnelRole::Pull })
            .unwrap_tunnel();
        c.apply(Command::RemoveTunnel(tunnel));
        assert_eq!(c.tunnel_label(tunnel), None);
    }

    #[test]
    fn test_apply_remove_component_tears_down_conflict() {
        let mut c = Circuit::new();
        let d1 = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let d2 = c.apply(Command::AddComponent(Component::input(0, 1))).unwrap_comp();
        let o = c.apply(Command::AddComponent(Component::output())).unwrap_comp();
        c.apply(Command::Link { a: d1, a_pin: PinId::output(0), b: o, b_pin: PinId::input(0) });
        c.apply(Command::Link { a: d2, a_pin: PinId::output(0), b: o, b_pin: PinId::input(0) });
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::Invalid);

        c.apply(Command::RemoveComponent(d2));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::ONE);
    }

    #[test]
    fn test_apply_tick_clock_returns_settle_result_and_latches() {
        let mut c = Circuit::new();
        let data = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let we = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let reg = c.apply(Command::AddComponent(Component::reg(1))).unwrap_comp();
        let out = c.apply(Command::AddComponent(Component::output())).unwrap_comp();
        c.apply(Command::Link { a: data, a_pin: PinId::output(0), b: reg, b_pin: PinId::input(0) });
        c.apply(Command::Link { a: we, a_pin: PinId::output(0), b: reg, b_pin: PinId::input(1) });
        c.apply(Command::Link { a: reg, a_pin: PinId::output(0), b: out, b_pin: PinId::input(0) });
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ZERO);

        let result = c.apply(Command::TickClock).unwrap_settle();
        assert_eq!(result, Ok(()));
        assert_eq!(c.read_output(out), Value::ONE);
    }

    #[test]
    fn test_apply_clear_nets_removes_all_nets() {
        let mut c = Circuit::new();
        let a = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let g = c
            .apply(Command::AddComponent(Component::gate(GateOp::Not, 1, 1)))
            .unwrap_comp();
        c.apply(Command::Link { a, a_pin: PinId::output(0), b: g, b_pin: PinId::input(0) });
        c.settle().unwrap();
        assert!(!c.nets.is_empty());

        c.apply(Command::ClearNets);
        assert!(c.nets.is_empty());
        assert!(c.components.contains_key(a));
        assert!(c.components.contains_key(g));
    }

    #[test]
    #[should_panic(expected = "expected CommandOutput::Comp")]
    fn test_command_output_unwrap_wrong_variant_panics() {
        let mut c = Circuit::new();
        let key = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        c.apply(Command::RemoveComponent(key)).unwrap_comp();
    }

    // ---- apply_tracked() capture correctness ----

    #[test]
    fn test_apply_tracked_add_component_undo_is_remove_component() {
        let mut c = Circuit::new();
        let (output, undo) = c.apply_tracked(Command::AddComponent(Component::input(1, 1)));
        let key = output.unwrap_comp();
        assert!(matches!(undo, UndoAction::RemoveComponent(k) if k == key));
    }

    #[test]
    fn test_apply_tracked_set_input_captures_old_value() {
        let mut c = Circuit::new();
        let i = c.apply(Command::AddComponent(Component::input(3, 4))).unwrap_comp();

        let (_output, undo) = c.apply_tracked(Command::SetInput { comp: i, bits: 9, width: 4 });
        match undo {
            UndoAction::SetInput { comp, old_bits, old_width } => {
                assert_eq!(comp, i);
                assert_eq!(old_bits, 3);
                assert_eq!(old_width, 4);
            }
            other => panic!("expected SetInput, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_tracked_set_input_on_non_input_is_noop() {
        let mut c = Circuit::new();
        let g = c
            .apply(Command::AddComponent(Component::gate(GateOp::Not, 1, 1)))
            .unwrap_comp();

        let (_output, undo) = c.apply_tracked(Command::SetInput { comp: g, bits: 1, width: 1 });
        assert!(matches!(undo, UndoAction::Noop));
    }

    #[test]
    fn test_apply_tracked_clear_nets_captures_relink_recipe() {
        let mut c = Circuit::new();
        let a = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let b = c.apply(Command::AddComponent(Component::output())).unwrap_comp();
        c.apply(Command::Link { a, a_pin: PinId::output(0), b, b_pin: PinId::input(0) });
        let tunnel = c
            .apply(Command::AddTunnel { label: "X".to_string(), role: TunnelRole::Pull })
            .unwrap_tunnel();
        c.apply(Command::LinkTunnel { tunnel, comp: a, pin: PinId::output(0) });

        let (_output, undo) = c.apply_tracked(Command::ClearNets);
        match undo {
            UndoAction::RelinkAll { links, tunnel_links } => {
                // The net has one source (a,out0) and one sink (b,in0);
                // sources are always walked before sinks, so the anchor is
                // deterministically (a, out0).
                assert_eq!(links, vec![(a, PinId::output(0), b, PinId::input(0))]);
                assert_eq!(tunnel_links, vec![(tunnel, a, PinId::output(0))]);
            }
            other => panic!("expected RelinkAll, got {other:?}"),
        }
        assert!(c.nets.is_empty());
    }

    #[test]
    fn test_apply_tracked_link_new_net_captures_detach_both() {
        let mut c = Circuit::new();
        let a = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let b = c.apply(Command::AddComponent(Component::output())).unwrap_comp();

        let (_output, undo) = c.apply_tracked(Command::Link {
            a,
            a_pin: PinId::output(0),
            b,
            b_pin: PinId::input(0),
        });
        match undo {
            UndoAction::Link(LinkUndo::DetachBoth {
                a: ca,
                a_pin,
                b: cb,
                b_pin,
            }) => {
                assert_eq!((ca, a_pin, cb, b_pin), (a, PinId::output(0), b, PinId::input(0)));
            }
            other => panic!("expected DetachBoth, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_tracked_link_attach_to_existing_captures_detach_one() {
        let mut c = Circuit::new();
        let a = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let b = c.apply(Command::AddComponent(Component::output())).unwrap_comp();
        let b2 = c.apply(Command::AddComponent(Component::output())).unwrap_comp();
        c.apply(Command::Link { a, a_pin: PinId::output(0), b, b_pin: PinId::input(0) });

        let (_output, undo) = c.apply_tracked(Command::Link {
            a,
            a_pin: PinId::output(0),
            b: b2,
            b_pin: PinId::input(0),
        });
        match undo {
            UndoAction::Link(LinkUndo::DetachOne { comp, pin }) => {
                assert_eq!((comp, pin), (b2, PinId::input(0)));
            }
            other => panic!("expected DetachOne, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_tracked_link_idempotent_is_noop() {
        let mut c = Circuit::new();
        let a = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let b = c.apply(Command::AddComponent(Component::output())).unwrap_comp();
        c.apply(Command::Link { a, a_pin: PinId::output(0), b, b_pin: PinId::input(0) });

        let (_output, undo) = c.apply_tracked(Command::Link {
            a,
            a_pin: PinId::output(0),
            b,
            b_pin: PinId::input(0),
        });
        assert!(matches!(undo, UndoAction::Link(LinkUndo::Noop)));
    }

    #[test]
    fn test_apply_tracked_link_split_captures_losing_net_state() {
        let mut c = Circuit::new();
        let driver1 = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let driver2 = c.apply(Command::AddComponent(Component::input(0, 1))).unwrap_comp();
        let sink1 = c.apply(Command::AddComponent(Component::output())).unwrap_comp();
        let sink2 = c.apply(Command::AddComponent(Component::output())).unwrap_comp();
        c.apply(Command::Link {
            a: driver1,
            a_pin: PinId::output(0),
            b: sink1,
            b_pin: PinId::input(0),
        });
        c.apply(Command::Link {
            a: driver2,
            a_pin: PinId::output(0),
            b: sink2,
            b_pin: PinId::input(0),
        });
        c.settle().unwrap();

        let net_a = c.components[sink1].net_of(PinId::input(0)).unwrap();
        let net_b = c.components[sink2].net_of(PinId::input(0)).unwrap();

        let (_output, undo) = c.apply_tracked(Command::Link {
            a: sink1,
            a_pin: PinId::input(0),
            b: sink2,
            b_pin: PinId::input(0),
        });
        match undo {
            UndoAction::Link(LinkUndo::Split {
                surviving_net,
                removed_net,
                removed_sources,
                removed_sinks,
                removed_net_tunnels,
            }) => {
                assert_eq!(surviving_net, net_a);
                assert_eq!(removed_net, net_b);
                assert_eq!(removed_sources, vec![(driver2, OutIdx(0))]);
                assert_eq!(removed_sinks, vec![(sink2, InIdx(0))]);
                assert!(removed_net_tunnels.is_empty());
            }
            other => panic!("expected Split, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_tracked_link_tunnel_captures_prior_net() {
        let mut c = Circuit::new();
        let driver = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let tunnel = c
            .apply(Command::AddTunnel { label: "A".to_string(), role: TunnelRole::Pull })
            .unwrap_tunnel();

        let (_output, undo) =
            c.apply_tracked(Command::LinkTunnel { tunnel, comp: driver, pin: PinId::output(0) });
        match undo {
            UndoAction::RestoreTunnelNet { tunnel: t, net } => {
                assert_eq!(t, tunnel);
                assert_eq!(net, None); // never attached before
            }
            other => panic!("expected RestoreTunnelNet, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_tracked_detach_tunnel_captures_prior_net() {
        let mut c = Circuit::new();
        let driver = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let tunnel = c
            .apply(Command::AddTunnel { label: "B".to_string(), role: TunnelRole::Pull })
            .unwrap_tunnel();
        c.apply(Command::LinkTunnel { tunnel, comp: driver, pin: PinId::output(0) });
        let prior_net = c.tunnels[tunnel].net;
        assert!(prior_net.is_some());

        let (_output, undo) = c.apply_tracked(Command::DetachTunnel(tunnel));
        match undo {
            UndoAction::RestoreTunnelNet { tunnel: t, net } => {
                assert_eq!(t, tunnel);
                assert_eq!(net, prior_net);
            }
            other => panic!("expected RestoreTunnelNet, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_tracked_remove_tunnel_captures_full_snapshot() {
        let mut c = Circuit::new();
        let driver = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let tunnel = c
            .apply(Command::AddTunnel { label: "C".to_string(), role: TunnelRole::Feed })
            .unwrap_tunnel();
        c.apply(Command::LinkTunnel { tunnel, comp: driver, pin: PinId::output(0) });
        let expected_net = c.tunnels[tunnel].net;

        let (_output, undo) = c.apply_tracked(Command::RemoveTunnel(tunnel));
        match undo {
            UndoAction::RestoreTunnel { label, role, net } => {
                assert_eq!(label, "C");
                assert_eq!(role, TunnelRole::Feed);
                assert_eq!(net, expected_net);
            }
            other => panic!("expected RestoreTunnel, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_tracked_rename_tunnel_captures_old_label() {
        let mut c = Circuit::new();
        let tunnel = c
            .apply(Command::AddTunnel { label: "OLD".to_string(), role: TunnelRole::Pull })
            .unwrap_tunnel();

        let (_output, undo) =
            c.apply_tracked(Command::RenameTunnel { tunnel, new_label: "NEW".to_string() });
        match undo {
            UndoAction::RenameTunnel { tunnel: t, old_label } => {
                assert_eq!(t, tunnel);
                assert_eq!(old_label, "OLD");
            }
            other => panic!("expected RenameTunnel, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_tracked_rename_tunnel_same_label_is_noop() {
        let mut c = Circuit::new();
        let tunnel = c
            .apply(Command::AddTunnel { label: "SAME".to_string(), role: TunnelRole::Pull })
            .unwrap_tunnel();

        let (_output, undo) =
            c.apply_tracked(Command::RenameTunnel { tunnel, new_label: "SAME".to_string() });
        assert!(matches!(undo, UndoAction::Noop));
    }

    #[test]
    fn test_apply_tracked_tick_clock_captures_pre_tick_value() {
        let mut c = Circuit::new();
        let data = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let we = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let reg = c.apply(Command::AddComponent(Component::reg(1))).unwrap_comp();
        let out = c.apply(Command::AddComponent(Component::output())).unwrap_comp();
        c.apply(Command::Link { a: data, a_pin: PinId::output(0), b: reg, b_pin: PinId::input(0) });
        c.apply(Command::Link { a: we, a_pin: PinId::output(0), b: reg, b_pin: PinId::input(1) });
        c.apply(Command::Link { a: reg, a_pin: PinId::output(0), b: out, b_pin: PinId::input(0) });
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ZERO); // settle() never latches

        let (_output, undo) = c.apply_tracked(Command::TickClock);
        match undo {
            UndoAction::RestoreSeqState { snapshots } => {
                assert_eq!(snapshots.len(), 1);
                assert_eq!(snapshots[0].0, reg);
                match snapshots[0].1 {
                    SeqState::Reg(v) => assert_eq!(v, Value::new(0, 1)), // pre-tick, not the just-latched 1
                }
            }
            other => panic!("expected RestoreSeqState, got {other:?}"),
        }
        assert_eq!(c.read_output(out), Value::ONE); // confirms the tick really did latch afterward
    }

    #[test]
    fn test_apply_tracked_remove_component_sole_driver_captures_sinks_and_tunnel() {
        let mut c = Circuit::new();
        let driver = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let sink = c.apply(Command::AddComponent(Component::output())).unwrap_comp();
        c.apply(Command::Link {
            a: driver,
            a_pin: PinId::output(0),
            b: sink,
            b_pin: PinId::input(0),
        });
        let tunnel = c
            .apply(Command::AddTunnel { label: "T".to_string(), role: TunnelRole::Pull })
            .unwrap_tunnel();
        c.apply(Command::LinkTunnel { tunnel, comp: driver, pin: PinId::output(0) });

        let (_output, undo) = c.apply_tracked(Command::RemoveComponent(driver));
        match undo {
            UndoAction::RestoreComponent { links, tunnel_links, .. } => {
                assert_eq!(links, vec![(PinId::output(0), sink, PinId::input(0))]);
                assert_eq!(tunnel_links, vec![(PinId::output(0), tunnel)]);
            }
            other => panic!("expected RestoreComponent, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_tracked_remove_component_one_of_two_drivers_captures_other_participants() {
        let mut c = Circuit::new();
        let d1 = c.apply(Command::AddComponent(Component::input(1, 1))).unwrap_comp();
        let d2 = c.apply(Command::AddComponent(Component::input(0, 1))).unwrap_comp();
        let o = c.apply(Command::AddComponent(Component::output())).unwrap_comp();
        c.apply(Command::Link { a: d1, a_pin: PinId::output(0), b: o, b_pin: PinId::input(0) });
        c.apply(Command::Link { a: d2, a_pin: PinId::output(0), b: o, b_pin: PinId::input(0) });

        let (_output, undo) = c.apply_tracked(Command::RemoveComponent(d2));
        match undo {
            UndoAction::RestoreComponent { links, tunnel_links, .. } => {
                assert_eq!(links.len(), 2);
                assert!(links.contains(&(PinId::output(0), d1, PinId::output(0))));
                assert!(links.contains(&(PinId::output(0), o, PinId::input(0))));
                assert!(tunnel_links.is_empty());
            }
            other => panic!("expected RestoreComponent, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_tracked_remove_component_self_loop_preserves_self_link() {
        // Regression test for the identity-based (not CompKey-based)
        // exclusion in RemoveComponent's capture: a component wired to its
        // own other pin must not have that edge silently dropped.
        let mut c = Circuit::new();
        let g = c
            .apply(Command::AddComponent(Component::gate(GateOp::Not, 1, 1)))
            .unwrap_comp();
        c.apply(Command::Link {
            a: g,
            a_pin: PinId::output(0),
            b: g,
            b_pin: PinId::input(0),
        });

        let (_output, undo) = c.apply_tracked(Command::RemoveComponent(g));
        match undo {
            UndoAction::RestoreComponent { links, .. } => {
                assert!(links.contains(&(PinId::output(0), g, PinId::input(0))));
                assert!(links.contains(&(PinId::input(0), g, PinId::output(0))));
            }
            other => panic!("expected RestoreComponent, got {other:?}"),
        }
    }

    #[test]
    fn test_component_spec_round_trips_pin_arity() {
        use crate::sim::component::FanDirection;

        let mut c = Circuit::new();
        let cases: Vec<Component> = vec![
            Component::input(3, 4),
            Component::output(),
            Component::gate(GateOp::And, 3, 2),
            Component::mux(4, 2),
            Component::demux(4, 2),
            Component::reg(8),
            Component::priority_encoder(3),
            Component::adder(4),
            Component::subtractor(4),
            Component::multiplier(4),
            Component::divider(4),
            Component::comparator(4),
            Component::splitter(vec![vec![0, 1], vec![2, 3]], FanDirection::Right),
        ];
        for comp in cases {
            let expected_inputs = comp.pins.inputs.len();
            let expected_outputs = comp.pins.outputs.len();
            let key = c.add_component(comp);
            let spec = c.components[key].spec();
            let rebuilt = spec.to_component();
            assert_eq!(rebuilt.pins.inputs.len(), expected_inputs);
            assert_eq!(rebuilt.pins.outputs.len(), expected_outputs);
        }
    }
}
