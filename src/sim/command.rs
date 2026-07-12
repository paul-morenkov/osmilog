use crate::sim::circuit::{Circuit, SettleError, TunnelKey, TunnelRole};
use crate::sim::component::{CompKey, Component, Input, Logic, LogicComb, PinId, SeqState};
use crate::sim::net::NetKey;

// A single structural mutation to a Circuit, expressed as data so
// Circuit::apply can dispatch it and return the UndoAction that reverses it -
// the seam the GUI's undo/redo records against.
#[derive(Debug)]
pub enum Command {
    AddComponent(Component),
    SetInput {
        comp: CompKey,
        bits: u32,
        width: u8,
    },
    ClearNets,
    Link {
        a: CompKey,
        a_pin: PinId,
        b: CompKey,
        b_pin: PinId,
    },
    AddTunnel {
        label: String,
        role: TunnelRole,
    },
    LinkTunnel {
        tunnel: TunnelKey,
        comp: CompKey,
        pin: PinId,
    },
    DetachTunnel(TunnelKey),
    RemoveTunnel(TunnelKey),
    RenameTunnel {
        tunnel: TunnelKey,
        new_label: String,
    },
    TickClock,
    ResetSequential,
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

    /// Panics unless this came from `Command::TickClock` or
    /// `Command::ResetSequential`.
    pub fn unwrap_settle(self) -> Result<(), SettleError> {
        match self {
            Self::Settle(r) => r,
            other => panic!("expected CommandOutput::Settle, got {other:?}"),
        }
    }
}

// Enough pre-state to reverse one applied Command, captured by Circuit::apply.
// Net structure is *derived* (the GUI rebuilds it from Wiring records after
// any edit, see gui::app::rebuild_circuit), so commands that only rebuild nets
// (ClearNets/Link/LinkTunnel/DetachTunnel) capture NoOp; only commands that
// change authoritative state capture a real inverse.
//
// Component/tunnel removal tombstones (see Circuit::remove_component) rather
// than deletes, so its inverse is a stable-key reactivation - no new keys, and
// a removed Reg's latched state is preserved for reactivation to restore.
#[derive(Debug)]
pub enum UndoAction {
    /// No-op, or a derived-net command that undo re-derives instead.
    NoOp,
    /// Undoes `Command::AddComponent`: tombstone the component that was added.
    DeactivateComponent(CompKey),
    /// Undoes `Command::RemoveComponent`: reactivate the tombstoned component
    /// in place (its `CompKey` and any `Reg` state were preserved).
    ReactivateComponent(CompKey),
    /// Undoes `Command::SetInput`.
    SetInput {
        comp: CompKey,
        old_bits: u32,
        old_width: u8,
    },
    /// Undoes `Command::AddTunnel`: tombstone the tunnel that was added.
    DeactivateTunnel(TunnelKey),
    /// Undoes `Command::RemoveTunnel`: reactivate the tombstoned tunnel in
    /// place (its `TunnelKey` is preserved).
    ReactivateTunnel(TunnelKey),
    /// Undoes `Command::RenameTunnel`.
    RenameTunnel {
        tunnel: TunnelKey,
        old_label: String,
    },
    /// Would undo `Command::TickClock`, but ticks are issued untracked (see
    /// `apply_undo`), so this variant is never actually reached.
    RestoreSeqState { snapshots: Vec<(CompKey, SeqState)> },
}

impl Circuit {
    /// Applies a `Command`, returning its output and the `UndoAction` that
    /// reverses it. Does NOT call `settle()` (callers are responsible, except
    /// `TickClock` which settles internally). Callers that don't need the
    /// undo take `.0`.
    pub fn apply(&mut self, command: Command) -> (CommandOutput, UndoAction) {
        puffin::profile_function!();
        match command {
            Command::AddComponent(comp) => {
                let key = self.add_component(comp);
                (
                    CommandOutput::Comp(key),
                    UndoAction::DeactivateComponent(key),
                )
            }
            Command::SetInput { comp, bits, width } => {
                let old = match &self.components[comp].logic {
                    Logic::Comb(LogicComb::Input(Input { bits: b, width: w })) => Some((*b, *w)),
                    _ => None,
                };
                self.set_input(comp, bits, width);
                let undo = match old {
                    Some((old_bits, old_width)) => UndoAction::SetInput {
                        comp,
                        old_bits,
                        old_width,
                    },
                    None => UndoAction::NoOp,
                };
                (CommandOutput::None, undo)
            }
            // ClearNets / Link / LinkTunnel / DetachTunnel only rebuild derived
            // net structure (see UndoAction) - undo re-derives it, so they
            // capture nothing.
            Command::ClearNets => {
                self.clear_nets();
                (CommandOutput::None, UndoAction::NoOp)
            }
            Command::Link { a, a_pin, b, b_pin } => {
                let net = self.link(a, a_pin, b, b_pin);
                (CommandOutput::Net(net), UndoAction::NoOp)
            }
            Command::AddTunnel { label, role } => {
                let key = self.add_tunnel(label, role);
                (
                    CommandOutput::Tunnel(key),
                    UndoAction::DeactivateTunnel(key),
                )
            }
            Command::LinkTunnel { tunnel, comp, pin } => {
                let net = self.link_tunnel(tunnel, comp, pin);
                (CommandOutput::Net(net), UndoAction::NoOp)
            }
            Command::DetachTunnel(tunnel) => {
                self.detach_tunnel(tunnel);
                (CommandOutput::None, UndoAction::NoOp)
            }
            Command::RemoveTunnel(tunnel) => {
                let was_active = self.tunnels.get(tunnel).is_some_and(|t| t.active);
                self.remove_tunnel(tunnel);
                let undo = if was_active {
                    UndoAction::ReactivateTunnel(tunnel)
                } else {
                    UndoAction::NoOp
                };
                (CommandOutput::None, undo)
            }
            Command::RenameTunnel { tunnel, new_label } => {
                let old_label = self.tunnels.get(tunnel).map(|t| t.label.clone());
                self.rename_tunnel(tunnel, new_label.clone());
                let undo = match old_label {
                    Some(old) if old != new_label => UndoAction::RenameTunnel {
                        tunnel,
                        old_label: old,
                    },
                    _ => UndoAction::NoOp,
                };
                (CommandOutput::None, undo)
            }
            Command::TickClock => {
                let snapshots: Vec<(CompKey, SeqState)> = self
                    .components
                    .iter()
                    .filter_map(|(k, c)| match &c.logic {
                        Logic::Seq(seq) if c.active => Some((k, seq.snapshot())),
                        _ => None,
                    })
                    .collect();
                let result = self.tick_clock();
                (
                    CommandOutput::Settle(result),
                    UndoAction::RestoreSeqState { snapshots },
                )
            }
            // Like TickClock, a simulation step rather than an edit - the GUI
            // issues it untracked (clock "Stop"), so its undo is a NoOp.
            Command::ResetSequential => (
                CommandOutput::Settle(self.reset_sequential()),
                UndoAction::NoOp,
            ),
            Command::RemoveComponent(key) => {
                let was_active = self.components.get(key).is_some_and(|c| c.active);
                self.remove_component(key);
                let undo = if was_active {
                    UndoAction::ReactivateComponent(key)
                } else {
                    UndoAction::NoOp
                };
                (CommandOutput::None, undo)
            }
        }
    }

    /// Reverses the `Command` that produced `action`, and returns the
    /// `UndoAction` that reverses *this* application - so undo/redo is one
    /// symmetric operation. Touches only authoritative state; net structure
    /// is derived and rebuilt separately by the GUI.
    pub fn apply_undo(&mut self, action: UndoAction) -> UndoAction {
        match action {
            UndoAction::NoOp => UndoAction::NoOp,
            UndoAction::DeactivateComponent(key) => {
                self.remove_component(key);
                UndoAction::ReactivateComponent(key)
            }
            UndoAction::ReactivateComponent(key) => {
                self.reactivate_component(key);
                UndoAction::DeactivateComponent(key)
            }
            UndoAction::DeactivateTunnel(key) => {
                self.remove_tunnel(key);
                UndoAction::ReactivateTunnel(key)
            }
            UndoAction::ReactivateTunnel(key) => {
                self.reactivate_tunnel(key);
                UndoAction::DeactivateTunnel(key)
            }
            UndoAction::SetInput {
                comp,
                old_bits,
                old_width,
            } => {
                // Capture the current value first so the returned inverse can
                // restore it on redo.
                let current = match &self.components[comp].logic {
                    Logic::Comb(LogicComb::Input(Input { bits, width })) => (*bits, *width),
                    _ => (old_bits, old_width),
                };
                self.set_input(comp, old_bits, old_width);
                UndoAction::SetInput {
                    comp,
                    old_bits: current.0,
                    old_width: current.1,
                }
            }
            UndoAction::RenameTunnel { tunnel, old_label } => {
                let current = self
                    .tunnels
                    .get(tunnel)
                    .map(|t| t.label.clone())
                    .unwrap_or_else(|| old_label.clone());
                self.rename_tunnel(tunnel, old_label);
                UndoAction::RenameTunnel {
                    tunnel,
                    old_label: current,
                }
            }
            // Clock ticks are issued untracked (see OsmilogApp's Tick Clock
            // handler), so a RestoreSeqState should never reach the history.
            UndoAction::RestoreSeqState { .. } => {
                debug_assert!(
                    false,
                    "RestoreSeqState reached apply_undo: clock ticks must be untracked"
                );
                UndoAction::NoOp
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::component::GateOp;
    use crate::sim::value::Value;

    // ---- Forward dispatch (Command -> Circuit mutation) ----

    #[test]
    fn test_apply_add_component_returns_comp_key_and_registers() {
        let mut c = Circuit::new();
        let key = c
            .apply(Command::AddComponent(Component::input(5, 3)))
            .0
            .unwrap_comp();
        // add_component eagerly evaluates, before any link()/settle().
        assert_eq!(c.components[key].pins.out_cache[0], Value::new(5, 3));
    }

    #[test]
    fn test_apply_set_input_updates_value() {
        let mut c = Circuit::new();
        let i = c
            .apply(Command::AddComponent(Component::input(0, 4)))
            .0
            .unwrap_comp();
        let o = c
            .apply(Command::AddComponent(Component::output()))
            .0
            .unwrap_comp();
        c.apply(Command::Link {
            a: i,
            a_pin: PinId::output(0),
            b: o,
            b_pin: PinId::input(0),
        });
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(0, 4));

        c.apply(Command::SetInput {
            comp: i,
            bits: 7,
            width: 4,
        });
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::new(7, 4));
    }

    #[test]
    fn test_apply_link_returns_net_key_and_wires_components() {
        let mut c = Circuit::new();
        let i = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        let o = c
            .apply(Command::AddComponent(Component::output()))
            .0
            .unwrap_comp();
        let net = c
            .apply(Command::Link {
                a: i,
                a_pin: PinId::output(0),
                b: o,
                b_pin: PinId::input(0),
            })
            .0
            .unwrap_net();
        assert!(c.nets.contains_key(net));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::ONE);
    }

    #[test]
    fn test_apply_add_tunnel_and_link_tunnel_return_correct_keys() {
        let mut c = Circuit::new();
        let driver = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        let tunnel = c
            .apply(Command::AddTunnel {
                label: "CLK".to_string(),
                role: TunnelRole::Pull,
            })
            .0
            .unwrap_tunnel();
        assert_eq!(c.tunnel_label(tunnel), Some("CLK"));

        let net = c
            .apply(Command::LinkTunnel {
                tunnel,
                comp: driver,
                pin: PinId::output(0),
            })
            .0
            .unwrap_net();
        assert!(c.nets.contains_key(net));
    }

    #[test]
    fn test_apply_rename_tunnel_updates_label() {
        let mut c = Circuit::new();
        let tunnel = c
            .apply(Command::AddTunnel {
                label: "OLD".to_string(),
                role: TunnelRole::Pull,
            })
            .0
            .unwrap_tunnel();
        c.apply(Command::RenameTunnel {
            tunnel,
            new_label: "NEW".to_string(),
        });
        assert_eq!(c.tunnel_label(tunnel), Some("NEW"));
    }

    #[test]
    fn test_apply_remove_component_tears_down_conflict() {
        let mut c = Circuit::new();
        let d1 = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        let d2 = c
            .apply(Command::AddComponent(Component::input(0, 1)))
            .0
            .unwrap_comp();
        let o = c
            .apply(Command::AddComponent(Component::output()))
            .0
            .unwrap_comp();
        c.apply(Command::Link {
            a: d1,
            a_pin: PinId::output(0),
            b: o,
            b_pin: PinId::input(0),
        });
        c.apply(Command::Link {
            a: d2,
            a_pin: PinId::output(0),
            b: o,
            b_pin: PinId::input(0),
        });
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::Invalid);

        c.apply(Command::RemoveComponent(d2));
        c.settle().unwrap();
        assert_eq!(c.read_output(o), Value::ONE);
    }

    #[test]
    fn test_apply_tick_clock_returns_settle_result_and_latches() {
        let mut c = Circuit::new();
        let data = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        let we = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        let reg = c
            .apply(Command::AddComponent(Component::reg(1)))
            .0
            .unwrap_comp();
        let out = c
            .apply(Command::AddComponent(Component::output()))
            .0
            .unwrap_comp();
        c.apply(Command::Link {
            a: data,
            a_pin: PinId::output(0),
            b: reg,
            b_pin: PinId::input(0),
        });
        c.apply(Command::Link {
            a: we,
            a_pin: PinId::output(0),
            b: reg,
            b_pin: PinId::input(1),
        });
        c.apply(Command::Link {
            a: reg,
            a_pin: PinId::output(0),
            b: out,
            b_pin: PinId::input(0),
        });
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ZERO);

        let result = c.apply(Command::TickClock).0.unwrap_settle();
        assert_eq!(result, Ok(()));
        assert_eq!(c.read_output(out), Value::ONE);
    }

    #[test]
    fn test_apply_clear_nets_removes_all_nets() {
        let mut c = Circuit::new();
        let a = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        let g = c
            .apply(Command::AddComponent(Component::gate(GateOp::Not, 1, 1)))
            .0
            .unwrap_comp();
        c.apply(Command::Link {
            a,
            a_pin: PinId::output(0),
            b: g,
            b_pin: PinId::input(0),
        });
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
        let key = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        c.apply(Command::RemoveComponent(key)).0.unwrap_comp();
    }

    // ---- UndoAction capture ----

    #[test]
    fn test_apply_add_component_undo_is_deactivate() {
        let mut c = Circuit::new();
        let (output, undo) = c.apply(Command::AddComponent(Component::input(1, 1)));
        let key = output.unwrap_comp();
        assert!(matches!(undo, UndoAction::DeactivateComponent(k) if k == key));
    }

    #[test]
    fn test_apply_remove_component_undo_is_reactivate() {
        let mut c = Circuit::new();
        let key = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        let (_output, undo) = c.apply(Command::RemoveComponent(key));
        assert!(matches!(undo, UndoAction::ReactivateComponent(k) if k == key));
        // Removal tombstones rather than deletes: the key still resolves.
        assert!(c.components.contains_key(key));
        assert!(!c.components[key].active);
    }

    #[test]
    fn test_apply_remove_already_removed_component_is_noop() {
        let mut c = Circuit::new();
        let key = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        c.apply(Command::RemoveComponent(key));
        let (_output, undo) = c.apply(Command::RemoveComponent(key));
        assert!(matches!(undo, UndoAction::NoOp));
    }

    #[test]
    fn test_reactivate_component_preserves_reg_latched_state() {
        // The gap this fixes: RestoreComponent rebuilt a removed Reg from its
        // spec, which omits the latched value - so undoing a register deletion
        // reset it to 0. Tombstoning keeps the live Component, so its latched
        // state survives removal and comes back on reactivation.
        let mut c = Circuit::new();
        let data = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        let we = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        let reg = c
            .apply(Command::AddComponent(Component::reg(1)))
            .0
            .unwrap_comp();
        let out = c
            .apply(Command::AddComponent(Component::output()))
            .0
            .unwrap_comp();
        c.link(data, PinId::output(0), reg, PinId::input(0));
        c.link(we, PinId::output(0), reg, PinId::input(1));
        c.link(reg, PinId::output(0), out, PinId::input(0));
        c.settle().unwrap();
        c.apply(Command::TickClock); // latch 1 into reg
        assert_eq!(c.read_output(out), Value::ONE);

        // Remove (tombstone) the register, then reactivate it and rewire its
        // output; the previously latched 1 must return.
        c.apply(Command::RemoveComponent(reg));
        c.reactivate_component(reg);
        let out2 = c
            .apply(Command::AddComponent(Component::output()))
            .0
            .unwrap_comp();
        c.link(reg, PinId::output(0), out2, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(out2), Value::ONE);
    }

    #[test]
    fn test_apply_set_input_captures_old_value() {
        let mut c = Circuit::new();
        let i = c
            .apply(Command::AddComponent(Component::input(3, 4)))
            .0
            .unwrap_comp();

        let (_output, undo) = c.apply(Command::SetInput {
            comp: i,
            bits: 9,
            width: 4,
        });
        match undo {
            UndoAction::SetInput {
                comp,
                old_bits,
                old_width,
            } => {
                assert_eq!(comp, i);
                assert_eq!(old_bits, 3);
                assert_eq!(old_width, 4);
            }
            other => panic!("expected SetInput, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_set_input_on_non_input_is_noop() {
        let mut c = Circuit::new();
        let g = c
            .apply(Command::AddComponent(Component::gate(GateOp::Not, 1, 1)))
            .0
            .unwrap_comp();

        let (_output, undo) = c.apply(Command::SetInput {
            comp: g,
            bits: 1,
            width: 1,
        });
        assert!(matches!(undo, UndoAction::NoOp));
    }

    #[test]
    fn test_apply_add_tunnel_undo_is_deactivate() {
        let mut c = Circuit::new();
        let (output, undo) = c.apply(Command::AddTunnel {
            label: "A".to_string(),
            role: TunnelRole::Pull,
        });
        let key = output.unwrap_tunnel();
        assert!(matches!(undo, UndoAction::DeactivateTunnel(k) if k == key));
    }

    #[test]
    fn test_apply_remove_tunnel_undo_is_reactivate() {
        let mut c = Circuit::new();
        let driver = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        let tunnel = c
            .apply(Command::AddTunnel {
                label: "C".to_string(),
                role: TunnelRole::Feed,
            })
            .0
            .unwrap_tunnel();
        c.apply(Command::LinkTunnel {
            tunnel,
            comp: driver,
            pin: PinId::output(0),
        });

        let (_output, undo) = c.apply(Command::RemoveTunnel(tunnel));
        assert!(matches!(undo, UndoAction::ReactivateTunnel(k) if k == tunnel));
        // Tombstoned: key still resolves, but the tunnel is inactive.
        assert!(!c.tunnels[tunnel].active);
    }

    #[test]
    fn test_apply_rename_tunnel_captures_old_label() {
        let mut c = Circuit::new();
        let tunnel = c
            .apply(Command::AddTunnel {
                label: "OLD".to_string(),
                role: TunnelRole::Pull,
            })
            .0
            .unwrap_tunnel();

        let (_output, undo) = c.apply(Command::RenameTunnel {
            tunnel,
            new_label: "NEW".to_string(),
        });
        match undo {
            UndoAction::RenameTunnel {
                tunnel: t,
                old_label,
            } => {
                assert_eq!(t, tunnel);
                assert_eq!(old_label, "OLD");
            }
            other => panic!("expected RenameTunnel, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_rename_tunnel_same_label_is_noop() {
        let mut c = Circuit::new();
        let tunnel = c
            .apply(Command::AddTunnel {
                label: "SAME".to_string(),
                role: TunnelRole::Pull,
            })
            .0
            .unwrap_tunnel();

        let (_output, undo) = c.apply(Command::RenameTunnel {
            tunnel,
            new_label: "SAME".to_string(),
        });
        assert!(matches!(undo, UndoAction::NoOp));
    }

    #[test]
    fn test_apply_derived_net_commands_capture_noop() {
        // Link / LinkTunnel / ClearNets / DetachTunnel only rebuild derived net
        // structure, so they record nothing to undo.
        let mut c = Circuit::new();
        let a = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        let b = c
            .apply(Command::AddComponent(Component::output()))
            .0
            .unwrap_comp();
        let tunnel = c
            .apply(Command::AddTunnel {
                label: "T".to_string(),
                role: TunnelRole::Pull,
            })
            .0
            .unwrap_tunnel();

        let (_o, undo) = c.apply(Command::Link {
            a,
            a_pin: PinId::output(0),
            b,
            b_pin: PinId::input(0),
        });
        assert!(matches!(undo, UndoAction::NoOp));

        let (_o, undo) = c.apply(Command::LinkTunnel {
            tunnel,
            comp: a,
            pin: PinId::output(0),
        });
        assert!(matches!(undo, UndoAction::NoOp));

        let (_o, undo) = c.apply(Command::DetachTunnel(tunnel));
        assert!(matches!(undo, UndoAction::NoOp));

        let (_o, undo) = c.apply(Command::ClearNets);
        assert!(matches!(undo, UndoAction::NoOp));
    }

    #[test]
    fn test_apply_tick_clock_captures_pre_tick_value() {
        let mut c = Circuit::new();
        let data = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        let we = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        let reg = c
            .apply(Command::AddComponent(Component::reg(1)))
            .0
            .unwrap_comp();
        let out = c
            .apply(Command::AddComponent(Component::output()))
            .0
            .unwrap_comp();
        c.apply(Command::Link {
            a: data,
            a_pin: PinId::output(0),
            b: reg,
            b_pin: PinId::input(0),
        });
        c.apply(Command::Link {
            a: we,
            a_pin: PinId::output(0),
            b: reg,
            b_pin: PinId::input(1),
        });
        c.apply(Command::Link {
            a: reg,
            a_pin: PinId::output(0),
            b: out,
            b_pin: PinId::input(0),
        });
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ZERO); // settle() never latches

        let (_output, undo) = c.apply(Command::TickClock);
        match undo {
            UndoAction::RestoreSeqState { snapshots } => {
                assert_eq!(snapshots.len(), 1);
                assert_eq!(snapshots[0].0, reg);
                match &snapshots[0].1 {
                    SeqState::Reg(v) => assert_eq!(*v, Value::new(0, 1)), // pre-tick, not the just-latched 1
                    _ => panic!(),
                }
            }
            other => panic!("expected RestoreSeqState, got {other:?}"),
        }
        assert_eq!(c.read_output(out), Value::ONE); // confirms the tick really did latch afterward
    }

    #[test]
    fn test_tick_clock_skips_tombstoned_reg() {
        // A tombstoned Reg must not tick - its preserved state has to stay
        // intact so a later reactivation restores it.
        let mut c = Circuit::new();
        let data = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        let we = c
            .apply(Command::AddComponent(Component::input(1, 1)))
            .0
            .unwrap_comp();
        let reg = c
            .apply(Command::AddComponent(Component::reg(1)))
            .0
            .unwrap_comp();
        c.link(data, PinId::output(0), reg, PinId::input(0));
        c.link(we, PinId::output(0), reg, PinId::input(1));
        c.settle().unwrap();

        // Tombstone the register, then tick: its captured snapshot list must be
        // empty (no active seq comps) and its state untouched.
        c.apply(Command::RemoveComponent(reg));
        let (_output, undo) = c.apply(Command::TickClock);
        match undo {
            UndoAction::RestoreSeqState { snapshots } => assert!(snapshots.is_empty()),
            other => panic!("expected RestoreSeqState, got {other:?}"),
        }
        // Reactivate and read: still the initial 0, never latched the 1.
        c.reactivate_component(reg);
        let out = c
            .apply(Command::AddComponent(Component::output()))
            .0
            .unwrap_comp();
        c.link(reg, PinId::output(0), out, PinId::input(0));
        c.settle().unwrap();
        assert_eq!(c.read_output(out), Value::ZERO);
    }
}
