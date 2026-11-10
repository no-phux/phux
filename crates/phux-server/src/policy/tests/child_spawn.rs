// Spawning a document kind beside a Terminal: the same parent rows as an
// agent session, so a scoped or delegated grant is not refused by default.

/// A tree, a surface, or a document opened beside a Terminal needs `CREATE`
/// on the parent's Group and `BIND` on the parent, exactly like an agent
/// session; a grant short of either is refused.
#[test]
fn a_scoped_grant_opens_every_child_kind_beside_a_terminal_it_binds() {
    let world = world();
    let parent = world.alpha.clone();
    let on_parent = format!("terminal:{}", local_id(&parent));
    let on_other = format!("terminal:{}", local_id(&world.beta));
    let group = format!("group:{}", world.alpha_group);
    let grant = |set: &[&str]| -> Vec<String> { set.iter().map(|s| (*s).to_owned()).collect() };
    for spec in phux_protocol::kinds::KINDS
        .iter()
        .filter(|spec| !spec.kind.is_terminal())
    {
        let frame = spawn(
            None,
            None,
            Some(SpawnResource {
                kind: spec.kind,
                ..SpawnResource::agent_session(parent.clone(), "text/plain")
            }),
        );
        let create = format!("create@{group}");
        let bind = format!("bind@{on_parent}");
        assert!(
            admits_set(&world, &grant(&[&create, &bind]), &frame),
            "{} refused CREATE+BIND beside its parent",
            spec.name
        );
        for short in [
            grant(&[&create]),
            grant(&[&bind]),
            grant(&[&create, &format!("bind@{on_other}")]),
        ] {
            assert!(
                !admits_set(&world, &short, &frame),
                "{} admitted {short:?}",
                spec.name
            );
        }
    }
}

/// A surface beside `parent` that declares `control`.
fn controlled_surface(parent: &WireResourceId, control: ControlClass) -> SpawnResource {
    SpawnResource {
        kind: phux_protocol::ids::ResourceKind::PixelSurface,
        control: Some(control),
        ..SpawnResource::agent_session(parent.clone(), "display")
    }
}

/// A page is opened on the parent rows; a window or display is control of
/// the machine, which no scoped grant opens, whatever it holds.
fn child_spawn_rows(world: &World) -> Vec<Row> {
    let parent = world.alpha.clone();
    let create = format!("create@group:{}", world.alpha_group);
    let bind = format!("bind@terminal:{}", local_id(&parent));
    let page = spawn(
        None,
        None,
        Some(controlled_surface(&parent, ControlClass::Page)),
    );
    let mut rows = vec![row(
        page,
        &[&[&create, &bind]],
        &[&[&create], &[&bind]],
    )];
    for control in [ControlClass::Window, ControlClass::Display] {
        let frame = spawn(None, None, Some(controlled_surface(&parent, control)));
        rows.push(row(frame, &[], &[&["*@global"], &[&create, &bind]]));
    }
    rows
}
