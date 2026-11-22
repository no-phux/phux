//! CONNECT admission: the pairing token from exactly one carrier.

use std::collections::HashMap;

use wtransport_proto::qpack::Decoder;

use super::connect_headers;

/// Admit a CONNECT from its raw QPACK payload, refusing duplicate
/// `Authorization` fields before a collapsed header map could hide them.
pub(super) fn admit_connect(
    payload: &[u8],
    tokens: Option<&crate::auth::ReloadingTokenStore>,
) -> Result<Option<crate::auth::AuthenticatedCredential>, &'static str> {
    match connect_headers::has_duplicate_authorization(payload) {
        Ok(true) | Err(_) => {
            return Err("duplicate or malformed authorization");
        }
        Ok(false) => {}
    }
    let headers = Decoder::decode(payload).map_err(|_| "CONNECT QPACK decode failed")?;
    tokens.map_or(Ok(None), |store| {
        authorize_request(&headers, store)
            .map(Some)
            .ok_or("missing or invalid token")
    })
}

/// Verify the bearer token from exactly one carrier: an `Authorization:
/// Bearer` header or a `token=` query parameter on `:path`.
fn authorize_request(
    headers: &HashMap<String, String>,
    store: &crate::auth::ReloadingTokenStore,
) -> Option<crate::auth::AuthenticatedCredential> {
    let token_hex = request_token(headers)?;
    let token = hex::decode(token_hex.trim()).ok()?;
    store.authenticate_and_touch(&token)
}

fn request_token(headers: &HashMap<String, String>) -> Option<&str> {
    let path = headers.get(":path").map_or("/", String::as_str);
    match (unique_bearer(headers), unique_query_token(path)) {
        (UniqueToken::Valid(token), UniqueToken::Missing)
        | (UniqueToken::Missing, UniqueToken::Valid(token)) => Some(token),
        _ => None,
    }
}

enum UniqueToken<'a> {
    Missing,
    Valid(&'a str),
    Invalid,
}

/// The `Bearer` value of the one `Authorization` header, matching the field
/// name case-insensitively.
fn unique_bearer(headers: &HashMap<String, String>) -> UniqueToken<'_> {
    let mut values = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.as_str());
    let Some(value) = values.next() else {
        return UniqueToken::Missing;
    };
    if values.next().is_some() {
        return UniqueToken::Invalid;
    }
    value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .filter(|token| !token.is_empty())
        .map_or(UniqueToken::Invalid, UniqueToken::Valid)
}

/// The `token` query parameter of a `:path`, e.g. `/session?token=<hex>`.
fn unique_query_token(path: &str) -> UniqueToken<'_> {
    let Some((_, query)) = path.split_once('?') else {
        return UniqueToken::Missing;
    };
    let mut carriers = query
        .split('&')
        .filter(|part| *part == "token" || part.starts_with("token="));
    let Some(carrier) = carriers.next() else {
        return UniqueToken::Missing;
    };
    let Some(value) = carrier.strip_prefix("token=") else {
        return UniqueToken::Invalid;
    };
    if value.is_empty() || carriers.next().is_some() {
        return UniqueToken::Invalid;
    }
    UniqueToken::Valid(value)
}
