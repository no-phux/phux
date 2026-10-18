use super::*;
use crate::app::fleet::Destination;

fn work(name: &str, host: Option<&str>, project: Option<&str>) -> Work {
    Work {
        destination: Destination {
            host: host.map(str::to_owned),
            authority: None,
            key: None,
            name: name.into(),
        },
        id: 1,
        project: project.map(|name| (format!("id:{name}"), name.to_owned())),
        terminals: 1,
        attention: None,
        intake: false,
        online: true,
    }
}

fn labels(groups: &[Group]) -> Vec<(Option<&str>, Vec<&str>)> {
    groups
        .iter()
        .map(|group| {
            (
                group.label.as_deref(),
                group
                    .work
                    .iter()
                    .map(|work| work.destination.name.as_str())
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn every_grouping_lists_the_same_work() {
    let catalog = vec![
        work("api", Some("studio"), Some("phux")),
        work("desk", None, Some("phux")),
        work("notes", None, None),
        work("build", Some("ci"), None),
    ];
    for grouping in [
        SidebarGrouping::Project,
        SidebarGrouping::Machine,
        SidebarGrouping::None,
    ] {
        let mut names: Vec<_> = group(catalog.clone(), grouping)
            .into_iter()
            .flat_map(|group| group.work)
            .map(|work| work.destination.name)
            .collect();
        names.sort();
        assert_eq!(names, ["api", "build", "desk", "notes"], "{grouping:?}");
    }
    assert_eq!(
        labels(&group(catalog.clone(), SidebarGrouping::Project)),
        [
            (Some("phux"), vec!["api", "desk"]),
            (Some("Other"), vec!["build", "notes"]),
        ]
    );
    assert_eq!(
        labels(&group(catalog.clone(), SidebarGrouping::Machine)),
        [
            (Some("This Mac"), vec!["desk", "notes"]),
            (Some("ci"), vec!["build"]),
            (Some("studio"), vec!["api"]),
        ]
    );
    assert_eq!(
        labels(&group(catalog, SidebarGrouping::None)),
        [(None, vec!["api", "build", "desk", "notes"])]
    );
}

#[test]
fn a_lone_group_needs_no_heading() {
    let catalog = vec![work("a", None, Some("phux")), work("b", None, Some("phux"))];
    assert_eq!(
        labels(&group(catalog.clone(), SidebarGrouping::Project)),
        [(None, vec!["a", "b"])]
    );
    assert_eq!(
        labels(&group(catalog, SidebarGrouping::Machine)),
        [(None, vec!["a", "b"])]
    );
    assert!(group(Vec::new(), SidebarGrouping::Project).is_empty());
}
