use std::collections::{HashMap, HashSet};

use egui::{Align2, Ui, Window};
use egui_macroquad::egui::ScrollArea;
use egui_macroquad::{egui, macroquad};
use macroquad::prelude::*;
use petgraph::algo::toposort;
use petgraph::stable_graph::{EdgeIndex, NodeIndex, StableGraph};
use petgraph::visit::{EdgeFiltered, EdgeRef};
use petgraph::{Direction, Graph};
use slotmap::{DefaultKey, SecondaryMap, SlotMap};
use std::fmt::Debug;

mod components;
mod utils;
mod wires;

use components::{color_from_signal, CompEvent, Component, PinIndex, Signal, TunnelKind};
use utils::{merge_graphs, split_graph_components};
use wires::{Wire, WireEnd, WireIndex, WireLink, WireSeg};

const TILE_SIZE: f32 = 10.;
const SANDBOX_POS: Vec2 = vec2(200., 0.);
const SANDBOX_SIZE: Vec2 = vec2(900., 700.);
const _WINDOW_SIZE: Vec2 = vec2(1000., 800.);
const _MENU_SIZE: Vec2 = vec2(200., _WINDOW_SIZE.y);
const HOVER_RADIUS: f32 = 6.;

#[derive(Default, Debug, Clone, Copy)]
enum ActionState {
    #[default]
    Idle,
    // A new component from the menu that has been temporarily added to the graph
    HoldingComponent(NodeIndex),
    // Left-clicked on a component in the sandbox area
    SelectingComponent(NodeIndex),
    // Moving a component that already was in the sandbox area
    MovingComponent(NodeIndex, Vec2),
    DrawingWire(WireTarget),
}

#[derive(Debug, Default)]
struct TunnelMembers {
    senders: HashSet<NodeIndex>,
    receivers: HashSet<NodeIndex>,
}

impl TunnelMembers {
    fn is_valid(&self) -> bool {
        self.senders.len() == 1
    }
}

#[derive(Debug)]
struct TunnelUpdate {
    label: String,
    tunnel_kind: TunnelKind,
    update_kind: TunnelUpdateKind,
}

#[derive(Debug)]
enum TunnelUpdateKind {
    Add,
    Remove,
    Flip,
    Rename(String),
}

#[derive(Debug, Default)]
struct CircuitContext {
    tunnels: HashMap<String, TunnelMembers>,
}

impl CircuitContext {
    fn update(&mut self, event: CtxEvent, cx: NodeIndex) {
        match event {
            CtxEvent::TunnelUpdate(update) => {
                let tunnels = self.tunnels.entry(update.label.clone()).or_default();
                match update.update_kind {
                    TunnelUpdateKind::Add => {
                        match update.tunnel_kind {
                            TunnelKind::Sender => tunnels.senders.insert(cx),
                            TunnelKind::Receiver => tunnels.receivers.insert(cx),
                        };
                    }
                    TunnelUpdateKind::Remove => {
                        match update.tunnel_kind {
                            TunnelKind::Sender => tunnels.senders.remove(&cx),
                            TunnelKind::Receiver => tunnels.receivers.remove(&cx),
                        };
                        if tunnels.senders.is_empty() && tunnels.receivers.is_empty() {
                            self.tunnels.remove(&update.label);
                        }
                    }
                    TunnelUpdateKind::Flip => match update.tunnel_kind {
                        TunnelKind::Sender => {
                            tunnels.senders.remove(&cx);
                            tunnels.receivers.insert(cx);
                        }
                        TunnelKind::Receiver => {
                            tunnels.receivers.remove(&cx);
                            tunnels.senders.insert(cx);
                        }
                    },
                    TunnelUpdateKind::Rename(new_label) => {
                        let remove_event = CtxEvent::TunnelUpdate(TunnelUpdate {
                            label: update.label,
                            tunnel_kind: update.tunnel_kind,
                            update_kind: TunnelUpdateKind::Remove,
                        });
                        let add_event = CtxEvent::TunnelUpdate(TunnelUpdate {
                            label: new_label,
                            tunnel_kind: update.tunnel_kind,
                            update_kind: TunnelUpdateKind::Add,
                        });
                        self.update(remove_event, cx);
                        self.update(add_event, cx);
                    }
                }
            }
        }
    }

    fn _is_valid(&self) -> bool {
        self.tunnels.values().all(|t| t.is_valid())
    }
}

#[derive(Debug)]
enum CtxEvent {
    TunnelUpdate(TunnelUpdate),
}

#[derive(Debug, Clone, Copy)]
enum HoverItem {
    Comp(NodeIndex),
    Pin(NodeIndex, PinIndex),
    Wire(WireIndex),
    WireEnd(WireIndex, WireEnd),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WireTarget {
    Pin(NodeIndex, PinIndex),
    Wire(WireIndex, WireEnd),
}

#[derive(Debug, Default)]
struct WiringManager {
    groups: SlotMap<DefaultKey, StableGraph<WireSeg, ()>>,
    out_pins: SecondaryMap<DefaultKey, (NodeIndex, usize)>,
    in_pins: SecondaryMap<DefaultKey, HashSet<(NodeIndex, usize)>>,
    graph_exs: SecondaryMap<DefaultKey, HashSet<EdgeIndex>>,
}

impl WiringManager {
    fn draw_all_wires(&self) {
        todo!()
    }

    fn try_add_wire(
        &mut self,
        graph: &mut StableGraph<Component, Wire>,
        start: WireTarget,
        end: Option<WireTarget>,
    ) -> bool {
        if let Some(end) = end {
            if start == end {
                return false; // can't create wire to same pin
            }
        }
        match (start, end) {
            (WireTarget::Pin(nx_a, px_a), None) => self.try_add_wire_pin_to_air(graph, nx_a, px_a),
            (WireTarget::Pin(nx_a, px_a), Some(end)) => match end {
                WireTarget::Pin(nx_b, px_b) => {
                    self.try_add_wire_pin_to_pin(graph, nx_a, px_a, nx_b, px_b)
                }
                WireTarget::Wire(wx_b, end_b) => {
                    self.try_add_wire_pin_to_wire(graph, nx_a, px_a, wx_b, end_b)
                }
            },
            (WireTarget::Wire(wx_a, end_a), None) => {
                self.try_add_wire_wire_to_air(graph, wx_a, end_a)
            }
            (WireTarget::Wire(wx_a, end_a), Some(end)) => match end {
                WireTarget::Pin(nx_b, px_b) => {
                    self.try_add_wire_pin_to_wire(graph, nx_b, px_b, wx_a, end_a)
                }
                WireTarget::Wire(wx_b, end_b) => {
                    self.try_add_wire_wire_to_wire(graph, wx_a, end_a, wx_b, end_b)
                }
            },
        }
    }

    fn try_add_wire_pin_to_air(
        &mut self,
        graph: &mut StableGraph<Component, Wire>,
        cx: NodeIndex,
        px: PinIndex,
    ) -> bool {
        // Need to create new wire group
        let mut group = StableGraph::new();
        // Add the new wire segment
        let start_pos = graph[cx].pin_pos(px);
        let end_pos = Vec2::from(mouse_position());
        let start_link = Some(WireLink::Pin(cx, px));
        let wire = WireSeg::new(start_pos, end_pos, start_link, None);
        group.add_node(wire);
        // Get wire group key for SlotMap and populate relevant secondary map
        let gx = self.groups.insert(group);
        // Since this is a new wire group, the HashSets will need to be created
        match px {
            PinIndex::Input(i) => self.in_pins.insert(gx, HashSet::from([(cx, i)])).is_some(),
            PinIndex::Output(i) => self.out_pins.insert(gx, (cx, i)).is_some(),
        };

        true
    }
    fn try_add_wire_wire_to_air(
        &mut self,
        graph: &mut StableGraph<Component, Wire>,
        wx: WireIndex,
        end: WireEnd,
    ) -> bool {
        // Get the wire group that must already exist
        let group = &mut self.groups[wx.group];
        // Add the new wire segment
        let start_pos = group[wx.nx].get_pos(end);
        let end_pos = Vec2::from(mouse_position());
        let start_link = Some(WireLink::Wire(wx.nx));
        let wire = WireSeg::new(start_pos, end_pos, start_link, None);
        let new_nx = group.add_node(wire);
        // add the edge between the new and old wires within the group
        group.add_edge(wx.nx, new_nx, ());
        //Since the new wire doesn't connect to anything else, the secondary maps don't need to be
        //adjusted
        true
    }
    fn try_add_wire_pin_to_pin(
        &mut self,
        graph: &mut StableGraph<Component, Wire>,
        cx1: NodeIndex,
        px1: PinIndex,
        cx2: NodeIndex,
        px2: PinIndex,
    ) -> bool {
        // Can't create a wire between a component and itself
        if cx1 == cx2 {
            return false;
        };
        // Make sure there is one input and one output pin, and make the output x1 and the input x2
        let (cx1, i1, cx2, i2) = match (px1, px2) {
            (PinIndex::Input(_), PinIndex::Input(_))
            | (PinIndex::Output(_), PinIndex::Output(_)) => return false,
            (PinIndex::Output(i), PinIndex::Input(j)) => (cx1, i, cx2, j),
            (PinIndex::Input(i), PinIndex::Output(j)) => (cx2, j, cx1, i),
        };
        let (px1, px2) = (PinIndex::Output(i1), PinIndex::Input(i2));
        // Make sure the input pin is not connected to any other wires
        if graph
            .edges_directed(cx2, Direction::Incoming)
            .any(|e| e.weight().end_pin == i2)
        {
            return false;
        }
        // All conditions are met, work on adding the wire
        // Need to create new wire group
        let mut group = StableGraph::new();
        // Add the new wire segment
        let start_pos = graph[cx1].pin_pos(px1);
        let end_pos = graph[cx2].pin_pos(px2);
        let start_link = Some(WireLink::Pin(cx1, px1));
        let end_link = Some(WireLink::Pin(cx2, px2));
        let wire = WireSeg::new(start_pos, end_pos, start_link, end_link);
        group.add_node(wire);
        // Get wire group key for SlotMap and populate both secondary maps
        let gx = self.groups.insert(group);
        // Since this is a new wire group, the HashSets will need to be created
        self.out_pins.insert(gx, (cx1, i1));
        self.in_pins.insert(gx, HashSet::from([(cx2, i2)]));
        // Since this immediately creates a complete wire, update the main graph
        let data_bits = graph[cx1].kind.get_pin_width(px1);
        // FIXME: change or get rid of the `is_virtual` flag
        let edge = Wire::new(cx1, i1, cx2, i2, data_bits, gx, false);
        let ex = graph.add_edge(cx1, cx2, edge);
        // Track comp graph edge indices in the wiring manager
        self.graph_exs.insert(gx, HashSet::from([ex]));

        true
    }
    fn try_add_wire_pin_to_wire(
        &mut self,
        graph: &mut StableGraph<Component, Wire>,
        cx: NodeIndex,
        px: PinIndex,
        wx: WireIndex,
        end: WireEnd,
    ) -> bool {
        // Get the wire group that must already exist
        let gx = wx.group;
        let group = &mut self.groups[gx];
        // Do some pre-checks:
        match px {
            // If it's an input, make sure it's not connected to any other wires
            PinIndex::Input(i) => {
                if graph
                    .edges_directed(cx, Direction::Incoming)
                    .any(|e| e.weight().end_pin == i)
                {
                    return false;
                }
            }
            // If it's an output, make sure this group doesn't already have an output
            PinIndex::Output(_) => {
                if self.out_pins.contains_key(gx) {
                    return false;
                }
            }
        };
        // Any other logical conditions aren't checked ahead. The user will just need to delete the
        // wire if there is a logical error.

        // Add the new wire segment
        let start_pos = graph[cx].pin_pos(px);
        let end_pos = group[wx.nx].get_pos(end);
        let start_link = Some(WireLink::Pin(cx, px));
        let end_link = Some(WireLink::Wire(wx.nx));
        let wire = WireSeg::new(start_pos, end_pos, start_link, end_link);
        let new_nx = group.add_node(wire);
        // add the edge between the new and old wires within the group
        group.add_edge(wx.nx, new_nx, ());

        // Add the new pin to the relevant SecondaryMap HashSet
        let data_bits = graph[cx].kind.get_pin_width(px);
        match px {
            PinIndex::Input(i) => {
                let is_new = self.in_pins[gx].insert((cx, i));
                // If the pin isn't already in the group, add an edge between it and every output
                // pin in the group
                if is_new {
                    if let Some(&(cx1, i1)) = self.out_pins.get(gx) {
                        let edge = Wire::new(cx1, i1, cx, i, data_bits, gx, false);
                        let ex = graph.add_edge(cx1, cx, edge);

                    }
                }
            }
            PinIndex::Output(i) => {
                let is_new = self.out_pins.insert(gx, (cx, i)).is_none();
                // If the pin isn't already in the group, add an edge between it and every input
                // pin in the group
                if is_new {
                    for &(cx2, i2) in &self.in_pins[gx] {
                        let edge = Wire::new(cx, i, cx2, i2, data_bits, gx, false);
                        graph.add_edge(cx, cx2, edge);
                    }
                }
            }
        }

        true
    }
    fn try_add_wire_wire_to_wire(
        &mut self,
        graph: &mut StableGraph<Component, Wire>,
        wx1: WireIndex,
        end1: WireEnd,
        wx2: WireIndex,
        end2: WireEnd,
    ) -> bool {
        // Make sure the two wire groups are different (no point in double linking a group)
        if wx1.group == wx2.group {
            return false;
        };
        // Get the two wire groups that must already exist
        let (gx1, gx2) = (wx1.group, wx2.group);
        // Make sure the two wire groups have no more than one output pin total
        if self.in_pins.contains_key(gx1) && self.in_pins.contains_key(gx2) {
            return false;
        }
        // Any other logical conditions aren't checked ahead. The user will just need to delete the
        // wire if there is a logical error.

        // Remove the two wire groups to prepare to merge them
        let group1 = self
            .groups
            .remove(gx1)
            .expect("Group must exist if wire exists");
        let group2 = self
            .groups
            .remove(gx2)
            .expect("Group must exist if wire exists");

        // Create the new group from the original two
        let (mut joined_group, group2_nx_map) = merge_graphs(&group1, &group2);
        let nx1 = wx1.nx; // the first group preserves its node indices
        let nx2 = group2_nx_map[&wx1.nx]; // the second group's node indices are mapped

        // Create the new wire segment and add it to one of the groups
        let start_pos = joined_group[nx1].get_pos(end1);
        let end_pos = joined_group[nx2].get_pos(end2);
        let start_link = Some(WireLink::Wire(nx1));
        let end_link = Some(WireLink::Wire(nx2));
        let connecting_wire = WireSeg::new(start_pos, end_pos, start_link, end_link);

        let new_nx = joined_group.add_node(connecting_wire);
        // add the edges between the new wire and the two wire segments from the two smaller groups
        joined_group.add_edge(nx1, new_nx, ());
        joined_group.add_edge(new_nx, nx2, ());
        // add the merged group
        let joined_gx = self.groups.insert(joined_group);
        // Merge the input and output pins
        let in_pin_1 = self.in_pins.remove(gx1);
        let in_pin_2 = self.in_pins.remove(gx2);

        let joined_in_pins = in_pin_1
            .iter()
            .chain(in_pin_2.iter())
            .fold(HashSet::new(), |a, b| &a | b);
        self.in_pins.insert(joined_gx, joined_in_pins);

        let out_pin_1 = self.out_pins.remove(gx1);
        let out_pin_2 = self.out_pins.remove(gx2);
        let joined_out_pin = out_pin_1.or(out_pin_2);
        if let Some(joined_out_pin) = joined_out_pin {
            self.out_pins.insert(joined_gx, joined_out_pin);
            // TODO: add necessary edges
            // FIXME: change existing edges
            todo!("Add edges between the out pin and every in pin");
        }

        true
    }
}

#[derive(Default, Debug)]
struct App {
    textures: HashMap<&'static str, Texture2D>,
    graph: StableGraph<Component, Wire>,
    wiring: WiringManager,
    action_state: ActionState,
    context: CircuitContext,
}

impl App {
    async fn new() -> Self {
        App {
            textures: Self::load_textures().await,
            ..Default::default()
        }
    }
    async fn load_textures() -> HashMap<&'static str, Texture2D> {
        HashMap::from([(
            "gates",
            load_texture("assets/logic_gates.png").await.unwrap(),
        )])
    }

    fn draw_all_components(&self) {
        for comp in self.graph.node_weights() {
            comp.draw(&self.textures);
        }
    }

    fn draw_selected_component_box(&self, cx: NodeIndex) {
        let comp = &self.graph[cx];
        let (w, h) = comp.kind.size().into();
        let border = 0.5 * TILE_SIZE;
        draw_rectangle_lines(
            comp.position.x - border,
            comp.position.y - border,
            w + 2. * border,
            h + 2. * border,
            2.,
            BLACK,
        );
    }

    fn draw_all_better_wires(&self) {
        self.wiring.draw_all_wires();
    }

    fn draw_all_wires(&self) {
        for wire in self.graph.edge_weights() {
            if !wire.is_virtual {
                self.draw_wire(wire);
            }
        }
    }
    fn draw_wire(&self, wire: &Wire) {
        let cx_a = &self.graph[wire.start_comp];
        let cx_b = &self.graph[wire.end_comp];
        let pos_a = cx_a.position + cx_a.output_pos[wire.start_pin];
        let pos_b = cx_b.position + cx_b.input_pos[wire.end_pin];
        let color = color_from_signal(wire.get_signal());
        let thickness = if wire.data_bits == 1 { 1. } else { 3. };
        draw_ortho_lines(pos_a, pos_b, color, thickness);
    }

    fn select_component(&mut self, cx: NodeIndex) {
        let comp = &mut self.graph[cx];
        if comp.kind.interact() {
            self.update_signals();
        }
    }
    fn draw_pin_highlight(&self, cx: NodeIndex, px: PinIndex) {
        let comp = &self.graph[cx];
        let pin_pos = match px {
            PinIndex::Input(i) => comp.input_pos[i],
            PinIndex::Output(i) => comp.output_pos[i],
        };
        // find absolute pin_pos (it is relative position out of the box)
        let pin_pos = comp.position + pin_pos;
        draw_circle_lines(pin_pos.x, pin_pos.y, 3., 1., DARKGREEN);
    }

    fn find_hovered_cx_and_pin(&self) -> Option<(NodeIndex, Option<PinIndex>)> {
        // Looks for a hovered component, and then for a hovered pin if a component is found.
        let cx = self.find_hovered_comp()?;
        let pin = self.find_hovered_pin(cx);
        Some((cx, pin))
    }

    fn find_hovered_comp(&self) -> Option<NodeIndex> {
        let mouse_pos = Vec2::from(mouse_position());

        for cx in self.graph.node_indices() {
            let comp = &self.graph[cx];
            if comp.contains(mouse_pos) {
                return Some(cx);
            }
        }
        None
    }

    fn find_hovered_pin(&self, cx: NodeIndex) -> Option<PinIndex> {
        let mouse_pos = Vec2::from(mouse_position());

        let comp = &self.graph[cx];

        for (i, pin_pos) in comp.input_pos.iter().enumerate() {
            let pin_pos = vec2(comp.position.x + pin_pos.x, comp.position.y + pin_pos.y);
            if mouse_pos.distance(pin_pos) < HOVER_RADIUS {
                return Some(PinIndex::Input(i));
            }
        }
        for (i, pin_pos) in comp.output_pos.iter().enumerate() {
            let pin_pos = vec2(comp.position.x + pin_pos.x, comp.position.y + pin_pos.y);
            if mouse_pos.distance(pin_pos) < HOVER_RADIUS {
                return Some(PinIndex::Output(i));
            }
        }
        None
    }

    fn find_hovered_wire(&self) -> Option<(WireIndex, Option<WireEnd>)> {
        // FIXME: allow the option of hovering a wire without hovering one of the ends.
        let mouse_pos = Vec2::from(mouse_position());
        for (group, wire_graph) in &self.wiring.groups {
            for nx in wire_graph.node_indices() {
                let wire = &wire_graph[nx];
                let end = if mouse_pos.distance(wire.start_pos) < HOVER_RADIUS {
                    Some(WireEnd::Start)
                } else if mouse_pos.distance(wire.end_pos) < HOVER_RADIUS {
                    Some(WireEnd::End)
                } else {
                    None
                };
                if let Some(end) = end {
                    return Some((WireIndex::new(group, nx), Some(end)));
                }
            }
        }
        None
    }

    fn find_hovered_object(&self) -> Option<HoverItem> {
        if let Some((cx, px)) = self.find_hovered_cx_and_pin() {
            Some(match px {
                Some(px) => HoverItem::Pin(cx, px),
                None => HoverItem::Comp(cx),
            })
        } else if let Some((wx, end)) = self.find_hovered_wire() {
            Some(match end {
                Some(end) => HoverItem::WireEnd(wx, end),
                None => HoverItem::Wire(wx),
            })
        } else {
            None
        }
    }

    fn try_add_better_wire(&mut self, start: WireTarget, end: Option<WireTarget>) -> bool {
        self.wiring.try_add_wire(&mut self.graph, start, end)
    }

    fn try_add_wire(
        &mut self,
        cx_a: NodeIndex,
        px_a: PinIndex,
        cx_b: NodeIndex,
        px_b: PinIndex,
    ) -> bool {
        // Do not allow wires within a single component
        if cx_a == cx_b {
            return false;
        }
        // Check that the two pins have the same number of data_bits
        let data_bits_a = self.graph[cx_a].kind.get_pin_width(px_a);
        let data_bits_b = self.graph[cx_b].kind.get_pin_width(px_b);
        if data_bits_a != data_bits_b {
            return false;
        }
        // determine which pin is the output (sender) and which is the input (receiver)
        let (cx_a, pin_a, cx_b, pin_b) = match (px_a, px_b) {
            (PinIndex::Output(pin_a), PinIndex::Input(pin_b)) => (cx_a, pin_a, cx_b, pin_b),
            (PinIndex::Input(pin_a), PinIndex::Output(pin_b)) => (cx_b, pin_b, cx_a, pin_a),
            // input->input or output->output are invalid connections; don't create the wire.
            _ => return false,
        };
        // Check that the input pin is not already occupied
        if self
            .graph
            .edges_directed(cx_b, Direction::Incoming)
            .any(|e| e.weight().end_pin == pin_b)
        {
            return false;
        }
        // We know the pins match in terms of data bits, so arbitrarily set wire data bits to
        // data_bits_a
        // FIXME: remove the DefaultKey
        let wire = Wire::new(
            cx_a,
            pin_a,
            cx_b,
            pin_b,
            data_bits_a,
            DefaultKey::default(),
            false,
        );
        self.graph.add_edge(cx_a, cx_b, wire);
        self.update_signals();
        true
    }

    fn add_component(&mut self, comp: Component) -> NodeIndex {
        let cx = self.graph.add_node(comp);
        if let Some(event) = self.graph[cx].kind.get_ctx_event(CompEvent::Added) {
            self.context.update(event, cx);
        }
        cx
    }

    fn remove_component(&mut self, cx: NodeIndex) -> Option<Component> {
        // before removing from the graph, manually set all outgoing edge end pin signals to None
        let outgoing_wxs = self
            .graph
            .edges_directed(cx, Direction::Outgoing)
            .map(|e| e.id())
            .collect::<Vec<_>>();
        for wx in outgoing_wxs {
            let out_cx = self.graph[wx].end_comp;
            let out_pin = PinIndex::Input(self.graph[wx].end_pin);
            self.graph[out_cx].kind.set_pin_value(out_pin, None);
        }
        let mut comp = self.graph.remove_node(cx)?;
        if let Some(event) = comp.kind.get_ctx_event(CompEvent::Removed) {
            self.context.update(event, cx);
        }
        Some(comp)
    }

    fn update_component(&mut self, cx: NodeIndex) {
        // This method is called after a UI interaction causes a component to change state.

        // Update the component's input and output positions
        let comp = &mut self.graph[cx];
        comp.update_after_prop_change();

        // Then, remove any wires/edges that are made out of bounds
        let n_inputs = comp.kind.n_in_pins();
        let n_outputs = comp.kind.n_out_pins();

        let incoming_to_remove = self
            .graph
            .edges_directed(cx, Direction::Incoming)
            .filter(|e| e.weight().end_pin >= n_inputs)
            .map(|e| e.id());
        let outgoing_to_remove = self
            .graph
            .edges_directed(cx, Direction::Outgoing)
            .filter(|e| e.weight().start_pin >= n_outputs)
            .map(|e| e.id());
        for wx in incoming_to_remove
            .chain(outgoing_to_remove)
            .collect::<Vec<_>>()
        {
            self.graph.remove_edge(wx);
        }
    }

    fn tick_clock(&mut self) {
        for comp in self.graph.node_weights_mut() {
            comp.clock_update();
        }
        self.update_signals();
    }

    fn add_tunnel_connections(&mut self) {
        // Note: all tunnels have px = 0 for either Input or Output
        for tunnel_members in self.context.tunnels.values() {
            if tunnel_members.is_valid() {
                let &start_comp = tunnel_members
                    .senders
                    .iter()
                    .next()
                    .expect("Exists if valid");
                let data_bits = self.graph[start_comp]
                    .kind
                    .get_pin_width(PinIndex::Output(0));
                for &end_comp in &tunnel_members.receivers {
                    // FIXME: remove DefaultKey
                    let virtual_wire = Wire::new(
                        start_comp,
                        0,
                        end_comp,
                        0,
                        data_bits,
                        DefaultKey::default(),
                        true,
                    );
                    self.graph.add_edge(start_comp, end_comp, virtual_wire);
                }
            } else {
                for &end_comp in &tunnel_members.receivers {
                    self.graph[end_comp]
                        .kind
                        .set_pin_value(PinIndex::Output(0), None);
                }
            }
        }
    }

    fn update_signals(&mut self) {
        // reset then create virtual edges for the tunnels
        // TODO: make this more efficient by only adding and removing necessary edges

        // Remove virtual edges
        self.graph.retain_edges(|g, e| !g[e].is_virtual);
        // Add updated virtual edges
        self.add_tunnel_connections();
        // Remove (valid) cycles by ignoring edges which lead into a clocked component.
        let de_cycled =
            EdgeFiltered::from_fn(&self.graph, |e| !self.graph[e.target()].kind.is_clocked());
        let order =
            toposort(&de_cycled, None).expect("Cycles should only involve clocked components");

        // step through all components in order of evaluation
        // FIXME: input pins that are not connected to anything should be set to None
        for cx in order {
            // When visiting a component, perform logic to convert inputs to outputs.
            // This also applies to clocked components, whose inputs will still be based on the previous clock cycle.
            self.graph[cx].do_logic();
            let mut edges = self.graph.neighbors(cx).detach();
            // step through all connected wires and their corresponding components
            while let Some((wx, next_cx)) = edges.next(&self.graph) {
                let wire = &self.graph[wx];
                let start_pin = PinIndex::Output(wire.start_pin);
                let end_pin = PinIndex::Input(wire.end_pin);
                if self.graph[cx].kind.get_pin_width(start_pin)
                    == self.graph[next_cx].kind.get_pin_width(end_pin)
                {
                    // use wire to determine relevant output and input pins
                    let signal_to_transmit = self.graph[cx]
                        .kind
                        .get_pin_value(start_pin)
                        .map(Signal::from_bitslice);
                    self.graph[next_cx]
                        .kind
                        .set_pin_value(end_pin, signal_to_transmit.as_deref());
                    self.graph[wx].set_signal(signal_to_transmit.as_deref());
                } else {
                    // Pin widths don't match, so set receiving pin and wire to None
                    self.graph[wx].set_signal(None);
                    self.graph[next_cx].kind.set_pin_value(end_pin, None);
                };
            }
        }
    }

    fn draw_temp_wire(&self, target: WireTarget) {
        let start_pos = match target {
            WireTarget::Pin(cx, px) => {
                let comp = &self.graph[cx];
                let pin_pos = match px {
                    PinIndex::Input(i) => comp.input_pos[i],
                    PinIndex::Output(i) => comp.output_pos[i],
                };

                snap_to_grid(comp.position + pin_pos)
            }
            WireTarget::Wire(wx, end) => {
                todo!()
            }
        };

        let end_pos = snap_to_grid(Vec2::from(mouse_position()));
        draw_ortho_lines(start_pos, end_pos, BLACK, 1.);
    }

    fn get_properties_ui(&mut self, ui: &mut Ui) {
        if let ActionState::SelectingComponent(cx) | ActionState::MovingComponent(cx, _) =
            self.action_state
        {
            let comp = &mut self.graph[cx];
            ui.label(comp.kind.name());
            let response = comp.draw_properties_ui(ui);
            if let Some(maybe_ctx_event) = response {
                self.update_component(cx);
                if let Some(ctx_event) = maybe_ctx_event {
                    self.context.update(ctx_event, cx);
                }
            }
        }
    }

    fn draw_grid(&self) {
        let (nx, ny) = (SANDBOX_SIZE / TILE_SIZE).into();
        for i in 0..nx.floor() as u32 {
            for j in 0..ny.floor() as u32 {
                let x = SANDBOX_POS.x + i as f32 * TILE_SIZE;
                let y = SANDBOX_POS.y + j as f32 * TILE_SIZE;
                let color = GRAY;
                draw_line(x, y, x + 1., y + 1., 1., color);
            }
        }
    }

    // draw wire so that it only travels orthogonally
    fn update(&mut self, selected_menu_comp_name: &mut Option<&str>) {
        let mouse_pos = Vec2::from(mouse_position());
        let hover_result = self.find_hovered_object();
        if in_sandbox_area(mouse_pos) {
            // Alternatively could remove ActionState to use its value without mutating App.
            // let prev_state = std::mem::take(&mut self.action_state);

            // Clone the current ActionState to allow mutation
            let prev_state = self.action_state;
            // Return the new ActionState from the match. This makes it hard to mess up.
            self.action_state = match prev_state {
                ActionState::Idle => match hover_result {
                    Some(hover) => match hover {
                        HoverItem::Comp(cx) if is_mouse_button_pressed(MouseButton::Left) => {
                            self.select_component(cx);
                            ActionState::SelectingComponent(cx)
                        }
                        HoverItem::Pin(cx, px) if is_mouse_button_pressed(MouseButton::Left) => {
                            ActionState::DrawingWire(WireTarget::Pin(cx, px))
                        }
                        HoverItem::WireEnd(wx, end)
                            if is_mouse_button_pressed(MouseButton::Left) =>
                        {
                            ActionState::DrawingWire(WireTarget::Wire(wx, end))
                        }
                        _ => ActionState::Idle,
                    },
                    None => ActionState::Idle,
                },
                ActionState::HoldingComponent(cx) => {
                    self.graph[cx].position =
                        snap_to_grid(mouse_pos - self.graph[cx].kind.size() / 2.);

                    if is_mouse_button_released(MouseButton::Left) {
                        // component is completely added to sandbox, so get rid of menu selection.
                        *selected_menu_comp_name = None;
                        ActionState::Idle
                    } else if is_mouse_button_released(MouseButton::Right)
                        || is_key_released(KeyCode::Escape)
                    {
                        // Remove temporary component from graph
                        self.graph.remove_node(cx);
                        ActionState::Idle
                    } else {
                        prev_state
                    }
                }
                ActionState::SelectingComponent(cx) => {
                    // `D` deletes the component
                    if is_key_released(KeyCode::D) {
                        self.remove_component(cx);
                        ActionState::Idle
                    // `Esc` de-selects the component
                    } else if is_key_released(KeyCode::Escape) {
                        ActionState::Idle
                    // Clicking either de-selects the component, selects a new component, or begins drawing a wire
                    } else if is_mouse_button_pressed(MouseButton::Left) {
                        match hover_result {
                            Some(hover) => match hover {
                                HoverItem::Comp(new_cx) => {
                                    self.select_component(new_cx);
                                    ActionState::SelectingComponent(new_cx)
                                }
                                HoverItem::Pin(cx, px) => {
                                    ActionState::DrawingWire(WireTarget::Pin(cx, px))
                                }
                                HoverItem::WireEnd(wx, end) => {
                                    ActionState::DrawingWire(WireTarget::Wire(wx, end))
                                }
                                _ => ActionState::Idle,
                            },
                            None => ActionState::Idle,
                        }
                    // If mouse is dragging, switch from selecting to moving component.
                    } else if is_mouse_button_down(MouseButton::Left)
                        && mouse_delta_position() != Vec2::ZERO
                    {
                        let offset = mouse_pos - self.graph[cx].position;
                        ActionState::MovingComponent(cx, offset)
                    } else {
                        ActionState::SelectingComponent(cx)
                    }
                }
                ActionState::MovingComponent(cx, offset) => {
                    // Update component position (and center on mouse)
                    self.graph[cx].position = snap_to_grid(mouse_pos - offset);
                    if is_mouse_button_released(MouseButton::Left) {
                        ActionState::SelectingComponent(cx)
                    } else {
                        prev_state
                    }
                }
                ActionState::DrawingWire(start_target) => {
                    // Potentially finalizing the wire
                    if is_mouse_button_released(MouseButton::Left) {
                        // FIXME: need to change this after new wiring method works
                        match hover_result {
                            Some(end_hover) => match end_hover {
                                HoverItem::Pin(end_cx, end_px) => {
                                    println!("Adding wire to pin");
                                    self.try_add_better_wire(
                                        start_target,
                                        Some(WireTarget::Pin(end_cx, end_px)),
                                    );
                                    // self.try_add_wire(start_cx, start_px, end_cx, end_px);
                                }
                                HoverItem::Comp(end_cx) => (),
                                HoverItem::WireEnd(wx, end) => {
                                    self.try_add_better_wire(
                                        start_target,
                                        Some(WireTarget::Wire(wx, end)),
                                    );
                                }
                                _ => (),
                            },
                            None => {
                                println!("Adding wire to nothing");
                                self.try_add_better_wire(start_target, None);
                            }
                        };

                        ActionState::Idle
                    // In the process of drawing the wire
                    } else if is_mouse_button_down(MouseButton::Left) {
                        self.draw_temp_wire(start_target);
                        ActionState::DrawingWire(start_target)
                    // Let go of wire without completing it
                    } else {
                        ActionState::Idle
                    }
                }
            };
        }
        // Tick clock on spacebar
        if is_key_pressed(KeyCode::Space) {
            self.tick_clock();
        }

        // Do all drawing at the end to make sure everything is updated
        // and so that the z-order is maintained.
        self.draw_all_components();
        self.draw_all_better_wires();
        self.draw_all_wires();
        if let Some((cx, Some(px))) = self.find_hovered_cx_and_pin() {
            self.draw_pin_highlight(cx, px);
        }
        if let ActionState::SelectingComponent(cx) = self.action_state {
            self.draw_selected_component_box(cx);
        }
    }
}

fn macroquad_config() -> Conf {
    Conf {
        window_title: String::from("Logisim"),
        window_width: (SANDBOX_POS.x + SANDBOX_SIZE.x).ceil() as i32,
        window_height: (SANDBOX_POS.y + SANDBOX_SIZE.y).ceil() as i32,
        ..Default::default()
    }
}

#[macroquad::main(macroquad_config)]
async fn main() {
    let mut app = App::new().await;

    let folder_structure = get_folder_structure();
    let mut selected_menu_comp_name = None;

    loop {
        clear_background(WHITE);
        set_default_camera();
        // Draw Left-Side Menu
        // egui-macroquad ui

        // Draw circuit sandbox area
        draw_rectangle(
            SANDBOX_POS.x,
            SANDBOX_POS.y,
            SANDBOX_SIZE.x,
            SANDBOX_SIZE.y,
            LIGHTGRAY,
        );
        app.draw_grid();
        // Draw in sandbox area
        app.update(&mut selected_menu_comp_name);
        // egui ui
        egui_macroquad::ui(|ctx| {
            Window::new("Logisim")
                .movable(false)
                .collapsible(false)
                .fixed_size((SANDBOX_POS.x - 15., SANDBOX_SIZE.y - 50.))
                .anchor(Align2::LEFT_TOP, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_min_height(SANDBOX_SIZE.y - 50.);
                    ScrollArea::vertical()
                        .id_source("Components")
                        .min_scrolled_height(300.)
                        .max_height(400.)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.heading("Components");
                            for (folder, comp_names) in &folder_structure {
                                ui.collapsing(*folder, |ui| {
                                    for &comp_name in comp_names {
                                        if ui.button(comp_name).clicked() {
                                            selected_menu_comp_name = Some(comp_name);
                                            let new_comp =
                                                components::default_comp_from_name(comp_name);
                                            let new_cx = app.add_component(new_comp);
                                            app.action_state =
                                                ActionState::HoldingComponent(new_cx);
                                        }
                                    }
                                });
                            }

                            ui.label(format!("{:?}", mouse_position()));
                        });
                    ui.separator();
                    ScrollArea::vertical()
                        .id_source("Properties")
                        .min_scrolled_height(300.)
                        .max_height(300.)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.set_min_height(300.);
                            ui.heading("Properties");
                            app.get_properties_ui(ui);
                        });
                    // ui.set_width(SANDBOX_POS.x);
                });
        });
        egui_macroquad::draw();

        next_frame().await;
    }
}

fn in_sandbox_area(pos: Vec2) -> bool {
    let sandbox_rect = Rect::new(SANDBOX_POS.x, SANDBOX_POS.y, SANDBOX_SIZE.x, SANDBOX_SIZE.y);
    sandbox_rect.contains(pos)
}

fn get_folder_structure() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("Gates", vec!["NOT", "AND", "OR"]),
        ("I/O", vec!["Input", "Output"]),
        ("Wiring", vec!["Tunnel", "Splitter"]),
        ("Plexers", vec!["Mux", "Demux"]),
        ("Memory", vec!["Register"]),
    ]
}

fn draw_ortho_lines(start: Vec2, end: Vec2, color: Color, thickness: f32) {
    // TODO: make this more sophisticated so that it chooses the right order (horiz/vert first)
    draw_line(start.x, start.y, end.x, start.y, thickness, color);
    draw_line(end.x, start.y, end.x, end.y, thickness, color);
}

fn snap_to_grid(point: Vec2) -> Vec2 {
    let x = (point.x / TILE_SIZE).round() * TILE_SIZE;
    let y = (point.y / TILE_SIZE).round() * TILE_SIZE;
    Vec2::new(x, y)
}
