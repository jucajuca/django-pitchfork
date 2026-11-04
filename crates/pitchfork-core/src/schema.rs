//! Document schema and field types.
//!
//! Port of the type half of `haystack/fields.py`: field types (text, char,
//! date, integer, float, boolean, multivalue, location, ngram) plus the
//! options the search engine needs (stored, indexed, multi, boost).
//! Template/model-attr preparation stays in Python; only the engine-facing
//! schema crosses the wire.

use serde::{Deserialize, Serialize};

/// Search field types, mirroring the `field_type` attributes of the
/// `SearchField` subclasses in `haystack/fields.py`.
///
/// - `CharField` -> `Char` ("string"), `MultiValueField` -> `Char` + `multi`
/// - `DecimalField` -> `Char` (haystack coerces decimals to strings)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Text,
    Char,
    Date,
    DateTime,
    Integer,
    Float,
    Boolean,
    Location,
    Ngram,
    EdgeNgram,
}

impl FieldType {
    /// Parse a haystack `field_type` string (plus the multivalue flag).
    pub fn from_haystack(field_type: &str, is_multivalued: bool) -> Option<FieldType> {
        let ty = match field_type {
            "string" => FieldType::Char,
            "date" => FieldType::Date,
            "datetime" => FieldType::DateTime,
            "integer" => FieldType::Integer,
            "float" => FieldType::Float,
            "boolean" => FieldType::Boolean,
            "location" => FieldType::Location,
            "ngram" => FieldType::Ngram,
            "edge_ngram" => FieldType::EdgeNgram,
            _ => return None,
        };
        // MultiValueField declares field_type "string"; the multi flag marks it.
        // The engine's text analysis decides the Text/Char distinction.
        if is_multivalued {
            Some(FieldType::Char)
        } else {
            Some(ty)
        }
    }
}

/// A single field definition in the schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    #[serde(default = "default_true")]
    pub stored: bool,
    #[serde(default = "default_true")]
    pub indexed: bool,
    /// Multivalue flag (`MultiValueField.is_multivalued`).
    #[serde(default)]
    pub multi: bool,
    /// The document=True field (haystack's `DOCUMENT_FIELD`, default "text").
    #[serde(default)]
    pub document: bool,
    #[serde(default = "default_boost")]
    pub boost: f64,
}

fn default_true() -> bool {
    true
}

fn default_boost() -> f64 {
    1.0
}

impl FieldDef {
    pub fn new(name: &str, field_type: FieldType) -> Self {
        FieldDef {
            name: name.to_string(),
            field_type,
            stored: true,
            indexed: true,
            multi: false,
            document: false,
            boost: 1.0,
        }
    }
}

/// The full document schema. Immutable once fingerprinted; a fingerprint
/// mismatch between adapter and server is a hard error, never a rebuild.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PitchforkSchema {
    pub fields: Vec<FieldDef>,
}

impl PitchforkSchema {
    pub fn new(fields: Vec<FieldDef>) -> Self {
        PitchforkSchema { fields }
    }

    pub fn get(&self, name: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// The document field name (`haystack.constants.DOCUMENT_FIELD`).
    pub fn document_field(&self) -> Option<&str> {
        self.fields
            .iter()
            .find(|f| f.document)
            .map(|f| f.name.as_str())
    }

    /// Stable fingerprint over the canonical JSON of the schema.
    /// FNV-1a 64-bit, hex-encoded — deterministic across processes.
    pub fn fingerprint(&self) -> String {
        let json = serde_json::to_vec(self).expect("schema serialization cannot fail");
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut hash = FNV_OFFSET;
        for byte in &json {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        format!("{hash:016x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PitchforkSchema {
        PitchforkSchema::new(vec![
            FieldDef {
                document: true,
                ..FieldDef::new("text", FieldType::Text)
            },
            FieldDef::new("title", FieldType::Char),
            FieldDef {
                multi: true,
                ..FieldDef::new("tags", FieldType::Char)
            },
            FieldDef::new("pub_date", FieldType::DateTime),
            FieldDef::new("views", FieldType::Integer),
            FieldDef::new("rating", FieldType::Float),
            FieldDef::new("active", FieldType::Boolean),
        ])
    }

    #[test]
    fn fingerprint_is_stable_and_order_sensitive() {
        let a = sample();
        assert_eq!(a.fingerprint(), a.fingerprint());

        let mut b = sample();
        b.fields[1].name = "name".to_string();
        assert_ne!(a.fingerprint(), b.fingerprint());

        let mut c = sample();
        c.fields.swap(0, 1);
        assert_ne!(a.fingerprint(), c.fingerprint());
    }

    #[test]
    fn document_field_found() {
        assert_eq!(sample().document_field(), Some("text"));
    }

    #[test]
    fn serde_round_trip() {
        let schema = sample();
        let json = serde_json::to_string(&schema).unwrap();
        let back: PitchforkSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(schema, back);
        assert_eq!(schema.fingerprint(), back.fingerprint());
    }

    #[test]
    fn haystack_type_mapping() {
        assert_eq!(
            FieldType::from_haystack("string", true),
            Some(FieldType::Char)
        );
        assert_eq!(
            FieldType::from_haystack("datetime", false),
            Some(FieldType::DateTime)
        );
        assert_eq!(FieldType::from_haystack("bogus", false), None);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// The fingerprint must be deterministic and injective enough that
        /// any field rename changes it.
        #[test]
        fn fingerprint_changes_on_rename(name in "[a-z]{1,10}") {
            let a = PitchforkSchema::new(vec![FieldDef::new("title", FieldType::Char)]);
            let b = PitchforkSchema::new(vec![FieldDef::new(&name, FieldType::Char)]);
            if name == "title" {
                prop_assert_eq!(a.fingerprint(), b.fingerprint());
            } else {
                prop_assert_ne!(a.fingerprint(), b.fingerprint());
            }
        }
    }
}
