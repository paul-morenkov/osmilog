use std::collections::HashMap;

use macroquad::ui::widgets::{Button, TreeNode};
use macroquad::ui::{hash, root_ui, Skin};
use macroquad::{prelude::*, ui::widgets::Window};

use petgraph::algo::toposort;
use petgraph::stable_graph::{NodeIndex, StableGraph};
use petgraph::visit::{EdgeFiltered, EdgeRef};
use petgraph::Direction::Incoming;

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
enum Gate {
    Not,
    Or,
    And,
}

impl Gate {
    fn tex_info(&self) -> TexInfo {
        match self {
            Gate::Not => TexInfo::new(vec2(448., 111.), vec2(80., 80.), 2.),
            Gate::And => TexInfo::new(vec2(72., 0.), vec2(90., 69.), 2.),
            Gate::Or => TexInfo::new(vec2(72., 233.), vec2(90., 78.), 2.),
        }
    }
    fn input_positions(&self) -> Vec<Vec2> {
        match self {
            Gate::Not => vec![vec2(0., 20.)],
            Gate::Or => vec![vec2(0., 10.), vec2(0., 28.)],
            Gate::And => vec![vec2(0., 8.), vec2(0., 25.)],
        }
    }
    fn evaluate(&self, inputs: &[bool]) -> bool {
        // TODO: make this work for any number of inputs
        match self {
            Gate::Not => !inputs[0],
            Gate::Or => inputs[0] || inputs[1],
            Gate::And => inputs[0] && inputs[1],
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum PinIndex {
    Input(usize),
    Output(usize),
}

#[derive(Debug, Clone)]
enum CompKind {
    Gate(Gate),
    Input,
    Output,
    Mux,
    Demux,
    Register,
}

impl CompKind {
    fn n_inputs(&self) -> usize {
        match self {
            CompKind::Gate(_) => 2,
            CompKind::Input => 0,
            CompKind::Output => 1,
            CompKind::Register => 2,
            CompKind::Mux => 3,
            CompKind::Demux => 2,
        }
    }
    fn n_outputs(&self) -> usize {
        match self {
            CompKind::Gate(_) => 1,
            CompKind::Input => 1,
            CompKind::Output => 0,
            CompKind::Register => 1,
            CompKind::Mux => 1,
            CompKind::Demux => 2,
        }
    }
    fn size(&self) -> Vec2 {
        match self {
            CompKind::Gate(g) => {
                let tex_info = g.tex_info();
                tex_info.size / tex_info.scale
            }
            CompKind::Input | CompKind::Output => vec2(20., 20.),
            CompKind::Register => vec2(40., 60.),
            CompKind::Mux => vec2(30., 50.),
            CompKind::Demux => vec2(30., 50.),
        }
    }
    fn input_positions(&self) -> Vec<Vec2> {
        match self {
            CompKind::Gate(g) => g.input_positions(),
            CompKind::Mux | CompKind::Demux => {
                let n_inputs = self.n_inputs() - 1;
                // n-1 inputs on the left side
                let mut input_pos = (0..n_inputs)
                    .map(|i| vec2(0., (i + 1) as f32 * self.size().y / (n_inputs + 1) as f32))
                    .collect::<Vec<_>>();
                // in_sel pin on the bottom
                input_pos.push(vec2(self.size().x / 2., self.size().y - 5.));
                input_pos
            }
            _ => {
                let n_inputs = self.n_inputs();
                (0..n_inputs)
                    .map(|i| vec2(0., (i + 1) as f32 * self.size().y / (n_inputs + 1) as f32))
                    .collect()
            }
        }
    }
    fn output_positions(&self) -> Vec<Vec2> {
        match self {
            CompKind::Gate(g) => {
                let tex_info = g.tex_info();
                vec![vec2(tex_info.size.x, tex_info.size.y / 2.) / tex_info.scale]
            }
            _ => {
                let n_outputs = self.n_outputs();
                (0..n_outputs)
                    .map(|i| {
                        vec2(
                            self.size().x,
                            (i + 1) as f32 * self.size().y / (n_outputs + 1) as f32,
                        )
                    })
                    .collect()
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Component {
    kind: CompKind,
    is_clocked: bool,
    position: Vec2,
    size: Vec2,
    // pins have a value and a position
    inputs: Vec<bool>,
    outputs: Vec<bool>,
    input_pos: Vec<Vec2>,
    output_pos: Vec<Vec2>,
}

impl Component {
    fn new(kind: CompKind, position: Vec2) -> Self {
        Self {
            position,
            size: kind.size(),
            is_clocked: match &kind {
                CompKind::Register => true,
                _ => false,
            },
            inputs: vec![false; kind.n_inputs()],
            outputs: vec![false; kind.n_outputs()],
            input_pos: kind.input_positions(),
            output_pos: kind.output_positions(),
            kind,
        }
    }
    fn evaluate(&mut self) {
        match &self.kind {
            // TODO: allow for gates with variable number of inputs. Gates always only have one output.
            CompKind::Gate(g) => self.outputs[0] = g.evaluate(&self.inputs),
            CompKind::Mux => {
                let in_sel = if self.inputs[2] { 1 } else { 0 };
                self.outputs[0] = self.inputs[in_sel];
            }
            CompKind::Demux => {
                let out_sel = if self.inputs[1] { 1 } else { 0 };
                for x in &mut self.outputs {
                    *x = false;
                }
                self.outputs[out_sel] = self.inputs[0];
            }
            // Registers do not evaluate, they clock update (The inputs and outputs do not interact combinationally).
            CompKind::Input | CompKind::Output | CompKind::Register => (),
        }
    }
    fn clock_update(&mut self) {
        // TODO: make is_clocked part of the type structure
        if self.is_clocked {
            match &self.kind {
                // inputs[0] => the data input
                // inputs[1] => the write-enable
                CompKind::Register => {
                    // Only send input to output if the WE pin is on.
                    if self.inputs[1] {
                        self.outputs[0] = self.inputs[0]
                    }
                }
                _ => (),
            }
        }
    }
    fn draw(&self, textures: &HashMap<&str, Texture2D>) {
        match &self.kind {
            CompKind::Gate(g) => {
                self.draw_from_texture_slice(textures.get("gates").unwrap(), g.tex_info());
            }
            CompKind::Input => {
                // Input component has exactly one output
                let color = if self.outputs[0] { GREEN } else { RED };
                draw_rectangle(self.position.x, self.position.y, 20., 20., color);
            }
            CompKind::Output => {
                let color = if self.inputs[0] { GREEN } else { RED };
                draw_rectangle(self.position.x, self.position.y, 20., 20., color);
            }
            CompKind::Register => {
                let in_color = if self.inputs[0] { GREEN } else { RED };
                draw_rectangle(
                    self.position.x,
                    self.position.y,
                    self.size.x / 2.,
                    self.size.y,
                    in_color,
                );
                let out_color = if self.outputs[0] { GREEN } else { RED };
                draw_rectangle(
                    self.position.x + self.size.x / 2.,
                    self.position.y,
                    self.size.x / 2.,
                    self.size.y,
                    out_color,
                );
                draw_text("D", self.position.x, self.position.y + 25., 20., BLACK);
                draw_text("WE", self.position.x, self.position.y + 45., 20., BLACK);
                draw_text(
                    "Q",
                    self.position.x + 30.,
                    self.position.y + 25.,
                    20.,
                    BLACK,
                );
            }
            CompKind::Mux => {
                let a = self.position;
                let b = self.position + vec2(self.size.x, 10.);
                let c = self.position + vec2(self.size.x, self.size.y - 10.);
                let d = self.position + vec2(0., self.size.y);
                draw_line(a.x, a.y, b.x, b.y, 1., BLACK);
                draw_line(b.x, b.y, c.x, c.y, 1., BLACK);
                draw_line(c.x, c.y, d.x, d.y, 1., BLACK);
                draw_line(d.x, d.y, a.x, a.y, 1., BLACK);
            }
            CompKind::Demux => {
                let a = self.position + vec2(0., 10.);
                let b = self.position + vec2(self.size.x, 0.);
                let c = self.position + vec2(self.size.x, self.size.y);
                let d = self.position + vec2(0., self.size.y - 10.);
                draw_line(a.x, a.y, b.x, b.y, 1., BLACK);
                draw_line(b.x, b.y, c.x, c.y, 1., BLACK);
                draw_line(c.x, c.y, d.x, d.y, 1., BLACK);
                draw_line(d.x, d.y, a.x, a.y, 1., BLACK);
            }
        }
    }
    fn draw_from_texture_slice(&self, tex: &Texture2D, tex_info: TexInfo) {
        draw_texture_ex(
            tex,
            self.position.x,
            self.position.y,
            WHITE,
            DrawTextureParams {
                dest_size: Some(tex_info.size / tex_info.scale),
                source: Some(Rect::new(
                    tex_info.offset.x,
                    tex_info.offset.y,
                    tex_info.size.x,
                    tex_info.size.y,
                )),
                rotation: 0.,
                flip_x: false,
                flip_y: false,
                pivot: None,
            },
        );
    }
}

struct TexInfo {
    offset: Vec2,
    size: Vec2,
    scale: f32,
}

impl TexInfo {
    fn new(offset: Vec2, size: Vec2, scale: f32) -> Self {
        Self {
            offset,
            size,
            scale,
        }
    }
}

#[derive(Debug)]
struct Wire {
    start_comp: NodeIndex,
    start_pin: usize,
    end_comp: NodeIndex,
    end_pin: usize,
    value: bool,
}

impl Wire {
    fn new(start_comp: NodeIndex, start_pin: usize, end_comp: NodeIndex, end_pin: usize) -> Self {
        Self {
            start_comp,
            start_pin,
            end_comp,
            end_pin,
            value: false,
        }
    }
}

#[derive(Default, Debug)]
enum ActionState {
    #[default]
    Idle,
    HoldingComponent(Component),
    SelectingComponent(NodeIndex),
    MovingComponent(NodeIndex),
    DrawingWire(NodeIndex, PinIndex),
}

#[derive(Debug, Default)]
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

        draw_rectangle_lines(
            comp.position.x,
            comp.position.y,
            comp.size.x,
            comp.size.y,
            2.,
            BLACK,
        );
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
        let color = match wire.value {
            true => GREEN,
            false => BLUE,
        };
        draw_ortho_lines(pos_a, pos_b, color);
    }

    fn draw_held_component(&self, comp: &Component) {
        // need to update held_component position first

        comp.draw(&self.textures);
    }

    fn select_component(&mut self, comp: NodeIndex) {
        let comp = &mut self.graph[comp];
        match comp.kind {
            CompKind::Input => {
                comp.outputs[0] = !comp.outputs[0];
                self.update_signals();
            }
            _ => (),
        };
    }
    fn draw_pin_highlight(&self, cx: NodeIndex, px: PinIndex) {
        let comp = &self.graph[cx];
        let pin_pos = match px {
            PinIndex::Input(i) => comp.input_pos[i],
            PinIndex::Output(i) => comp.output_pos[i],
        };
        // find absolute pin_pos (it is relative position out of the box)
        let pin_pos = comp.position + pin_pos;
        draw_circle_lines(pin_pos.x, pin_pos.y, 5., 1., GREEN);
    }

    fn find_hovered_cx_and_pin(&self) -> Option<(NodeIndex, Option<PinIndex>)> {
        // Looks for a hovered component, and then for a hovered pin if a component is found.
        let comp = self.find_hovered_comp()?;
        let pin = self.find_hovered_pin(comp);
        Some((comp, pin))
    }

    fn find_hovered_comp(&self) -> Option<NodeIndex> {
        let (mx, my) = mouse_position();

        for cx in self.graph.node_indices() {
            let comp = &self.graph[cx];
            if mx >= comp.position.x
                && mx <= comp.position.x + comp.size.x
                && my >= comp.position.y
                && my <= comp.position.y + comp.size.y
            {
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

        let wire = Wire::new(cx_a, pin_a, cx_b, pin_b);
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
        let de_cycled = EdgeFiltered::from_fn(&self.graph, |e| !self.graph[e.target()].is_clocked);
        let order =
            toposort(&de_cycled, None).expect("Cycles should only involve clocked components");

        // step through all components in order of evaluation
        for cx in order {
            // When visiting a component, perform logic to convert inputs to outputs.
            // This also applies to clocked components, whose inputs will still be based on the previous clock cycle.
            self.graph[cx].evaluate();
            let mut edges = self.graph.neighbors(cx).detach();
            // step through all connected wires and their corresponding components
            while let Some((wx, next_node_idx)) = edges.next(&self.graph) {
                let start_pin = self.graph[wx].start_pin;
                let end_pin = self.graph[wx].end_pin;
                // use wire to determine relevant output and input pins
                // TODO: maybe rework the graph so that edges are between the pins, not the components?
                let signal_to_transmit = self.graph[cx].outputs[start_pin];
                self.graph[next_node_idx].inputs[end_pin] = signal_to_transmit;
                self.graph[wx].value = signal_to_transmit;
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

        draw_ortho_lines(start_pos, end_pos, BLACK);
    }
    // draw wire so that it only travels orthogonally
    fn update(&mut self, selected_menu_comp_name: &mut Option<&str>) {
        let mouse_pos = Vec2::from(mouse_position());
        let hover_result = self.find_hovered_cx_and_pin();
        if in_sandbox_area(mouse_pos) {
            // Temporarily remove ActionState use its value without mutating App.
            let prev_state = std::mem::take(&mut self.action_state);
            // Immediately set it back after deciding what the new state should be.
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
                ActionState::HoldingComponent(mut comp) => {
                    comp.position = mouse_pos - comp.size / 2.;

                    if is_mouse_button_released(MouseButton::Left) {
                        self.graph.add_node(comp);
                        *selected_menu_comp_name = None;
                        ActionState::Idle
                    } else if is_mouse_button_released(MouseButton::Right)
                        || is_key_released(KeyCode::Escape)
                    {
                        ActionState::Idle
                    } else {
                        self.draw_held_component(&comp);
                        ActionState::HoldingComponent(comp)
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
                        prev_state
                    }
                }
                ActionState::MovingComponent(cx) => {
                    // Update component position (and center on mouse)
                    self.graph[cx].position = mouse_pos - self.graph[cx].size / 2.;
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

const SANDBOX_POS: Vec2 = vec2(210., 0.);
const SANDBOX_SIZE: Vec2 = vec2(600., 600.);
const WINDOW_SIZE: Vec2 = vec2(1000., 600.);
const MENU_SIZE: Vec2 = vec2(200., WINDOW_SIZE.y);

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
        clear_background(GRAY);
        set_default_camera();
        // Draw Components Menu
        Window::new(hash!(), Vec2::ZERO, MENU_SIZE)
            .label("Components")
            .titlebar(true)
            .movable(false)
            .ui(&mut root_ui(), |ui| {
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
                                    // create component in for App
                                    let kind = match comp_name {
                                        "NOT" => CompKind::Gate(Gate::Not),
                                        "AND" => CompKind::Gate(Gate::And),
                                        "OR" => CompKind::Gate(Gate::Or),
                                        "Input" => CompKind::Input,
                                        "Output" => CompKind::Output,
                                        "Register" => CompKind::Register,
                                        "Mux" => CompKind::Mux,
                                        "Demux" => CompKind::Demux,
                                        _ => {
                                            panic!("Unknown component attempted to be created.")
                                        }
                                    };
                                    let new_comp = Component::new(kind, Vec2::ZERO);
                                    app.action_state = ActionState::HoldingComponent(new_comp);
                                };
                            }
                        });
                }
            });
        // Draw circuit sandbox area
        draw_rectangle(
            SANDBOX_POS.x,
            SANDBOX_POS.y,
            SANDBOX_SIZE.x,
            SANDBOX_POS.y,
            GRAY,
        );
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

fn draw_ortho_lines(start: Vec2, end: Vec2, color: Color) {
    // TODO: make this more sophisticated so that it chooses the right order (horiz/vert first)
    draw_line(start.x, start.y, end.x, start.y, 1., color);
    draw_line(end.x, start.y, end.x, end.y, 1., color);
}
