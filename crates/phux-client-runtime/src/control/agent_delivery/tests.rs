use super::*;
use crate::control::{ControlOptions, Status};
use phux_client_core::session::agent_stream::{AgentEventKind, AgentEventRecord};
use phux_protocol::{BootstrapId, StreamId};

fn publication(id: u32, mode: AgentRecordMode, seqs: &[u64]) -> AgentPublication {
    AgentPublication {
        resource_id: ResourceId::local(id),
        stream: AgentStreamInfo {
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            last_seq: seqs.last().copied().unwrap_or(0),
        },
        mode,
        records: seqs
            .iter()
            .map(|seq| AgentEventRecord {
                seq: *seq,
                ts_ms: 1,
                kind: AgentEventKind::Prompt,
                data: serde_json::json!({}),
            })
            .collect(),
    }
}

fn batches(plane: &mut ControlPlane) -> Vec<(ResourceId, AgentRecordMode, Vec<u64>)> {
    plane
        .take_events_leased()
        .into_iter()
        .filter_map(|event| match event {
            Event::AgentPublication { publication, .. } => Some((
                publication.resource_id.clone(),
                publication.mode,
                publication
                    .records
                    .iter()
                    .map(|record| record.seq)
                    .collect(),
            )),
            _ => None,
        })
        .collect()
}

fn rebind(plane: &mut ControlPlane) {
    plane.park_agent_delivery();
    plane.agent_delivery.admit(&ResourceId::local(1));
    plane.agent_delivery.admit(&ResourceId::local(2));
    plane.release_agent_delivery();
}

#[test]
fn full_agent_cut_supersedes_unaccepted_records_but_resume_keeps_them() {
    use AgentRecordMode::{Append, Replace};
    for leased in [false, true] {
        let mut plane = ControlPlane::new(ControlOptions::default());
        plane.status = Status::Attached;
        rebind(&mut plane);
        plane.commit_agent_publication(publication(1, Append, &[3]));
        if leased {
            assert_eq!(
                batches(&mut plane),
                [(ResourceId::local(1), Append, vec![3])]
            );
        }
        plane.commit_agent_publication(publication(2, Append, &[90]));
        plane.commit_agent_publication(publication(1, Replace, &[3, 4]));
        let full = vec![
            (ResourceId::local(2), Append, vec![90]),
            (ResourceId::local(1), Replace, vec![3, 4]),
        ];
        assert_eq!(
            batches(&mut plane),
            full,
            "queued prefix is superseded, sibling is not"
        );
        rebind(&mut plane);
        assert_eq!(
            batches(&mut plane),
            full,
            "only the full cut and sibling replay"
        );
        plane.commit_agent_publication(publication(1, Append, &[5]));
        rebind(&mut plane);
        let mut resumed = full;
        resumed.push((ResourceId::local(1), Append, vec![5]));
        assert_eq!(
            batches(&mut plane),
            resumed,
            "a resume needs its unaccepted prefix"
        );
    }
}
