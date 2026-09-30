//! Host-side browse and fuzzy-search request/reply payloads (L3 §5).

use crate::ids::SatelliteHost;
use crate::wire::{decode::Decoder, encode::Encoder, error::DecodeError, field};

/// Maximum rows in a single `PATH_RESULTS` reply.
pub const MAX_PATH_RESULTS: usize = 1024;

/// One absolute, losslessly encoded UTF-8 path on the queried host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRow {
    /// Absolute path to insert; consumers escape it for their target shell.
    pub path: String,
    /// File, directory, or symbolic link (do not infer from the name).
    pub kind: PathKind,
}

/// Filesystem entry kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Symbolic link, whether or not its target resolves.
    Symlink,
}

impl PathKind {
    const fn as_wire(self) -> u8 {
        match self {
            Self::File => 0,
            Self::Directory => 1,
            Self::Symlink => 2,
        }
    }

    fn from_wire(tag: u8) -> Result<Self, DecodeError> {
        match tag {
            0 => Ok(Self::File),
            1 => Ok(Self::Directory),
            2 => Ok(Self::Symlink),
            _ => Err(DecodeError::UnknownEnumValue {
                field: "path_kind",
                value: u32::from(tag),
            }),
        }
    }
}

/// Whether results represent the entire search, a warming index, or a cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStatus {
    /// Complete for this query.
    Complete,
    /// A bounded partial result while host discovery is still warming.
    Warming,
    /// A hard result or traversal bound was hit; not exhaustive.
    Truncated,
}

impl PathStatus {
    const fn as_wire(self) -> u8 {
        match self {
            Self::Complete => 0,
            Self::Warming => 1,
            Self::Truncated => 2,
        }
    }

    fn from_wire(tag: u8) -> Result<Self, DecodeError> {
        match tag {
            0 => Ok(Self::Complete),
            1 => Ok(Self::Warming),
            2 => Ok(Self::Truncated),
            _ => Err(DecodeError::UnknownEnumValue {
                field: "path_status",
                value: u32::from(tag),
            }),
        }
    }
}

/// Typed refusal of a host path query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathErrorCode {
    /// Root does not exist.
    NotFound,
    /// The caller lacks permission to search the root.
    PermissionDenied,
    /// Root is not a directory.
    NotADirectory,
    /// Invalid root/query, host route, timeout or other I/O error.
    Other,
}

impl PathErrorCode {
    const fn as_wire(self) -> u8 {
        match self {
            Self::NotFound => 0,
            Self::PermissionDenied => 1,
            Self::NotADirectory => 2,
            Self::Other => 3,
        }
    }

    const fn from_wire(tag: u8) -> Self {
        match tag {
            0 => Self::NotFound,
            1 => Self::PermissionDenied,
            2 => Self::NotADirectory,
            _ => Self::Other,
        }
    }
}

/// Successful results, rooted on the queried host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathResults {
    /// Resolved absolute lexical root.
    pub root: String,
    /// Lexical parent of `root`, absent at filesystem root.
    pub parent: Option<String>,
    /// Matched absolute file, directory and symlink paths.
    pub rows: Vec<PathRow>,
    /// Whether a retry can return more, or this was exhaustive.
    pub status: PathStatus,
}

/// Failed query; the diagnostic is for display, never parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathQueryError {
    /// Attempted root, normalized when possible.
    pub root: String,
    /// Typed reason for the refusal.
    pub code: PathErrorCode,
    /// Display-only diagnostic.
    pub message: String,
}

/// Result carried by a correlated `PATH_RESULTS` reply.
pub type PathQueryResult = Result<PathResults, PathQueryError>;

pub(super) fn encode_query(
    enc: &mut Encoder<'_>,
    request_id: u32,
    root: &str,
    query: &str,
    recursive: bool,
    host: Option<&SatelliteHost>,
) {
    enc.write_field_with(field::path_query::REQUEST_ID, |e| {
        e.write_u32_be(request_id);
    });
    enc.write_field(field::path_query::ROOT, root.as_bytes());
    enc.write_field(field::path_query::QUERY, query.as_bytes());
    enc.write_field_with(field::path_query::RECURSIVE, |e| {
        e.write_u8(u8::from(recursive));
    });
    if let Some(host) = host {
        enc.write_field(field::path_query::HOST, host.as_str().as_bytes());
    }
}

pub(super) fn encode_results(enc: &mut Encoder<'_>, request_id: u32, result: &PathQueryResult) {
    enc.write_field_with(field::path_results::REQUEST_ID, |e| {
        e.write_u32_be(request_id);
    });
    match result {
        Ok(reply) => {
            enc.write_field(field::path_results::ROOT, reply.root.as_bytes());
            if let Some(parent) = &reply.parent {
                enc.write_field(field::path_results::PARENT, parent.as_bytes());
            }
            enc.write_field_with(field::path_results::ROWS, |e| {
                e.write_u32_be(u32::try_from(reply.rows.len()).unwrap_or(u32::MAX));
                for row in &reply.rows {
                    e.write_str(&row.path);
                    e.write_u8(row.kind.as_wire());
                }
            });
            enc.write_field_with(field::path_results::STATUS, |e| {
                e.write_u8(reply.status.as_wire());
            });
        }
        Err(error) => {
            enc.write_field(field::path_results::ROOT, error.root.as_bytes());
            enc.write_field_with(field::path_results::ERROR, |e| {
                e.write_u8(error.code.as_wire());
            });
            enc.write_field(field::path_results::MESSAGE, error.message.as_bytes());
        }
    }
}

fn utf8(value: &[u8]) -> Result<String, DecodeError> {
    core::str::from_utf8(value)
        .map(str::to_owned)
        .map_err(|_| DecodeError::InvalidUtf8)
}

fn u32_field(value: &[u8]) -> Result<u32, DecodeError> {
    let mut d = Decoder::new(value);
    let result = d.read_u32_be()?;
    if !d.at_body_end() {
        return Err(DecodeError::LengthOverflow);
    }
    Ok(result)
}

const fn u8_field(value: &[u8]) -> Result<u8, DecodeError> {
    if value.len() != 1 {
        return Err(DecodeError::LengthOverflow);
    }
    Ok(value[0])
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<(), DecodeError> {
    if slot.replace(value).is_some() {
        return Err(DecodeError::LengthOverflow);
    }
    Ok(())
}

#[derive(Default)]
struct QueryFields {
    request_id: Option<u32>,
    root: Option<String>,
    query: Option<String>,
    recursive: Option<u8>,
    host: Option<SatelliteHost>,
}

impl QueryFields {
    fn absorb(&mut self, id: u32, value: &[u8]) -> Result<(), DecodeError> {
        match id {
            field::path_query::REQUEST_ID => set_once(&mut self.request_id, u32_field(value)?),
            field::path_query::ROOT => set_once(&mut self.root, utf8(value)?),
            field::path_query::QUERY => set_once(&mut self.query, utf8(value)?),
            field::path_query::RECURSIVE => set_once(&mut self.recursive, u8_field(value)?),
            field::path_query::HOST => set_once(&mut self.host, SatelliteHost::new(utf8(value)?)),
            _ => Ok(()),
        }
    }

    fn finish(self) -> Result<(u32, String, String, bool, Option<SatelliteHost>), DecodeError> {
        let recursive = match self.recursive {
            Some(0) => false,
            Some(1) => true,
            _ => return Err(DecodeError::LengthOverflow),
        };
        Ok((
            self.request_id.ok_or(DecodeError::LengthOverflow)?,
            self.root.ok_or(DecodeError::LengthOverflow)?,
            self.query.ok_or(DecodeError::LengthOverflow)?,
            recursive,
            self.host,
        ))
    }
}

/// Read one `PATH_QUERY` body.
pub(in crate::wire) fn decode_query(
    d: &mut Decoder<'_>,
) -> Result<(u32, String, String, bool, Option<SatelliteHost>), DecodeError> {
    let mut fields = QueryFields::default();
    while let Some((id, value)) = d.read_field()? {
        fields.absorb(id, value)?;
    }
    fields.finish()
}

fn decode_rows(value: &[u8]) -> Result<Vec<PathRow>, DecodeError> {
    let mut d = Decoder::new(value);
    let count = usize::try_from(d.read_u32_be()?).map_err(|_| DecodeError::LengthOverflow)?;
    if count > MAX_PATH_RESULTS {
        return Err(DecodeError::PathResultLimitExceeded);
    }
    let mut rows = d.bounded_capacity(count);
    for _ in 0..count {
        let path = d.read_str()?.to_owned();
        if !absolute_path(&path) {
            return Err(DecodeError::LengthOverflow);
        }
        rows.push(PathRow {
            path,
            kind: PathKind::from_wire(d.read_u8()?)?,
        });
    }
    if !d.at_body_end() {
        return Err(DecodeError::LengthOverflow);
    }
    Ok(rows)
}

#[derive(Default)]
struct ResultFields {
    request_id: Option<u32>,
    root: Option<String>,
    parent: Option<String>,
    rows: Option<Vec<PathRow>>,
    status: Option<PathStatus>,
    error: Option<PathErrorCode>,
    message: Option<String>,
}

impl ResultFields {
    fn absorb(&mut self, id: u32, value: &[u8]) -> Result<(), DecodeError> {
        match id {
            field::path_results::REQUEST_ID => set_once(&mut self.request_id, u32_field(value)?),
            field::path_results::ROOT => set_once(&mut self.root, utf8(value)?),
            field::path_results::PARENT => set_once(&mut self.parent, utf8(value)?),
            field::path_results::ROWS => set_once(&mut self.rows, decode_rows(value)?),
            field::path_results::STATUS => {
                set_once(&mut self.status, PathStatus::from_wire(u8_field(value)?)?)
            }
            field::path_results::ERROR => {
                set_once(&mut self.error, PathErrorCode::from_wire(u8_field(value)?))
            }
            field::path_results::MESSAGE => set_once(&mut self.message, utf8(value)?),
            _ => Ok(()),
        }
    }

    fn finish(self) -> Result<(u32, PathQueryResult), DecodeError> {
        let request_id = self.request_id.ok_or(DecodeError::LengthOverflow)?;
        let root = self.root.ok_or(DecodeError::LengthOverflow)?;
        let result = if let Some(code) = self.error {
            if self.rows.is_some() || self.status.is_some() || self.parent.is_some() {
                return Err(DecodeError::LengthOverflow);
            }
            Err(PathQueryError {
                root,
                code,
                message: self.message.ok_or(DecodeError::LengthOverflow)?,
            })
        } else {
            if self.message.is_some() {
                return Err(DecodeError::LengthOverflow);
            }
            if !absolute_path(&root)
                || self
                    .parent
                    .as_ref()
                    .is_some_and(|parent| !absolute_path(parent))
            {
                return Err(DecodeError::LengthOverflow);
            }
            Ok(PathResults {
                root,
                parent: self.parent,
                rows: self.rows.ok_or(DecodeError::LengthOverflow)?,
                status: self.status.ok_or(DecodeError::LengthOverflow)?,
            })
        };
        Ok((request_id, result))
    }
}

fn absolute_path(path: &str) -> bool {
    path.starts_with('/') && !path.contains('\0')
}

/// Read one `PATH_RESULTS` body; incomplete or contradictory replies fail.
pub(in crate::wire) fn decode_results(
    d: &mut Decoder<'_>,
) -> Result<(u32, PathQueryResult), DecodeError> {
    let mut fields = ResultFields::default();
    while let Some((id, value)) = d.read_field()? {
        fields.absorb(id, value)?;
    }
    fields.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    fn body(build: impl FnOnce(&mut Encoder<'_>)) -> Vec<u8> {
        let mut buf = BytesMut::new();
        build(&mut Encoder::new(&mut buf));
        buf.to_vec()
    }

    #[test]
    fn browse_and_satellite_search_round_trip() {
        for (recursive, host) in [(false, None), (true, Some(SatelliteHost::new("dev")))] {
            let bytes = body(|e| encode_query(e, 42, "~/src", "cfg", recursive, host.as_ref()));
            assert_eq!(
                decode_query(&mut Decoder::new(&bytes)),
                Ok((42, "~/src".into(), "cfg".into(), recursive, host))
            );
        }
    }

    #[test]
    fn frames_dispatch_with_distinct_l3_tags() {
        let frames = [
            super::super::FrameKind::PathQuery {
                request_id: 7,
                root: "/srv".into(),
                query: "cfg".into(),
                recursive: true,
                host: Some(SatelliteHost::new("dev")),
            },
            super::super::FrameKind::PathResults {
                request_id: 7,
                result: Ok(PathResults {
                    root: "/srv".into(),
                    parent: Some("/".into()),
                    rows: vec![PathRow {
                        path: "/srv/config.rs".into(),
                        kind: PathKind::File,
                    }],
                    status: PathStatus::Complete,
                }),
            },
        ];
        for frame in frames {
            let mut bytes = BytesMut::new();
            frame.encode(&mut bytes);
            let (decoded, rest) = super::super::FrameKind::decode(&bytes).expect("frame");
            assert_eq!(decoded, frame);
            assert!(rest.is_empty());
        }
    }

    #[test]
    fn feature_extension_round_trips_in_hello_ok_without_changing_word_zero() {
        use crate::caps::{
            BootstrapLimits, BootstrapProfile, ServerCapabilities, ServerFeatureExt,
            ServerFeatureExtSet, ServerFeatureSet,
        };

        let caps = ServerCapabilities::new()
            .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::PathQuery]));
        let frame = super::super::FrameKind::HelloOk {
            protocol_major: 0,
            protocol_minor: 9,
            protocol_patch: 0,
            server_caps: caps,
            server_id: vec![1],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        };
        let mut bytes = BytesMut::new();
        frame.encode(&mut bytes);
        let (decoded, rest) = super::super::FrameKind::decode(&bytes).expect("caps frame");
        assert!(rest.is_empty());
        assert_eq!(decoded, frame);
        let super::super::FrameKind::HelloOk { server_caps, .. } = decoded else {
            panic!("HELLO_OK")
        };
        assert_eq!(server_caps.features, ServerFeatureSet::new());
        assert!(
            server_caps
                .features_ext
                .contains(ServerFeatureExt::PathQuery)
        );
    }

    #[test]
    fn complete_warming_truncated_and_refusal_round_trip() {
        for status in [
            PathStatus::Complete,
            PathStatus::Warming,
            PathStatus::Truncated,
        ] {
            let reply = Ok(PathResults {
                root: "/home/é".into(),
                parent: Some("/home".into()),
                rows: vec![
                    PathRow {
                        path: "/home/é/a.rs".into(),
                        kind: PathKind::File,
                    },
                    PathRow {
                        path: "/home/é/dir".into(),
                        kind: PathKind::Directory,
                    },
                    PathRow {
                        path: "/home/é/link".into(),
                        kind: PathKind::Symlink,
                    },
                ],
                status,
            });
            let bytes = body(|e| encode_results(e, 4, &reply));
            assert_eq!(decode_results(&mut Decoder::new(&bytes)), Ok((4, reply)));
        }
        let refusal = Err(PathQueryError {
            root: "/private".into(),
            code: PathErrorCode::PermissionDenied,
            message: "permission denied".into(),
        });
        let bytes = body(|e| encode_results(e, 9, &refusal));
        assert_eq!(decode_results(&mut Decoder::new(&bytes)), Ok((9, refusal)));
    }

    #[test]
    fn unknown_fields_are_skipped_but_invalid_known_fields_fail() {
        let bytes = body(|e| {
            encode_query(e, 1, "/", "a", false, None);
            e.write_field(99, b"future");
        });
        assert_eq!(
            decode_query(&mut Decoder::new(&bytes)),
            Ok((1, "/".into(), "a".into(), false, None))
        );
        let bytes = body(|e| {
            encode_query(e, 1, "/", "a", false, None);
            e.write_field(field::path_query::RECURSIVE, &[2]);
        });
        assert!(decode_query(&mut Decoder::new(&bytes)).is_err());
        let bytes = body(|e| {
            e.write_field_with(field::path_query::REQUEST_ID, |f| f.write_u32_be(1));
            e.write_field(field::path_query::ROOT, b"/");
        });
        assert!(decode_query(&mut Decoder::new(&bytes)).is_err());
    }

    #[test]
    fn row_count_bound_and_malformed_row_data() {
        let at_bound = body(|e| {
            e.write_u32_be(u32::try_from(MAX_PATH_RESULTS).expect("bounded"));
            for _ in 0..MAX_PATH_RESULTS {
                e.write_str("/a");
                e.write_u8(0);
            }
        });
        assert_eq!(decode_rows(&at_bound).unwrap().len(), MAX_PATH_RESULTS);
        assert_eq!(
            decode_rows(&(u32::try_from(MAX_PATH_RESULTS).expect("bounded") + 1).to_be_bytes()),
            Err(DecodeError::PathResultLimitExceeded)
        );
        let mut trailing = body(|e| e.write_u32_be(0));
        trailing.push(1);
        assert!(decode_rows(&trailing).is_err());
        let unknown_kind = body(|e| {
            e.write_u32_be(1);
            e.write_str("/a");
            e.write_u8(9);
        });
        assert!(decode_rows(&unknown_kind).is_err());
        let relative = body(|e| {
            e.write_u32_be(1);
            e.write_str("relative");
            e.write_u8(0);
        });
        assert!(decode_rows(&relative).is_err());
    }

    #[test]
    fn missing_status_conflicting_refusal_and_unknown_status_fail() {
        let missing = body(|e| {
            e.write_field_with(field::path_results::REQUEST_ID, |f| f.write_u32_be(1));
            e.write_field(field::path_results::ROOT, b"/");
            e.write_field_with(field::path_results::ROWS, |f| f.write_u32_be(0));
        });
        assert!(decode_results(&mut Decoder::new(&missing)).is_err());
        let conflicting = body(|e| {
            encode_results(
                e,
                1,
                &Err(PathQueryError {
                    root: "/".into(),
                    code: PathErrorCode::Other,
                    message: "no".into(),
                }),
            );
            e.write_field_with(field::path_results::ROWS, |f| f.write_u32_be(0));
        });
        assert!(decode_results(&mut Decoder::new(&conflicting)).is_err());
        let unknown = body(|e| {
            encode_results(
                e,
                1,
                &Ok(PathResults {
                    root: "/".into(),
                    parent: None,
                    rows: vec![],
                    status: PathStatus::Complete,
                }),
            );
            e.write_field(field::path_results::STATUS, &[9]);
        });
        assert!(decode_results(&mut Decoder::new(&unknown)).is_err());
    }

    #[test]
    fn successful_results_reject_nonabsolute_or_nul_root_and_parent() {
        for (root, parent) in [
            ("relative", None),
            ("/okay\0bad", None),
            ("/okay", Some("relative")),
            ("/okay", Some("/parent\0bad")),
        ] {
            let reply = Ok(PathResults {
                root: root.into(),
                parent: parent.map(str::to_owned),
                rows: vec![],
                status: PathStatus::Complete,
            });
            let bytes = body(|encoder| encode_results(encoder, 1, &reply));
            assert!(decode_results(&mut Decoder::new(&bytes)).is_err());
        }
    }
}
