// Copy/paste: `Clipboard` holds a `CircuitSnapshot` of a copied selection (the
// same index-based records Save/Load uses), not live SlotMap keys,
// so it survives further edits/undo-redo to the originals. Wiring scope on
// copy is strict-selection-only: exactly the components/tunnels/wire
// segments in the selection passed to `copy`, mirroring `delete_bulk`'s
// traversal - no connectivity-inference/auto-follow-wiring.
//
// Deliberately OsmilogApp-agnostic, mirroring `Wiring`'s own independence:
// `copy`/`plan_paste` take exactly the borrowed data they need and know
// nothing about `History`/`Command`/undo. `OsmilogApp::copy_selection`/
// `paste_clipboard` (in app.rs) call into this and handle materializing the
// result into live state plus undo batching themselves.

use std::collections::{HashMap, HashSet};

use slotmap::SlotMap;

use crate::gui::app::{PlacedCompKey, PlacedTunnel, PlacedTunnelKey, Selected};
use crate::gui::geometry::GridPos;
use crate::gui::placed_component::PlacedComponent;
use crate::gui::wiring::{NodeAttach, WireSegKey, Wiring};
use crate::io::{
    CircuitSnapshot, ComponentEntry, NodeAttachEntry, NodeEntry, SegEntry, TunnelEntry,
};
use crate::sim::component::{InIdx, OutIdx, PinId};

// Grid cells added to a pasted item's position relative to its copied
// original, on both axes. Matches the width of the narrowest placed
// components (EDGE_BODY_W/IO_W = 2 in geometry.rs), so a paste reads as a
// clearly offset duplicate without jumping far from the originals.
pub(crate) const PASTE_OFFSET_STEP: i32 = 2;

fn offset_grid_pos(gp: GridPos, off: GridPos) -> GridPos {
    GridPos::new(gp.x + off.x, gp.y + off.y)
}

fn base_offset() -> GridPos {
    GridPos::new(PASTE_OFFSET_STEP, PASTE_OFFSET_STEP)
}

/// Holds a snapshot of a copied selection, independent of any live
/// PlacedCompKey/PlacedTunnelKey/WireNodeKey/WireSegKey - a `CircuitSnapshot`,
/// the same index-based records io.rs uses for save/load, scoped to just the
/// copied subset. Surviving edits/undo-redo to the originals is the entire
/// point: paste only ever reads this snapshot, so it can't be invalidated by
/// anything that happens to the copied items afterward. Also owns the walking
/// paste offset (see `plan_paste`).
pub struct Clipboard {
    snapshot: Option<CircuitSnapshot>,
    next_offset: GridPos,
}

impl Clipboard {
    pub fn new() -> Self {
        Self {
            snapshot: None,
            next_offset: base_offset(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.snapshot.is_none()
    }

    /// Snapshots `selected` out of the given live GUI state. No-op if
    /// `selected` is empty. Wiring scope is strict: a node/segment is only
    /// captured if its owning wire segment is itself in `selected` (not
    /// merely reachable from a selected component) - see module docs.
    /// Resets the walking paste offset back to the base step.
    pub fn copy(
        &mut self,
        components: &SlotMap<PlacedCompKey, PlacedComponent>,
        tunnels: &SlotMap<PlacedTunnelKey, PlacedTunnel>,
        wiring: &Wiring,
        selected: &[Selected],
    ) {
        if selected.is_empty() {
            return;
        }

        let mut included_components: HashSet<PlacedCompKey> = HashSet::new();
        let mut included_tunnels: HashSet<PlacedTunnelKey> = HashSet::new();
        let mut included_wires: HashSet<WireSegKey> = HashSet::new();
        for sel in selected {
            match *sel {
                Selected::Component(k) => {
                    included_components.insert(k);
                }
                Selected::Tunnel(k) => {
                    included_tunnels.insert(k);
                }
                Selected::Wire(k) => {
                    included_wires.insert(k);
                }
            }
        }

        let mut comp_index: HashMap<PlacedCompKey, usize> = HashMap::new();
        let comp_entries: Vec<ComponentEntry> = components
            .iter()
            .filter(|(k, pc)| pc.active && included_components.contains(k))
            .enumerate()
            .map(|(i, (k, pc))| {
                comp_index.insert(k, i);
                ComponentEntry {
                    spec: pc.spec.clone(),
                    grid_pos: pc.grid_pos,
                }
            })
            .collect();

        let mut tunnel_index: HashMap<PlacedTunnelKey, usize> = HashMap::new();
        let tunnel_entries: Vec<TunnelEntry> = tunnels
            .iter()
            .filter(|(k, pt)| pt.active && included_tunnels.contains(k))
            .enumerate()
            .map(|(i, (k, pt))| {
                tunnel_index.insert(k, i);
                TunnelEntry {
                    label: pt.label.clone(),
                    role: pt.role,
                    grid_pos: pt.grid_pos,
                }
            })
            .collect();

        // Node set is exactly the endpoints of included wire segments - not
        // active_nodes() broadly - since wiring scope is strict-selection.
        let mut node_index: HashMap<crate::gui::wiring::WireNodeKey, usize> = HashMap::new();
        let mut node_entries: Vec<NodeEntry> = Vec::new();
        for (seg_key, seg) in wiring.active_segments() {
            if !included_wires.contains(&seg_key) {
                continue;
            }
            for nk in [seg.a, seg.b] {
                if node_index.contains_key(&nk) {
                    continue;
                }
                let node = &wiring.nodes[nk];
                // A node's Pin/Tunnel attach only survives into the copy if
                // its owning component/tunnel is *also* included; otherwise
                // it would reference an index that doesn't exist in this
                // clipboard, so it's downgraded to a Free (unattached) stub
                // rather than a dangling reference.
                let attach = match node.attach {
                    NodeAttach::Free => NodeAttachEntry::Free,
                    NodeAttach::Pin(pck, pin) => match comp_index.get(&pck) {
                        Some(&comp) => {
                            let (is_input, pin_index) = match pin {
                                PinId::In(InIdx(p)) => (true, p),
                                PinId::Out(OutIdx(p)) => (false, p),
                            };
                            NodeAttachEntry::Pin {
                                comp,
                                is_input,
                                pin_index,
                            }
                        }
                        None => NodeAttachEntry::Free,
                    },
                    NodeAttach::Tunnel(ptk) => match tunnel_index.get(&ptk) {
                        Some(&tunnel) => NodeAttachEntry::Tunnel { tunnel },
                        None => NodeAttachEntry::Free,
                    },
                };
                node_index.insert(nk, node_entries.len());
                node_entries.push(NodeEntry {
                    pos: node.pos,
                    attach,
                });
            }
        }

        let seg_entries: Vec<SegEntry> = wiring
            .active_segments()
            .filter(|(k, _)| included_wires.contains(k))
            .map(|(_, seg)| SegEntry {
                a: node_index[&seg.a],
                b: node_index[&seg.b],
            })
            .collect();

        self.snapshot = Some(CircuitSnapshot {
            components: comp_entries,
            tunnels: tunnel_entries,
            nodes: node_entries,
            segments: seg_entries,
        });
        self.next_offset = base_offset();
    }

    /// Returns an offset-adjusted copy of the snapshot, ready for a caller
    /// to materialize into live state (positions already shifted - the
    /// caller does no further position math). Advances the internal walking
    /// offset for the *next* call, so repeated calls without an intervening
    /// `copy` step further each time (a diagonal "staircase"). `None` if
    /// nothing has been copied yet.
    pub fn plan_paste(&mut self) -> Option<CircuitSnapshot> {
        let file = self.snapshot.as_ref()?;
        let offset = self.next_offset;
        let shifted = CircuitSnapshot {
            components: file
                .components
                .iter()
                .map(|e| ComponentEntry {
                    spec: e.spec.clone(),
                    grid_pos: offset_grid_pos(e.grid_pos, offset),
                })
                .collect(),
            tunnels: file
                .tunnels
                .iter()
                .map(|e| TunnelEntry {
                    label: e.label.clone(),
                    role: e.role,
                    grid_pos: offset_grid_pos(e.grid_pos, offset),
                })
                .collect(),
            nodes: file
                .nodes
                .iter()
                .map(|e| NodeEntry {
                    pos: offset_grid_pos(e.pos, offset),
                    attach: e.attach,
                })
                .collect(),
            segments: file.segments.clone(),
        };
        self.next_offset = offset_grid_pos(self.next_offset, base_offset());
        Some(shifted)
    }
}

impl Default for Clipboard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::wiring::NodeAttach;
    use crate::sim::circuit::TunnelRole;
    use crate::sim::component::{ComponentSpec, Input};

    fn placed_component(grid_pos: GridPos) -> PlacedComponent {
        let spec = ComponentSpec::Input(Input { bits: 0, width: 1 });
        PlacedComponent::new(crate::sim::component::CompKey::default(), spec, grid_pos)
    }

    fn placed_tunnel(label: &str, grid_pos: GridPos) -> PlacedTunnel {
        PlacedTunnel {
            key: crate::sim::circuit::TunnelKey::default(),
            label: label.to_string(),
            role: TunnelRole::Feed,
            grid_pos,
            active: true,
        }
    }

    #[test]
    fn test_copy_noop_when_selected_empty() {
        let components: SlotMap<PlacedCompKey, PlacedComponent> = SlotMap::default();
        let tunnels: SlotMap<PlacedTunnelKey, PlacedTunnel> = SlotMap::default();
        let wiring = Wiring::new();
        let mut clip = Clipboard::new();
        clip.copy(&components, &tunnels, &wiring, &[]);
        assert!(clip.is_empty());
    }

    #[test]
    fn test_copy_single_component_snapshot_shape() {
        let mut components: SlotMap<PlacedCompKey, PlacedComponent> = SlotMap::default();
        let key = components.insert(placed_component(GridPos::new(3, 4)));
        let tunnels: SlotMap<PlacedTunnelKey, PlacedTunnel> = SlotMap::default();
        let wiring = Wiring::new();

        let mut clip = Clipboard::new();
        clip.copy(&components, &tunnels, &wiring, &[Selected::Component(key)]);
        assert!(!clip.is_empty());

        let file = clip.plan_paste().unwrap();
        assert_eq!(file.components.len(), 1);
        assert_eq!(file.components[0].grid_pos, GridPos::new(5, 6));
        assert!(file.tunnels.is_empty());
        assert!(file.nodes.is_empty());
        assert!(file.segments.is_empty());
    }

    #[test]
    fn test_copy_wire_only_downgrades_dangling_pin_attach() {
        let mut components: SlotMap<PlacedCompKey, PlacedComponent> = SlotMap::default();
        let c0 = components.insert(placed_component(GridPos::new(0, 0)));
        let c1 = components.insert(placed_component(GridPos::new(10, 0)));
        let tunnels: SlotMap<PlacedTunnelKey, PlacedTunnel> = SlotMap::default();

        let mut wiring = Wiring::new();
        wiring.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c0, PinId::output(0)),
            NodeAttach::Pin(c1, PinId::input(0)),
        );
        let seg = wiring.active_segments().next().unwrap().0;

        // Select just the wire segment, not the components it attaches to.
        let mut clip = Clipboard::new();
        clip.copy(&components, &tunnels, &wiring, &[Selected::Wire(seg)]);

        let file = clip.plan_paste().unwrap();
        assert!(file.components.is_empty());
        assert_eq!(file.nodes.len(), 2);
        assert!(file
            .nodes
            .iter()
            .all(|n| matches!(n.attach, NodeAttachEntry::Free)));
        assert_eq!(file.segments.len(), 1);
    }

    #[test]
    fn test_copy_component_and_its_wire_preserves_pin_attach() {
        let mut components: SlotMap<PlacedCompKey, PlacedComponent> = SlotMap::default();
        let c0 = components.insert(placed_component(GridPos::new(0, 0)));
        let c1 = components.insert(placed_component(GridPos::new(10, 0)));
        let tunnels: SlotMap<PlacedTunnelKey, PlacedTunnel> = SlotMap::default();

        let mut wiring = Wiring::new();
        wiring.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c0, PinId::output(0)),
            NodeAttach::Pin(c1, PinId::input(0)),
        );
        let seg = wiring.active_segments().next().unwrap().0;

        let mut clip = Clipboard::new();
        clip.copy(
            &components,
            &tunnels,
            &wiring,
            &[
                Selected::Component(c0),
                Selected::Component(c1),
                Selected::Wire(seg),
            ],
        );

        let file = clip.plan_paste().unwrap();
        assert_eq!(file.components.len(), 2);
        assert_eq!(file.nodes.len(), 2);
        assert!(file
            .nodes
            .iter()
            .any(|n| matches!(n.attach, NodeAttachEntry::Pin { comp: 0, .. })));
        assert!(file
            .nodes
            .iter()
            .any(|n| matches!(n.attach, NodeAttachEntry::Pin { comp: 1, .. })));
    }

    #[test]
    fn test_copy_tunnel() {
        let components: SlotMap<PlacedCompKey, PlacedComponent> = SlotMap::default();
        let mut tunnels: SlotMap<PlacedTunnelKey, PlacedTunnel> = SlotMap::default();
        let key = tunnels.insert(placed_tunnel("A", GridPos::new(1, 1)));
        let wiring = Wiring::new();

        let mut clip = Clipboard::new();
        clip.copy(&components, &tunnels, &wiring, &[Selected::Tunnel(key)]);
        let file = clip.plan_paste().unwrap();
        assert_eq!(file.tunnels.len(), 1);
        assert_eq!(file.tunnels[0].label, "A");
    }

    #[test]
    fn test_plan_paste_none_when_empty() {
        let mut clip = Clipboard::new();
        assert!(clip.plan_paste().is_none());
    }

    #[test]
    fn test_plan_paste_applies_offset_and_walks_on_repeated_calls() {
        let mut components: SlotMap<PlacedCompKey, PlacedComponent> = SlotMap::default();
        let key = components.insert(placed_component(GridPos::new(0, 0)));
        let tunnels: SlotMap<PlacedTunnelKey, PlacedTunnel> = SlotMap::default();
        let wiring = Wiring::new();

        let mut clip = Clipboard::new();
        clip.copy(&components, &tunnels, &wiring, &[Selected::Component(key)]);

        let first = clip.plan_paste().unwrap();
        assert_eq!(first.components[0].grid_pos, GridPos::new(2, 2));
        let second = clip.plan_paste().unwrap();
        assert_eq!(second.components[0].grid_pos, GridPos::new(4, 4));

        // A fresh copy resets the walking offset back to the base step.
        clip.copy(&components, &tunnels, &wiring, &[Selected::Component(key)]);
        let third = clip.plan_paste().unwrap();
        assert_eq!(third.components[0].grid_pos, GridPos::new(2, 2));
    }
}
