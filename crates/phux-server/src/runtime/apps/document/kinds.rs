//! The three document kinds the generic app serves.

use super::engine::{DocumentKind, SurfaceDocument, TreeDocument, TypedDocument};
use bytes::Bytes;
use phux_protocol::caps::BootstrapStreamProfile;

use super::DocumentApi;

/// What a tree or surface observes (`--app Safari`, a page title), as its
/// record name, so every client's tab reads it. A target a name may not
/// carry (a control character) leaves the resource unnamed.
fn target_name(target: &str) -> Option<String> {
    (!target.trim().is_empty() && !target.chars().any(char::is_control)).then(|| target.to_owned())
}

impl DocumentApi for TreeDocument {
    fn validate_bootstrap(_target: &str, bytes: &[u8]) -> Result<(), String> {
        serde_json::from_slice::<serde::de::IgnoredAny>(bytes)
            .map(|_| ())
            .map_err(|error| format!("the document is not JSON: {error}"))
    }

    const PROFILE: BootstrapStreamProfile = BootstrapStreamProfile::AccessibilityTreeJsonV1;
    const WIRE_KIND: phux_protocol::ids::ResourceKind =
        phux_protocol::ids::ResourceKind::AccessibilityTree;
    const LABEL: &'static str = "accessibility tree";

    fn launch_target(launch: &phux_core::LaunchSpec) -> Option<&str> {
        match launch {
            phux_core::LaunchSpec::Tree(tree) => Some(&tree.target),
            _ => None,
        }
    }

    fn creation(target: &str, _name: Option<&str>) -> phux_core::NewResource {
        phux_core::NewResource::child(
            phux_core::LaunchSpec::Tree(phux_core::resource::TreeLaunch {
                target: target.to_owned(),
            }),
            target_name(target),
        )
    }
}

impl DocumentApi for SurfaceDocument {
    fn validate_bootstrap(_target: &str, bytes: &[u8]) -> Result<(), String> {
        phux_core::image::image_size(bytes).map(|_| ())
    }

    const PROFILE: BootstrapStreamProfile = BootstrapStreamProfile::PixelSurfacePngV1;
    const WIRE_KIND: phux_protocol::ids::ResourceKind =
        phux_protocol::ids::ResourceKind::PixelSurface;
    const LABEL: &'static str = "pixel surface";

    fn launch_target(launch: &phux_core::LaunchSpec) -> Option<&str> {
        match launch {
            phux_core::LaunchSpec::Surface(surface) => Some(&surface.target),
            _ => None,
        }
    }

    fn creation(target: &str, _name: Option<&str>) -> phux_core::NewResource {
        phux_core::NewResource::child(
            phux_core::LaunchSpec::Surface(phux_core::resource::SurfaceLaunch {
                target: target.to_owned(),
            }),
            target_name(target),
        )
    }
}

/// A typed document's `target` is its content type, which every viewer's
/// bootstrap names before the newest document.
impl DocumentApi for TypedDocument {
    fn validate_bootstrap(content_type: &str, bytes: &[u8]) -> Result<(), String> {
        <Self as DocumentKind>::validate(content_type, bytes)
    }

    fn bootstrap_payload(content_type: &str, document: Option<&Bytes>) -> Bytes {
        let document = document.map_or(&[][..], |bytes| &bytes[..]);
        phux_protocol::wire::frame::typed_document::encode_bootstrap(content_type, document).into()
    }

    const PROFILE: BootstrapStreamProfile = BootstrapStreamProfile::DocumentTypedV1;
    const WIRE_KIND: phux_protocol::ids::ResourceKind = phux_protocol::ids::ResourceKind::Document;
    const LABEL: &'static str = "document";

    fn launch_target(launch: &phux_core::LaunchSpec) -> Option<&str> {
        match launch {
            phux_core::LaunchSpec::Document(document) => Some(&document.content_type),
            _ => None,
        }
    }

    fn creation(content_type: &str, name: Option<&str>) -> phux_core::NewResource {
        phux_core::NewResource::child(
            phux_core::LaunchSpec::Document(phux_core::resource::DocumentLaunch {
                content_type: content_type.to_owned(),
            }),
            name.map(str::to_owned),
        )
    }
}
