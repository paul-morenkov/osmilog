use std::collections::HashMap;

use macroquad::ui::widgets::{Button, TreeNode};
use macroquad::ui::{hash, root_ui, Skin};
use macroquad::{prelude::*, ui::widgets::Window};

use petgraph::stable_graph::{NodeIndex, StableGraph};
use scene::Node;

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
}

impl CompKind {
    fn n_inputs(&self) -> usize {
        match self {
            CompKind::Gate(_) => 2,
            CompKind::Input => 0,
        }
    }
    fn n_outputs(&self) -> usize {
        match self {
            CompKind::Gate(_) => 1,
            CompKind::Input => 1,
        }
    }
    fn size(&self) -> Vec2 {
        match self {
            CompKind::Gate(g) => {
                let tex_info = g.tex_info();
                tex_info.size / tex_info.scale
            }
            CompKind::Input => vec2(20., 20.),
        }
    }
    fn input_positions(&self) -> Vec<Vec2> {
        match self {
            CompKind::Gate(g) => g.input_positions(),
            CompKind::Input => vec![],
        }
    }
    fn output_positions(&self) -> Vec<Vec2> {
        match self {
            CompKind::Gate(g) => {
                let tex_info = g.tex_info();
                vec![vec2(tex_info.size.x, tex_info.size.y / 2.) / tex_info.scale]
            }
            CompKind::Input => vec![vec2(20., 10.)],
        }
    }
}

#[derive(Debug, Clone)]
struct Component {
    kind: CompKind,
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
            inputs: vec![false; kind.n_inputs()],
            outputs: vec![false; kind.n_outputs()],
            input_pos: kind.input_positions(),
            output_pos: kind.output_positions(),
            kind,
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

#[derive(Default, Debug)]
enum ActionState {
    #[default]
    Idle,
    HoldingComponent(Component),
    SelectingComponent,
    DrawingWire(WireDrawState),
}

#[derive(Debug)]
enum WireDrawState {
    Started(NodeIndex, PinIndex),
    Drawing,
}

#[derive(Debug, Default)]
struct App {
    textures: HashMap<&'static str, Texture2D>,
    selected_component: Option<NodeIndex>,
    graph: StableGraph<Component, ()>,
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
        for i in self.graph.node_indices() {
            // draw each component
            let comp = self
                .graph
                .node_weight(i)
                .expect("Node index should be valid");
            comp.draw(&self.textures);
            //draw selection box around selected component
            if self.selected_component == Some(i) {
                draw_rectangle_lines(
                    comp.position.x,
                    comp.position.y,
                    comp.size.x,
                    comp.size.y,
                    2.,
                    BLACK,
                );
            }
        }
    }

    fn draw_held_component(&self, comp: &Component) {
        // need to update held_component position first

        comp.draw(&self.textures);
    }

    fn handle_right_click(&mut self) {
        let (mx, my) = mouse_position();
        let mut target = None;
        for i in self.graph.node_indices() {
            let comp = self
                .graph
                .node_weight(i)
                .expect("Node index should be valid");
            if mx >= comp.position.x
                && mx <= comp.position.x + comp.size.x
                && my >= comp.position.y
                && my <= comp.position.y + comp.size.y
            {
                target = Some(i);
                break;
            }
        }

        self.selected_component = target;
        if let Some(i) = target {
            Self::right_click_on(
                self.graph
                    .node_weight_mut(i)
                    .expect("Node index should be valid"),
            );
        }
    }
    fn right_click_on(comp: &mut Component) {
        match comp.kind {
            CompKind::Gate(_) => {}
            CompKind::Input => comp.outputs[0] = !comp.outputs[0],
        };
    }
    fn draw_pin_highlight(&self, comp_idx: NodeIndex, pin_idx: PinIndex) {
        let comp = self
            .graph
            .node_weight(comp_idx)
            .expect("Node index is valid");
        let pin_pos = match pin_idx {
            PinIndex::Input(i) => comp.input_pos[i],
            PinIndex::Output(i) => comp.output_pos[i],
        };
        // find absolute pin_pos (it is relative position out of the box)
        let pin_pos = comp.position + pin_pos;
        draw_circle_lines(pin_pos.x, pin_pos.y, 5., 1., GREEN);
    }

    fn find_hovered_comp_and_pin(&self) -> Option<(NodeIndex, PinIndex)> {
        self.find_hovered_comp().and_then(|comp_idx| {
            self.find_hovered_pin(comp_idx)
                .map(|pin_idx| (comp_idx, pin_idx))
        })
    }
    fn find_hovered_comp(&self) -> Option<NodeIndex> {
        let (mx, my) = mouse_position();

        for i in self.graph.node_indices() {
            let comp = self
                .graph
                .node_weight(i)
                .expect("Node index should be valid");
            if mx >= comp.position.x
                && mx <= comp.position.x + comp.size.x
                && my >= comp.position.y
                && my <= comp.position.y + comp.size.y
            {
                return Some(i);
            }
        }
        None
    }

    fn find_hovered_pin(&self, comp_idx: NodeIndex) -> Option<PinIndex> {
        let mouse_pos = Vec2::from(mouse_position());

        let comp = self
            .graph
            .node_weight(comp_idx)
            .expect("Node Index should be valid");

        for (i, pin_pos) in comp.input_pos.iter().enumerate() {
            let pin_pos = vec2(comp.position.x + pin_pos.x, comp.position.y + pin_pos.y);
            if mouse_pos.distance(pin_pos) < 10. {
                return Some(PinIndex::Input(i));
            }
        }
        for (i, pin_pos) in comp.output_pos.iter().enumerate() {
            let pin_pos = vec2(comp.position.x + pin_pos.x, comp.position.y + pin_pos.y);
            if mouse_pos.distance(pin_pos) < 10. {
                return Some(PinIndex::Output(i));
            }
        }
        None
    }
    fn draw_temp_wire(&self, comp_idx: NodeIndex, pin_idx: PinIndex) {
        let comp = self
            .graph
            .node_weight(comp_idx)
            .expect("Node index should be valid");
        let pin_pos = match pin_idx {
            PinIndex::Input(i) => comp.input_pos[i],
            PinIndex::Output(i) => comp.output_pos[i],
        };
        let start_pos = comp.position + pin_pos;
        let end_pos = Vec2::from(mouse_position());
        // draw wire so that it only travels orthogonally
        // TODO: make this more sophisticated so that it chooses the right order (horiz/vert first)
        draw_line(start_pos.x, start_pos.y, end_pos.x, start_pos.y, 1., BLACK);
        draw_line(end_pos.x, start_pos.y, end_pos.x, end_pos.y, 1., BLACK);
    }
    fn update(&mut self) {
        let mouse_pos = Vec2::from(mouse_position());
        if in_sandbox_area(mouse_pos) {
            // Temporarily remove ActionState use its value without mutating App.
            let state = std::mem::take(&mut self.action_state);
            // Immediately set it back after deciding what the new state should be.
            self.action_state = match state {
                ActionState::Idle => {
                    if is_mouse_button_released(MouseButton::Right) {
                        self.handle_right_click();
                        ActionState::Idle
                    } else {
                        if let Some(hovered_comp_idx) = self.find_hovered_comp() {
                            if let Some(hovered_pin_idx) = self.find_hovered_pin(hovered_comp_idx) {
                                self.draw_pin_highlight(hovered_comp_idx, hovered_pin_idx);

                                if is_mouse_button_pressed(MouseButton::Left) {
                                    // start a wire draw?
                                    ActionState::DrawingWire(WireDrawState::Started(
                                        hovered_comp_idx,
                                        hovered_pin_idx,
                                    ))
                                } else {
                                    ActionState::Idle
                                }
                            } else {
                                ActionState::Idle
                            }
                        } else {
                            ActionState::Idle
                        }
                    }
                }
                ActionState::HoldingComponent(mut comp) => {
                    comp.position = mouse_pos;

                    if is_mouse_button_released(MouseButton::Left) {
                        self.graph.add_node(comp);
                        ActionState::Idle
                    } else if is_mouse_button_released(MouseButton::Right) {
                        ActionState::Idle
                    } else {
                        self.draw_held_component(&comp);
                        ActionState::HoldingComponent(comp)
                    }
                }
                ActionState::SelectingComponent => ActionState::SelectingComponent,
                ActionState::DrawingWire(state) => match state {
                    WireDrawState::Started(comp, pin) => {
                        // Potentially finalizing the wire
                        if is_mouse_button_released(MouseButton::Left) {
                            match self.find_hovered_comp_and_pin() {
                                // Landed on a pin
                                Some((end_comp, end_pin)) => {
                                    // On the same component we started from -> don't add a wire
                                    // TODO: Some components might allow this?
                                    if end_comp == comp {
                                        // Let go on the same pin you started -> don't add a wire
                                        // On a different pin somewhere
                                    } else {
                                        // Check for valid variation of pin combinations
                                        match (end_pin, pin) {
                                            // TODO: figure out how edges/wires will work
                                            (PinIndex::Input(_), PinIndex::Output(_)) => {
                                                println!("Drawing out to in");
                                                self.graph.add_edge(comp, end_comp, ());
                                            }
                                            (PinIndex::Output(_), PinIndex::Input(_)) => {
                                                println!("Drawing in to out");
                                                self.graph.add_edge(end_comp, comp, ());
                                            }
                                            _ => (), // Invalid pin combination
                                        };
                                    }
                                }
                                None => (),
                            }
                            ActionState::Idle
                        // In the process of drawing the wire
                        } else if is_mouse_button_down(MouseButton::Left) {
                            self.draw_temp_wire(comp, pin);
                            if let Some((comp_idx, pin_idx)) = self.find_hovered_comp_and_pin() {
                                self.draw_pin_highlight(comp_idx, pin_idx);
                            }
                            ActionState::DrawingWire(state)
                        } else {
                            ActionState::Idle
                        }
                    }
                    WireDrawState::Drawing => todo!(),
                },
            };
        }
        self.draw_all_components();
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
        let mouse_pos = Vec2::from(mouse_position());
        // Draw Components Menu
        Window::new(hash!(), Vec2::ZERO, MENU_SIZE)
            .label("Components")
            .titlebar(true)
            .movable(false)
            .ui(&mut root_ui(), |ui| {
                for (&folder, comp_names) in &folder_structure {
                    TreeNode::new(hash!(folder), folder)
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
                                        _ => panic!("Unknown component attempted to be created."),
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
        app.update();

        next_frame().await;
    }
}

fn in_sandbox_area(pos: Vec2) -> bool {
    let sandbox_rect = Rect::new(SANDBOX_POS.x, SANDBOX_POS.y, SANDBOX_SIZE.x, SANDBOX_SIZE.y);
    sandbox_rect.contains(pos)
}

fn get_folder_structure() -> HashMap<&'static str, Vec<&'static str>> {
    HashMap::from([("Gates", vec!["NOT", "AND", "OR"]), ("Misc", vec!["Input"])])
}
