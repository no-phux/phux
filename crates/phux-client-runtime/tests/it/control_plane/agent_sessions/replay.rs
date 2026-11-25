//! Agent bootstrap, cursor recovery and retained-publication replay.

use super::*;

#[test]
fn same_incarnation_reconnect_requests_the_kernel_committed_agent_cursor() {
    let (mut plane, attach_id) = subscribing_with_resume(true, true);
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
    let requests = agent_requests(&mut plane);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1, None, "no committed generation yet");
    commit_initial_agent(&mut plane, requests[0].0);
    let old = agent_info(&plane).unwrap();
    let requests = reconnect_agents(&mut plane, true, 0xAB);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1, Some(2));
    assert_eq!(agent_info(&plane), Some(old));
    agent_begin(&mut plane, 2, Some(2), 4);
    agent_chunk(&mut plane, 2, 0, agent_payload(&[(3, "tool_start")])).unwrap();
    agent_chunk(&mut plane, 2, 1, agent_payload(&[(4, "provider_raw")])).unwrap();
    assert!(
        take_agent_records(&mut plane).is_empty(),
        "no staging publication"
    );
    assert_eq!(agent_cursor(&plane), Some(2));
    agent_ready(&mut plane, 2).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![3, 4], false)]);
    assert_eq!(agent_cursor(&plane), Some(4));
    agent_live(&mut plane, 2, &[(5, "stop")]).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![5], false)]);
    assert_eq!(agent_cursor(&plane), Some(5));
    assert!(agent_live(&mut plane, 1, &[(3, "stop")]).is_err());
    assert!(take_agent_records(&mut plane).is_empty());
}

#[test]
fn a_full_bootstrap_answering_an_evicted_resume_cursor_replaces() {
    let mut plane = resumed_with_initial_agent();
    assert_eq!(reconnect_agents(&mut plane, true, 0xAB).len(), 1);
    agent_begin(&mut plane, 2, None, 5);
    agent_chunk(
        &mut plane,
        2,
        0,
        agent_payload(&[(4, "prompt"), (5, "stop")]),
    )
    .unwrap();
    assert!(take_agent_records(&mut plane).is_empty());
    agent_ready(&mut plane, 2).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![4, 5], true)]);
    let requests = reconnect_agents(&mut plane, true, 0xAB);
    assert_eq!(requests[0].1, Some(5));
    agent_bootstrap(&mut plane, 3, None, &[(7, "prompt"), (8, "stop")]);
    assert_eq!(take_agent_records(&mut plane), [(vec![7, 8], true)]);
}

#[test]
fn new_incarnation_forgets_agent_cursors_before_subscribing_recycled_ids() {
    let mut plane = resumed_with_initial_agent();
    let requests = reconnect_agents(&mut plane, true, 0xAC);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1, None);
    assert_eq!(agent_cursor(&plane), None);
    assert!(agent_info(&plane).is_none());
    assert_eq!(
        plane.take_events().into_iter().filter(|event| matches!(event, Event::AgentSessionClosed { terminal_id, .. } if *terminal_id == ResourceId::local(AGENT))).count(),
        1
    );
    agent_bootstrap(&mut plane, 1, None, &[(1, "prompt")]);
    assert_eq!(take_agent_records(&mut plane), [(vec![1], true)]);
    assert_eq!(agent_cursor(&plane), Some(1));
}

#[test]
fn zero_missed_agent_records_resume_without_duplicates_then_handoff_to_live() {
    let mut plane = resumed_with_initial_agent();
    let requests = reconnect_agents(&mut plane, true, 0xAB);
    assert_eq!(requests[0].1, Some(2));
    agent_begin(&mut plane, 2, Some(2), 2);
    agent_ready(&mut plane, 2).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![], false)]);
    let committed = agent_info(&plane).unwrap();
    assert_eq!((committed.stream_id, committed.bootstrap_id), agent_ids(2));
    assert_eq!(committed.last_seq, 2);
    agent_live(&mut plane, 2, &[(3, "stop")]).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![3], false)]);
    // Explicit same-connection reattach also appends only the missed suffix.
    agent_bootstrap(&mut plane, 3, Some(3), &[(4, "prompt")]);
    assert_eq!(take_agent_records(&mut plane), [(vec![4], false)]);
    agent_live(&mut plane, 3, &[(5, "stop")]).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![5], false)]);
}

#[test]
fn failed_agent_delta_bootstraps_never_publish_or_advance_a_resume_cursor() {
    for (chunk_seq, payload) in [
        (0, agent_payload(&[(3, "stop"), (5, "ask")])),
        (0, agent_payload(&[(3, "stop"), (3, "ask")])),
        (0, agent_payload(&[(4, "stop")])),
        (0, agent_payload(&[(3, "stop")])),
        (0, String::new()),
        (0, String::from("{malformed}\n")),
        (0, agent_payload(&[(3, "stop")]) + "{\"seq\":4}\n"),
        (1, agent_payload(&[(3, "stop"), (4, "ask")])),
    ] {
        let mut plane = resumed_with_initial_agent();
        let committed = agent_info(&plane);
        agent_begin(&mut plane, 2, Some(2), 4);
        let _ = agent_chunk(&mut plane, 2, chunk_seq, payload);
        assert!(agent_ready(&mut plane, 2).is_err());
        assert!(take_agent_records(&mut plane).is_empty());
        assert_eq!(agent_cursor(&plane), Some(2));
        assert_eq!(agent_info(&plane), committed);
        let requests = reconnect_agents(&mut plane, true, 0xAB);
        assert_eq!(requests[0].1, Some(2));
        agent_bootstrap(&mut plane, 3, Some(2), &[(3, "stop")]);
        assert_eq!(take_agent_records(&mut plane), [(vec![3], false)]);
    }
}

#[test]
fn empty_full_agent_bootstrap_is_a_consumer_visible_replacement() {
    let mut plane = resumed_with_initial_agent();
    agent_begin(&mut plane, 2, None, 2);
    agent_ready(&mut plane, 2).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![], true)]);
    assert_eq!(agent_cursor(&plane), Some(2));
}

#[test]
fn reconnect_before_agent_ready_requests_full_instead_of_unverified_records() {
    let (mut plane, attach_id) = subscribing_with_resume(true, true);
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
    assert_eq!(agent_requests(&mut plane)[0].1, None);
    agent_begin(&mut plane, 1, None, 3);
    agent_chunk(
        &mut plane,
        1,
        0,
        agent_payload(&[(1, "prompt"), (2, "ask")]),
    )
    .unwrap();
    assert!(take_agent_records(&mut plane).is_empty());
    assert_eq!(agent_cursor(&plane), None);
    let requests = reconnect_agents(&mut plane, true, 0xAB);
    assert_eq!(requests[0].1, None);
    agent_bootstrap(
        &mut plane,
        2,
        None,
        &[(1, "prompt"), (2, "ask"), (3, "stop")],
    );
    assert_eq!(take_agent_records(&mut plane), [(vec![1, 2, 3], true)]);
    assert_eq!(agent_cursor(&plane), Some(3));
}

#[test]
fn resource_close_and_parent_detach_clear_the_committed_agent_cursor() {
    for close in [true, false] {
        let mut plane = resumed_with_initial_agent();
        let closed = retire_agent(&mut plane, close);
        assert_eq!(agent_cursor(&plane), None, "close={close}");
        assert!(agent_info(&plane).is_none(), "close={close}");
        assert_eq!(closed, [Some(2)], "close={close}");
    }
}

#[test]
fn undrained_frames_replay_in_order_after_replacement_without_loss() {
    let mut plane = resumed_with_initial_agent();
    agent_live(&mut plane, 1, &[(3, "tool_start")]).unwrap();
    agent_live(&mut plane, 1, &[(4, "tool_end")]).unwrap();
    // Replacement can open without a preceding connection_lost.
    replace_attached(&mut plane, with_agent(terminal()));
    assert_eq!(
        take_agent_records(&mut plane),
        [(vec![3], false), (vec![4], false)]
    );
    let requests = agent_requests(&mut plane);
    assert_eq!(requests[0].1, Some(4));
    agent_bootstrap(&mut plane, 2, Some(4), &[(5, "stop")]);
    assert_eq!(take_agent_records(&mut plane), [(vec![5], false)]);
}

#[test]
fn close_and_detach_do_not_replay_undrained_publications() {
    for close in [true, false] {
        let mut plane = resumed_with_initial_agent();
        agent_live(&mut plane, 1, &[(3, "stop")]).unwrap();
        assert_eq!(retire_agent(&mut plane, close), [Some(3)], "close={close}");
        replace_attached(&mut plane, with_agent(terminal()));
        assert!(take_agent_records(&mut plane).is_empty());
    }
}

