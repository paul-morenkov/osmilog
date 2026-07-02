use egui::{pos2, vec2, Pos2, Rect, Vec2};

pub const BUBBLE_R: f32 = 4.0;
const BEZIER_STEPS: usize = 16;

#[derive(Clone)]
pub enum ShapeCmd {
    MoveTo(Vec2),
    LineTo(Vec2),
    CubicTo(Vec2, Vec2, Vec2),
}

#[derive(Clone)]
pub struct PinAnchor {
    pub norm_pos: Vec2,
    pub wire_dir: Vec2,
    pub pixel_offset: f32,
}

impl PinAnchor {
    pub fn left(y: f32) -> Self {
        Self {
            norm_pos: vec2(0.0, y),
            wire_dir: vec2(-1.0, 0.0),
            pixel_offset: 0.0,
        }
    }
    pub fn right(y: f32) -> Self {
        Self {
            norm_pos: vec2(1.0, y),
            wire_dir: vec2(1.0, 0.0),
            pixel_offset: 0.0,
        }
    }
    pub fn right_bubble(y: f32) -> Self {
        Self {
            norm_pos: vec2(1.0, y),
            wire_dir: vec2(1.0, 0.0),
            pixel_offset: BUBBLE_R * 2.0,
        }
    }
    pub fn bottom_mid(x: f32, y: f32) -> Self {
        Self {
            norm_pos: vec2(x, y),
            wire_dir: vec2(0.0, 1.0),
            pixel_offset: 0.0,
        }
    }
}

// A hardcoded, non-editable label (e.g. a Register's "D"/"WE" pin
// annotations) at a fixed position within the component's normalized
// [0,1]^2 box. Only meaningful for Components - Tunnels use
// `ComponentShape::dynamic_label_pos` instead, since their single label's
// *text* is user-editable at runtime rather than known at shape() time.
pub struct ComponentLabel {
    pub text: &'static str,
    pub pos: Vec2,
}

pub struct ComponentShape {
    pub size: Vec2,
    /// Full outline used for the stroke (may be concave).
    pub outline: Vec<ShapeCmd>,
    /// Convex-only outline used for the fill. When `None`, `outline` is used for both.
    /// Required for shapes whose outline is concave, because epaint's fill tessellator
    /// assumes a convex polygon (triangle fan + feathering with inward normals).
    pub fill_outline: Option<Vec<ShapeCmd>>,
    pub input_anchors: Vec<PinAnchor>,
    pub output_anchors: Vec<PinAnchor>,
    pub extra_strokes: Vec<Vec<ShapeCmd>>,
    pub output_bubbles: Vec<bool>,
    /// Hardcoded pin/section labels, positioned per component type/parameters. Empty for
    /// component types with nothing non-obvious to annotate.
    pub labels: Vec<ComponentLabel>,
    /// Position for a single externally-supplied, editable label string. Only meaningful for
    /// Tunnels (see PlacedTunnel.label) - unused (left at the type's default) by Components.
    pub dynamic_label_pos: Vec2,
}

pub fn tessellate_path(cmds: &[ShapeCmd], rect: Rect) -> Vec<Pos2> {
    // Converts from normalized coordinate to rect coordinate
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
