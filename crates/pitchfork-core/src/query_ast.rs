//! Query AST + parser helpers.
//!
//! Port of the filtering half of `haystack/query.py` and
//! `haystack/backends/__init__.py::SearchNode`: the SQ tree (AND/OR/NOT,
//! negation) and the `__`-separated filter lookups (`__exact`, `__contains`,
//! `__startswith`, `__endswith`, `__gt/gte/lt/lte`, `__in`, `__range`,
//! `__fuzzy`, `__content`) become a typed AST. The Python adapter encodes
//! haystack `SearchNode` trees into this AST; the Rust engine evaluates it.

use crate::schema::{FieldType, PitchforkSchema};
use crate::wire::WireValue;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Filter lookups. Mirrors `haystack.constants.VALID_FILTERS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    Exact,
    Contains,
    #[serde(rename = "startswith")]
    StartsWith,
    #[serde(rename = "endswith")]
    EndsWith,
    Gt,
    Gte,
    Lt,
    Lte,
    In,
    Range,
    Fuzzy,
    /// Full-text content search (`content` / no-lookup default in haystack).
    Content,
}

impl FilterOp {
    /// Parse a `__`-suffix lookup name, mirroring
    /// `SearchNode.split_expression` (unknown suffixes fall back to `content`
    /// on the Python side, so `from_str` is only called with validated names).
    pub fn from_lookup(lookup: &str) -> Option<FilterOp> {
        match lookup {
            "exact" => Some(FilterOp::Exact),
            "contains" => Some(FilterOp::Contains),
            "startswith" => Some(FilterOp::StartsWith),
            "endswith" => Some(FilterOp::EndsWith),
            "gt" => Some(FilterOp::Gt),
            "gte" => Some(FilterOp::Gte),
            "lt" => Some(FilterOp::Lt),
            "lte" => Some(FilterOp::Lte),
            "in" => Some(FilterOp::In),
            "range" => Some(FilterOp::Range),
            "fuzzy" => Some(FilterOp::Fuzzy),
            "content" => Some(FilterOp::Content),
            _ => None,
        }
    }

    pub fn as_lookup(&self) -> &'static str {
        match self {
            FilterOp::Exact => "exact",
            FilterOp::Contains => "contains",
            FilterOp::StartsWith => "startswith",
            FilterOp::EndsWith => "endswith",
            FilterOp::Gt => "gt",
            FilterOp::Gte => "gte",
            FilterOp::Lt => "lt",
            FilterOp::Lte => "lte",
            FilterOp::In => "in",
            FilterOp::Range => "range",
            FilterOp::Fuzzy => "fuzzy",
            FilterOp::Content => "content",
        }
    }
}

impl fmt::Display for FilterOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_lookup())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "node", rename_all = "snake_case")]
pub enum QueryNode {
    /// Match every document (`matching_all_fragment` -> "*").
    MatchAll,
    /// Match no documents.
    MatchNone,
    And { children: Vec<QueryNode> },
    Or { children: Vec<QueryNode> },
    Not { child: Box<QueryNode> },
    Filter {
        field: String,
        op: FilterOp,
        value: WireValue,
    },
}

impl QueryNode {
    /// Convenience constructor for a filter leaf.
    pub fn filter(field: &str, op: FilterOp, value: WireValue) -> QueryNode {
        QueryNode::Filter {
            field: field.to_string(),
            op,
            value,
        }
    }

    /// Validate this node against a schema. Returns an error for unknown
    /// fields (hard error, never a silent no-op) and impossible values.
    pub fn validate(&self, schema: &PitchforkSchema) -> Result<(), String> {
        match self {
            QueryNode::MatchAll | QueryNode::MatchNone => Ok(()),
            QueryNode::And { children } | QueryNode::Or { children } => {
                for child in children {
                    child.validate(schema)?;
                }
                Ok(())
            }
            QueryNode::Not { child } => child.validate(schema),
            QueryNode::Filter { field, op, value } => {
                // Reserved identifier fields always exist.
                if field == "id" || field == "django_ct" || field == "django_id" {
                    return Ok(());
                }
                let def = schema
                    .get(field)
                    .ok_or_else(|| format!("unknown filter field '{field}'"))?;
                validate_value(def.field_type, *op, value, field)
            }
        }
    }
}

fn validate_value(ty: FieldType, op: FilterOp, value: &WireValue, field: &str) -> Result<(), String> {
    use WireValue::*;
    match ty {
        FieldType::Integer => matches_number(value, op, field),
        FieldType::Float => matches_number(value, op, field),
        FieldType::Boolean => {
            if matches!(value, Bool(_)) {
                Ok(())
            } else {
                Err(format!("filter on boolean field '{field}' requires a boolean value"))
            }
        }
        FieldType::Date | FieldType::DateTime => {
            if matches!(value, Str(_)) || (matches!(op, FilterOp::In | FilterOp::Range) && matches!(value, List(_))) {
                Ok(())
            } else {
                Err(format!(
                    "filter on date field '{field}' requires an ISO-8601 string value"
                ))
            }
        }
        FieldType::Text | FieldType::Char | FieldType::Ngram | FieldType::EdgeNgram | FieldType::Location => Ok(()),
    }
}

fn matches_number(value: &WireValue, op: FilterOp, field: &str) -> Result<(), String> {
    match value {
        WireValue::Int(_) | WireValue::Float(_) => Ok(()),
        WireValue::List(_) if matches!(op, FilterOp::In | FilterOp::Range) => Ok(()),
        _ => Err(format!(
            "filter on numeric field '{field}' requires a numeric value"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_round_trip_all_node_kinds() {
        let nodes = vec![
            QueryNode::MatchAll,
            QueryNode::MatchNone,
            QueryNode::And {
                children: vec![
                    QueryNode::filter("title", FilterOp::Exact, WireValue::Str("hello".into())),
                    QueryNode::Or {
                        children: vec![
                            QueryNode::filter("views", FilterOp::Gt, WireValue::Int(10)),
                            QueryNode::Not {
                                child: Box::new(QueryNode::filter(
                                    "title",
                                    FilterOp::StartsWith,
                                    WireValue::Str("in".into()),
                                )),
                            },
                        ],
                    },
                ],
            },
            QueryNode::filter("title", FilterOp::In, WireValue::List(vec![WireValue::Str("a".into()), WireValue::Str("b".into())])),
            QueryNode::filter("views", FilterOp::Range, WireValue::List(vec![WireValue::Int(1), WireValue::Int(5)])),
            QueryNode::filter("text", FilterOp::Fuzzy, WireValue::Str("helo".into())),
        ];

        for node in nodes {
            let json = serde_json::to_string(&node).unwrap();
            let back: QueryNode = serde_json::from_str(&json).unwrap();
            assert_eq!(node, back);
        }
    }

    #[test]
    fn lookup_parsing() {
        for lookup in [
            "exact", "contains", "startswith", "endswith", "gt", "gte", "lt", "lte", "in",
            "range", "fuzzy", "content",
        ] {
            assert!(FilterOp::from_lookup(lookup).is_some(), "missing {lookup}");
        }
        assert_eq!(FilterOp::from_lookup("bogus"), None);
    }

    #[test]
    fn validation_rejects_unknown_fields() {
        let schema = PitchforkSchema::new(vec![crate::schema::FieldDef::new(
            "title",
            FieldType::Char,
        )]);
        assert!(QueryNode::filter("nope", FilterOp::Exact, WireValue::Str("x".into()))
            .validate(&schema)
            .is_err());
        assert!(QueryNode::filter("title", FilterOp::Exact, WireValue::Str("x".into()))
            .validate(&schema)
            .is_ok());
        assert!(QueryNode::filter("views", FilterOp::Exact, WireValue::Str("x".into()))
            .validate(&schema)
            .is_err());
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use crate::wire::WireValue;
    use proptest::prelude::*;

    fn arb_wire_value() -> impl Strategy<Value = WireValue> {
        prop_oneof![
            Just(WireValue::Null),
            any::<bool>().prop_map(WireValue::Bool),
            any::<i64>().prop_map(WireValue::Int),
            // Realistic float range: serde_json's shortest-repr formatting
            // may drop a digit on extreme subnormals, which never occur in
            // haystack values (names, dates, counts, ratings).
            (-1e9f64..1e9f64).prop_map(WireValue::Float),
            ".*".prop_map(WireValue::Str),
            prop::collection::vec("[a-z]{0,10}", 0..4).prop_map(|items| {
                WireValue::List(items.into_iter().map(WireValue::Str).collect())
            }),
        ]
    }

    fn arb_filter() -> impl Strategy<Value = QueryNode> {
        (("[a-z]{1,8}", 0..12usize), arb_wire_value()).prop_map(|((field, op_idx), value)| {
            let op = match op_idx {
                0 => FilterOp::Exact,
                1 => FilterOp::Contains,
                2 => FilterOp::StartsWith,
                3 => FilterOp::EndsWith,
                4 => FilterOp::Gt,
                5 => FilterOp::Gte,
                6 => FilterOp::Lt,
                7 => FilterOp::Lte,
                8 => FilterOp::In,
                9 => FilterOp::Range,
                10 => FilterOp::Fuzzy,
                _ => FilterOp::Content,
            };
            QueryNode::Filter { field, op, value }
        })
    }

    /// Random ASTs up to depth 2: compound nodes over filters and leaves.
    fn arb_node() -> impl Strategy<Value = QueryNode> {
        prop_oneof![
            arb_filter(),
            prop::collection::vec(arb_filter(), 1..4).prop_map(|children| QueryNode::And { children }),
            prop::collection::vec(arb_filter(), 1..4).prop_map(|children| QueryNode::Or { children }),
            arb_filter().prop_map(|f| QueryNode::Not { child: Box::new(f) }),
            prop::collection::vec(arb_filter(), 1..3).prop_map(|children| QueryNode::And {
                children: children
                    .into_iter()
                    .chain(std::iter::once(QueryNode::MatchAll))
                    .collect()
            }),
            Just(QueryNode::MatchAll),
        ]
    }

    proptest! {
        /// Every query AST must survive a JSON round trip unchanged.
        #[test]
        fn ast_json_round_trip(node in arb_node()) {
            let json = serde_json::to_string(&node).expect("serialize");
            let back: QueryNode = serde_json::from_str(&json).expect("deserialize");
            prop_assert_eq!(node, back);
        }
    }
}
