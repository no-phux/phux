//! Generate reconnect inventories from the canonical cockpit_fixture ATTACHED.
//! Run with phux-protocol and bytes dependencies; arguments are input and output directories.
use bytes::BytesMut;
use phux_protocol::{TerminalId, wire::frame::FrameKind};
use std::{error::Error, path::Path};

fn write(input: &Path, output: &Path, name: &str, last: u32) -> Result<(), Box<dyn Error>> {
    let bytes = std::fs::read(input.join("attached.bin"))?;
    let mut rest = bytes.as_slice();
    let mut frames = Vec::new();
    while !rest.is_empty() {
        let (frame, next) = FrameKind::decode(rest)?;
        frames.push(frame);
        rest = next;
    }
    let ids: Vec<_> = (7..22).chain([last]).map(TerminalId::local).collect();
    let mut attached = frames[0].clone();
    if let FrameKind::Attached { snapshot, .. } = &mut attached {
        let template = snapshot.panes[0].clone();
        snapshot.panes = ids
            .iter()
            .map(|id| {
                let mut pane = template.clone();
                pane.id = id.clone();
                pane
            })
            .collect();
    }
    let mut encoded = BytesMut::new();
    attached.encode(&mut encoded);
    for id in &ids {
        for template in &frames[1..4] {
            let mut frame = template.clone();
            match &mut frame {
                FrameKind::BootstrapBegin { terminal_id, .. }
                | FrameKind::BootstrapReady { terminal_id, .. } => *terminal_id = id.clone(),
                FrameKind::BootstrapChunk {
                    terminal_id,
                    payload,
                    ..
                } => {
                    *terminal_id = id.clone();
                    // TITLE arrives before ATTACH_READY and must fit the replacement slot.
                    if last == 23 && id == &TerminalId::local(23) {
                        *payload = b"\x1b[2J\x1b[HNEW REPLICA\x1b]2;replacement\x07"
                            .as_slice()
                            .into();
                    }
                }
                _ => unreachable!("canonical three-frame bootstrap"),
            }
            frame.encode(&mut encoded);
        }
    }
    frames[4].encode(&mut encoded);
    std::fs::write(output.join(name), encoded)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    let input = Path::new(&args[1]);
    let output = Path::new(&args[2]);
    write(input, output, "reconnect_initial.bin", 22)?;
    write(input, output, "reconnect_replacement.bin", 23)
}
