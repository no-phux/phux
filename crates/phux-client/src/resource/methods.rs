//! `phux resource methods` (PHA-406 D4, ADR-0125): which catalog methods a
//! resource answers on this connection.
//!
//! The generated kind catalog ([`phux_protocol::kinds`]) intersected with the
//! resource's kind and the features the server negotiated. This is a
//! client-side read of a compiled constant plus one `GET_STATE`: discovery
//! grants nothing, and a method name here is not an invocation handle.
//! Authorization stays at dispatch.

use std::path::Path;

use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
use phux_protocol::ids::{ResourceId, ResourceKind};
use phux_protocol::kinds::{KINDS, KindSpec, MethodSpec, SUBSTRATE_METHODS, Verb, Verbs};
use serde_json::{Value, json};

use super::LookupError;
use crate::attach::connection::Connection;
use crate::selector::format_terminal_id;

/// `schema_version` of the `resource methods --json` document.
pub const METHODS_SCHEMA_VERSION: u32 = 1;

/// Why a method is not available on this resource and connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// The method, or its kind, needs a feature the server did not
    /// advertise in `HELLO_OK`.
    FeatureUnadvertised,
    /// The method belongs to another kind's facet.
    WrongKind,
    /// The method is reachable only over the owner Unix socket.
    Transport,
    /// The catalog allocates it, but no shipped codec or server implements
    /// it yet.
    Unimplemented,
}

impl Unavailable {
    /// The name the `--json` document uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FeatureUnadvertised => "feature_unadvertised",
            Self::WrongKind => "wrong_kind",
            Self::Transport => "transport",
            Self::Unimplemented => "unimplemented",
        }
    }
}

/// One catalog method, judged against one resource and connection.
#[derive(Debug, Clone, Copy)]
pub struct MethodAvailability {
    /// The catalog entry.
    pub method: &'static MethodSpec,
    /// The kind whose facet defines it; `None` for the substrate every kind
    /// shares.
    pub facet: Option<ResourceKind>,
    /// Why it is unavailable, or `None` when it is available.
    pub unavailable: Option<Unavailable>,
}

impl MethodAvailability {
    /// Whether the method is available.
    #[must_use]
    pub const fn available(&self) -> bool {
        self.unavailable.is_none()
    }

    /// One `methods[]` entry of the `--json` document.
    #[must_use]
    pub fn to_json(self) -> Value {
        json!({
            "name": self.method.name,
            "facet": self.facet.map_or("substrate", ResourceKind::as_str),
            "verb": verb_label(self.method.verbs()),
            "mutating": self.method.mutating(),
            "dangerous": self.method.dangerous,
            "available": self.available(),
            "reason": self.unavailable.map(Unavailable::as_str),
        })
    }
}

/// The verbs as one label, `BIND+OBSERVE` in bit order, or `none` for a
/// method no verb admits (an exemption).
fn verb_label(verbs: Verbs) -> String {
    if verbs.is_empty() {
        return "none".to_owned();
    }
    verbs.iter().map(Verb::name).collect::<Vec<_>>().join("+")
}

/// The negotiated context a method is judged in.
#[derive(Debug, Clone, Copy)]
pub struct Negotiated {
    /// The features the server advertised.
    pub features: ServerFeatureSet,
    /// Whether the connection is the owner Unix socket.
    pub owner_uds: bool,
}

/// Every substrate and facet method, judged for a resource of `kind`.
#[must_use]
pub fn availability(kind: ResourceKind, negotiated: Negotiated) -> Vec<MethodAvailability> {
    let substrate = SUBSTRATE_METHODS
        .iter()
        .map(|method| judged(method, None, kind, negotiated));
    let facets = KINDS.iter().flat_map(|spec| {
        spec.methods
            .iter()
            .map(move |method| judged(method, Some(spec), kind, negotiated))
    });
    substrate.chain(facets).collect()
}

fn judged(
    method: &'static MethodSpec,
    facet: Option<&'static KindSpec>,
    kind: ResourceKind,
    negotiated: Negotiated,
) -> MethodAvailability {
    MethodAvailability {
        method,
        facet: facet.map(|spec| spec.kind),
        unavailable: judge(method, facet, kind, negotiated),
    }
}

fn judge(
    method: &MethodSpec,
    facet: Option<&KindSpec>,
    kind: ResourceKind,
    negotiated: Negotiated,
) -> Option<Unavailable> {
    if facet.is_some_and(|spec| spec.kind != kind) {
        return Some(Unavailable::WrongKind);
    }
    if !method.shipped {
        return Some(Unavailable::Unimplemented);
    }
    let unadvertised = method
        .gate
        .into_iter()
        .chain(facet.and_then(|spec| spec.gate))
        .any(|gate| !advertised(negotiated.features, gate));
    if unadvertised {
        return Some(Unavailable::FeatureUnadvertised);
    }
    if method.owner_uds_only() && !negotiated.owner_uds {
        return Some(Unavailable::Transport);
    }
    None
}

const fn advertised(features: ServerFeatureSet, gate: ServerFeature) -> bool {
    features.contains(gate)
}

/// The catalog as it applies to one resource.
#[derive(Debug, Clone)]
pub struct ResourceMethods {
    /// The resource.
    pub resource: ResourceId,
    /// Its kind.
    pub kind: ResourceKind,
    /// Every substrate and facet method, judged.
    pub methods: Vec<MethodAvailability>,
}

impl ResourceMethods {
    /// The `resource methods --json` document, shared by the CLI and MCP.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "schema_version": METHODS_SCHEMA_VERSION,
            "resource": format_terminal_id(&self.resource),
            "kind": super::kind_name(self.kind),
            "methods": self
                .methods
                .iter()
                .copied()
                .map(MethodAvailability::to_json)
                .collect::<Vec<_>>(),
        })
    }
}

/// Judge the catalog for `resource` over a fresh connection to `socket`.
///
/// The connection is a Unix socket, so the owner-UDS transport predicate
/// holds for it.
///
/// # Errors
///
/// [`LookupError::NotFound`] when the resource is not in the inventory, or
/// [`LookupError::Attach`] on a transport failure.
pub async fn methods(socket: &Path, resource: &ResourceId) -> Result<ResourceMethods, LookupError> {
    let mut conn = Connection::connect(socket).await?;
    let (info, _snapshot, _unreachable) = super::lookup_on(&mut conn, resource).await?;
    let features = conn
        .negotiated_bootstrap()
        .map_or_else(ServerFeatureSet::new, |negotiated| {
            negotiated.server_features
        });
    drop(conn);
    let negotiated = Negotiated {
        features,
        owner_uds: true,
    };
    Ok(ResourceMethods {
        resource: resource.clone(),
        kind: info.kind,
        methods: availability(info.kind, negotiated),
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};
    use tokio::net::UnixListener;

    use super::*;
    use crate::testkit::{ScriptSpec, ScriptedServer};

    fn entry<'a>(doc: &'a Value, name: &str) -> &'a Value {
        doc["methods"]
            .as_array()
            .expect("methods")
            .iter()
            .find(|method| method["name"] == name)
            .unwrap_or_else(|| panic!("no {name} in {doc}"))
    }

    async fn methods_against(features: ServerFeatureSet) -> Value {
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("methods.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(7))
                .with_resources(vec![ResourceInfo::new(
                    ResourceId::local(7),
                    WindowId::new(1),
                    80,
                    24,
                )]);
        let spec = ScriptSpec::new().server_features(features).state(snapshot);
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });
        let report = methods(&socket, &ResourceId::local(7))
            .await
            .expect("methods");
        server.await.expect("scripted server");
        report.to_json()
    }

    #[tokio::test]
    async fn resource_methods_marks_unadvertised_features_unavailable() {
        let doc = methods_against(ServerFeatureSet::new()).await;
        assert_eq!(doc["kind"], "terminal");
        let moved = entry(&doc, "MOVE_RESOURCE");
        assert_eq!(moved["available"], false);
        assert_eq!(moved["reason"], "feature_unadvertised");
        assert_eq!(moved["mutating"], true);

        let screen = entry(&doc, "GET_SCREEN");
        assert_eq!(screen["available"], true);
        assert!(screen["reason"].is_null());
        assert_eq!(screen["mutating"], false);
        assert_eq!(screen["verb"], "OBSERVE");
        assert_eq!(screen["facet"], "terminal");

        assert_eq!(
            entry(&doc, "APPEND_RESOURCE_OUTPUT")["reason"],
            "wrong_kind"
        );
        assert_eq!(entry(&doc, "INPUT_RAW")["reason"], "unimplemented");
        assert_eq!(entry(&doc, "GET_STATE")["facet"], "substrate");

        let advertised =
            methods_against(ServerFeatureSet::with(&[ServerFeature::MoveResource])).await;
        assert_eq!(entry(&advertised, "MOVE_RESOURCE")["available"], true);
    }

    #[test]
    fn the_owner_socket_predicate_is_a_transport_reason() {
        let remote = Negotiated {
            features: ServerFeatureSet::new(),
            owner_uds: false,
        };
        // No substrate or facet method is owner-UDS-only today; the rule
        // still holds for any that becomes one.
        for judged in availability(ResourceKind::Terminal, remote) {
            if judged.method.owner_uds_only() && judged.unavailable != Some(Unavailable::WrongKind)
            {
                assert_eq!(judged.unavailable, Some(Unavailable::Transport));
            }
        }
    }
}
