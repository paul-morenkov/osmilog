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

/// Stable id for a [`WireNode`]; survives remove + re-insert, never reused.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct WireNodeKey(pub(crate) u64);

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct WireSegKey(pub(crate) u64);

const HIT_RADIUS: f32 = 5.0;

/// What a node connects to. `Free` means an unbound corner or junction.
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

/// A segment between two nodes. `a` and `b` share a row or a column.
#[derive(Clone, Copy, Debug)]
pub struct WireSegment {
    pub a: WireNodeKey,
    pub b: WireNodeKey,
}

/// A connected group of wire nodes. `pins`/`tunnels` feed circuit nets; `nodes` is the full set.
pub struct Group {
    pub nodes: Vec<WireNodeKey>,
    pub pins: Vec<(PlacedCompKey, PinId)>,
    pub tunnels: Vec<PlacedTunnelKey>,
}

/// One invertible change to a [`Wiring`]. `after` is what redo installs; `before` is what undo restores.
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

/// Size is proportional to what the edit touched, not the whole graph.
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
    // Monotonic, never reused, so undo never aliases a removed key onto a new one.
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

    /// Inserts without recording undo — for snapshot/clipboard install paths.
    pub fn insert_node_untracked(&mut self, node: WireNode) -> WireNodeKey {
        let key = self.next_node_key();
        self.nodes.insert(key, node);
        key
    }

    pub fn insert_segment_untracked(&mut self, a: WireNodeKey, b: WireNodeKey) -> WireSegKey {
        let key = self.next_seg_key();
        self.segments.insert(key, WireSegment { a, b });
        key
    }

    // ── Iteration ───────────────────────────────────────────────────────────

    fn node_at_grid(&self, pos: GridPos) -> Option<WireNodeKey> {
        self.nodes
            .iter()
            .find(|(_, &n)| n.pos == pos)
            .map(|(k, _)| *k)
    }

    // For one node's degree. `degrees` computes all of them in one pass instead.
    pub fn degree(&self, node: WireNodeKey) -> usize {
        self.segments
            .values()
            .filter(|s| s.a == node || s.b == node)
            .count()
    }

    // Degree of every node with at least one segment, in a single pass. Nodes with
    // degree 0 are absent from the map. Avoids per-node `degree` calls in drawing.
    pub fn degrees(&self) -> HashMap<WireNodeKey, usize> {
        let mut counts: HashMap<WireNodeKey, usize> = HashMap::new();
        for s in self.segments.values() {
            *counts.entry(s.a).or_default() += 1;
            *counts.entry(s.b).or_default() += 1;
        }
        counts
    }

    // The segment `pos` lies strictly between its endpoints on, if any.
    fn segment_through(&self, pos: GridPos) -> Option<WireSegKey> {
        self.segments.iter().find_map(|(k, seg)| {
            let a = self.nodes[&seg.a].pos;
            let b = self.nodes[&seg.b].pos;
            let on = if a.x == b.x && pos.x == a.x {
                let (lo, hi) = (a.y.min(b.y), a.y.max(b.y));
                pos.y > lo && pos.y < hi
            } else if a.y == b.y && pos.y == a.y {
                let (lo, hi) = (a.x.min(b.x), a.x.max(b.x));
                pos.x > lo && pos.x < hi
            } else {
                false
            };
            on.then_some(*k)
        })
    }

    // ── Editing primitives ──────────────────────────────────────────────────
    // Each threads `&mut Vec<WiringOp>` so its caller accumulates one delta.

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
            .segments
            .values()
            .any(|s| (s.a == a && s.b == b) || (s.a == b && s.b == a));
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

    fn take_segment(&mut self, key: WireSegKey, ops: &mut Vec<WiringOp>) {
        if let Some(before) = self.segments.remove(&key) {
            ops.push(WiringOp::SetSeg {
                key,
                before: Some(before),
                after: None,
            });
        }
    }

    // Finds or creates the node at gp. If gp lands partway along a segment,
    // this splits the segment so the returned node becomes a real junction.
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

    // Only binds a node that is still Free, so an already-bound node is never clobbered.
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

    /// Adds a route through `points`; each adjacent pair must be axis-aligned.
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

    /// Inserts a disjoint subgraph. Unlike `add_route`, this never merges with
    /// existing geometry at a shared `GridPos`.
    pub fn add_subgraph(
        &mut self,
        nodes: &[(GridPos, NodeAttach)],
        segments: &[(usize, usize)],
    ) -> (Vec<WireNodeKey>, Vec<WireSegKey>, WiringDelta) {
        let mut ops = Vec::new();
        let mut node_keys = Vec::with_capacity(nodes.len());
        for &(pos, attach) in nodes {
            node_keys.push(self.insert_node(WireNode { pos, attach }, &mut ops));
        }
        let mut seg_keys = Vec::with_capacity(segments.len());
        for &(a, b) in segments {
            let key = self.next_seg_key();
            let seg = WireSegment {
                a: node_keys[a],
                b: node_keys[b],
            };
            self.segments.insert(key, seg);
            ops.push(WiringOp::SetSeg {
                key,
                before: None,
                after: Some(seg),
            });
            seg_keys.push(key);
        }
        (node_keys, seg_keys, WiringDelta(ops))
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

    fn remove_node(&mut self, node: WireNodeKey, ops: &mut Vec<WiringOp>) {
        let touching: Vec<WireSegKey> = self
            .segments
            .iter()
            .filter(|(_, s)| s.a == node || s.b == node)
            .map(|(k, _)| *k)
            .collect();
        for k in touching {
            self.take_segment(k, ops);
        }
        self.take_node(node, ops);
    }

    /// Drop all nodes bound to a removed component (and their segments).
    pub fn remove_component_nodes(&mut self, pck: PlacedCompKey) -> WiringDelta {
        let mut ops = Vec::new();
        let doomed: Vec<_> = self
            .nodes
            .iter()
            .filter(|(_, n)| matches!(n.attach, NodeAttach::Pin(k, _) if k == pck))
            .map(|(k, _)| *k)
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
            .nodes
            .iter()
            .filter(|(_, n)| matches!(n.attach, NodeAttach::Tunnel(k) if k == ptk))
            .map(|(k, _)| *k)
            .collect();
        for k in doomed {
            self.remove_node(k, &mut ops);
        }
        self.cleanup(&mut ops);
        WiringDelta(ops)
    }

    /// Removes wire nodes bound to pins a reconfigure dropped.
    pub fn prune_stale_pins(
        &mut self,
        pck: PlacedCompKey,
        n_inputs: usize,
        n_outputs: usize,
    ) -> WiringDelta {
        let mut ops = Vec::new();
        let doomed: Vec<WireNodeKey> = self
            .nodes
            .iter()
            .filter(|(_, n)| match n.attach {
                NodeAttach::Pin(k, PinId::In(i)) => k == pck && (i.0 as usize) >= n_inputs,
                NodeAttach::Pin(k, PinId::Out(i)) => k == pck && (i.0 as usize) >= n_outputs,
                _ => false,
            })
            .map(|(k, _)| *k)
            .collect();
        for k in doomed {
            self.remove_node(k, &mut ops);
        }
        self.cleanup(&mut ops);
        WiringDelta(ops)
    }

    /// Repositions nodes bound to `pck`'s pins after a move or reconfigure.
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
        let orphans: Vec<_> = self
            .nodes
            .keys()
            .filter(|&&k| self.degree(k) == 0)
            .cloned()
            .collect();
        for k in orphans {
            self.take_node(k, ops);
        }
    }

    // ── Undo / redo replay ──────────────────────────────────────────────────

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

    /// Replays ops in recorded order, so nodes exist before segments reference them.
    pub fn redo_delta(&mut self, delta: &WiringDelta) {
        for op in &delta.0 {
            self.apply_op(op);
        }
    }

    /// Replays ops in reverse, removing segments before their nodes. Restores a
    /// split's original segment after its halves are gone.
    pub fn undo_delta(&mut self, delta: &WiringDelta) {
        for op in delta.0.iter().rev() {
            self.revert_op(op);
        }
    }

    // ── Connectivity ────────────────────────────────────────────────────────

    /// Connected groups of the active segment graph. Isolated nodes are skipped.
    pub fn groups(&self) -> Vec<Group> {
        puffin::profile_function!();
        // Union-find over active node keys.
        let mut parent: HashMap<WireNodeKey, WireNodeKey> =
            self.nodes.keys().map(|&k| (k, k)).collect();

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

        // Nodes touched by an active segment; an orphan node contributes nothing.
        let mut connected: HashSet<WireNodeKey> = HashSet::new();
        for s in self.segments.values() {
            connected.insert(s.a);
            connected.insert(s.b);
            let ra = find(&mut parent, s.a);
            let rb = find(&mut parent, s.b);
            if ra != rb {
                parent.insert(ra, rb);
            }
        }

        let mut by_root: HashMap<WireNodeKey, Group> = HashMap::new();
        for (&k, node) in &self.nodes {
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
        self.nodes
            .iter()
            .find(|(_, n)| camera.grid_to_screen(n.pos).distance(pos) <= hit_r)
            .map(|(k, _)| *k)
    }

    /// Nearest active segment to `pos`, plus the on-grid point a branch would tap.
    pub fn segment_at_pos(&self, pos: Pos2, camera: Camera) -> Option<(WireSegKey, GridPos)> {
        let hit_r = camera.scale(HIT_RADIUS);
        let mut best: Option<(WireSegKey, GridPos, f32)> = None;
        for (k, s) in &self.segments {
            let a = camera.grid_to_screen(self.nodes[&s.a].pos);
            let b = camera.grid_to_screen(self.nodes[&s.b].pos);
            let (dist, proj) = point_segment(pos, a, b);
            if dist <= hit_r && best.as_ref().is_none_or(|(_, _, d)| dist < *d) {
                let gp = camera.screen_to_grid(proj);
                best = Some((*k, gp, dist));
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
        (w.nodes.len(), w.segments.len(), groups.len() * 1000 + pins)
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
        // Split at [5,0] joins all three pins into one group.
        let groups = w.groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].pins.len(), 3);
        // Two split halves + the branch = 3 active segments (pre-split segment gone).
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
        let branch = w
            .segments
            .iter()
            .find(|(_, s)| w.nodes[&s.a].pos.x == w.nodes[&s.b].pos.x)
            .map(|(k, _)| k)
            .unwrap();
        w.delete_segment(*branch);
        let groups = w.groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].pins.len(), 2);
        assert!(w
            .nodes
            .iter()
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
            .segments
            .iter()
            .find(|(_, s)| w.nodes[&s.a].pos.x == w.nodes[&s.b].pos.x)
            .map(|(k, _)| k)
            .unwrap();
        let delta = w.delete_segment(*branch);
        assert!(!delta.is_empty());
        let after = snapshot(&w);
        assert_ne!(before, after);

        w.undo_delta(&delta);
        assert_eq!(snapshot(&w), before);
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
        let seg = *w.segments.keys().next().unwrap();
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
        assert_eq!(w.nodes.len(), 2);
        assert_eq!(w.segments.len(), 1);
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
        let before_nodes = w.nodes.len();

        // Unlike add_route, a coincident GridPos does not dedupe into the existing node.
        w.add_subgraph(&[(GridPos::new(0, 0), NodeAttach::Free)], &[]);
        assert_eq!(w.nodes.len(), before_nodes + 1);
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
        let seg = *w.segments.keys().next().unwrap();
        let delta = w.delete_segment(seg);
        // Deletion genuinely removes the entry (no tombstone left behind).
        assert!(!w.segments.contains_key(&seg));
        // Undo re-inserts it under the same key from the delta's payload.
        w.undo_delta(&delta);
        assert_eq!(w.segments.len(), 1);
        assert!(w.segments.contains_key(&seg));
    }
}
