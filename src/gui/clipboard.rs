// Copy/paste. `Clipboard` holds a `CircuitSnapshot` (Save/Load's index-based records,
// not live SlotMap keys), so it survives further edits and undo/redo to the originals.
// Copy scope is strict-selection-only, no connectivity-inference/auto-follow-wiring.
// OsmilogApp-agnostic: `OsmilogApp::copy_selection`/`paste_clipboard` handle undo batching.

use std::collections::{HashMap, HashSet};

use crate::gui::app::{PlacedCompKey, PlacedTunnel, PlacedTunnelKey, Selected};
use crate::gui::geometry::GridPos;
use crate::gui::placed_component::PlacedComponent;
use crate::gui::wiring::{NodeAttach, WireSegKey, Wiring};
use crate::io::{
    CircuitSnapshot, ComponentEntry, NodeAttachEntry, NodeEntry, SegEntry, TunnelEntry,
};
use crate::sim::component::{InIdx, OutIdx, PinId};

// Matches the narrowest placed component's width (geometry.rs), so a paste reads
// as a clearly offset duplicate without jumping far from the originals.
pub(crate) const PASTE_OFFSET_STEP: i32 = 2;

fn offset_grid_pos(gp: GridPos, off: GridPos) -> GridPos {
    GridPos::new(gp.x + off.x, gp.y + off.y)
}

fn base_offset() -> GridPos {
    GridPos::new(PASTE_OFFSET_STEP, PASTE_OFFSET_STEP)
}

/// A snapshot of a copied selection. It holds no live keys, so later edits or undo/redo
/// cannot invalidate it.
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

    /// No-op if `selected` is empty. Resets the paste offset to the base step.
    pub fn copy(
        &mut self,
        components: &HashMap<PlacedCompKey, PlacedComponent>,
        tunnels: &HashMap<PlacedTunnelKey, PlacedTunnel>,
        wiring: &Wiring,
        selected: &[Selected],
    ) {
        if selected.is_empty() {
            return;
        }
        self.snapshot = Some(build_selection_snapshot(
            components, tunnels, wiring, selected,
        ));
        self.next_offset = base_offset();
    }

    /// Returns the snapshot with positions already shifted. Each call without an
    /// intervening `copy` shifts one step further, so repeated pastes step diagonally.
    /// `None` if nothing has been copied yet.
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

/// Captures only what is directly in `selected`: a wire node/segment counts only if its
/// own segment is selected, not merely reachable from a selected component.
pub(crate) fn build_selection_snapshot(
    components: &HashMap<PlacedCompKey, PlacedComponent>,
    tunnels: &HashMap<PlacedTunnelKey, PlacedTunnel>,
    wiring: &Wiring,
    selected: &[Selected],
) -> CircuitSnapshot {
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
        .filter(|(k, _)| included_components.contains(k))
        .enumerate()
        .map(|(i, (k, pc))| {
            comp_index.insert(*k, i);
            ComponentEntry {
                spec: pc.spec.clone(),
                grid_pos: pc.grid_pos,
            }
        })
        .collect();

    let mut tunnel_index: HashMap<PlacedTunnelKey, usize> = HashMap::new();
    let tunnel_entries: Vec<TunnelEntry> = tunnels
        .iter()
        .filter(|(k, _)| included_tunnels.contains(k))
        .enumerate()
        .map(|(i, (k, pt))| {
            tunnel_index.insert(*k, i);
            TunnelEntry {
                label: pt.label.clone(),
                role: pt.role,
                grid_pos: pt.grid_pos,
            }
        })
        .collect();

    // Node set is exactly the endpoints of included wire segments, per strict-selection scope.
    let mut node_index: HashMap<crate::gui::wiring::WireNodeKey, usize> = HashMap::new();
    let mut node_entries: Vec<NodeEntry> = Vec::new();
    for (seg_key, seg) in &wiring.segments {
        if !included_wires.contains(seg_key) {
            continue;
        }
        for nk in [seg.a, seg.b] {
            if node_index.contains_key(&nk) {
                continue;
            }
            let node = &wiring.nodes[&nk];
            // A Pin/Tunnel attach survives only if its owner is also included; otherwise
            // it downgrades to Free rather than reference an index absent from this copy.
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
        .segments
        .iter()
        .filter(|(k, _)| included_wires.contains(k))
        .map(|(_, seg)| SegEntry {
            a: node_index[&seg.a],
            b: node_index[&seg.b],
        })
        .collect();

    CircuitSnapshot {
        components: comp_entries,
        tunnels: tunnel_entries,
        nodes: node_entries,
        segments: seg_entries,
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
        PlacedComponent::new(crate::sim::component::CompKey(0), spec, grid_pos)
    }

    fn placed_tunnel(label: &str, grid_pos: GridPos) -> PlacedTunnel {
        PlacedTunnel {
            key: crate::sim::circuit::TunnelKey(0),
            label: label.to_string(),
            role: TunnelRole::Feed,
            grid_pos,
        }
    }

    // Map length is a fine monotonic id source since these maps are append-only in tests.
    fn add_comp(
        map: &mut HashMap<PlacedCompKey, PlacedComponent>,
        pc: PlacedComponent,
    ) -> PlacedCompKey {
        let key = PlacedCompKey(map.len() as u64);
        map.insert(key, pc);
        key
    }

    fn add_tunnel(
        map: &mut HashMap<PlacedTunnelKey, PlacedTunnel>,
        pt: PlacedTunnel,
    ) -> PlacedTunnelKey {
        let key = PlacedTunnelKey(map.len() as u64);
        map.insert(key, pt);
        key
    }

    #[test]
    fn test_copy_noop_when_selected_empty() {
        let components: HashMap<PlacedCompKey, PlacedComponent> = HashMap::new();
        let tunnels: HashMap<PlacedTunnelKey, PlacedTunnel> = HashMap::new();
        let wiring = Wiring::new();
        let mut clip = Clipboard::new();
        clip.copy(&components, &tunnels, &wiring, &[]);
        assert!(clip.is_empty());
    }

    #[test]
    fn test_copy_single_component_snapshot_shape() {
        let mut components: HashMap<PlacedCompKey, PlacedComponent> = HashMap::new();
        let key = add_comp(&mut components, placed_component(GridPos::new(3, 4)));
        let tunnels: HashMap<PlacedTunnelKey, PlacedTunnel> = HashMap::new();
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
        let mut components: HashMap<PlacedCompKey, PlacedComponent> = HashMap::new();
        let c0 = add_comp(&mut components, placed_component(GridPos::new(0, 0)));
        let c1 = add_comp(&mut components, placed_component(GridPos::new(10, 0)));
        let tunnels: HashMap<PlacedTunnelKey, PlacedTunnel> = HashMap::new();

        let mut wiring = Wiring::new();
        wiring.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c0, PinId::output(0)),
            NodeAttach::Pin(c1, PinId::input(0)),
        );
        let seg = wiring.segments.keys().next().unwrap();

        // Select just the wire segment, not the components it attaches to.
        let mut clip = Clipboard::new();
        clip.copy(&components, &tunnels, &wiring, &[Selected::Wire(*seg)]);

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
        let mut components: HashMap<PlacedCompKey, PlacedComponent> = HashMap::new();
        let c0 = add_comp(&mut components, placed_component(GridPos::new(0, 0)));
        let c1 = add_comp(&mut components, placed_component(GridPos::new(10, 0)));
        let tunnels: HashMap<PlacedTunnelKey, PlacedTunnel> = HashMap::new();

        let mut wiring = Wiring::new();
        wiring.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c0, PinId::output(0)),
            NodeAttach::Pin(c1, PinId::input(0)),
        );
        let seg = wiring.segments.keys().next().unwrap();

        let mut clip = Clipboard::new();
        clip.copy(
            &components,
            &tunnels,
            &wiring,
            &[
                Selected::Component(c0),
                Selected::Component(c1),
                Selected::Wire(*seg),
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
        let components: HashMap<PlacedCompKey, PlacedComponent> = HashMap::new();
        let mut tunnels: HashMap<PlacedTunnelKey, PlacedTunnel> = HashMap::new();
        let key = add_tunnel(&mut tunnels, placed_tunnel("A", GridPos::new(1, 1)));
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
        let mut components: HashMap<PlacedCompKey, PlacedComponent> = HashMap::new();
        let key = add_comp(&mut components, placed_component(GridPos::new(0, 0)));
        let tunnels: HashMap<PlacedTunnelKey, PlacedTunnel> = HashMap::new();
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
