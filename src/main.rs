use std::collections::HashMap;

use macroquad::prelude::*;
use macroquad::ui::widgets::{Button, Group, TreeNode, Window};
use macroquad::ui::{hash, root_ui, Skin, Ui};

use petgraph::algo::toposort;
use petgraph::stable_graph::{NodeIndex, StableGraph};
use petgraph::visit::{EdgeFiltered, EdgeRef};
use petgraph::Direction::{self, Incoming};
use std::fmt::Debug;

use components::{signal_zeros, Component, PinIndex, Signal};

mod components;

const TILE_SIZE: f32 = 10.;
const SANDBOX_POS: Vec2 = vec2(210., 0.);
// TODO: make sandbox size in terms of tiles?
const SANDBOX_SIZE: Vec2 = vec2(600., 600.);
const WINDOW_SIZE: Vec2 = vec2(1000., 600.);
const MENU_SIZE: Vec2 = vec2(200., WINDOW_SIZE.y);

#[derive(Debug)]
struct Wire {
    // TODO: does Wire even need data_bits? Can just use Signal::len
    start_comp: NodeIndex,
    start_pin: usize,
    end_comp: NodeIndex,
    end_pin: usize,
    data_bits: u8,
    value: Signal,
}

impl Wire {
    fn new(
        start_comp: NodeIndex,
        start_pin: usize,
        end_comp: NodeIndex,
        end_pin: usize,
        data_bits: u8,
    ) -> Self {
        Self {
            start_comp,
            start_pin,
            end_comp,
            end_pin,
            data_bits,
            value: signal_zeros(data_bits),
        }
    }
}

#[derive(Default, Debug, Clone, Copy)]
enum ActionState {
    #[default]
    Idle,
    // A new component from the menu that has been temporarily added to the graph
    HoldingComponent(NodeIndex),
    // Left-clicked on a component in the sandbox area
    SelectingComponent(NodeIndex),
    // Moving a component that already was in the sandbox area
    MovingComponent(NodeIndex),
    DrawingWire(NodeIndex, PinIndex),
}

#[derive(Default, Debug)]
struct App {
    textures: HashMap<&'static str, Texture2D>,
    graph: StableGraph<Component, Wire>,
    action_state: ActionState,
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
        draw_rectangle_lines(comp.position.x, comp.position.y, w, h, 2., BLACK);
    }

    fn draw_all_wires(&self) {
        for wire in self.graph.edge_weights() {
            self.draw_wire(wire);
        }
    }
    fn draw_wire(&self, wire: &Wire) {
        let cx_a = &self.graph[wire.start_comp];
        let cx_b = &self.graph[wire.end_comp];
        let pos_a = cx_a.position + cx_a.output_pos[wire.start_pin];
        let pos_b = cx_b.position + cx_b.input_pos[wire.end_pin];
        let color = if wire.value.any() { GREEN } else { BLUE };
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
        draw_circle_lines(pin_pos.x, pin_pos.y, 3., 1., GREEN);
    }

    fn find_hovered_cx_and_pin(&self) -> Option<(NodeIndex, Option<PinIndex>)> {
        // Looks for a hovered component, and then for a hovered pin if a component is found.
        let comp = self.find_hovered_comp()?;
        let pin = self.find_hovered_pin(comp);
        Some((comp, pin))
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
        let max_dist = 10.;

        for (i, pin_pos) in comp.input_pos.iter().enumerate() {
            let pin_pos = vec2(comp.position.x + pin_pos.x, comp.position.y + pin_pos.y);
            if mouse_pos.distance(pin_pos) < max_dist {
                return Some(PinIndex::Input(i));
            }
        }
        for (i, pin_pos) in comp.output_pos.iter().enumerate() {
            let pin_pos = vec2(comp.position.x + pin_pos.x, comp.position.y + pin_pos.y);
            if mouse_pos.distance(pin_pos) < max_dist {
                return Some(PinIndex::Output(i));
            }
        }
        None
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
        let data_bits_a = self.graph[cx_a].kind.get_pin_value(px_a).len();
        let data_bits_b = self.graph[cx_b].kind.get_pin_value(px_b).len();
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
            .edges_directed(cx_b, Incoming)
            .any(|e| e.weight().end_pin == pin_b)
        {
            return false;
        }
        // We know the pins match in terms of data bits, so arbitrarily set wire data bits to
        // data_bits_a
        let wire = Wire::new(cx_a, pin_a, cx_b, pin_b, data_bits_a as u8);
        self.graph.add_edge(cx_a, cx_b, wire);
        self.update_signals();
        true
    }

    fn remove_component(&mut self, cx: NodeIndex) {
        self.graph.remove_node(cx);
    }

    fn tick_clock(&mut self) {
        for comp in self.graph.node_weights_mut() {
            comp.clock_update();
        }
        self.update_signals();
    }

    fn update_signals(&mut self) {
        // Remove (valid) cycles by ignoring edges which lead into a clocked component.
        let de_cycled =
            EdgeFiltered::from_fn(&self.graph, |e| !self.graph[e.target()].kind.is_clocked());
        let order =
            toposort(&de_cycled, None).expect("Cycles should only involve clocked components");

        // step through all components in order of evaluation
        for cx in order {
            // When visiting a component, perform logic to convert inputs to outputs.
            // This also applies to clocked components, whose inputs will still be based on the previous clock cycle.
            self.graph[cx].do_logic();
            let mut edges = self.graph.neighbors(cx).detach();
            // step through all connected wires and their corresponding components
            while let Some((wx, next_node_idx)) = edges.next(&self.graph) {
                let start_pin = PinIndex::Output(self.graph[wx].start_pin);
                let end_pin = PinIndex::Input(self.graph[wx].end_pin);
                // use wire to determine relevant output and input pins
                let signal_to_transmit = self.graph[cx].kind.get_pin_value(start_pin).clone();
                self.graph[next_node_idx]
                    .kind
                    .set_pin_value(end_pin, &signal_to_transmit);
                self.graph[wx].value.copy_from_bitslice(&signal_to_transmit);
            }
        }
    }

    fn draw_temp_wire(&self, cx: NodeIndex, px: PinIndex) {
        let comp = &self.graph[cx];
        let pin_pos = match px {
            PinIndex::Input(i) => comp.input_pos[i],
            PinIndex::Output(i) => comp.output_pos[i],
        };
        let start_pos = comp.position + pin_pos;
        let end_pos = Vec2::from(mouse_position());

        // FIXME: switch to grid-snapped versions when ready
        // let start_pos = snap_to_grid(comp.position + pin_pos);
        // let end_pos = snap_to_grid(Vec2::from(mouse_position()));
        draw_ortho_lines(start_pos, end_pos, BLACK, 1.);
    }

    fn get_properties_ui(&mut self, ui: &mut Ui) {
        if let ActionState::SelectingComponent(cx) = self.action_state {
            let comp = &mut self.graph[cx];
            // Properties UI always starts with name of the component.
            Group::new(hash!(), vec2(MENU_SIZE.x, 30.))
                .position(Vec2::ZERO)
                .ui(ui, |ui| {
                    ui.label(vec2(0., 0.), "ID");
                    ui.label(vec2(50., 0.), comp.kind.name());
                });
            let new_comp = comp.draw_properties_ui(ui);
            if let Some(new_comp) = new_comp {
                self.graph[cx] = Component::new(new_comp, self.graph[cx].position);
                // remove all wires connected to this component
                let wxs_to_remove = self
                    .graph
                    .edges_directed(cx, Direction::Outgoing)
                    .map(|e| e.id())
                    .chain(
                        self.graph
                            .edges_directed(cx, Direction::Incoming)
                            .map(|e| e.id()),
                    )
                    .collect::<Vec<_>>();
                for wx in wxs_to_remove {
                    self.graph.remove_edge(wx);
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
        let hover_result = self.find_hovered_cx_and_pin();
        if in_sandbox_area(mouse_pos) {
            // Alternatively could remove ActionState to use its value without mutating App.
            // let prev_state = std::mem::take(&mut self.action_state);

            // Clone the current ActionState to allow mutation
            let prev_state = self.action_state;
            // Return the new ActionState from the match. This makes it hard to mess up.
            self.action_state = match prev_state {
                ActionState::Idle => 'idle: {
                    match hover_result {
                        // Hovering  on component, but NOT pin
                        Some((cx, None)) => {
                            if is_mouse_button_pressed(MouseButton::Left) {
                                self.select_component(cx);
                                break 'idle ActionState::SelectingComponent(cx);
                            }
                        }
                        // Hovering on pin
                        Some((cx, Some(px))) => {
                            if is_mouse_button_pressed(MouseButton::Left) {
                                break 'idle ActionState::DrawingWire(cx, px);
                            }
                        }
                        // Not hovering anything
                        None => break 'idle ActionState::Idle,
                    }
                    ActionState::Idle
                }
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
                            // Hovering  on component, but NOT pin
                            Some((new_cx, None)) => {
                                self.select_component(new_cx);
                                ActionState::SelectingComponent(new_cx)
                            }
                            // Hovering on pin
                            Some((new_cx, Some(new_px))) => {
                                ActionState::DrawingWire(new_cx, new_px)
                            }
                            // Not hovering anything
                            None => ActionState::Idle,
                        }
                    // If mouse is dragging, switch from selecting to moving component.
                    } else if is_mouse_button_down(MouseButton::Left)
                        && mouse_delta_position() != Vec2::ZERO
                    {
                        ActionState::MovingComponent(cx)
                    } else {
                        ActionState::SelectingComponent(cx)
                    }
                }
                ActionState::MovingComponent(cx) => {
                    // Update component position (and center on mouse)
                    self.graph[cx].position =
                        snap_to_grid(mouse_pos - self.graph[cx].kind.size() / 2.);
                    if is_mouse_button_released(MouseButton::Left) {
                        ActionState::SelectingComponent(cx)
                    } else {
                        prev_state
                    }
                }
                ActionState::DrawingWire(start_cx, start_px) => {
                    // Potentially finalizing the wire
                    if is_mouse_button_released(MouseButton::Left) {
                        // Landed on a pin
                        if let Some((end_cx, Some(end_px))) = hover_result {
                            // This function handles all error cases like bad pin match-up, self-connection, and multiple output connections
                            self.try_add_wire(start_cx, start_px, end_cx, end_px);
                        }
                        ActionState::Idle
                    // In the process of drawing the wire
                    } else if is_mouse_button_down(MouseButton::Left) {
                        self.draw_temp_wire(start_cx, start_px);
                        ActionState::DrawingWire(start_cx, start_px)
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
        self.draw_all_wires();
        if let Some((cx, Some(px))) = self.find_hovered_cx_and_pin() {
            self.draw_pin_highlight(cx, px);
        }
        if let ActionState::SelectingComponent(cx) = self.action_state {
            self.draw_selected_component_box(cx);
        }
    }
}

fn create_skin() -> Skin {
    let window_style = root_ui()
        .style_builder()
        .background(Image {
            width: 3,
            height: 3,
            bytes: vec![
                68, 68, 68, 255, 68, 68, 68, 255, 68, 68, 68, 255, 68, 68, 68, 255, 238, 238, 238,
                255, 68, 68, 68, 255, 68, 68, 68, 255, 68, 68, 68, 255, 68, 68, 68, 255,
            ],
        })
        .color_inactive(Color::from_rgba(255, 255, 255, 255))
        .background_margin(RectOffset::new(1., 1., 1., 1.))
        .build();
    Skin {
        window_style,
        ..root_ui().default_skin()
    }
}

#[macroquad::main("Logisim")]
async fn main() {
    let mut app = App::new().await;
    let skin = create_skin();
    root_ui().push_skin(&skin);

    let folder_structure = get_folder_structure();
    let mut selected_menu_comp_name = None;

    loop {
        clear_background(WHITE);
        set_default_camera();
        // Draw Left-Side Menu
        Window::new(hash!("left-menu"), Vec2::ZERO, MENU_SIZE)
            .label("Components")
            .titlebar(true)
            .movable(false)
            .ui(&mut root_ui(), |ui| {
                // Draw Components menu
                Group::new(hash!("components"), vec2(MENU_SIZE.x, MENU_SIZE.y / 2.))
                    .position(Vec2::ZERO)
                    .ui(ui, |ui| {
                        for (folder, comp_names) in &folder_structure {
                            TreeNode::new(hash!(*folder), *folder)
                                .init_unfolded()
                                .ui(ui, |ui| {
                                    for &comp_name in comp_names {
                                        if Button::new(comp_name)
                                            .selected(selected_menu_comp_name == Some(comp_name))
                                            .ui(ui)
                                        {
                                            // track selection in menu UI
                                            selected_menu_comp_name = Some(comp_name);
                                            // create component for App
                                            let new_comp =
                                                components::default_comp_from_name(comp_name);
                                            let new_cx = app.graph.add_node(new_comp);
                                            app.action_state =
                                                ActionState::HoldingComponent(new_cx);
                                        };
                                    }
                                });
                        }
                    });
                Group::new(hash!("properties"), vec2(MENU_SIZE.x, MENU_SIZE.y / 2.))
                    .position(vec2(0., MENU_SIZE.y / 2.))
                    .ui(ui, |ui| {
                        app.get_properties_ui(ui);
                    });
            });
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
    let x = (point.x / TILE_SIZE).floor() * TILE_SIZE;
    let y = (point.y / TILE_SIZE).floor() * TILE_SIZE;
    Vec2::new(x, y)
}
