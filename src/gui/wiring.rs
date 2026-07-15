//! GUI-side wiring: a geometry + topology graph, kept separate from the
//! simulation `Circuit`. Wires are grid-aligned `WireNode`s joined by
//! axis-aligned `WireSegment`s; connectivity is *derived* via union-find
//! (`groups`), and `OsmilogApp::rebuild_circuit` is the only place that turns
//! a group into `Circuit::link`/`link_tunnel` calls.
//!
//! Attachment is by *key*, not position: a wire merely crossing a pin or
//! another wire does not connect. `resolve_point` creates a junction by
//! splitting a segment when a route starts/ends partway along it.
//!
//! ## Stable keys, move-based undo
//!
//! Nodes and segments live in plain `HashMap`s keyed by app-assigned `u64`
//! ids ([`WireNodeKey`]/[`WireSegKey`]) allocated from monotonic counters and
//! never reused. Deleting genuinely `remove()`s the entry; undo re-inserts it
//! under the *same* key, so keys never dangle. Each edit records a compact,
//! invertible [`WiringDelta`] of [`WiringOp`]s that carry the before/after
//! payloads for every node/segment slot they touched (`before`/`after` = the
//! `Option<..>` map value on each side), so replaying the delta forward (redo)
//! or backward (undo) reconstructs the exact graph - including `resolve_point`'s
//! mid-wire split.

use std::collections::{HashMap, HashSet};

use egui::Pos2;

use crate::gui::app::{PlacedCompKey, PlacedTunnelKey};
use crate::gui::geometry::{Camera, GridPos};
use crate::sim::component::PinId;

/// Stable, app-assigned id for a [`WireNode`]. Survives remove + re-insert so
/// undo restores a deleted node under its original key; never reused.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct WireNodeKey(pub(crate) u64);

/// Stable, app-assigned id for a [`WireSegment`] (see [`WireNodeKey`]).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct WireSegKey(pub(crate) u64);

// How close (in pixels) the cursor must be to a segment/node to hit it.
const HIT_RADIUS: f32 = 5.0;

/// What a wire node is bound to. `Free` nodes are corners, junctions, or
/// dangling endpoints; the other two tie a node to a pin.
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

/// One connected group of wire nodes. `pins`/`tunnels` are what gets linked
/// into a circuit net; `nodes` is the full set (used for colouring).
pub struct Group {
    pub nodes: Vec<WireNodeKey>,
    pub pins: Vec<(PlacedCompKey, PinId)>,
    pub tunnels: Vec<PlacedTunnelKey>,
}

/// One invertible change to a [`Wiring`], recorded into a [`WiringDelta`] and
/// consumed by `undo_delta`/`redo_delta`. Each op is the before/after value of
/// a single node or segment slot (`None` = the slot is empty). `after` is what
/// redo installs; `before` is what undo restores. This uniformly covers
/// insertion (`before: None`), deletion (`after: None`), and in-place edits
/// such as an attach change (both `Some`).
#[derive(Clone, Copy, Debug)]
pub enum WiringOp {
    SetNode {
        key: WireNodeKey,
        before: Option<WireNode>,
        after: Option<WireNode>,
    },
    SetSeg {
        key: WireSegKey,
        before: Option<WireSegment>,
        after: Option<WireSegment>,
    },
}

/// The recorded effect of one wiring edit. Size is proportional to what the
/// edit touched, never the whole graph. Empty means nothing changed.
#[derive(Clone, Debug, Default)]
pub struct WiringDelta(Vec<WiringOp>);

impl WiringDelta {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Default, Clone, Debug)]
pub struct Wiring {
    pub nodes: HashMap<WireNodeKey, WireNode>,
    pub segments: HashMap<WireSegKey, WireSegment>,
    // Monotonic id allocators; never reused, so undo re-inserts a removed
    // node/segment under its original key with no risk of aliasing a later one.
    next_node: u64,
    next_seg: u64,
}

impl Wiring {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_node_key(&mut self) -> WireNodeKey {
        let k = WireNodeKey(self.next_node);
        self.next_node += 1;
        k
    }

    fn next_seg_key(&mut self) -> WireSegKey {
        let k = WireSegKey(self.next_seg);
        self.next_seg += 1;
        k
    }

    /// Inserts a node under a fresh key without recording an undo op - for the
    /// history-free snapshot/clipboard install paths. Returns the key.
    pub fn insert_node_untracked(&mut self, node: WireNode) -> WireNodeKey {
        let key = self.next_node_key();
        self.nodes.insert(key, node);
        key
    }

    /// Inserts a segment under a fresh key without recording an undo op (see
    /// `insert_node_untracked`). Returns the key.
    pub fn insert_segment_untracked(&mut self, a: WireNodeKey, b: WireNodeKey) -> WireSegKey {
        let key = self.next_seg_key();
        self.segments.insert(key, WireSegment { a, b });
        key
    }

    // ── Iteration ───────────────────────────────────────────────────────────
    // Thin accessors yielding owned keys (like the old slotmap iterators), so
    // callers stay unchanged. No tombstones exist any more, so these iterate
    // the whole map.

    pub fn active_nodes(&self) -> impl Iterator<Item = (WireNodeKey, &WireNode)> {
        self.nodes.iter().map(|(k, n)| (*k, n))
    }

    pub fn active_segments(&self) -> impl Iterator<Item = (WireSegKey, &WireSegment)> {
        self.segments.iter().map(|(k, s)| (*k, s))
    }

    fn node_at_grid(&self, gp: GridPos) -> Option<WireNodeKey> {
        self.active_nodes()
            .find(|(_, n)| n.pos == gp)
            .map(|(k, _)| k)
    }

    // Count of active segments incident on a node (its degree). Scans every
    // segment, so callers needing the degree of *one* node use this; callers
    // needing *many* (drawing all junction dots) use `degrees` instead, which
    // computes them all in a single pass rather than one scan per node.
    pub fn degree(&self, node: WireNodeKey) -> usize {
        self.active_segments()
            .filter(|(_, s)| s.a == node || s.b == node)
            .count()
    }

    // Incident active-segment count for every node that has at least one, in a
    // single pass over the segments (nodes with degree 0 are simply absent, so
    // look them up with `.get(&k).copied().unwrap_or(0)`). Replaces per-node
    // `degree` calls in the per-frame drawing path, which were O(nodes x segments).
    pub fn degrees(&self) -> HashMap<WireNodeKey, usize> {
        let mut counts: HashMap<WireNodeKey, usize> = HashMap::new();
        for (_, s) in self.active_segments() {
            *counts.entry(s.a).or_default() += 1;
            *counts.entry(s.b).or_default() += 1;
        }
        counts
    }

    // The active segment (if any) that gp lies strictly inside: colinear,
    // axis-aligned, and between (not on) the endpoints. Splitting here is what
    // turns a mid-wire tap into a real junction.
    fn segment_through(&self, gp: GridPos) -> Option<WireSegKey> {
        self.active_segments().find_map(|(k, seg)| {
            let a = self.nodes[&seg.a].pos;
            let b = self.nodes[&seg.b].pos;
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
    // public mutators) accumulates a single delta. "Delete" genuinely removes
    // the node/segment; the recorded op carries its payload so undo re-inserts
    // it under the same key.

    // Inserts a node under a fresh key and records the op. Returns the key.
    fn insert_node(&mut self, node: WireNode, ops: &mut Vec<WiringOp>) -> WireNodeKey {
        let key = self.next_node_key();
        self.nodes.insert(key, node);
        ops.push(WiringOp::SetNode {
            key,
            before: None,
            after: Some(node),
        });
        key
    }

    // Removes a node (if present) and records the op.
    fn take_node(&mut self, key: WireNodeKey, ops: &mut Vec<WiringOp>) {
        if let Some(before) = self.nodes.remove(&key) {
            ops.push(WiringOp::SetNode {
                key,
                before: Some(before),
                after: None,
            });
        }
    }

    fn add_segment(&mut self, a: WireNodeKey, b: WireNodeKey, ops: &mut Vec<WiringOp>) {
        if a == b {
            return;
        }
        let exists = self
            .active_segments()
            .any(|(_, s)| (s.a == a && s.b == b) || (s.a == b && s.b == a));
        if !exists {
            let key = self.next_seg_key();
            let seg = WireSegment { a, b };
            self.segments.insert(key, seg);
            ops.push(WiringOp::SetSeg {
                key,
                before: None,
                after: Some(seg),
            });
        }
    }

    // Removes a segment (if present) and records the op.
    fn take_segment(&mut self, key: WireSegKey, ops: &mut Vec<WiringOp>) {
        if let Some(before) = self.segments.remove(&key) {
            ops.push(WiringOp::SetSeg {
                key,
                before: Some(before),
                after: None,
            });
        }
    }

    // Find-or-create the node at gp. If gp lands partway along a segment, that
    // segment is split so the returned node becomes a real junction. New nodes
    // start `Free`.
    fn resolve_point(&mut self, gp: GridPos, ops: &mut Vec<WiringOp>) -> WireNodeKey {
        if let Some(k) = self.node_at_grid(gp) {
            return k;
        }
        if let Some(seg_key) = self.segment_through(gp) {
            let seg = self.segments[&seg_key];
            self.take_segment(seg_key, ops);
            let mid = self.insert_node(
                WireNode {
                    pos: gp,
                    attach: NodeAttach::Free,
                },
                ops,
            );
            self.add_segment(seg.a, mid, ops);
            self.add_segment(mid, seg.b, ops);
            return mid;
        }
        self.insert_node(
            WireNode {
                pos: gp,
                attach: NodeAttach::Free,
            },
            ops,
        )
    }

    // Only sets an attachment onto a node that is still Free, so a wire ending
    // on a pin binds that pin without clobbering an already-bound node.
    fn set_attach_if_free(
        &mut self,
        node: WireNodeKey,
        attach: NodeAttach,
        ops: &mut Vec<WiringOp>,
    ) {
        if attach != NodeAttach::Free {
            let before = self.nodes[&node];
            if before.attach == NodeAttach::Free {
                let mut after = before;
                after.attach = attach;
                self.nodes.insert(node, after);
                ops.push(WiringOp::SetNode {
                    key: node,
                    before: Some(before),
                    after: Some(after),
                });
            }
        }
    }

    /// Add a polyline wire through `points` (grid coords, each adjacent pair
    /// axis-aligned), binding the first/last node to `start_attach`/`end_attach`.
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

    /// Insert a wholly new, disjoint node/segment subgraph.
    /// Unlike `add_route`, this never merges with existing geometry at a
    /// shared `GridPos`. `segments` are index pairs into `nodes`. Returns the
    /// fresh node keys, the fresh segment keys, and the undo delta.
    pub fn add_subgraph(
        &mut self,
        nodes: &[(GridPos, NodeAttach)],
        segments: &[(usize, usize)],
    ) -> (Vec<WireNodeKey>, Vec<WireSegKey>, WiringDelta) {
        let mut ops = Vec::new();
        let mut keys = Vec::with_capacity(nodes.len());
        for &(pos, attach) in nodes {
            keys.push(self.insert_node(WireNode { pos, attach }, &mut ops));
        }
        let mut seg_keys = Vec::with_capacity(segments.len());
        for &(a, b) in segments {
            let key = self.next_seg_key();
            let seg = WireSegment {
                a: keys[a],
                b: keys[b],
            };
            self.segments.insert(key, seg);
            ops.push(WiringOp::SetSeg {
                key,
                before: None,
                after: Some(seg),
            });
            seg_keys.push(key);
        }
        (keys, seg_keys, WiringDelta(ops))
    }

    /// Remove a segment, then any node left with no incident segments.
    pub fn delete_segment(&mut self, seg: WireSegKey) -> WiringDelta {
        let mut ops = Vec::new();
        if !self.segments.contains_key(&seg) {
            return WiringDelta(ops);
        }
        self.take_segment(seg, &mut ops);
        self.cleanup(&mut ops);
        WiringDelta(ops)
    }

    // Remove a node and every segment touching it.
    fn remove_node(&mut self, node: WireNodeKey, ops: &mut Vec<WiringOp>) {
        let touching: Vec<WireSegKey> = self
            .active_segments()
            .filter(|(_, s)| s.a == node || s.b == node)
            .map(|(k, _)| k)
            .collect();
        for k in touching {
            self.take_segment(k, ops);
        }
        self.take_node(node, ops);
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

    /// After a reconfigure drops pins, remove wire nodes bound to pins that no
    /// longer exist.
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

    /// Reposition every node bound to `pck`'s pins (called after a move or
    /// reconfigure); attached segments simply stretch to follow.
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

    /// Reposition every node bound to `ptk` to the tunnel's current position.
    pub fn sync_tunnel_nodes(&mut self, ptk: PlacedTunnelKey, gp: GridPos) {
        for n in self.nodes.values_mut() {
            if let NodeAttach::Tunnel(k) = n.attach {
                if k == ptk {
                    n.pos = gp;
                }
            }
        }
    }

    // Remove nodes with no incident segments (orphans left by a delete/split).
    fn cleanup(&mut self, ops: &mut Vec<WiringOp>) {
        let orphans: Vec<WireNodeKey> = self
            .active_nodes()
            .map(|(k, _)| k)
            .filter(|&k| self.degree(k) == 0)
            .collect();
        for k in orphans {
            self.take_node(k, ops);
        }
    }

    // ── Undo / redo replay ──────────────────────────────────────────────────

    // Install `val` into the node slot (`Some` inserts, `None` removes).
    fn set_node_slot(&mut self, key: WireNodeKey, val: Option<WireNode>) {
        match val {
            Some(n) => {
                self.nodes.insert(key, n);
            }
            None => {
                self.nodes.remove(&key);
            }
        }
    }

    fn set_seg_slot(&mut self, key: WireSegKey, val: Option<WireSegment>) {
        match val {
            Some(s) => {
                self.segments.insert(key, s);
            }
            None => {
                self.segments.remove(&key);
            }
        }
    }

    fn apply_op(&mut self, op: &WiringOp) {
        match *op {
            WiringOp::SetNode { key, after, .. } => self.set_node_slot(key, after),
            WiringOp::SetSeg { key, after, .. } => self.set_seg_slot(key, after),
        }
    }

    fn revert_op(&mut self, op: &WiringOp) {
        match *op {
            WiringOp::SetNode { key, before, .. } => self.set_node_slot(key, before),
            WiringOp::SetSeg { key, before, .. } => self.set_seg_slot(key, before),
        }
    }

    /// Re-apply a delta (redo). Ops run in recorded order, so nodes are
    /// created before segments that reference them.
    pub fn redo_delta(&mut self, delta: &WiringDelta) {
        for op in &delta.0 {
            self.apply_op(op);
        }
    }

    /// Reverse a delta (undo). Ops run in reverse order, so segments are
    /// removed before the nodes they reference, and a split's original segment
    /// is restored after its halves are gone.
    pub fn undo_delta(&mut self, delta: &WiringDelta) {
        for op in delta.0.iter().rev() {
            self.revert_op(op);
        }
    }

    // ── Connectivity ────────────────────────────────────────────────────────

    /// Connected groups of the active segment graph. Isolated nodes (no
    /// active segments) are skipped. Drives both the circuit rebuild and
    /// per-segment colouring.
    pub fn groups(&self) -> Vec<Group> {
        puffin::profile_function!();
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

        // Nodes touched by at least one active segment, recorded during the same
        // pass. Only these form a group; an orphan node (should not normally
        // exist post-cleanup) contributes nothing. Tracking membership here
        // avoids a per-node `degree` scan below, which was O(nodes x segments).
        let mut connected: HashSet<WireNodeKey> = HashSet::new();
        for (_, s) in self.active_segments() {
            connected.insert(s.a);
            connected.insert(s.b);
            let ra = find(&mut parent, s.a);
            let rb = find(&mut parent, s.b);
            if ra != rb {
                parent.insert(ra, rb);
            }
        }

        let mut by_root: HashMap<WireNodeKey, Group> = HashMap::new();
        for (k, node) in self.active_nodes() {
            if !connected.contains(&k) {
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
    pub fn node_at_pos(&self, pos: Pos2, camera: Camera) -> Option<WireNodeKey> {
        let hit_r = camera.scale(HIT_RADIUS);
        self.active_nodes()
            .find(|(_, n)| camera.grid_to_screen(n.pos).distance(pos) <= hit_r)
            .map(|(k, _)| k)
    }

    /// The active segment nearest to `pos` (within the hit radius) and the
    /// on-grid point along it closest to `pos` - the point a branch would tap.
    pub fn segment_at_pos(&self, pos: Pos2, camera: Camera) -> Option<(WireSegKey, GridPos)> {
        let hit_r = camera.scale(HIT_RADIUS);
        let mut best: Option<(WireSegKey, GridPos, f32)> = None;
        for (k, s) in self.active_segments() {
            let a = camera.grid_to_screen(self.nodes[&s.a].pos);
            let b = camera.grid_to_screen(self.nodes[&s.b].pos);
            let (dist, proj) = point_segment(pos, a, b);
            if dist <= hit_r && best.as_ref().is_none_or(|(_, _, d)| dist < *d) {
                let gp = camera.screen_to_grid(proj);
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
        (0..n as u64).map(PlacedCompKey).collect()
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
            .find(|(_, s)| w.nodes[&s.a].pos.x == w.nodes[&s.b].pos.x)
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
            .find(|(_, s)| w.nodes[&s.a].pos.x == w.nodes[&s.b].pos.x)
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
    fn test_add_subgraph_creates_disjoint_graph() {
        let mut w = Wiring::new();
        let c = comp_keys(2);
        let (keys, seg_keys, _delta) = w.add_subgraph(
            &[
                (GridPos::new(0, 0), NodeAttach::Pin(c[0], PinId::output(0))),
                (GridPos::new(10, 0), NodeAttach::Pin(c[1], PinId::input(0))),
            ],
            &[(0, 1)],
        );
        assert_eq!(keys.len(), 2);
        assert_eq!(seg_keys.len(), 1);
        assert_eq!(w.active_nodes().count(), 2);
        assert_eq!(w.active_segments().count(), 1);
        let groups = w.groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].pins.len(), 2);
    }

    #[test]
    fn test_add_subgraph_delta_round_trips() {
        let mut w = Wiring::new();
        let c = comp_keys(3);
        w.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c[0], PinId::output(0)),
            NodeAttach::Pin(c[1], PinId::input(0)),
        );
        let before = snapshot(&w);

        let (_, _, delta) = w.add_subgraph(
            &[
                (GridPos::new(20, 0), NodeAttach::Pin(c[2], PinId::output(0))),
                (GridPos::new(30, 0), NodeAttach::Free),
            ],
            &[(0, 1)],
        );
        let after = snapshot(&w);
        assert_ne!(before, after);

        w.undo_delta(&delta);
        assert_eq!(snapshot(&w), before);
        w.redo_delta(&delta);
        assert_eq!(snapshot(&w), after);
    }

    #[test]
    fn test_add_subgraph_does_not_merge_with_coincident_existing_node() {
        let mut w = Wiring::new();
        let c = comp_keys(2);
        w.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c[0], PinId::output(0)),
            NodeAttach::Pin(c[1], PinId::input(0)),
        );
        let before_nodes = w.active_nodes().count();

        // A subgraph node landing on an already-occupied GridPos must not be
        // deduped/spliced into the existing node there (unlike add_route's
        // resolve_point) - it's an independent copy.
        w.add_subgraph(&[(GridPos::new(0, 0), NodeAttach::Free)], &[]);
        assert_eq!(w.active_nodes().count(), before_nodes + 1);
    }

    #[test]
    fn test_delete_segment_then_undo_round_trips() {
        let mut w = Wiring::new();
        let c = comp_keys(2);
        w.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c[0], PinId::output(0)),
            NodeAttach::Pin(c[1], PinId::input(0)),
        );
        let seg = w.active_segments().next().map(|(k, _)| k).unwrap();
        let delta = w.delete_segment(seg);
        // Deletion genuinely removes the entry (no tombstone left behind).
        assert!(!w.segments.contains_key(&seg));
        // Undo re-inserts it under the same key from the delta's payload.
        w.undo_delta(&delta);
        assert_eq!(w.active_segments().count(), 1);
        assert!(w.segments.contains_key(&seg));
    }
}
