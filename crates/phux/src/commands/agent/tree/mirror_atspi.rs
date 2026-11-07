//! The Linux side of `phux agent tree mirror`: an application's
//! accessibility tree over AT-SPI (D-Bus), in the macOS mirror's JSON shape:
//! `role`, then `title` (the AT-SPI name), `description` and `value` when not
//! empty, then `children`. Roles are AT-SPI role names (`push button`), not
//! macOS's `AXButton`.
//!
//! The accessibility bus is found through the session bus
//! (`org.a11y.Bus.GetAddress`), or `AT_SPI_BUS_ADDRESS`; the application is
//! the registry's child whose name is APP (any case), or whose connection
//! belongs to `--pid`.

#![cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "macOS builds this module only to test the walk")
)]

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

/// One accessible object, as the walk needs it.
#[derive(Debug, Default)]
pub(super) struct Node<R> {
    pub(super) role: String,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) value: Option<String>,
    pub(super) children: Vec<R>,
}

/// Where nodes come from: the bus, or a test's table.
pub(super) trait Source {
    type Ref;
    /// The object at `at`, or `None` once it no longer answers.
    async fn node(&self, at: &Self::Ref) -> Option<Node<Self::Ref>>;
}

/// One object as JSON, with its children while `depth` and `budget` allow;
/// `Null` for an object that no longer answers. The same walk as macOS.
pub(super) fn walk<'a, S: Source>(
    source: &'a S,
    at: &'a S::Ref,
    depth: usize,
    budget: &'a mut usize,
) -> Pin<Box<dyn Future<Output = Value> + 'a>> {
    Box::pin(async move {
        let Some(node) = source.node(at).await else {
            return Value::Null;
        };
        *budget = budget.saturating_sub(1);
        let mut object = serde_json::Map::new();
        object.insert("role".into(), node.role.into());
        for (key, text) in [
            ("title", Some(node.name)),
            ("description", Some(node.description)),
            ("value", node.value),
        ] {
            if let Some(text) = text.filter(|text| !text.is_empty()) {
                object.insert(key.into(), text.into());
            }
        }
        if depth > 0 && *budget > 0 {
            let mut nested = Vec::new();
            for child in &node.children {
                if *budget == 0 {
                    break;
                }
                let child = walk(source, child, depth - 1, budget).await;
                if !child.is_null() {
                    nested.push(child);
                }
            }
            if !nested.is_empty() {
                object.insert("children".into(), nested.into());
            }
        }
        Value::Object(object)
    })
}

#[cfg(target_os = "linux")]
pub(super) use bus::App;

#[cfg(target_os = "linux")]
mod bus {
    use std::process::ExitCode;
    use std::time::Duration;

    use serde_json::Value;
    use zbus::wire::{DynamicDeserialize, DynamicType};
    use zbus::{Connection, OwnedObjectPath, OwnedVariant};

    use super::{Node, Source, walk};
    use crate::commands::agent::tree::mirror::MAX_NODES;
    use crate::commands::json_err;
    use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

    /// An object on the bus: its connection's name and its path.
    type Ref = (String, OwnedObjectPath);

    const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
    const ROOT: &str = "/org/a11y/atspi/accessible/root";
    const NULL: &str = "/org/a11y/atspi/null";

    /// How long one call may take; an application that hangs reads as gone.
    const CALL_TIMEOUT: Duration = Duration::from_secs(3);

    /// A running application, read over AT-SPI.
    pub(in crate::commands::agent::tree) struct App {
        conn: Connection,
        root: Ref,
        pid: u32,
    }

    impl App {
        /// The registered application named `app` (or owned by `pid`).
        pub(in crate::commands::agent::tree) async fn find(
            app: &str,
            pid: Option<u32>,
            json: bool,
        ) -> Result<Self, ExitCode> {
            let conn = a11y_bus().await.map_err(|err| {
                json_err::emit(
                    json,
                    &json_err::CliError::new(
                        json_err::codes::WORKLOAD,
                        format!("cannot reach the accessibility bus: {err}"),
                        "run inside the graphical session's D-Bus (DBUS_SESSION_BUS_ADDRESS) with at-spi2-core installed",
                    ),
                    EXIT_FAILURE,
                )
            })?;
            let registry = ("org.a11y.atspi.Registry".to_owned(), path(ROOT));
            let apps: Vec<Ref> = call(&conn, &registry, ACCESSIBLE, "GetChildren", &())
                .await
                .unwrap_or_default();
            let mut names = Vec::new();
            for root in apps {
                let name = property(&conn, &root, "Name").await.unwrap_or_default();
                let owner: Option<u32> = call(
                    &conn,
                    &(
                        "org.freedesktop.DBus".to_owned(),
                        path("/org/freedesktop/DBus"),
                    ),
                    "org.freedesktop.DBus",
                    "GetConnectionUnixProcessID",
                    &(root.0.as_str(),),
                )
                .await
                .ok();
                let wanted =
                    pid.map_or_else(|| name.eq_ignore_ascii_case(app), |p| owner == Some(p));
                if wanted {
                    let pid = owner.unwrap_or_default();
                    return Ok(Self { conn, root, pid });
                }
                names.push(if name.is_empty() { root.0 } else { name });
            }
            let wanted = pid.map_or_else(|| format!("named {app:?}"), |p| format!("with pid {p}"));
            Err(json_err::emit(
                json,
                &json_err::CliError::new(
                    json_err::codes::NO_SUCH_TARGET,
                    format!(
                        "no AT-SPI application {wanted}; registered: {}",
                        names.join(", ")
                    ),
                    "name a registered application, or pass `--pid`; Qt and Chromium apps register only with accessibility on (QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1, --force-renderer-accessibility)",
                ),
                EXIT_USAGE,
            ))
        }

        pub(in crate::commands::agent::tree) const fn pid(&self) -> u32 {
            self.pid
        }

        /// The whole tree, or `Null` once the application stops answering.
        #[allow(
            clippy::future_not_send,
            reason = "the walk is not Send; the CLI runtime is single-threaded"
        )]
        pub(in crate::commands::agent::tree) async fn read(&self, depth: usize) -> Value {
            let mut budget = MAX_NODES;
            walk(self, &self.root, depth, &mut budget).await
        }
    }

    impl Source for App {
        type Ref = Ref;

        async fn node(&self, at: &Ref) -> Option<Node<Ref>> {
            let conn = &self.conn;
            let (role, name, description, interfaces, children) = tokio::join!(
                call::<_, String>(conn, at, ACCESSIBLE, "GetRoleName", &()),
                property(conn, at, "Name"),
                property(conn, at, "Description"),
                call::<_, Vec<String>>(conn, at, ACCESSIBLE, "GetInterfaces", &()),
                call::<_, Vec<Ref>>(conn, at, ACCESSIBLE, "GetChildren", &()),
            );
            let interfaces = interfaces.unwrap_or_default();
            let has = |name: &str| interfaces.iter().any(|i| i == name);
            let value = if has("org.a11y.atspi.Value") {
                let current: Option<OwnedVariant> = call(
                    conn,
                    at,
                    "org.freedesktop.DBus.Properties",
                    "Get",
                    &("org.a11y.atspi.Value", "CurrentValue"),
                )
                .await
                .ok();
                current
                    .and_then(|v| f64::try_from(v).ok())
                    .map(|n| n.to_string())
            } else if has("org.a11y.atspi.EditableText") {
                call(conn, at, "org.a11y.atspi.Text", "GetText", &(0_i32, -1_i32))
                    .await
                    .ok()
            } else {
                None
            };
            let children = children
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, child)| child.as_str() != NULL)
                .map(|(bus, child)| (if bus.is_empty() { at.0.clone() } else { bus }, child))
                .collect();
            Some(Node {
                role: role.ok()?,
                name: name.unwrap_or_default(),
                description: description.unwrap_or_default(),
                value,
                children,
            })
        }
    }

    /// The accessibility bus: `AT_SPI_BUS_ADDRESS`, else the address the
    /// session bus's `org.a11y.Bus` gives.
    async fn a11y_bus() -> zbus::Result<Connection> {
        let address = match std::env::var("AT_SPI_BUS_ADDRESS") {
            Ok(address) if !address.is_empty() => address,
            _ => {
                let session = Connection::session().await?;
                let reply = session
                    .call_method(
                        Some("org.a11y.Bus"),
                        "/org/a11y/bus",
                        Some("org.a11y.Bus"),
                        "GetAddress",
                        &(),
                    )
                    .await?;
                reply.body().deserialize::<String>()?
            }
        };
        zbus::connection::Builder::address(address.as_str())
            .method_timeout(CALL_TIMEOUT)
            .build()
            .await
    }

    async fn call<B, R>(
        conn: &Connection,
        at: &Ref,
        interface: &str,
        method: &str,
        body: &B,
    ) -> zbus::Result<R>
    where
        B: serde::Serialize + DynamicType + Sync,
        R: for<'d> DynamicDeserialize<'d>,
    {
        let reply = conn
            .call_method(
                Some(at.0.as_str()),
                at.1.as_str(),
                Some(interface),
                method,
                body,
            )
            .await?;
        reply.body().deserialize::<R>()
    }

    /// A text property of the `Accessible` interface.
    async fn property(conn: &Connection, at: &Ref, name: &str) -> Option<String> {
        let value: OwnedVariant = call(
            conn,
            at,
            "org.freedesktop.DBus.Properties",
            "Get",
            &(ACCESSIBLE, name),
        )
        .await
        .ok()?;
        String::try_from(value).ok()
    }

    fn path(raw: &'static str) -> OwnedObjectPath {
        OwnedObjectPath::from(zbus::ObjectPath::from_static_str_unchecked(raw))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{Node, Source, walk};

    /// One row: role, name, value, children.
    type Row = (&'static str, &'static str, Option<&'static str>, Vec<u32>);

    /// A tree as a table of rows by id.
    struct Table(HashMap<u32, Row>);

    impl Source for Table {
        type Ref = u32;
        #[allow(clippy::unused_async_trait_impl, reason = "the table answers at once")]
        async fn node(&self, at: &u32) -> Option<Node<u32>> {
            let (role, name, value, children) = self.0.get(at)?;
            Some(Node {
                role: (*role).into(),
                name: (*name).into(),
                description: String::new(),
                value: value.map(Into::into),
                children: children.clone(),
            })
        }
    }

    fn table() -> Table {
        Table(HashMap::from([
            (1, ("application", "Files", None, vec![2])),
            (2, ("frame", "Home", None, vec![3, 4, 99])),
            (3, ("push button", "Back", None, vec![])),
            (4, ("text", "", Some("~/notes"), vec![])),
        ]))
    }

    fn read(depth: usize, budget: usize) -> serde_json::Value {
        let source = table();
        let mut budget = budget;
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(walk(&source, &1, depth, &mut budget))
    }

    #[test]
    fn nodes_map_to_the_macos_shape_and_gone_children_drop() {
        assert_eq!(
            read(24, 100),
            serde_json::json!({"role": "application", "title": "Files", "children": [
                {"role": "frame", "title": "Home", "children": [
                    {"role": "push button", "title": "Back"},
                    {"role": "text", "value": "~/notes"}
                ]}
            ]})
        );
    }

    #[test]
    fn depth_and_budget_bound_the_walk() {
        assert_eq!(
            read(0, 100),
            serde_json::json!({"role": "application", "title": "Files"})
        );
        let cut = read(24, 3);
        assert_eq!(cut["children"][0]["children"].as_array().unwrap().len(), 1);
        let source = table();
        let mut budget = 10;
        let gone = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(walk(&source, &42, 24, &mut budget));
        assert!(gone.is_null());
    }
}
