use std::path::Path;

use phux_client::attach::connection::Connection;
use phux_client::layout::{Direction as LayoutDirection, SplitDir};
use phux_client::layout_ops::LayoutOps;
use phux_protocol::ResourceId;
use phux_protocol::ids::GroupId;
use phux_protocol::input::paste::PasteTrust;
use phux_protocol::wire::frame::{ArrangementNode, Command, FrameKind, SpawnResult};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{METHODS, Request, panes};

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Direction {
    Right,
    Left,
    Down,
    Up,
}

impl Direction {
    const fn axis(self) -> SplitDir {
        match self {
            Self::Right | Self::Left => SplitDir::Horizontal,
            Self::Down | Self::Up => SplitDir::Vertical,
        }
    }

    const fn before(self) -> bool {
        matches!(self, Self::Left | Self::Up)
    }

    const fn neighbor(self) -> LayoutDirection {
        match self {
            Self::Right => LayoutDirection::Right,
            Self::Left => LayoutDirection::Left,
            Self::Down => LayoutDirection::Down,
            Self::Up => LayoutDirection::Up,
        }
    }
}

#[derive(Deserialize)]
struct PaneRef {
    id: String,
    tab: String,
}
#[derive(Deserialize)]
struct Split {
    from: PaneRef,
    direction: Direction,
    command: Vec<String>,
    size: Option<f32>,
}

fn params<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, String> {
    serde_json::from_value(value.clone()).map_err(|e| format!("invalid params: {e}"))
}

pub(super) async fn dispatch(server: &Path, request: &Request) -> Result<Value, String> {
    match request.method.as_str() {
        "hello" => {
            if request.params.get("protocol").and_then(Value::as_u64) != Some(1) {
                return Err("unsupported terminal-browser protocol (expected 1)".into());
            }
            Ok(json!({ "name": "phux", "methods": METHODS }))
        }
        "getCurrentPane" | "listPanes" | "neighbor" => inspect(server, request).await,
        "split" => split(server, request).await,
        "sendText" => send_text(server, &request.params).await,
        "pasteKey" => paste_key(server, &request.params).await,
        // Focus belongs to each attached client. Do not advertise a global
        // focus mutation or misuse INPUT_FOCUS (host-window focus reports).
        _ => Err("unsupported terminal-browser method".into()),
    }
}

async fn inspect(server: &Path, request: &Request) -> Result<Value, String> {
    let processes = panes::Processes::read().await?;
    let inventory = panes::read(server, &processes).await?;
    match request.method.as_str() {
        "listPanes" => Ok(json!({ "panes": inventory.panes })),
        "getCurrentPane" => {
            let tty = request.params.get("tty").and_then(Value::as_str);
            Ok(json!({ "pane": inventory.current(tty, &request.caller, &processes) }))
        }
        _ => {
            #[derive(Deserialize)]
            struct Neighbor {
                from: PaneRef,
                direction: Direction,
            }
            let query: Neighbor = params(&request.params)?;
            validate_pane(&inventory, &query.from)?;
            Ok(json!({ "pane": inventory.neighbor(&query.from.id, query.direction.neighbor()) }))
        }
    }
}

fn validate_pane(inventory: &panes::Inventory, pane: &PaneRef) -> Result<(), String> {
    if inventory
        .pane(&pane.id)
        .is_some_and(|known| known.pane.tab == pane.tab)
    {
        Ok(())
    } else {
        Err("pane no longer belongs to that tab".into())
    }
}

async fn split(server: &Path, request: &Request) -> Result<Value, String> {
    let split: Split = params(&request.params)?;
    let size = validate_split(&split, &request.caller.cwd)?;
    create_split(server, request, split, size).await
}

fn validate_split(split: &Split, cwd: &str) -> Result<f32, String> {
    let size = split.size.unwrap_or(0.5);
    if !(size.is_finite() && 0.0 < size && size < 1.0) {
        return Err("split size must be finite and strictly between 0 and 1".into());
    }
    if split.command.is_empty() || split.command[0].is_empty() {
        return Err("split command must be a nonempty argv array".into());
    }
    if !Path::new(cwd).is_absolute() {
        return Err("caller cwd must be absolute".into());
    }
    Ok(size)
}

async fn create_split(
    server: &Path,
    request: &Request,
    mut split: Split,
    size: f32,
) -> Result<Value, String> {
    let processes = panes::Processes::read().await?;
    let inventory = panes::read(server, &processes).await?;
    validate_pane(&inventory, &split.from)?;
    let anchor = inventory.representative(&split.from.id)?;
    split.from.id = format!("@{}", anchor.local_id().ok_or("not a local pane")?);
    // The existing create contract inserts after the anchor. For left/up,
    // swap the two leaves afterward; never relaunch or kill on failure.
    let ratio = if split.direction.before() {
        size
    } else {
        1.0 - size
    };
    let new = spawn_split(server, &request.caller.cwd, &split, ratio).await?;
    if split.direction.before() {
        reverse_split(server, anchor, new.clone()).await.map_err(|error| {
            format!("pane {new:?} was created, but left/up placement failed: {error}; inspect phux ls before retrying")
        })?;
    }
    Ok(json!({}))
}

async fn spawn_split(
    server: &Path,
    cwd: &str,
    split: &Split,
    ratio: f32,
) -> Result<ResourceId, String> {
    let frame = FrameKind::SpawnResource {
        request_id: 1,
        group: GroupId::new(1),
        command: Some(split.command.clone()),
        cwd: Some(cwd.to_owned()),
        env: None,
        term: None,
        satellite: None,
        owner_terminal: None,
        agent_session: None,
        initial_size: None,
        resource: None,
    };
    let operation = phux_client::spawn::PlacedSpawn::from_spawn_frame(
        frame,
        split.direction.axis(),
        ratio,
        None,
        None,
    )
    .map_err(|e| e.to_string())?;
    let selector = phux_client::selector::parse(&split.from.id).map_err(|e| e.to_string())?;
    let (result, _) = operation
        .run(server, &selector, 1)
        .await
        .map_err(|e| e.to_string())?;
    match result {
        SpawnResult::Ok(terminal_id) => Ok(terminal_id),
        SpawnResult::Err(error) => Err(phux_client::spawn::spawn_error_message(&error)),
        _ => Err("split outcome unknown; inspect phux ls before retrying".into()),
    }
}

async fn reverse_split(server: &Path, anchor: ResourceId, new: ResourceId) -> Result<(), String> {
    let mut conn = Connection::connect(server)
        .await
        .map_err(|e| e.to_string())?;
    let view = phux_client::state::get_state_on(&mut conn)
        .await
        .map_err(|e| e.to_string())?;
    let session = phux_client::resource::session_of(view.snapshot(), &anchor)
        .ok_or("anchor session disappeared")?;
    let mut read = LayoutOps::new(&mut conn, session, 100)
        .read_arrangement()
        .await
        .map_err(|e| e.to_string())?;
    let anchor_entry = read
        .binding
        .entry(&anchor)
        .ok_or("anchor entry disappeared")?;
    let new_entry = read.binding.entry(&new).ok_or("new entry disappeared")?;
    let swapped = read
        .stored
        .document
        .groups
        .iter_mut()
        .any(|group| reverse_siblings(&mut group.tree, &anchor_entry, &new_entry));
    if !swapped {
        return Err("the new split changed concurrently".into());
    }
    let command = Command::replace_arrangement(
        phux_client::org::mint_operation(),
        read.key,
        read.revision,
        read.stored.document,
        conn.server_features_ext(),
    );
    // One CAS, never swap arbitrary positions after a concurrent layout edit.
    phux_client::org::commit(&mut conn, 200, command)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn reverse_siblings(tree: &mut ArrangementNode, anchor: &[u8; 16], new: &[u8; 16]) -> bool {
    let ArrangementNode::Split { first, second, .. } = tree else {
        return false;
    };
    if matches!(first.as_ref(), ArrangementNode::Leaf(entry) if &entry.entry == anchor)
        && matches!(second.as_ref(), ArrangementNode::Leaf(entry) if &entry.entry == new)
    {
        std::mem::swap(first, second);
        return true;
    }
    reverse_siblings(first, anchor, new) || reverse_siblings(second, anchor, new)
}
async fn send_text(server: &Path, value: &Value) -> Result<Value, String> {
    #[derive(Deserialize)]
    struct SendText {
        pane: String,
        text: String,
    }
    let query: SendText = params(value)?;
    phux_client::send_keys::paste_to(
        server,
        panes::resource_id(&query.pane)?,
        query.text.into_bytes(),
        PasteTrust::Trusted,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(json!({}))
}

async fn paste_key(server: &Path, value: &Value) -> Result<Value, String> {
    #[derive(Deserialize)]
    struct PasteKey {
        pane: String,
    }
    let query: PasteKey = params(value)?;
    phux_client::send_keys::send_to(server, panes::resource_id(&query.pane)?, &["C-v".into()])
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::ids::ResourceKey;
    use phux_protocol::wire::frame::{EntryRef, EntryTarget, SplitDirection};

    fn leaf(id: u8) -> ArrangementNode {
        ArrangementNode::Leaf(EntryRef::new(
            [id; 16],
            EntryTarget::Member(ResourceKey::new([id; 16]).unwrap()),
        ))
    }

    #[test]
    fn left_and_up_keep_the_requested_new_pane_fraction() {
        for dir in [SplitDirection::Horizontal, SplitDirection::Vertical] {
            let mut tree = ArrangementNode::Split {
                dir,
                ratio: 0.4,
                first: Box::new(leaf(1)),
                second: Box::new(leaf(2)),
            };
            assert!(reverse_siblings(&mut tree, &[1; 16], &[2; 16]));
            let ArrangementNode::Split {
                dir: actual_dir,
                ratio,
                first,
                second,
            } = tree
            else {
                panic!("split")
            };
            assert_eq!(actual_dir, dir);
            assert_eq!(ratio, 0.4);
            assert_eq!(*first, leaf(2));
            assert_eq!(*second, leaf(1));
        }
    }

    #[test]
    fn a_concurrent_split_is_not_reordered_as_if_it_were_the_original() {
        let mut tree = ArrangementNode::Split {
            dir: SplitDirection::Horizontal,
            ratio: 0.4,
            first: Box::new(leaf(1)),
            second: Box::new(ArrangementNode::Split {
                dir: SplitDirection::Vertical,
                ratio: 0.5,
                first: Box::new(leaf(2)),
                second: Box::new(leaf(3)),
            }),
        };
        let before = tree.clone();
        assert!(!reverse_siblings(&mut tree, &[1; 16], &[2; 16]));
        assert_eq!(tree, before);
    }

    #[test]
    fn reversing_one_split_preserves_unresolved_resources_and_slot_tabs() {
        let mut slot = EntryRef::new(
            [1; 16],
            EntryTarget::Member(ResourceKey::new([1; 16]).unwrap()),
        );
        slot.tabs.push(EntryRef::new(
            [4; 16],
            EntryTarget::Member(ResourceKey::new([4; 16]).unwrap()),
        ));
        slot.shown_at = 1;
        let anchor = ArrangementNode::Leaf(slot);
        let untouched = ArrangementNode::Split {
            dir: SplitDirection::Horizontal,
            ratio: 0.3,
            first: Box::new(leaf(5)),
            second: Box::new(leaf(6)),
        };
        let mut tree = ArrangementNode::Split {
            dir: SplitDirection::Vertical,
            ratio: 0.7,
            first: Box::new(ArrangementNode::Split {
                dir: SplitDirection::Horizontal,
                ratio: 0.4,
                first: Box::new(anchor.clone()),
                second: Box::new(leaf(2)),
            }),
            second: Box::new(untouched.clone()),
        };
        assert!(reverse_siblings(&mut tree, &[1; 16], &[2; 16]));
        assert_eq!(
            tree,
            ArrangementNode::Split {
                dir: SplitDirection::Vertical,
                ratio: 0.7,
                first: Box::new(ArrangementNode::Split {
                    dir: SplitDirection::Horizontal,
                    ratio: 0.4,
                    first: Box::new(leaf(2)),
                    second: Box::new(anchor),
                }),
                second: Box::new(untouched),
            }
        );
    }
}
