//! GUI-side wiring: a geometry + topology netlist, kept deliberately separate
//! from the simulation `Circuit`.
//!
//! Wires are a graph of grid-aligned `WireNode`s connected by axis-aligned
//! `WireSegment`s. Unlike the old pin-to-pin `Wire` record, a wire here can run
//! into empty space, branch off another wire at any point, and be selected and
//! deleted a segment at a time. Connectivity is *derived* from the segment
//! graph (union-find), and each connected group of nodes maps to one circuit
//! net: the app replays `Circuit::link`/`link_tunnel` for a group's endpoints
//! after any edit (see `groups`, and `OsmilogApp::rebuild_circuit`). This module
//! therefore never touches `Circuit` directly - it only knows the GUI's own
//! `PlacedCompKey`/`PlacedTunnelKey` and `PinId`, so the two systems meet only
//! through that replay step.
//!
//! Attachment is by *key*, not position: a wire merely crossing a pin or another
//! wire does not connect. A junction exists only where a shared node does - which
//! `resolve_point` creates explicitly by splitting a segment when a new wire
//! starts or ends partway along it.

use std::collections::HashMap;

use egui::{Pos2, Vec2};
use slotmap::{new_key_type, SlotMap};

use crate::gui::app::{PlacedCompKey, PlacedTunnelKey};
use crate::gui::geometry::{snap_to_grid, GridPos, GRID_SIZE};
use crate::sim::component::PinId;

new_key_type! {
    pub struct WireNodeKey;
    pub struct WireSegKey;
}

// How close (in pixels) the cursor must be to a segment/node to hit it.
const HIT_RADIUS: f32 = 5.0;

/// What a wire node is bound to. `Free` nodes are corners, junctions, or
/// dangling endpoints in empty space; the other two tie a node to a component
/// pin or a tunnel's single pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeAttach {
    Free,
    Pin(PlacedCompKey, PinId),
    Tunnel(PlacedTunnelKey),
}

#[derive(Clone, Copy, Debug)]
pub struct WireNode {
    /// Grid coordinates (same convention as `PlacedComponent::grid_pos`).
    pub pos: GridPos,
    pub attach: NodeAttach,
}

/// An axis-aligned segment between two nodes (invariant: `a.pos` and `b.pos`
/// share a row or a column).
#[derive(Clone, Copy, Debug)]
pub struct WireSegment {
    pub a: WireNodeKey,
    pub b: WireNodeKey,
}

/// The endpoints of one connected group of wire, in GUI keys. `pins` and
/// `tunnels` are what the app links into a single circuit net; `nodes` is the
/// full node set (used to colour every segment in the group).
pub struct Group {
    pub nodes: Vec<WireNodeKey>,
    pub pins: Vec<(PlacedCompKey, PinId)>,
    pub tunnels: Vec<PlacedTunnelKey>,
}

#[derive(Default, Clone, Debug)]
pub struct Wiring {
    pub nodes: SlotMap<WireNodeKey, WireNode>,
    pub segments: SlotMap<WireSegKey, WireSegment>,
}

impl Wiring {
    pub fn new() -> Self {
        Self::default()
    }

    // ── Geometry helpers ────────────────────────────────────────────────────

    fn to_screen(gp: GridPos, pan: Vec2) -> Pos2 {
        egui::pos2(
            gp.x as f32 * GRID_SIZE + pan.x,
            gp.y as f32 * GRID_SIZE + pan.y,
        )
    }

    fn node_at_grid(&self, gp: GridPos) -> Option<WireNodeKey> {
        self.nodes.iter().find(|(_, n)| n.pos == gp).map(|(k, _)| k)
    }

    // Count of segments incident on a node (its degree). Used both for cleanup
    // (degree 0 -> orphan) and drawing (degree >= 3 -> junction dot).
    pub fn degree(&self, node: WireNodeKey) -> usize {
        self.segments
            .values()
            .filter(|s| s.a == node || s.b == node)
            .count()
    }

    // The segment (if any) that gp lies strictly inside: colinear, axis-aligned,
    // and between (not on) the endpoints. Splitting here is what turns a
    // mid-wire tap into a real junction.
    fn segment_through(&self, gp: GridPos) -> Option<WireSegKey> {
        self.segments.iter().find_map(|(k, seg)| {
            let a = self.nodes[seg.a].pos;
            let b = self.nodes[seg.b].pos;
            let on = if a.x == b.x && gp.x == a.x {
                let (lo, hi) = (a.y.min(b.y), a.y.max(b.y));
                gp.y > lo && gp.y < hi
            } else if a.y == b.y && gp.y == a.y {
                let (lo, hi) = (a.x.min(b.x), a.x.max(b.x));
                gp.x > lo && gp.x < hi
            } else {
                false
            };
            on.then_some(k)
        })
    }

    // ── Editing primitives ──────────────────────────────────────────────────

    fn add_segment(&mut self, a: WireNodeKey, b: WireNodeKey) {
        if a == b {
            return;
        }
        let exists = self
            .segments
            .values()
            .any(|s| (s.a == a && s.b == b) || (s.a == b && s.b == a));
        if !exists {
            self.segments.insert(WireSegment { a, b });
        }
    }

    // Find-or-create the node at gp. If gp lands partway along an existing
    // segment, that segment is split so the returned node becomes a real
    // junction. New nodes start `Free`.
    fn resolve_point(&mut self, gp: GridPos) -> WireNodeKey {
        if let Some(k) = self.node_at_grid(gp) {
            return k;
        }
        if let Some(seg_key) = self.segment_through(gp) {
            let seg = self.segments[seg_key];
            self.segments.remove(seg_key);
            let mid = self.nodes.insert(WireNode {
                pos: gp,
                attach: NodeAttach::Free,
            });
            self.add_segment(seg.a, mid);
            self.add_segment(mid, seg.b);
            return mid;
        }
        self.nodes.insert(WireNode {
            pos: gp,
            attach: NodeAttach::Free,
        })
    }

    // Only sets an attachment onto a node that is still Free, so a wire ending
    // on a pin binds that pin without clobbering an already-bound node.
    fn set_attach_if_free(&mut self, node: WireNodeKey, attach: NodeAttach) {
        if attach != NodeAttach::Free {
            let n = &mut self.nodes[node];
            if n.attach == NodeAttach::Free {
                n.attach = attach;
            }
        }
    }

    /// Add a polyline wire through `points` (grid coords, each adjacent pair
    /// axis-aligned). `start_attach`/`end_attach` bind the first/last node to a
    /// pin or tunnel when the route lands on one; interior points stay `Free`.
    pub fn add_route(
        &mut self,
        points: &[GridPos],
        start_attach: NodeAttach,
        end_attach: NodeAttach,
    ) {
        if points.len() < 2 {
            return;
        }
        let keys: Vec<WireNodeKey> = points.iter().map(|&p| self.resolve_point(p)).collect();
        self.set_attach_if_free(keys[0], start_attach);
        self.set_attach_if_free(*keys.last().unwrap(), end_attach);
        for w in keys.windows(2) {
            self.add_segment(w[0], w[1]);
        }
    }

    /// Remove a segment, then drop any node left with no segments.
    pub fn delete_segment(&mut self, seg: WireSegKey) {
        self.segments.remove(seg);
        self.cleanup();
    }

    // Remove a node and every segment touching it.
    fn remove_node(&mut self, node: WireNodeKey) {
        self.segments.retain(|_, s| s.a != node && s.b != node);
        self.nodes.remove(node);
    }

    /// Drop all nodes bound to a removed component (and their segments).
    pub fn remove_component_nodes(&mut self, pck: PlacedCompKey) {
        let doomed: Vec<WireNodeKey> = self
            .nodes
            .iter()
            .filter(|(_, n)| matches!(n.attach, NodeAttach::Pin(k, _) if k == pck))
            .map(|(k, _)| k)
            .collect();
        for k in doomed {
            self.remove_node(k);
        }
        self.cleanup();
    }

    /// Drop all nodes bound to a removed tunnel (and their segments).
    pub fn remove_tunnel_nodes(&mut self, ptk: PlacedTunnelKey) {
        let doomed: Vec<WireNodeKey> = self
            .nodes
            .iter()
            .filter(|(_, n)| matches!(n.attach, NodeAttach::Tunnel(k) if k == ptk))
            .map(|(k, _)| k)
            .collect();
        for k in doomed {
            self.remove_node(k);
        }
        self.cleanup();
    }

    /// After a component is reconfigured to fewer pins, drop wire nodes bound to
    /// pins that no longer exist.
    pub fn prune_stale_pins(&mut self, pck: PlacedCompKey, n_inputs: usize, n_outputs: usize) {
        let doomed: Vec<WireNodeKey> = self
            .nodes
            .iter()
            .filter(|(_, n)| match n.attach {
                NodeAttach::Pin(k, PinId::In(i)) => k == pck && (i.0 as usize) >= n_inputs,
                NodeAttach::Pin(k, PinId::Out(i)) => k == pck && (i.0 as usize) >= n_outputs,
                _ => false,
            })
            .map(|(k, _)| k)
            .collect();
        for k in doomed {
            self.remove_node(k);
        }
        self.cleanup();
    }

    /// Reposition every node bound to `pck`'s pins to that pin's current grid
    /// position (called after a component moves or is reconfigured). Attached
    /// segments simply stretch to follow.
    pub fn sync_component_nodes(
        &mut self,
        pck: PlacedCompKey,
        mut pin_grid: impl FnMut(PinId) -> GridPos,
    ) {
        for n in self.nodes.values_mut() {
            if let NodeAttach::Pin(k, pin) = n.attach {
                if k == pck {
                    n.pos = pin_grid(pin);
                }
            }
        }
    }

    /// Reposition every node bound to `ptk` to the tunnel's current pin grid
    /// position.
    pub fn sync_tunnel_nodes(&mut self, ptk: PlacedTunnelKey, gp: GridPos) {
        for n in self.nodes.values_mut() {
            if let NodeAttach::Tunnel(k) = n.attach {
                if k == ptk {
                    n.pos = gp;
                }
            }
        }
    }

    // Drop nodes with no incident segments (orphans left by a delete/split).
    fn cleanup(&mut self) {
        let orphans: Vec<WireNodeKey> =
            self.nodes.keys().filter(|&k| self.degree(k) == 0).collect();
        for k in orphans {
            self.nodes.remove(k);
        }
    }

    // ── Connectivity ────────────────────────────────────────────────────────

    /// Connected groups of the segment graph, each carrying its node keys and
    /// the component/tunnel endpoints on it. Isolated nodes (no segments) are
    /// skipped. Drives both the circuit rebuild and per-segment colouring.
    pub fn groups(&self) -> Vec<Group> {
        // Union-find over node keys, unioning the two ends of every segment.
        let mut parent: HashMap<WireNodeKey, WireNodeKey> =
            self.nodes.keys().map(|k| (k, k)).collect();

        fn find(parent: &mut HashMap<WireNodeKey, WireNodeKey>, x: WireNodeKey) -> WireNodeKey {
            let mut root = x;
            while parent[&root] != root {
                root = parent[&root];
            }
            // Path compression.
            let mut cur = x;
            while parent[&cur] != root {
                let next = parent[&cur];
                parent.insert(cur, root);
                cur = next;
            }
            root
        }

        for s in self.segments.values() {
            let ra = find(&mut parent, s.a);
            let rb = find(&mut parent, s.b);
            if ra != rb {
                parent.insert(ra, rb);
            }
        }

        let mut by_root: HashMap<WireNodeKey, Group> = HashMap::new();
        // Only nodes that carry at least one segment form a group; an orphan
        // node (should not normally exist post-cleanup) contributes nothing.
        for k in self.nodes.keys() {
            if self.degree(k) == 0 {
                continue;
            }
            let root = find(&mut parent, k);
            let g = by_root.entry(root).or_insert_with(|| Group {
                nodes: Vec::new(),
                pins: Vec::new(),
                tunnels: Vec::new(),
            });
            g.nodes.push(k);
            match self.nodes[k].attach {
                NodeAttach::Free => {}
                NodeAttach::Pin(pck, pin) => g.pins.push((pck, pin)),
                NodeAttach::Tunnel(ptk) => g.tunnels.push(ptk),
            }
        }

        by_root.into_values().collect()
    }

    // ── Hit testing (screen space) ──────────────────────────────────────────

    /// The node under `pos`, if any (within the pin hit radius).
    pub fn node_at_pos(&self, pos: Pos2, pan: Vec2) -> Option<WireNodeKey> {
        self.nodes
            .iter()
            .find(|(_, n)| Self::to_screen(n.pos, pan).distance(pos) <= HIT_RADIUS)
            .map(|(k, _)| k)
    }

    /// The segment nearest to `pos` (within the hit radius) and the on-grid
    /// point along it closest to `pos` - the point a branch would tap.
    pub fn segment_at_pos(&self, pos: Pos2, pan: Vec2) -> Option<(WireSegKey, GridPos)> {
        let mut best: Option<(WireSegKey, GridPos, f32)> = None;
        for (k, s) in self.segments.iter() {
            let a = Self::to_screen(self.nodes[s.a].pos, pan);
            let b = Self::to_screen(self.nodes[s.b].pos, pan);
            let (dist, proj) = point_segment(pos, a, b);
            if dist <= HIT_RADIUS && best.as_ref().is_none_or(|(_, _, d)| dist < *d) {
                let gp = snap_to_grid(proj, pan);
                best = Some((k, gp, dist));
            }
        }
        best.map(|(k, gp, _)| (k, gp))
    }
}

// Distance from p to the segment [a,b] plus the closest point on it.
fn point_segment(p: Pos2, a: Pos2, b: Pos2) -> (f32, Pos2) {
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 == 0.0 {
        return (p.distance(a), a);
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    let proj = a + ab * t;
    (p.distance(proj), proj)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fabricate distinct PlacedCompKeys without a whole app.
    fn comp_keys(n: usize) -> Vec<PlacedCompKey> {
        let mut sm: SlotMap<PlacedCompKey, ()> = SlotMap::with_key();
        (0..n).map(|_| sm.insert(())).collect()
    }

    #[test]
    fn test_add_route_two_pins_one_group() {
        let mut w = Wiring::new();
        let c = comp_keys(2);
        w.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c[0], PinId::output(0)),
            NodeAttach::Pin(c[1], PinId::input(0)),
        );
        let groups = w.groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].pins.len(), 2);
    }

    #[test]
    fn test_branch_midwire_splits_and_joins() {
        let mut w = Wiring::new();
        let c = comp_keys(3);
        // A horizontal wire between two pins.
        w.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c[0], PinId::output(0)),
            NodeAttach::Pin(c[1], PinId::input(0)),
        );
        // Branch straight down from the middle of that wire to a third pin.
        w.add_route(
            &[GridPos::new(5, 0), GridPos::new(5, 5)],
            NodeAttach::Free,
            NodeAttach::Pin(c[2], PinId::input(0)),
        );
        // The original segment was split at [5,0] (a junction), and all three
        // pins now share one group.
        let groups = w.groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].pins.len(), 3);
        // Two halves of the split + the branch = 3 segments.
        assert_eq!(w.segments.len(), 3);
    }

    #[test]
    fn test_delete_branch_segment_prunes_and_splits() {
        let mut w = Wiring::new();
        let c = comp_keys(3);
        w.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c[0], PinId::output(0)),
            NodeAttach::Pin(c[1], PinId::input(0)),
        );
        w.add_route(
            &[GridPos::new(5, 0), GridPos::new(5, 5)],
            NodeAttach::Free,
            NodeAttach::Pin(c[2], PinId::input(0)),
        );
        // Delete the vertical branch segment (the one that is not horizontal).
        let branch = w
            .segments
            .iter()
            .find(|(_, s)| w.nodes[s.a].pos.x == w.nodes[s.b].pos.x)
            .map(|(k, _)| k)
            .unwrap();
        w.delete_segment(branch);
        // The third pin's node is gone (orphaned), and the main wire still joins
        // the first two pins.
        let groups = w.groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].pins.len(), 2);
        assert!(w
            .nodes
            .values()
            .all(|n| !matches!(n.attach, NodeAttach::Pin(k, _) if k == c[2])));
    }
}
