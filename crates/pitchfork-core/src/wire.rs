//! PitchforkWireV1: the versioned wire protocol (request/result types).
//!
//! Design rules (from the django-bolt lessons):
//! - One versioned contract; every new cross-boundary type adds the Python
//!   constant (`django_pitchfork/wire.py::WIRE_VERSION`) and this Rust static
//!   in the same commit.
//! - Results use int body kinds (0 = hits, 1 = empty, 2 = error) so the Rust
//!   side resolves to static structs without per-response string dispatch.

use crate::query_ast::QueryNode;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Wire protocol version. Must match `django_pitchfork/wire.py::WIRE_VERSION`.
pub const WIRE_VERSION: u32 = 1;

/// A JSON value as it crosses the wire (document field values, facet
/// keys). Untagged: strings, numbers, bools, lists and null map naturally
/// to/from `serde_json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<WireValue>),
}

impl WireValue {
    pub fn from_json(value: serde_json::Value) -> Option<WireValue> {
        serde_json::from_value(value).ok()
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("wire value serialization cannot fail")
    }
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// Search request body (POST /search).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchRequestV1 {
    pub v: u32,
    /// Schema fingerprint; mismatch is a hard error.
    pub schema_fp: String,
    /// The query AST.
    pub ast: QueryNode,
    #[serde(default)]
    pub start: usize,
    #[serde(default)]
    pub end: Option<usize>,
    /// Order-by field names (haystack `sort_by`; leading `-` = descending).
    #[serde(default)]
    pub sort: Vec<String>,
    /// Facet field names (haystack `facets` dict keys).
    #[serde(default)]
    pub facets: Vec<String>,
    /// Highlight the document field.
    #[serde(default)]
    pub highlight: bool,
    /// Request a spelling suggestion.
    #[serde(default)]
    pub spellcheck: bool,
    /// The query to spell-check (defaults to the main query when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spelling_query: Option<String>,
    /// Geospatial filter (haystack `within` / `dwithin`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spatial: Option<SpatialFilterV1>,
    /// More-like-this: the source document id (uses /more_like_this instead).
    #[serde(default)]
    pub mlt_doc_id: Option<String>,
}

/// Geospatial filters (haystack `within` = bounding box, `dwithin` = radius).
/// Points travel as "lat,lng" strings (haystack's LocationField format).
///
/// Wire rule: the Python side (django_pitchfork/wire.py SPATIAL_*) and this
/// Rust enum are changed in the same commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpatialFilterV1 {
    /// Bounding box between two corners (haystack `within`).
    Within {
        field: String,
        point_1: String,
        point_2: String,
    },
    /// Radius around a point (haystack `dwithin`), distance in kilometers.
    #[serde(rename = "dwithin")]
    DWithin {
        field: String,
        point: String,
        radius_km: f64,
    },
}

/// Index request body (POST /index). The schema travels with the documents;
/// the server caches compiled schemas by fingerprint and hard-errors on
/// mismatch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexRequestV1 {
    pub v: u32,
    /// Optional: the adapter doesn't know the fingerprint before /setup
    /// registers the schema; the server computes and returns it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_fp: Option<String>,
    pub schema: crate::schema::PitchforkSchema,
    /// Documents: flat maps of fieldname -> value, including the reserved
    /// keys `id`, `django_ct`, `django_id`.
    pub docs: Vec<BTreeMap<String, WireValue>>,
}

/// Remove request body (POST /remove).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoveRequestV1 {
    pub v: u32,
    pub schema_fp: String,
    /// Document ids to remove (haystack: "app.model.pk").
    pub ids: Vec<String>,
}

/// Clear request body (POST /clear).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClearRequestV1 {
    pub v: u32,
    #[serde(default)]
    pub schema_fp: Option<String>,
}

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

/// Response body kinds.
pub const KIND_HITS: u32 = 0;
pub const KIND_EMPTY: u32 = 1;
pub const KIND_ERROR: u32 = 2;

/// A single search hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub id: String,
    pub django_ct: String,
    pub django_id: String,
    pub score: f64,
    /// All stored field values (excluding the reserved keys above).
    #[serde(default)]
    pub stored: BTreeMap<String, WireValue>,
    /// Highlighted fragments per field, when highlight was requested.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub snippets: BTreeMap<String, Vec<String>>,
}

/// Facet counts: field name -> ordered list of (value, count) pairs,
/// sorted by descending count then value, matching whoosh's ordering.
pub type FacetCounts = BTreeMap<String, Vec<(WireValue, i64)>>;

/// Search response (kind 0 = hits, 1 = empty, 2 = error).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResponseV1 {
    pub v: u32,
    pub kind: u32,
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub hits: Vec<Hit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facets: Option<FacetCounts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spelling: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl SearchResponseV1 {
    pub fn hits(total: i64, hits: Vec<Hit>) -> SearchResponseV1 {
        SearchResponseV1 {
            v: WIRE_VERSION,
            kind: if hits.is_empty() { KIND_EMPTY } else { KIND_HITS },
            total,
            hits,
            facets: None,
            spelling: None,
            error: None,
        }
    }

    pub fn empty() -> SearchResponseV1 {
        SearchResponseV1::hits(0, Vec::new())
    }

    pub fn error(message: impl Into<String>) -> SearchResponseV1 {
        SearchResponseV1 {
            v: WIRE_VERSION,
            kind: KIND_ERROR,
            total: 0,
            hits: Vec::new(),
            facets: None,
            spelling: None,
            error: Some(message.into()),
        }
    }
}

/// Acknowledgement response for /index, /remove, /clear, /more_like_this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AckResponseV1 {
    pub v: u32,
    pub ok: bool,
    /// Server-computed schema fingerprint, returned by /setup and /index so
    /// the adapter can echo it back in search requests without duplicating
    /// the fingerprint algorithm. (Python side: `django_pitchfork/wire.py`.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_fp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl AckResponseV1 {
    pub fn ok() -> AckResponseV1 {
        AckResponseV1 {
            v: WIRE_VERSION,
            ok: true,
            schema_fp: None,
            error: None,
        }
    }

    pub fn error(message: impl Into<String>) -> AckResponseV1 {
        AckResponseV1 {
            v: WIRE_VERSION,
            ok: false,
            schema_fp: None,
            error: Some(message.into()),
        }
    }
}

/// Health response (GET /health).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthResponseV1 {
    pub v: u32,
    pub status: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{FieldDef, FieldType, PitchforkSchema};
    use crate::query_ast::FilterOp;

    #[test]
    fn wire_version_matches_plan() {
        assert_eq!(WIRE_VERSION, 1);
    }

    #[test]
    fn search_request_round_trip() {
        let req = SearchRequestV1 {
            v: WIRE_VERSION,
            schema_fp: "a1b2".into(),
            ast: QueryNode::filter("title", FilterOp::Exact, WireValue::Str("x".into())),
            start: 0,
            end: Some(20),
            sort: vec!["-pub_date".into()],
            facets: vec!["author".into()],
            highlight: false,
            spellcheck: true,
            spelling_query: Some("indxed".into()),
            spatial: None,
            mlt_doc_id: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"schema_fp\":\"a1b2\""));
        let back: SearchRequestV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn response_kinds_are_int_tagged() {
        let hits = SearchResponseV1::hits(
            142,
            vec![Hit {
                id: "core.mockmodel.1".into(),
                django_ct: "core.mockmodel".into(),
                django_id: "1".into(),
                score: 1.5,
                stored: BTreeMap::new(),
                snippets: BTreeMap::new(),
            }],
        );
        let json = serde_json::to_value(&hits).unwrap();
        assert_eq!(json["kind"], 0);

        let err = SearchResponseV1::error("boom");
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["kind"], 2);
        assert_eq!(json["error"], "boom");
    }

    #[test]
    fn index_request_carries_schema() {
        let mut doc = BTreeMap::new();
        doc.insert("id".to_string(), WireValue::Str("core.mockmodel.1".into()));
        doc.insert("django_ct".to_string(), WireValue::Str("core.mockmodel".into()));
        doc.insert("django_id".to_string(), WireValue::Str("1".into()));
        doc.insert("text".to_string(), WireValue::Str("hello world".into()));

        let req = IndexRequestV1 {
            v: WIRE_VERSION,
            schema_fp: Some("a1b2".into()),
            schema: PitchforkSchema::new(vec![FieldDef {
                document: true,
                ..FieldDef::new("text", FieldType::Text)
            }]),
            docs: vec![doc],
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: IndexRequestV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
        assert_eq!(back.docs.len(), 1);
    }
}
