use std::collections::HashMap;

use macroquad::ui::widgets::{Button, Group, TreeNode};
use macroquad::ui::{hash, root_ui, Drag, Skin, Ui};
use macroquad::{prelude::*, ui::widgets::Window};

use petgraph::{
    stable_graph::{NodeIndex, StableGraph},
    Graph,
};

#[derive(Debug, Eq, PartialEq, Hash)]
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
            Gate::Not => vec![vec2(0., 18.)],
            Gate::Or => vec![vec2(0., 10.), vec2(0., 28.)],
            Gate::And => vec![vec2(0., 8.), vec2(0., 25.)],
        }
    }
}
#[derive(Debug)]
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

#[derive(Debug)]
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

enum WireDrawState {
    Not,
    StartAt(Vec2),
}

struct CompInfo {
    tex_info: TexInfo,
    input_locs: Vec<Vec2>,
    output_locs: Vec<Vec2>,
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
struct App {
    textures: HashMap<&'static str, Texture2D>,
    held_component: Option<Component>,
    selected_component: Option<NodeIndex>,
    components: Vec<Component>,
    graph: StableGraph<Component, ()>,
    mouse_drag_start: Option<Vec2>,
}

impl App {
    async fn new() -> Self {
        App {
            textures: Self::load_textures().await,
            held_component: Default::default(),
            selected_component: None,
            components: Vec::new(),
            graph: StableGraph::default(),
            mouse_drag_start: Default::default(),
        }
    }
    async fn load_textures() -> HashMap<&'static str, Texture2D> {
        HashMap::from([(
            "gates",
            load_texture("assets/logic_gates.png").await.unwrap(),
        )])
    }
    fn add_held_component(&mut self) {
        if let Some(comp) = self.held_component.take() {
            self.graph.add_node(comp);
        }
    }
    fn draw_all_components(&self) {
        for i in self.graph.node_indices() {
            // draw each component
            let comp = self.graph.node_weight(i).unwrap();
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

    fn draw_held_component(&self) {
        // need to update held_component position first

        if let Some(comp) = &self.held_component {
            comp.draw(&self.textures);
        }
    }

    fn handle_right_click(&mut self) {
        let (mx, my) = mouse_position();
        let mut target = None;
        for i in self.graph.node_indices() {
            let comp = self.graph.node_weight(i).unwrap();
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
            Self::right_click_on(self.graph.node_weight_mut(i).unwrap());
        }
    }
    fn right_click_on(comp: &mut Component) {
        match comp.kind {
            CompKind::Gate(_) => {}
            CompKind::Input => comp.outputs[0] = !comp.outputs[0],
        };
    }
    fn draw_pin_highlight(&self) {
        let mouse_pos = Vec2::from(mouse_position());
        for comp in self.graph.node_weights() {
            for pin_pos in comp.input_pos.iter().chain(comp.output_pos.iter()) {
                let pin_pos = vec2(comp.position.x + pin_pos.x, comp.position.y + pin_pos.y);
                if mouse_pos.distance(pin_pos) < 10. {
                    draw_circle_lines(pin_pos.x, pin_pos.y, 5., 1., GREEN);
                    break;
                }
            }
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
                                    app.held_component = Some(Component::new(kind, Vec2::ZERO));
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
        if in_sandbox_area(mouse_pos) {
            match &mut app.held_component {
                Some(comp) => {
                    comp.position = mouse_pos;
                    app.draw_held_component();
                }
                None => {
                    app.draw_pin_highlight();
                }
            }

            // left mouse btn for placing components
            if is_mouse_button_released(MouseButton::Left) {
                app.add_held_component();
            }
            // right mouse btn for toggling inputs (for now)
            if is_mouse_button_released(MouseButton::Right) {
                app.handle_right_click();
            }
        }
        app.draw_all_components();
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
