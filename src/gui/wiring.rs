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
//!
//! ## Tombstoning (undo deltas)
//!
//! Editing never `remove()`s from the `SlotMap`s. A "deleted" node/segment is
//! instead flagged `active = false`, so its key stays valid forever and every
//! edit can be recorded as a compact, invertible [`WiringDelta`] of bit flips
//! (see [`WiringOp`]) rather than a whole-graph snapshot. Undo/redo just flip
//! `active` back and forth, which is why keys must never move. Every
//! connectivity/hit/drawing read therefore iterates through [`active_nodes`]/
//! [`active_segments`], never the raw maps. Tombstones accumulate with
//! cumulative edits (not circuit size); [`remove_unreferenced_tombstones`]
//! reclaims them once no history entry references them.
//!
//! [`active_nodes`]: Wiring::active_nodes
//! [`active_segments`]: Wiring::active_segments
//! [`remove_unreferenced_tombstones`]: Wiring::remove_unreferenced_tombstones

use std::collections::{HashMap, HashSet};

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
    /// `false` marks a tombstone - a node kept in the map (so its key stays
    /// valid for undo) but excluded from every connectivity/drawing read. See
    /// the module-level "Tombstoning" note.
    pub active: bool,
}

/// An axis-aligned segment between two nodes (invariant: `a.pos` and `b.pos`
/// share a row or a column).
#[derive(Clone, Copy, Debug)]
pub struct WireSegment {
    pub a: WireNodeKey,
    pub b: WireNodeKey,
    /// `false` marks a tombstone (see [`WireNode::active`]).
    pub active: bool,
}

/// The endpoints of one connected group of wire, in GUI keys. `pins` and
/// `tunnels` are what the app links into a single circuit net; `nodes` is the
/// full node set (used to colour every segment in the group).
pub struct Group {
    pub nodes: Vec<WireNodeKey>,
    pub pins: Vec<(PlacedCompKey, PinId)>,
    pub tunnels: Vec<PlacedTunnelKey>,
}

/// One primitive, invertible change to a [`Wiring`]. A [`WiringDelta`] is an
/// ordered list of these, recorded by the editing methods and consumed by
/// `undo_delta`/`redo_delta`. Because edits never remove keys (they tombstone),
/// every change reduces to flipping an `active` bit or swapping a node's attach
/// - each trivially reversible from the data it carries.
#[derive(Clone, Debug)]
pub enum WiringOp {
    /// A node's `active` flag was set to `active` (from `!active`). Covers both
    /// creation (`false`->`true`) and deletion (`true`->`false`).
    NodeActive { key: WireNodeKey, active: bool },
    /// A segment's `active` flag was set to `active` (from `!active`).
    SegActive { key: WireSegKey, active: bool },
    /// A node's attachment changed from `old` to `new`.
    SetAttach {
        node: WireNodeKey,
        old: NodeAttach,
        new: NodeAttach,
    },
}

/// The recorded effect of one wiring edit, enough to reverse or replay it. Its
/// stored size is proportional to the entries the edit touched, never the whole
/// graph. An empty delta means the edit changed nothing.
#[derive(Clone, Debug, Default)]
pub struct WiringDelta(Vec<WiringOp>);

impl WiringDelta {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Add every node/segment key this delta references into the given sets -
    /// used to decide which tombstones a GC pass must keep alive (see
    /// [`Wiring::remove_unreferenced_tombstones`]).
    pub fn collect_keys(
        &self,
        nodes: &mut HashSet<WireNodeKey>,
        segs: &mut HashSet<WireSegKey>,
    ) {
        for op in &self.0 {
            match op {
                WiringOp::NodeActive { key, .. } => {
                    nodes.insert(*key);
                }
                WiringOp::SegActive { key, .. } => {
                    segs.insert(*key);
                }
                WiringOp::SetAttach { node, .. } => {
                    nodes.insert(*node);
                }
            }
        }
    }
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

    // ── Active (non-tombstone) iteration ────────────────────────────────────
    // Every connectivity/hit/drawing read must go through these two, never the
    // raw `nodes`/`segments` maps, so tombstones stay invisible.

    /// Iterate live (non-tombstoned) nodes.
    pub fn active_nodes(&self) -> impl Iterator<Item = (WireNodeKey, &WireNode)> {
        self.nodes.iter().filter(|(_, n)| n.active)
    }

    /// Iterate live (non-tombstoned) segments.
    pub fn active_segments(&self) -> impl Iterator<Item = (WireSegKey, &WireSegment)> {
        self.segments.iter().filter(|(_, s)| s.active)
    }

    // ── Geometry helpers ────────────────────────────────────────────────────

    fn to_screen(gp: GridPos, pan: Vec2) -> Pos2 {
        egui::pos2(
            gp.x as f32 * GRID_SIZE + pan.x,
            gp.y as f32 * GRID_SIZE + pan.y,
        )
    }

    fn node_at_grid(&self, gp: GridPos) -> Option<WireNodeKey> {
        self.active_nodes().find(|(_, n)| n.pos == gp).map(|(k, _)| k)
    }

    // Count of active segments incident on a node (its degree). Used both for
    // cleanup (degree 0 -> orphan) and drawing (degree >= 3 -> junction dot).
    pub fn degree(&self, node: WireNodeKey) -> usize {
        self.active_segments()
            .filter(|(_, s)| s.a == node || s.b == node)
            .count()
    }

    // The active segment (if any) that gp lies strictly inside: colinear,
    // axis-aligned, and between (not on) the endpoints. Splitting here is what
    // turns a mid-wire tap into a real junction.
    fn segment_through(&self, gp: GridPos) -> Option<WireSegKey> {
        self.active_segments().find_map(|(k, seg)| {
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
    //
    // Each of these threads `&mut Vec<WiringOp>` so its caller (one of the five
    // public mutators) accumulates a single delta. "Delete" means tombstone
    // (`active = false`) plus a recorded flip - never `SlotMap::remove`.

    fn add_segment(&mut self, a: WireNodeKey, b: WireNodeKey, ops: &mut Vec<WiringOp>) {
        if a == b {
            return;
        }
        let exists = self
            .active_segments()
            .any(|(_, s)| (s.a == a && s.b == b) || (s.a == b && s.b == a));
        if !exists {
            let key = self.segments.insert(WireSegment {
                a,
                b,
                active: true,
            });
            ops.push(WiringOp::SegActive { key, active: true });
        }
    }

    // Find-or-create the active node at gp. If gp lands partway along an active
    // segment, that segment is split so the returned node becomes a real
    // junction. New nodes start `Free`.
    fn resolve_point(&mut self, gp: GridPos, ops: &mut Vec<WiringOp>) -> WireNodeKey {
        if let Some(k) = self.node_at_grid(gp) {
            return k;
        }
        if let Some(seg_key) = self.segment_through(gp) {
            let seg = self.segments[seg_key];
            self.segments[seg_key].active = false;
            ops.push(WiringOp::SegActive {
                key: seg_key,
                active: false,
            });
            let mid = self.nodes.insert(WireNode {
                pos: gp,
                attach: NodeAttach::Free,
                active: true,
            });
            ops.push(WiringOp::NodeActive {
                key: mid,
                active: true,
            });
            self.add_segment(seg.a, mid, ops);
            self.add_segment(mid, seg.b, ops);
            return mid;
        }
        let key = self.nodes.insert(WireNode {
            pos: gp,
            attach: NodeAttach::Free,
            active: true,
        });
        ops.push(WiringOp::NodeActive { key, active: true });
        key
    }

    // Only sets an attachment onto a node that is still Free, so a wire ending
    // on a pin binds that pin without clobbering an already-bound node.
    fn set_attach_if_free(&mut self, node: WireNodeKey, attach: NodeAttach, ops: &mut Vec<WiringOp>) {
        if attach != NodeAttach::Free {
            let n = &mut self.nodes[node];
            if n.attach == NodeAttach::Free {
                n.attach = attach;
                ops.push(WiringOp::SetAttach {
                    node,
                    old: NodeAttach::Free,
                    new: attach,
                });
            }
        }
    }

    /// Add a polyline wire through `points` (grid coords, each adjacent pair
    /// axis-aligned). `start_attach`/`end_attach` bind the first/last node to a
    /// pin or tunnel when the route lands on one; interior points stay `Free`.
    /// Returns the delta of everything created/split.
    pub fn add_route(
        &mut self,
        points: &[GridPos],
        start_attach: NodeAttach,
        end_attach: NodeAttach,
    ) -> WiringDelta {
        let mut ops = Vec::new();
        if points.len() < 2 {
            return WiringDelta(ops);
        }
        let mut keys = Vec::with_capacity(points.len());
        for &p in points {
            keys.push(self.resolve_point(p, &mut ops));
        }
        self.set_attach_if_free(keys[0], start_attach, &mut ops);
        self.set_attach_if_free(*keys.last().unwrap(), end_attach, &mut ops);
        for w in keys.windows(2) {
            self.add_segment(w[0], w[1], &mut ops);
        }
        WiringDelta(ops)
    }

    /// Tombstone a segment, then any node left with no active segments.
    pub fn delete_segment(&mut self, seg: WireSegKey) -> WiringDelta {
        let mut ops = Vec::new();
        match self.segments.get_mut(seg) {
            Some(s) if s.active => s.active = false,
            _ => return WiringDelta(ops),
        }
        ops.push(WiringOp::SegActive {
            key: seg,
            active: false,
        });
        self.cleanup(&mut ops);
        WiringDelta(ops)
    }

    // Tombstone a node and every active segment touching it.
    fn remove_node(&mut self, node: WireNodeKey, ops: &mut Vec<WiringOp>) {
        let touching: Vec<WireSegKey> = self
            .active_segments()
            .filter(|(_, s)| s.a == node || s.b == node)
            .map(|(k, _)| k)
            .collect();
        for k in touching {
            self.segments[k].active = false;
            ops.push(WiringOp::SegActive {
                key: k,
                active: false,
            });
        }
        if self.nodes[node].active {
            self.nodes[node].active = false;
            ops.push(WiringOp::NodeActive {
                key: node,
                active: false,
            });
        }
    }

    /// Drop all nodes bound to a removed component (and their segments).
    pub fn remove_component_nodes(&mut self, pck: PlacedCompKey) -> WiringDelta {
        let mut ops = Vec::new();
        let doomed: Vec<WireNodeKey> = self
            .active_nodes()
            .filter(|(_, n)| matches!(n.attach, NodeAttach::Pin(k, _) if k == pck))
            .map(|(k, _)| k)
            .collect();
        for k in doomed {
            self.remove_node(k, &mut ops);
        }
        self.cleanup(&mut ops);
        WiringDelta(ops)
    }

    /// Drop all nodes bound to a removed tunnel (and their segments).
    pub fn remove_tunnel_nodes(&mut self, ptk: PlacedTunnelKey) -> WiringDelta {
        let mut ops = Vec::new();
        let doomed: Vec<WireNodeKey> = self
            .active_nodes()
            .filter(|(_, n)| matches!(n.attach, NodeAttach::Tunnel(k) if k == ptk))
            .map(|(k, _)| k)
            .collect();
        for k in doomed {
            self.remove_node(k, &mut ops);
        }
        self.cleanup(&mut ops);
        WiringDelta(ops)
    }

    /// After a component is reconfigured to fewer pins, drop wire nodes bound to
    /// pins that no longer exist.
    pub fn prune_stale_pins(
        &mut self,
        pck: PlacedCompKey,
        n_inputs: usize,
        n_outputs: usize,
    ) -> WiringDelta {
        let mut ops = Vec::new();
        let doomed: Vec<WireNodeKey> = self
            .active_nodes()
            .filter(|(_, n)| match n.attach {
                NodeAttach::Pin(k, PinId::In(i)) => k == pck && (i.0 as usize) >= n_inputs,
                NodeAttach::Pin(k, PinId::Out(i)) => k == pck && (i.0 as usize) >= n_outputs,
                _ => false,
            })
            .map(|(k, _)| k)
            .collect();
        for k in doomed {
            self.remove_node(k, &mut ops);
        }
        self.cleanup(&mut ops);
        WiringDelta(ops)
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

    // Tombstone nodes with no incident active segments (orphans left by a
    // delete/split).
    fn cleanup(&mut self, ops: &mut Vec<WiringOp>) {
        let orphans: Vec<WireNodeKey> = self
            .active_nodes()
            .map(|(k, _)| k)
            .filter(|&k| self.degree(k) == 0)
            .collect();
        for k in orphans {
            self.nodes[k].active = false;
            ops.push(WiringOp::NodeActive {
                key: k,
                active: false,
            });
        }
    }

    // ── Undo / redo replay ──────────────────────────────────────────────────
    // Representation is complete and tested, but nothing wires these to a UI
    // gesture yet (see the crate's undo/redo In-Progress note).

    fn apply_op(&mut self, op: &WiringOp) {
        match op {
            WiringOp::NodeActive { key, active } => {
                if let Some(n) = self.nodes.get_mut(*key) {
                    n.active = *active;
                }
            }
            WiringOp::SegActive { key, active } => {
                if let Some(s) = self.segments.get_mut(*key) {
                    s.active = *active;
                }
            }
            WiringOp::SetAttach { node, new, .. } => {
                if let Some(n) = self.nodes.get_mut(*node) {
                    n.attach = *new;
                }
            }
        }
    }

    fn revert_op(&mut self, op: &WiringOp) {
        match op {
            WiringOp::NodeActive { key, active } => {
                if let Some(n) = self.nodes.get_mut(*key) {
                    n.active = !*active;
                }
            }
            WiringOp::SegActive { key, active } => {
                if let Some(s) = self.segments.get_mut(*key) {
                    s.active = !*active;
                }
            }
            WiringOp::SetAttach { node, old, .. } => {
                if let Some(n) = self.nodes.get_mut(*node) {
                    n.attach = *old;
                }
            }
        }
    }

    /// Re-apply a delta (redo). Ops run in recorded order, which always creates
    /// nodes before the segments that reference them.
    pub fn redo_delta(&mut self, delta: &WiringDelta) {
        for op in &delta.0 {
            self.apply_op(op);
        }
    }

    /// Reverse a delta (undo). Ops run in reverse order, which always restores
    /// nodes before the segments that reference them.
    pub fn undo_delta(&mut self, delta: &WiringDelta) {
        for op in delta.0.iter().rev() {
            self.revert_op(op);
        }
    }

    /// Reclaim tombstones: permanently `remove()` any inactive node/segment
    /// whose key is in neither keep set. Callers pass the union of keys still
    /// referenced by the undo history (see `History::referenced_wire_keys`), so
    /// only genuinely unreachable tombstones are dropped. Unwired for now -
    /// meant to run periodically or when history is cleared/branched.
    pub fn remove_unreferenced_tombstones(
        &mut self,
        keep_nodes: &HashSet<WireNodeKey>,
        keep_segs: &HashSet<WireSegKey>,
    ) {
        self.segments
            .retain(|k, s| s.active || keep_segs.contains(&k));
        self.nodes.retain(|k, n| n.active || keep_nodes.contains(&k));
    }

    // ── Connectivity ────────────────────────────────────────────────────────

    /// Connected groups of the active segment graph, each carrying its node keys
    /// and the component/tunnel endpoints on it. Isolated nodes (no active
    /// segments) are skipped. Drives both the circuit rebuild and per-segment
    /// colouring.
    pub fn groups(&self) -> Vec<Group> {
        // Union-find over active node keys, unioning the two ends of every
        // active segment.
        let mut parent: HashMap<WireNodeKey, WireNodeKey> =
            self.active_nodes().map(|(k, _)| (k, k)).collect();

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

        for (_, s) in self.active_segments() {
            let ra = find(&mut parent, s.a);
            let rb = find(&mut parent, s.b);
            if ra != rb {
                parent.insert(ra, rb);
            }
        }

        let mut by_root: HashMap<WireNodeKey, Group> = HashMap::new();
        // Only nodes that carry at least one active segment form a group; an
        // orphan node (should not normally exist post-cleanup) contributes
        // nothing.
        for (k, node) in self.active_nodes() {
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
            match node.attach {
                NodeAttach::Free => {}
                NodeAttach::Pin(pck, pin) => g.pins.push((pck, pin)),
                NodeAttach::Tunnel(ptk) => g.tunnels.push(ptk),
            }
        }

        by_root.into_values().collect()
    }

    // ── Hit testing (screen space) ──────────────────────────────────────────

    /// The active node under `pos`, if any (within the pin hit radius).
    pub fn node_at_pos(&self, pos: Pos2, pan: Vec2) -> Option<WireNodeKey> {
        self.active_nodes()
            .find(|(_, n)| Self::to_screen(n.pos, pan).distance(pos) <= HIT_RADIUS)
            .map(|(k, _)| k)
    }

    /// The active segment nearest to `pos` (within the hit radius) and the
    /// on-grid point along it closest to `pos` - the point a branch would tap.
    pub fn segment_at_pos(&self, pos: Pos2, pan: Vec2) -> Option<(WireSegKey, GridPos)> {
        let mut best: Option<(WireSegKey, GridPos, f32)> = None;
        for (k, s) in self.active_segments() {
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

    // A stable fingerprint of the live graph, for asserting undo/redo returns
    // the wiring to a prior state.
    fn snapshot(w: &Wiring) -> (usize, usize, usize) {
        let groups = w.groups();
        let pins: usize = groups.iter().map(|g| g.pins.len()).sum();
        (
            w.active_nodes().count(),
            w.active_segments().count(),
            groups.len() * 1000 + pins,
        )
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
        // Two halves of the split + the branch = 3 active segments (the
        // pre-split segment is tombstoned, not counted).
        assert_eq!(w.active_segments().count(), 3);
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
            .active_segments()
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
            .active_nodes()
            .all(|(_, n)| !matches!(n.attach, NodeAttach::Pin(k, _) if k == c[2])));
    }

    #[test]
    fn test_delete_segment_delta_round_trips() {
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
        let before = snapshot(&w);

        let branch = w
            .active_segments()
            .find(|(_, s)| w.nodes[s.a].pos.x == w.nodes[s.b].pos.x)
            .map(|(k, _)| k)
            .unwrap();
        let delta = w.delete_segment(branch);
        assert!(!delta.is_empty());
        let after = snapshot(&w);
        assert_ne!(before, after);

        // Undo restores the pre-delete graph exactly...
        w.undo_delta(&delta);
        assert_eq!(snapshot(&w), before);
        // ...and redo reproduces the post-delete graph exactly.
        w.redo_delta(&delta);
        assert_eq!(snapshot(&w), after);
    }

    #[test]
    fn test_add_route_delta_round_trips_with_split() {
        let mut w = Wiring::new();
        let c = comp_keys(3);
        w.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c[0], PinId::output(0)),
            NodeAttach::Pin(c[1], PinId::input(0)),
        );
        let before = snapshot(&w);

        // This add_route splits the existing segment - the interesting delta.
        let delta = w.add_route(
            &[GridPos::new(5, 0), GridPos::new(5, 5)],
            NodeAttach::Free,
            NodeAttach::Pin(c[2], PinId::input(0)),
        );
        let after = snapshot(&w);
        assert_ne!(before, after);

        w.undo_delta(&delta);
        assert_eq!(snapshot(&w), before);
        w.redo_delta(&delta);
        assert_eq!(snapshot(&w), after);
    }

    #[test]
    fn test_remove_component_nodes_delta_round_trips() {
        let mut w = Wiring::new();
        let c = comp_keys(2);
        w.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c[0], PinId::output(0)),
            NodeAttach::Pin(c[1], PinId::input(0)),
        );
        let before = snapshot(&w);

        let delta = w.remove_component_nodes(c[0]);
        let after = snapshot(&w);
        assert_ne!(before, after);

        w.undo_delta(&delta);
        assert_eq!(snapshot(&w), before);
        w.redo_delta(&delta);
        assert_eq!(snapshot(&w), after);
    }

    #[test]
    fn test_delete_inactive_segment_is_empty_delta() {
        let mut w = Wiring::new();
        let c = comp_keys(2);
        w.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c[0], PinId::output(0)),
            NodeAttach::Pin(c[1], PinId::input(0)),
        );
        let seg = w.active_segments().next().map(|(k, _)| k).unwrap();
        assert!(!w.delete_segment(seg).is_empty());
        // Deleting the same (now tombstoned) segment again changes nothing.
        assert!(w.delete_segment(seg).is_empty());
    }

    #[test]
    fn test_remove_unreferenced_tombstones_reclaims_only_unkept() {
        let mut w = Wiring::new();
        let c = comp_keys(2);
        w.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c[0], PinId::output(0)),
            NodeAttach::Pin(c[1], PinId::input(0)),
        );
        let seg = w.active_segments().next().map(|(k, _)| k).unwrap();
        let delta = w.delete_segment(seg);
        // The map still holds the tombstoned entries.
        assert!(!w.segments.is_empty());

        // Keeping the delta's keys preserves them for a possible undo.
        let mut keep_nodes = HashSet::new();
        let mut keep_segs = HashSet::new();
        delta.collect_keys(&mut keep_nodes, &mut keep_segs);
        w.remove_unreferenced_tombstones(&keep_nodes, &keep_segs);
        assert!(w.segments.contains_key(seg));
        // Undo still works after a keep-everything GC.
        w.undo_delta(&delta);
        assert_eq!(w.active_segments().count(), 1);

        // With nothing kept, GC drops the tombstones outright.
        w.delete_segment(seg);
        w.remove_unreferenced_tombstones(&HashSet::new(), &HashSet::new());
        assert!(!w.segments.contains_key(seg));
    }
}
