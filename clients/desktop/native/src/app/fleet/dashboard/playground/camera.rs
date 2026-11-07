//! Playground map selection and camera state.
use super::plan::Pt;

/// The maps: the office and matrix skins over the campus, and the galaxy,
/// the fleet as a command center (mobile's galaxy view).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Map {
    Office,
    Matrix,
    Galaxy,
}

impl Map {
    pub fn from_id(id: &str) -> Map {
        match id {
            "matrix" => Map::Matrix,
            "galaxy" => Map::Galaxy,
            _ => Map::Galaxy,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Map::Office => "office",
            Map::Matrix => "matrix",
            Map::Galaxy => "galaxy",
        }
    }

    pub fn next(self) -> Map {
        match self {
            Map::Office => Map::Matrix,
            Map::Matrix => Map::Galaxy,
            Map::Galaxy => Map::Office,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Map::Office => "Office map",
            Map::Matrix => "Matrix map",
            Map::Galaxy => "Galaxy map",
        }
    }
}
pub(super) const SCALES: (f32, f32) = (0.3, 2.);

/// Where the person is looking: an offset in scene points and a scale.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Camera {
    pub offset: Pt,
    pub scale: f32,
    /// Whether the camera has been pointed at the first room that needs you.
    pub placed: bool,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            offset: Pt::default(),
            scale: 0.9,
            placed: false,
        }
    }
}

impl Camera {
    /// Centre `target` (world points) in a scene of `width` by `height`.
    pub(super) fn look(&mut self, target: Pt, width: f32, height: f32) {
        self.offset = Pt {
            x: width / 2. - target.x * self.scale,
            y: height / 2. - target.y * self.scale,
        };
        self.placed = true;
    }

    /// Centre `target` at `scale`.
    pub(super) fn look_at(&mut self, target: Pt, scale: f32, width: f32, height: f32) {
        self.scale = scale;
        self.look(target, width, height);
    }

    /// Zoom by `delta` (0.1 is ten percent) about the scene's centre.
    pub(super) fn pinch(&mut self, delta: f32, width: f32, height: f32) {
        let next = (self.scale * (1. + delta)).clamp(SCALES.0, SCALES.1);
        let ratio = next / self.scale;
        let centre = Pt {
            x: width / 2.,
            y: height / 2.,
        };
        self.offset = Pt {
            x: centre.x - (centre.x - self.offset.x) * ratio,
            y: centre.y - (centre.y - self.offset.y) * ratio,
        };
        self.scale = next;
    }

    pub(super) fn screen(&self, world: Pt) -> Pt {
        Pt {
            x: self.offset.x + world.x * self.scale,
            y: self.offset.y + world.y * self.scale,
        }
    }
}

#[derive(Default)]
pub(crate) struct View {
    pub on: bool,
    pub camera: Camera,
    /// The last pointer position while the left button pans.
    pub drag: Option<Pt>,
    /// The map the camera was placed for; another map places it afresh.
    pub map: Option<Map>,
    /// The waiting agent the galaxy's readout flew to last, so the next click moves on.
    pub last_waiting: Option<String>,
    /// The galaxy node under the pointer, by its tile id: the reticle's target.
    pub hovered: Option<String>,
    /// Where the camera first landed, which a fixture film moves from.
    #[cfg(feature = "ui-fixtures")]
    pub film_base: Option<Camera>,
}

#[cfg(feature = "ui-fixtures")]
impl View {
    /// A fixture film's camera: the placed camera panned by `pan` scene
    /// points, then zoomed by `zoom` about the centre of a `width` by
    /// `height` scene. Nothing moves until the scene has placed the camera.
    pub(crate) fn film_camera(&mut self, pan: (f32, f32), zoom: f32, width: f32, height: f32) {
        if !self.camera.placed {
            return;
        }
        let mut camera = *self.film_base.get_or_insert(self.camera);
        camera.offset.x += pan.0;
        camera.offset.y += pan.1;
        camera.pinch(zoom - 1., width, height);
        self.camera = camera;
    }
}
