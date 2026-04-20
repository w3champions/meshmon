//! Keyset-cursor codec for the catalogue list endpoint.
//!
//! A [`Cursor`] is the opaque forward-paging token exchanged between
//! server and client. It carries:
//!
//! - `s` — the [`SortBy`] column the page is ordered by.
//! - `d` — the [`SortDir`] of that ordering.
//! - `v` — the sort-column value of the last row in the previous page,
//!   or JSON `null` when that row sat in the `NULLS LAST` tail.
//! - `i` — the `id` of the last row, always present — `id DESC` is the
//!   invariant tiebreaker across every sort variant.
//!
//! The wire form is `base64(URL_SAFE_NO_PAD)` over the serde JSON
//! encoding. The base64 layer is cosmetic (URL-safe) — the token is
//! **not** authenticated or tamper-resistant. The server revalidates
//! `(s, d)` against the request's `sort` / `sort_dir` before trusting
//! the cursor; any mismatch or decode error yields an empty page with
//! `next_cursor = None` at the handler layer (see
//! [`super::handlers::list`]).
//!
//! Stability contract: the on-the-wire JSON shape is load-bearing
//! across deployments — an in-flight cursor from an older release must
//! still decode after a rolling upgrade. The field renames (`s`/`d`/
//! `v`/`i`) and their order must not change without a compatibility
//! migration. The [`golden_cursor_json_shape_is_stable`] test pins the
//! JSON shape so a drift fails loudly.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::dto::{SortBy, SortDir};

/// Opaque forward-paging token over `(sort_column, id)` keyset.
///
/// See the module-level docs for the wire contract; the short field
/// names are intentional — cursors show up in URLs and logs, and every
/// saved byte helps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Sort column the cursor was minted for.
    #[serde(rename = "s")]
    pub sort: SortBy,
    /// Sort direction the cursor was minted for.
    #[serde(rename = "d")]
    pub dir: SortDir,
    /// Last row's sort-column value, or JSON `null` when the last row
    /// was inside the `NULLS LAST` tail. `serde_json::Value` keeps the
    /// codec column-type-agnostic; the repo layer converts per variant.
    #[serde(rename = "v")]
    pub value: serde_json::Value,
    /// Last row's `id`. Present in every cursor — `id DESC` is the
    /// mandatory tiebreaker for the keyset.
    #[serde(rename = "i")]
    pub id: Uuid,
}

/// Failure modes surfaced by [`Cursor::decode`].
///
/// These are distinguished so the handler can tell a malformed token
/// apart from a token that deserialized into an incoherent shape, but
/// in practice the handler collapses them all into "empty page, no
/// next cursor" — see the module-level docs for the rationale.
#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    /// Input was not a valid `base64(URL_SAFE_NO_PAD)` byte string.
    #[error("invalid base64 cursor: {0}")]
    Base64(#[from] base64::DecodeError),
    /// The decoded bytes were not valid UTF-8 JSON, or the JSON didn't
    /// match the [`Cursor`] shape.
    #[error("invalid cursor json: {0}")]
    Json(#[from] serde_json::Error),
}

impl Cursor {
    /// Encode to the wire form: `base64(URL_SAFE_NO_PAD, json(self))`.
    ///
    /// `json` uses the serde-default compact layout (no whitespace) so
    /// the emitted token is minimal. Serialization is infallible for
    /// the concrete field set — the `serde_json::Value` branch accepts
    /// any value `serde_json` can itself produce, which covers every
    /// type returned by the repo layer.
    pub fn encode(&self) -> String {
        // `to_vec` only fails on map keys that aren't strings or on
        // `Serialize` impls that error — neither applies here.
        let bytes = serde_json::to_vec(self).expect("cursor is always serializable");
        URL_SAFE_NO_PAD.encode(bytes)
    }

    /// Decode the wire form. Returns [`CursorError`] on base64 or JSON
    /// failure; the handler treats either as "ignore and return empty".
    pub fn decode(raw: &str) -> Result<Self, CursorError> {
        let bytes = URL_SAFE_NO_PAD.decode(raw)?;
        let cursor = serde_json::from_slice(&bytes)?;
        Ok(cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed UUID literal so the golden-shape test is reproducible.
    const FIXED_ID: Uuid = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);

    fn sample_cursor() -> Cursor {
        Cursor {
            sort: SortBy::City,
            dir: SortDir::Asc,
            value: serde_json::Value::String("Berlin".into()),
            id: FIXED_ID,
        }
    }

    #[test]
    fn encode_decode_round_trips() {
        let original = sample_cursor();
        let wire = original.encode();
        let back = Cursor::decode(&wire).expect("decode round-trip");
        assert_eq!(original, back);
    }

    #[test]
    fn encode_decode_round_trips_for_null_value() {
        // NULL-valued sort columns (e.g. the `NULLS LAST` tail for
        // `city`) serialize their `v` as JSON null; round-trip must
        // survive that branch too.
        let original = Cursor {
            sort: SortBy::City,
            dir: SortDir::Asc,
            value: serde_json::Value::Null,
            id: FIXED_ID,
        };
        let wire = original.encode();
        let back = Cursor::decode(&wire).expect("decode null-value cursor");
        assert_eq!(original, back);
        assert!(back.value.is_null());
    }

    #[test]
    fn tampered_base64_errors() {
        // `!` is not a URL_SAFE_NO_PAD alphabet character, so any
        // token containing it must fail at the base64 layer before
        // reaching the JSON decoder.
        let err = Cursor::decode("not!valid!base64").expect_err("decode should fail");
        assert!(matches!(err, CursorError::Base64(_)), "got: {err:?}");
    }

    #[test]
    fn valid_base64_but_garbage_payload_errors() {
        // Valid base64 that decodes to bytes serde_json cannot parse
        // as a Cursor — must surface as the JSON error branch so the
        // handler can distinguish it if it ever cares to.
        let wire = URL_SAFE_NO_PAD.encode(b"not cursor json");
        let err = Cursor::decode(&wire).expect_err("decode should fail");
        assert!(matches!(err, CursorError::Json(_)), "got: {err:?}");
    }

    /// The wire contract: the serialized JSON shape is
    /// `{"s":"city","d":"asc","v":"Berlin","i":"<uuid>"}`. Asserting
    /// against the intermediate `serde_json::Value` (not the base64
    /// bytes) keeps the test readable while still catching any
    /// accidental rename of the short keys or the `rename_all` of the
    /// enums. The codec is round-tripped separately by the tests above.
    #[test]
    fn golden_cursor_json_shape_is_stable() {
        let cursor = sample_cursor();
        let shape = serde_json::to_value(&cursor).expect("serialize to value");
        let expected = serde_json::json!({
            "s": "city",
            "d": "asc",
            "v": "Berlin",
            "i": FIXED_ID.to_string(),
        });
        assert_eq!(shape, expected);
    }
}
