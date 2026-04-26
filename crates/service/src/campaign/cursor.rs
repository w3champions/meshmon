//! Opaque keyset-pagination cursors for the paginated pair_details endpoint
//! (`GET /api/campaigns/{id}/evaluation/candidates/{destination_ip}/pair_details`).
//!
//! Each cursor captures three things: the sort column the page-1 caller
//! requested, the value of that column on the last row of the previous
//! page, and the `(source_agent_id, destination_agent_id)` tiebreak tail
//! drawn from the post-T54 composite primary key. Encoding is JSON inside
//! base64 (URL-safe, no padding) — clients should treat the byte stream
//! as opaque and never inspect or fabricate it.
//!
//! Decode validates three things in order: base64 + JSON parse succeeds,
//! the sort-column discriminant is a known variant (defends against
//! cursors truncated by a careless intermediary), and the cursor's
//! recorded sort matches the one the caller asked for. Any of the three
//! failing maps to `400 invalid_cursor` on the wire.

use super::dto::PairDetailSortCol;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Polymorphic value of the `sort` column on the last row of the
/// previous page. Carrying the typed variant (rather than a raw `String`)
/// keeps the cursor-predicate SQL generator type-safe — a numeric column
/// that somehow ends up paired with a `String` value here surfaces as a
/// validation error, not a runtime panic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SortValue {
    /// Numeric column (improvement_ms, direct_rtt_ms, ...).
    F64(f64),
    /// Text column (source_agent_id, destination_agent_id).
    String(String),
    /// Boolean column (qualifies).
    Bool(bool),
    /// Sort column whose last-row value was SQL `NULL`. Currently used
    /// by the edge-pair endpoint when sorting by `best_route_ms` and
    /// the page tail lands on an unreachable row (which persists `NULL`
    /// instead of an infinity sentinel). The cursor predicate handles
    /// the NULL transition with `NULLS LAST` semantics.
    Null,
}

/// Decoded cursor body. The wire form is base64(JSON(this)).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PairDetailCursor {
    /// Sort column the page-1 caller requested. Must match the next
    /// page's `sort` query parameter — a mismatch surfaces as
    /// `invalid_cursor`.
    pub sort_col: PairDetailSortCol,
    /// Value of `sort_col` on the last row of the previous page.
    pub sort_value: SortValue,
    /// Composite-PK tiebreak: source side.
    pub source_agent_id: String,
    /// Composite-PK tiebreak: destination side.
    pub destination_agent_id: String,
}

/// Reasons cursor decoding can fail. All three map to the same
/// `400 invalid_cursor` response on the wire — splitting them out is for
/// telemetry / log clarity.
#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    /// Base64 or JSON decode failure (or otherwise unparseable bytes).
    #[error("cursor decode failed: {0}")]
    Decode(String),
    /// Decoded `sort_col` does not match the request's `sort` parameter.
    #[error("cursor sort column does not match request sort")]
    SortMismatch,
    /// Decoded `sort_col` is not a known [`PairDetailSortCol`] variant.
    /// In practice serde already rejects unknown variants at JSON parse
    /// time, so this maps onto [`CursorError::Decode`]; kept as a
    /// distinct variant so a future encoding change can surface
    /// truncated-discriminant failures separately.
    #[error("cursor sort column discriminant out of range")]
    InvalidEnum,
}

impl PairDetailCursor {
    /// Encode to the opaque `cursor` string the wire response carries
    /// in `next_cursor`. Returns the base64-wrapped JSON byte stream.
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("PairDetailCursor JSON serialization is total");
        URL_SAFE_NO_PAD.encode(json)
    }

    /// Decode and validate against the request's expected sort column.
    /// Maps every failure to a [`CursorError`] for the caller to surface
    /// as `400 invalid_cursor`.
    pub fn decode(raw: &str, expected: PairDetailSortCol) -> Result<Self, CursorError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|e| CursorError::Decode(format!("base64: {e}")))?;
        let cursor: PairDetailCursor = serde_json::from_slice(&bytes)
            .map_err(|e| CursorError::Decode(format!("json: {e}")))?;
        if cursor.sort_col != expected {
            return Err(CursorError::SortMismatch);
        }
        Ok(cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_base64_json() {
        let c = PairDetailCursor {
            sort_col: PairDetailSortCol::ImprovementMs,
            sort_value: SortValue::F64(42.5),
            source_agent_id: "src-a".into(),
            destination_agent_id: "dst-b".into(),
        };
        let s = c.encode();
        let back = PairDetailCursor::decode(&s, PairDetailSortCol::ImprovementMs).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn rejects_garbage_base64() {
        let err = PairDetailCursor::decode("!!!not-base64!!!", PairDetailSortCol::ImprovementMs)
            .unwrap_err();
        assert!(matches!(err, CursorError::Decode(_)));
    }

    #[test]
    fn rejects_valid_base64_but_invalid_json() {
        let raw = URL_SAFE_NO_PAD.encode(b"\x00\x01\x02\x03");
        let err = PairDetailCursor::decode(&raw, PairDetailSortCol::ImprovementMs).unwrap_err();
        assert!(matches!(err, CursorError::Decode(_)));
    }

    #[test]
    fn rejects_unknown_sort_col_variant() {
        // Hand-rolled JSON with a sort_col that doesn't exist on the enum.
        let raw = URL_SAFE_NO_PAD.encode(
            br#"{"sort_col":"not_a_real_column","sort_value":{"kind":"f64","value":1.0},
                 "source_agent_id":"a","destination_agent_id":"b"}"#,
        );
        let err = PairDetailCursor::decode(&raw, PairDetailSortCol::ImprovementMs).unwrap_err();
        assert!(matches!(err, CursorError::Decode(_)));
    }

    #[test]
    fn rejects_sort_mismatch() {
        let c = PairDetailCursor {
            sort_col: PairDetailSortCol::ImprovementMs,
            sort_value: SortValue::F64(1.0),
            source_agent_id: "a".into(),
            destination_agent_id: "b".into(),
        };
        let err =
            PairDetailCursor::decode(&c.encode(), PairDetailSortCol::DirectRttMs).unwrap_err();
        assert!(matches!(err, CursorError::SortMismatch));
    }

    #[test]
    fn string_and_bool_sort_values_round_trip() {
        for v in [
            SortValue::String("agent-1".into()),
            SortValue::Bool(true),
            SortValue::Bool(false),
        ] {
            let c = PairDetailCursor {
                sort_col: PairDetailSortCol::SourceAgentId,
                sort_value: v.clone(),
                source_agent_id: "x".into(),
                destination_agent_id: "y".into(),
            };
            let back =
                PairDetailCursor::decode(&c.encode(), PairDetailSortCol::SourceAgentId).unwrap();
            assert_eq!(back.sort_value, v);
        }
    }
}
