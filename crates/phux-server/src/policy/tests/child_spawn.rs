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

/// Declaring any control class (a page included) claims what viewers'
/// `input@CLASS` grants reach: no scoped grant opens one, whatever it holds,
/// while the same surface with no class opens on the parent rows.
fn child_spawn_rows(world: &World) -> Vec<Row> {
    let parent = world.alpha.clone();
    let create = format!("create@group:{}", world.alpha_group);
    let bind = format!("bind@terminal:{}", local_id(&parent));
    ControlClass::ALL
        .into_iter()
        .map(|control| {
            let frame = spawn(None, None, Some(controlled_surface(&parent, control)));
            row(frame, &[], &[&["*@global"], &[&create, &bind]])
        })
        .collect()
}

/// A scoped grant with CREATE+BIND on the parent opens a class-less surface
/// but no page, window or display.
#[test]
fn a_scoped_grant_opens_no_controlled_document() {
    let world = world();
    let parent = world.alpha.clone();
    let grant = [
        format!("create@group:{}", world.alpha_group),
        format!("bind@terminal:{}", local_id(&parent)),
    ];
    let plain = SpawnResource {
        control: None,
        ..controlled_surface(&parent, ControlClass::Page)
    };
    assert!(admits_set(&world, &grant, &spawn(None, None, Some(plain))));
    for control in ControlClass::ALL {
        let frame = spawn(None, None, Some(controlled_surface(&parent, control)));
        assert!(
            !admits_set(&world, &grant, &frame),
            "a scoped grant opened a {} document",
            control.name()
        );
    }
}
