use egui::{pos2, vec2, Pos2, Rect, Vec2};

pub const BUBBLE_R: f32 = 4.0;
const BEZIER_STEPS: usize = 16;

#[derive(Clone, Debug)]
pub enum ShapeCmd {
    MoveTo(Vec2),
    LineTo(Vec2),
    CubicTo(Vec2, Vec2, Vec2),
}

/// A pin's location as an integer grid-cell offset. This keeps every pin on a grid intersection.
#[derive(Clone, Debug)]
pub struct PinAnchor {
    /// Offset in cells, not pixels.
    pub cell: Vec2,
    /// Unit vector, away from the component body.
    pub wire_dir: Vec2,
}

impl PinAnchor {
    // `u32` cell coordinates make an off-grid anchor impossible to construct.
    fn at(col: u32, row: u32, wire_dir: Vec2) -> Self {
        Self {
            cell: vec2(col as f32, row as f32),
            wire_dir,
        }
    }
    pub fn left(row: u32) -> Self {
        Self::at(0, row, vec2(-1.0, 0.0))
    }
    pub fn right(w_cells: u32, row: u32) -> Self {
        Self::at(w_cells, row, vec2(1.0, 0.0))
    }
    /// One cell past the right edge, so the inversion bubble drawn in the gap stays on-grid.
    pub fn right_bubble(w_cells: u32, row: u32) -> Self {
        Self::at(w_cells + 1, row, vec2(1.0, 0.0))
    }
    pub fn bottom(col: u32, h_cells: u32) -> Self {
        Self::at(col, h_cells, vec2(0.0, 1.0))
    }
    pub fn top(col: u32) -> Self {
        Self::at(col, 0, vec2(0.0, -1.0))
    }
}

// A fixed-position, non-editable label at a position in the component's normalized
// [0,1]^2 box (e.g. a Register's "D"/"WE" pins). Tunnels use `dynamic_label_pos` instead.
#[derive(Debug)]
pub struct ComponentLabel {
    pub text: &'static str,
    pub pos: Vec2,
    pub font_size: f32,
}

impl Default for ComponentLabel {
    fn default() -> Self {
        Self {
            text: Default::default(),
            pos: Default::default(),
            font_size: crate::gui::geometry::LABEL_FONT_SIZE,
        }
    }
}

#[derive(Debug)]
pub struct ComponentShape {
    pub size: Vec2,
    /// May be concave.
    pub outline: Vec<ShapeCmd>,
    /// Convex-only version of `outline`, since epaint's fill tessellator needs convexity.
    /// `None` reuses `outline`.
    pub fill_outline: Option<Vec<ShapeCmd>>,
    pub input_anchors: Vec<PinAnchor>,
    pub output_anchors: Vec<PinAnchor>,
    pub extra_strokes: Vec<Vec<ShapeCmd>>,
    pub output_bubbles: Vec<bool>,
    pub labels: Vec<ComponentLabel>,
    /// A tunnel's user-editable label position (`PlacedTunnel.label`). Unused by Components.
    pub dynamic_label_pos: Vec2,
}

pub fn tessellate_path(cmds: &[ShapeCmd], rect: Rect) -> Vec<Pos2> {
    puffin::profile_function!();
    let scale = |v: Vec2| {
        pos2(
            rect.left() + v.x * rect.width(),
            rect.top() + v.y * rect.height(),
        )
    };
    let mut pts: Vec<Pos2> = Vec::new();
    let mut cur = pos2(0.0, 0.0);
    for cmd in cmds {
        match cmd {
            ShapeCmd::MoveTo(v) => {
                cur = scale(*v);
                pts.push(cur);
            }
            ShapeCmd::LineTo(v) => {
                cur = scale(*v);
                pts.push(cur);
            }
            ShapeCmd::CubicTo(c1, c2, end) => {
                let p0 = cur;
                let p1 = scale(*c1);
                let p2 = scale(*c2);
                let p3 = scale(*end);
                for i in 1..=BEZIER_STEPS {
                    let t = i as f32 / BEZIER_STEPS as f32;
                    let u = 1.0 - t;
                    cur = pos2(
                        u * u * u * p0.x
                            + 3.0 * u * u * t * p1.x
                            + 3.0 * u * t * t * p2.x
                            + t * t * t * p3.x,
                        u * u * u * p0.y
                            + 3.0 * u * u * t * p1.y
                            + 3.0 * u * t * t * p2.y
                            + t * t * t * p3.y,
                    );
                    pts.push(cur);
                }
            }
        }
    }
    pts
}
