//! Elements of an accessibility tree, by node instead of pixel
//! (`docs/applications.md#agents-clicking-elements`).
//!
//! A tree document names each node by a stable `id`: the macOS and AT-SPI
//! mirrors write the path of child indices from the root (`0.3.1`) and the
//! node's `frame` in screen points, `[x, y, w, h]`; a page's `DevTools` tree
//! is flat and names each node by its `nodeId`. [`click`] finds a node and
//! asks the tree's producer to activate it (`AXPress`, AT-SPI `DoAction`,
//! a `DevTools` click on the element). When the tree refuses the intent, it
//! clicks the node's centre on the sibling window surface instead, mapped
//! into that surface's frame pixels.

use std::path::Path;

use phux_protocol::ids::{ResourceId, ResourceKind};
use phux_protocol::wire::frame::ControlClass;
use serde_json::Value;

use super::{DocumentInput, TreeError, image_size, send_input, snapshot};
use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// A rectangle in screen points, y down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl Rect {
    fn of(value: &Value) -> Option<Self> {
        let n = |i: usize| value.get(i).and_then(Value::as_f64);
        Some(Self {
            x: n(0)?,
            y: n(1)?,
            w: n(2)?,
            h: n(3)?,
        })
    }
}

/// One node of a tree document.
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    /// What [`DocumentInput::Activate`] names.
    pub id: String,
    /// Its role as the producer names it (`AXButton`, `push button`).
    pub role: String,
    /// The title, else the description: what a person reads on it.
    pub name: String,
    /// Its bounds, when the producer wrote them.
    pub frame: Option<Rect>,
    /// The bounds of the window it is in (the nearest window-role ancestor).
    pub window: Option<Rect>,
}

/// Which node to act on.
#[derive(Debug, Clone, Copy)]
pub enum Selector<'a> {
    /// By its `id`.
    Id(&'a str),
    /// By its name (ignoring case; an exact name beats a substring), and
    /// optionally its role.
    Name {
        /// The name.
        name: &'a str,
        /// The role, ignoring case.
        role: Option<&'a str>,
    },
}

/// Every node of `document`, in document order.
#[must_use]
pub fn elements(document: &Value) -> Vec<Element> {
    let mut out = Vec::new();
    if let Some(nodes) = document["nodes"].as_array() {
        // A `DevTools` tree: flat, named by `nodeId`, no bounds.
        let text = |node: &Value, key: &str| node[key]["value"].as_str().unwrap_or("").to_owned();
        out.extend(
            nodes
                .iter()
                .filter(|node| node["ignored"] != Value::Bool(true))
                .filter_map(|node| {
                    Some(Element {
                        id: node["nodeId"].as_str()?.to_owned(),
                        role: text(node, "role"),
                        name: text(node, "name"),
                        frame: None,
                        window: None,
                    })
                }),
        );
    } else {
        nested(document, None, &mut out);
    }
    out
}

fn nested(node: &Value, window: Option<Rect>, out: &mut Vec<Element>) {
    let text = |key: &str| node[key].as_str().unwrap_or("");
    let frame = Rect::of(&node["frame"]);
    let role = text("role");
    let window = if matches!(role, "AXWindow" | "window" | "frame" | "dialog") {
        frame.or(window)
    } else {
        window
    };
    if let Some(id) = node["id"].as_str() {
        let name = if text("title").is_empty() {
            text("description")
        } else {
            text("title")
        };
        out.push(Element {
            id: id.to_owned(),
            role: role.to_owned(),
            name: name.to_owned(),
            frame,
            window,
        });
    }
    for child in node["children"].as_array().into_iter().flatten() {
        nested(child, window, out);
    }
}

/// Why no single node was found.
#[derive(Debug, Clone, PartialEq)]
pub enum FindError {
    /// No node matches; `known` says whether the document names nodes at all.
    NoMatch {
        /// Whether the document names any node.
        known: bool,
    },
    /// Several nodes match equally; act on one by its id.
    Ambiguous(Vec<Element>),
}

/// The one node `selector` names in `all`.
///
/// # Errors
///
/// [`FindError`] when none or several match.
pub fn find(all: &[Element], selector: Selector<'_>) -> Result<Element, FindError> {
    let matched: Vec<&Element> = match selector {
        Selector::Id(id) => all.iter().filter(|e| e.id == id).collect(),
        Selector::Name { name, role } => {
            let want = name.to_lowercase();
            let fits = |e: &&Element| role.is_none_or(|role| e.role.eq_ignore_ascii_case(role));
            let exact: Vec<&Element> = all
                .iter()
                .filter(fits)
                .filter(|e| e.name.to_lowercase() == want)
                .collect();
            if exact.is_empty() {
                all.iter()
                    .filter(fits)
                    .filter(|e| e.name.to_lowercase().contains(&want))
                    .collect()
            } else {
                exact
            }
        }
    };
    match matched.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(FindError::NoMatch {
            known: !all.is_empty(),
        }),
        many => Err(FindError::Ambiguous(
            many.iter().map(|e| (*e).clone()).collect(),
        )),
    }
}

/// The pixel of an `image`-sized frame of `window` under the centre of
/// `node`; `None` when the centre is outside the window or nothing is sized.
///
/// Points per frame pixel covers a Retina backing scale and `--max-width`
/// downscaling at once.
#[must_use]
pub fn frame_pixel(node: Rect, window: Rect, image: (u32, u32)) -> Option<(u32, u32)> {
    let axis = |centre: f64, origin: f64, len: f64, px: u32| {
        let at = (centre - origin) * f64::from(px) / len;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked to lie inside the frame"
        )]
        (len > 0.0 && px > 0 && (0.0..f64::from(px)).contains(&at)).then(|| at.floor() as u32)
    };
    Some((
        axis(node.x + node.w / 2.0, window.x, window.w, image.0)?,
        axis(node.y + node.h / 2.0, window.y, window.h, image.1)?,
    ))
}

/// How a click was carried out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clicked {
    /// The tree's producer was asked to activate the node.
    Semantic,
    /// The node's centre was clicked on `surface`, at frame pixel `x, y`.
    Pixel {
        /// The window surface clicked.
        surface: ResourceId,
        /// Frame pixels from the left edge.
        x: u32,
        /// Frame pixels from the top edge.
        y: u32,
    },
}

/// Why [`click`] did nothing.
#[derive(Debug)]
pub enum ClickError {
    /// A refusal or connection failure.
    Tree(TreeError),
    /// The tree's document is not JSON, or there is none yet.
    NoDocument,
    /// No single node matched.
    Find(FindError),
    /// The node cannot be clicked by pixel (`why`), after the tree refused
    /// the intent (`refused`) or when a pixel click was asked for.
    NoPixel {
        /// The tree's refusal, when it was asked first.
        refused: Option<TreeError>,
        /// Why no pixel click.
        why: String,
    },
}

impl From<TreeError> for ClickError {
    fn from(err: TreeError) -> Self {
        Self::Tree(err)
    }
}

impl From<AttachError> for ClickError {
    fn from(err: AttachError) -> Self {
        Self::Tree(err.into())
    }
}

impl std::fmt::Display for ClickError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tree(err) => write!(f, "{err}"),
            Self::NoDocument => f.write_str("the tree has no JSON document yet"),
            Self::Find(FindError::NoMatch { known: true }) => f.write_str("no node matches"),
            Self::Find(FindError::NoMatch { known: false }) => f.write_str(
                "no node matches: the tree names no nodes (its producer predates node ids)",
            ),
            Self::Find(FindError::Ambiguous(many)) => {
                write!(f, "{} nodes match:", many.len())?;
                for e in many.iter().take(20) {
                    write!(f, " [{}] {} {:?};", e.id, e.role, e.name)?;
                }
                Ok(())
            }
            Self::NoPixel {
                refused: Some(refused),
                why,
            } => write!(f, "the tree refused the click ({refused}) and {why}"),
            Self::NoPixel { refused: None, why } => f.write_str(why),
        }
    }
}

/// Find the node `selector` names in the tree `tree` and activate it.
///
/// By its producer when the tree takes the intent, else (or with `pixel`) by
/// a click at its centre on the window surface beside the tree. Either path
/// needs the same input authority as any `SEND_DOCUMENT_INPUT`.
///
/// # Errors
///
/// [`ClickError`]: no single node, or every path refused.
pub async fn click(
    socket: &Path,
    tree: &ResourceId,
    selector: Selector<'_>,
    pixel: bool,
) -> Result<(Element, Clicked), ClickError> {
    let document = snapshot(&mut Connection::connect(socket).await?, tree).await?;
    let document: Value =
        serde_json::from_slice(&document.bytes).map_err(|_| ClickError::NoDocument)?;
    let element = find(&elements(&document), selector).map_err(ClickError::Find)?;
    if !pixel {
        let activate = DocumentInput::Activate {
            node: element.id.clone(),
        };
        match send_input(&mut Connection::connect(socket).await?, tree, activate).await {
            Ok(()) => return Ok((element, Clicked::Semantic)),
            Err(refused) => {
                return match by_pixel(socket, tree, &element).await? {
                    Ok(clicked) => Ok((element, clicked)),
                    Err(why) => Err(ClickError::NoPixel {
                        refused: Some(refused),
                        why,
                    }),
                };
            }
        }
    }
    match by_pixel(socket, tree, &element).await? {
        Ok(clicked) => Ok((element, clicked)),
        Err(why) => Err(ClickError::NoPixel { refused: None, why }),
    }
}

/// Click `element`'s centre on the window surface beside `tree`; the inner
/// `Err` says why it cannot be.
// ponytail: assumes the surface shows the node's window (true for `phux agent
// app run`, which mirrors the first window); a display surface is refused.
async fn by_pixel(
    socket: &Path,
    tree: &ResourceId,
    element: &Element,
) -> Result<Result<Clicked, String>, ClickError> {
    let (Some(frame), Some(window)) = (element.frame, element.window) else {
        return Ok(Err(
            "the node has no screen bounds, so it cannot be clicked by pixel".to_owned(),
        ));
    };
    let resources = crate::state::get_state(socket)
        .await?
        .into_snapshot_ignoring_degradation()
        .resources;
    let parent = resources
        .iter()
        .find(|info| info.id == *tree)
        .and_then(|info| info.parent.clone());
    let Some(surface) = resources.iter().find(|info| {
        info.kind == ResourceKind::PixelSurface
            && info.control == Some(ControlClass::Window)
            && parent.is_some()
            && info.parent == parent
    }) else {
        return Ok(Err(
            "no window surface shares its pane, so it cannot be clicked by pixel".to_owned(),
        ));
    };
    let image = snapshot(&mut Connection::connect(socket).await?, &surface.id).await?;
    let Ok((_, width, height)) = image_size(&image.bytes) else {
        return Ok(Err("the window surface has no frame yet".to_owned()));
    };
    let Some((x, y)) = frame_pixel(frame, window, (width, height)) else {
        return Ok(Err("the node lies outside its window's frame".to_owned()));
    };
    let mut conn = Connection::connect(socket).await?;
    send_input(&mut conn, &surface.id, DocumentInput::Click { x, y }).await?;
    Ok(Ok(Clicked::Pixel {
        surface: surface.id.clone(),
        x,
        y,
    }))
}

#[cfg(test)]
mod tests;
