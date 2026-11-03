//! Tantivy-backed search engine implementing the `SearchEngine` trait.
//!
//! Layer 2 of the workspace. Lessons applied:
//! - Compile at registration (`setup`), reuse forever: the tantivy schema,
//!   index, reader and writer are built once per schema fingerprint — never
//!   per query. A fingerprint mismatch is a hard error, never a rebuild.
//! - Known gaps (spelling, MLT quality, spatial) are stubs tracked for
//!   phase 5.

pub mod spell;

use pitchfork_core::query_ast::{FilterOp, QueryNode};
use pitchfork_core::schema::{FieldDef, FieldType, PitchforkSchema};
use pitchfork_core::wire::{
    AckResponseV1, FacetCounts, Hit, IndexRequestV1, RemoveRequestV1, SearchRequestV1,
    SearchResponseV1, WireValue,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::ops::Bound;
use std::path::PathBuf;
use std::sync::RwLock;
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{
    BooleanQuery, EmptyQuery, FuzzyTermQuery, Occur, Query, RangeQuery, RegexQuery, TermQuery,
    TermSetQuery,
};
use tantivy::schema::{
    DateOptions, Document, IndexRecordOption, NumericOptions, OwnedValue, Schema, SchemaBuilder,
    TextFieldIndexing, TextOptions, TantivyDocument,
};
use tantivy::{
    DateTime as TantivyDate, DocAddress, Index, IndexReader, IndexWriter, Order, Searcher, Term,
};

pub const RESERVED_FIELDS: [&str; 3] = ["id", "django_ct", "django_id"];

#[derive(Debug)]
pub enum EngineError {
    Io(std::io::Error),
    Tantivy(tantivy::TantivyError),
    Schema(String),
    Query(String),
    Wire(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::Io(e) => write!(f, "I/O error: {e}"),
            EngineError::Tantivy(e) => write!(f, "search engine error: {e}"),
            EngineError::Schema(m) => write!(f, "schema error: {m}"),
            EngineError::Query(m) => write!(f, "query error: {m}"),
            EngineError::Wire(m) => write!(f, "wire error: {m}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        EngineError::Io(e)
    }
}

impl From<tantivy::TantivyError> for EngineError {
    fn from(e: tantivy::TantivyError) -> Self {
        EngineError::Tantivy(e)
    }
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// The engine contract shared by the Tantivy impl (phase B, embedded) and
/// the server (phase C). One trait, two modes, identical wire types.
pub trait SearchEngine: Send + Sync {
    fn search(&self, req: &SearchRequestV1) -> Result<SearchResponseV1>;
    fn index(&self, req: &IndexRequestV1) -> Result<AckResponseV1>;
    fn remove(&self, req: &RemoveRequestV1) -> Result<AckResponseV1>;
    fn clear(&self) -> Result<AckResponseV1>;
    /// More-like-this by document id. V1: term-based similarity.
    fn more_like_this(&self, doc_id: &str, req: &SearchRequestV1) -> Result<SearchResponseV1>;
    /// Current schema fingerprint, if a schema has been registered.
    fn schema_fp(&self) -> Option<String>;
    fn schema(&self) -> Option<PitchforkSchema>;
}

// ---------------------------------------------------------------------------
// TantivyEngine
// ---------------------------------------------------------------------------

struct CompiledIndex {
    schema_fp: String,
    pitchfork_schema: PitchforkSchema,
    tantivy_schema: Schema,
    reader: IndexReader,
    writer: IndexWriter,
}

/// Tantivy-backed engine. `setup` compiles once; every later call reuses the
/// compiled reader/writer (bolt's "do it once at registration" rule).
pub struct TantivyEngine {
    dir: PathBuf,
    inner: RwLock<Option<CompiledIndex>>,
}

impl TantivyEngine {
    pub fn new(dir: impl Into<PathBuf>) -> Result<TantivyEngine> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(TantivyEngine {
            dir,
            inner: RwLock::new(None),
        })
    }

    /// Compile the schema, open (or create) the index. Re-registering the
    /// same fingerprint is a no-op; a different fingerprint is a hard error.
    pub fn setup(&self, schema: &PitchforkSchema) -> Result<()> {
        let fp = schema.fingerprint();
        {
            let inner = self.inner.read().expect("engine lock poisoned");
            if let Some(compiled) = inner.as_ref() {
                if compiled.schema_fp == fp {
                    return Ok(());
                }
                return Err(EngineError::Schema(format!(
                    "schema fingerprint mismatch: server has '{}', adapter sent '{}'. \
                     Rebuild the index instead of swapping schemas silently.",
                    compiled.schema_fp, fp
                )));
            }
        }

        let tantivy_schema = build_tantivy_schema(schema)?;
        let mut index = match Index::create_in_dir(&self.dir, tantivy_schema.clone()) {
            Ok(index) => index,
            Err(_) => Index::open_in_dir(&self.dir)?,
        };
        // Analyzers for ngram fields (edge ngram = prefix-only ngram).
        index.set_tokenizers(RecordTokenizers::default_settings());

        let writer: IndexWriter = index.writer(32 * 1024 * 1024)?;
        let reader: IndexReader = index.reader()?;

        let mut inner = self.inner.write().expect("engine lock poisoned");
        if let Some(compiled) = inner.as_ref() {
            if compiled.schema_fp == fp {
                // Another thread raced us; same fingerprint is fine.
                return Ok(());
            }
            return Err(EngineError::Schema(
                "schema fingerprint mismatch during setup".to_string(),
            ));
        }
        // The Index is retained by the reader/writer; no separate handle needed.
        let _ = &index;
        *inner = Some(CompiledIndex {
            schema_fp: fp,
            pitchfork_schema: schema.clone(),
            tantivy_schema,
            reader,
            writer,
        });
        Ok(())
    }

    fn compiled(&self) -> Result<std::sync::RwLockReadGuard<'_, Option<CompiledIndex>>> {
        let inner = self.inner.read().expect("engine lock poisoned");
        if inner.is_none() {
            return Err(EngineError::Schema(
                "no schema registered; call setup() (or POST /setup) before searching"
                    .to_string(),
            ));
        }
        Ok(inner)
    }

    fn compiled_mut(&self) -> std::sync::RwLockWriteGuard<'_, Option<CompiledIndex>> {
        self.inner.write().expect("engine lock poisoned")
    }

    fn with_searcher<T>(&self, f: impl FnOnce(&Searcher, &CompiledIndex) -> Result<T>) -> Result<T> {
        let inner = self.compiled()?;
        let compiled = inner.as_ref().expect("checked non-none");
        compiled.reader.reload()?;
        let searcher = compiled.reader.searcher();
        f(&searcher, compiled)
    }

}

/// Tokenizer registry for ngram fields.
struct RecordTokenizers;

impl RecordTokenizers {
    fn default_settings() -> tantivy::tokenizer::TokenizerManager {
        let manager = tantivy::tokenizer::TokenizerManager::default();
        manager.register(
            "ngram",
            tantivy::tokenizer::NgramTokenizer::new(2, 8, false)
                .expect("static ngram configuration is valid"),
        );
        manager.register(
            "edge_ngram",
            tantivy::tokenizer::NgramTokenizer::new(1, 8, true)
                .expect("static edge-ngram configuration is valid"),
        );
        manager
    }
}

impl SearchEngine for TantivyEngine {
    fn schema_fp(&self) -> Option<String> {
        self.inner
            .read()
            .expect("engine lock poisoned")
            .as_ref()
            .map(|c| c.schema_fp.clone())
    }

    fn schema(&self) -> Option<PitchforkSchema> {
        self.inner
            .read()
            .expect("engine lock poisoned")
            .as_ref()
            .map(|c| c.pitchfork_schema.clone())
    }

    fn search(&self, req: &SearchRequestV1) -> Result<SearchResponseV1> {
        self.with_searcher(|searcher, compiled| {
            if compiled.schema_fp != req.schema_fp {
                return Ok(SearchResponseV1::error(format!(
                    "schema fingerprint mismatch: server '{}', request '{}'",
                    compiled.schema_fp, req.schema_fp
                )));
            }

            // Geospatial filter (haystack within/dwithin): bbox range queries
            // with a haversine post-filter for dwithin.
            if let Some(spatial) = &req.spatial {
                return spatial_search(searcher, compiled, req, spatial);
            }

            let query = build_query(searcher, compiled, &req.ast)?;
            let (total, addresses, scores) = collect(
                searcher,
                &query,
                req.start,
                req.end,
                &req.sort,
                compiled,
            )?;

            let mut hits = Vec::with_capacity(addresses.len());
            for (score, addr) in scores.iter().zip(addresses.iter()) {
                let retrieved: TantivyDocument = searcher.doc(*addr)?;
                hits.push(to_hit(compiled, &retrieved, *score));
            }

            let facets = if req.facets.is_empty() {
                None
            } else {
                Some(aggregate_facets(searcher, compiled, &req.facets, &addresses)?)
            };

            let snippets = if req.highlight {
                build_snippets(searcher, compiled, &*query, &hits)?
            } else {
                BTreeMap::new()
            };

            for hit in &mut hits {
                if let Some(fragments) = snippets.get(&hit.id) {
                    hit.snippets
                        .insert(compiled.document_field_name().to_string(), fragments.clone());
                }
            }

            let spelling = if req.spellcheck {
                let query_text = req.spelling_query.as_deref().unwrap_or("");
                spell::spelling_suggestion(searcher, compiled, query_text)?
            } else {
                None
            };

            Ok(SearchResponseV1 {
                v: pitchfork_core::WIRE_VERSION,
                kind: if hits.is_empty() { 1 } else { 0 },
                total,
                hits,
                facets,
                spelling,
                error: None,
            })
        })
    }

    fn index(&self, req: &IndexRequestV1) -> Result<AckResponseV1> {
        if req.v != pitchfork_core::WIRE_VERSION {
            return Ok(AckResponseV1::error(format!(
                "unsupported wire version {} (expected {})",
                req.v,
                pitchfork_core::WIRE_VERSION
            )));
        }
        self.setup(&req.schema)?;

        let mut inner = self.compiled_mut();
        let compiled = inner.as_mut().expect("setup guaranteed a compiled index");

        let req_fp = req.schema_fp.clone().unwrap_or_else(|| compiled.schema_fp.clone());
        if compiled.schema_fp != req_fp {
            return Ok(AckResponseV1::error(format!(
                "schema fingerprint mismatch: server '{}', request '{}'",
                compiled.schema_fp, req_fp
            )));
        }

        for doc_map in &req.docs {
            let tdoc = build_document(compiled, doc_map).map_err(EngineError::Wire)?;
            compiled.writer.add_document(tdoc)?;
        }
        compiled.writer.commit()?;
        Ok(AckResponseV1::ok())
    }

    fn remove(&self, req: &RemoveRequestV1) -> Result<AckResponseV1> {
        let mut inner = self.compiled_mut();
        let compiled = inner.as_mut().expect("checked non-none");
        if compiled.schema_fp != req.schema_fp {
            return Ok(AckResponseV1::error(format!(
                "schema fingerprint mismatch: server '{}', request '{}'",
                compiled.schema_fp, req.schema_fp
            )));
        }
        let id_field = compiled.tantivy_schema.get_field("id")?;
        for id in &req.ids {
            compiled
                .writer
                .delete_term(Term::from_field_text(id_field, id));
        }
        compiled.writer.commit()?;
        Ok(AckResponseV1::ok())
    }

    fn clear(&self) -> Result<AckResponseV1> {
        let mut inner = self.compiled_mut();
        let compiled = inner.as_mut().expect("checked non-none");
        compiled.writer.delete_all_documents()?;
        compiled.writer.commit()?;
        Ok(AckResponseV1::ok())
    }

    fn more_like_this(&self, doc_id: &str, req: &SearchRequestV1) -> Result<SearchResponseV1> {
        // V1 MLT: pull the source document's text terms and OR them together,
        // excluding the source document itself. Phase 5 upgrades this.
        self.with_searcher(|searcher, compiled| {
            let id_field = compiled.tantivy_schema.get_field("id")?;
            let tq = TermQuery::new(Term::from_field_text(id_field, doc_id), IndexRecordOption::Basic);

            let source: Vec<(f32, DocAddress)> =
                searcher.search(&tq, &TopDocs::with_limit(1).order_by_score())?;
            let addr = source.first().map(|(_, a)| *a).ok_or_else(|| {
                EngineError::Query(format!("more_like_this: document '{doc_id}' not found"))
            })?;
            let source_doc: TantivyDocument = searcher.doc(addr)?;

            let text_field_name = compiled.document_field_name().to_string();
            let text_field = compiled.tantivy_schema.get_field(&text_field_name).map_err(|_| {
                EngineError::Schema(format!("missing document field '{text_field_name}'"))
            })?;

            let mut terms: Vec<String> = Vec::new();
            for value in source_doc.get_all(text_field) {
                let owned = OwnedValue::from(value);
                if let OwnedValue::Str(s) = owned {
                    for word in s.split_whitespace() {
                        let cleaned: String = word
                            .chars()
                            .filter(|c| c.is_alphanumeric())
                            .collect::<String>()
                            .to_lowercase();
                        if cleaned.len() >= 3 && !terms.contains(&cleaned) {
                            terms.push(cleaned);
                        }
                    }
                }
            }
            terms.truncate(25);

            if terms.is_empty() {
                return Ok(SearchResponseV1::empty());
            }

            let subqueries: Vec<(Occur, Box<dyn Query>)> = terms
                .iter()
                .map(|t| {
                    let q: Box<dyn Query> = Box::new(TermQuery::new(
                        Term::from_field_text(text_field, t),
                        IndexRecordOption::WithFreqs,
                    ));
                    (Occur::Should, q)
                })
                .collect();
            let mlt_query = BooleanQuery::new(subqueries);

            let limit = req
                .end
                .unwrap_or(10)
                .saturating_sub(req.start)
                .clamp(1, 100);
            let raw: Vec<(f32, DocAddress)> = searcher
                .search(
                    &mlt_query,
                    &TopDocs::with_limit(limit + 1)
                        .and_offset(req.start)
                        .order_by_score(),
                )?
                .into_iter()
                .filter(|(_, a)| *a != addr)
                .take(limit)
                .collect();

            let total = raw.len() as i64;
            let hits: Vec<Hit> = raw
                .into_iter()
                .map(|(score, a)| {
                    let d: TantivyDocument = searcher.doc(a).expect("doc address from search");
                    to_hit(compiled, &d, f64::from(score))
                })
                .collect();

            Ok(SearchResponseV1::hits(total, hits))
        })
    }
}

impl CompiledIndex {
    fn document_field_name(&self) -> &str {
        self.pitchfork_schema.document_field().unwrap_or("text")
    }
}

// ---------------------------------------------------------------------------
// Collection + sorting
// ---------------------------------------------------------------------------

/// Runs the query, returning (total, doc addresses, per-doc scores).
/// With a sort spec, the first field's fast value becomes the "score".
fn collect(
    searcher: &Searcher,
    query: &dyn Query,
    start: usize,
    end: Option<usize>,
    sort: &[String],
    compiled: &CompiledIndex,
) -> Result<(i64, Vec<DocAddress>, Vec<f64>)> {
    let total = searcher.search(query, &Count)? as i64;
    let limit = end.unwrap_or(usize::MAX / 2).saturating_sub(start);
    // Cap the window like whoosh pages; V1 caps at 10k.
    let limit = limit.min(10_000);

    if sort.is_empty() {
        let hits: Vec<(f32, DocAddress)> = searcher.search(
            query,
            &TopDocs::with_limit(limit.max(1))
                .and_offset(start)
                .order_by_score(),
        )?;
        let (scores, addresses): (Vec<f64>, Vec<DocAddress>) = hits.into_iter().map(|(s, a)| (f64::from(s), a)).unzip();
        return Ok((total, addresses, scores));
    }

    // Haystack sort_by: leading '-' means descending. V1 sorts by the first
    // field only (whoosh has the same all-or-nothing rule).
    let (field, order) = parse_sort(&sort[0])?;
    // Validate the sort field exists (the fast-field collector would panic
    // otherwise).
    compiled.tantivy_schema.get_field(field).map_err(|_| {
        EngineError::Query(format!("unknown sort field '{field}'"))
    })?;
    let ty = compiled
        .pitchfork_schema
        .get(field)
        .map(|d| d.field_type);

    macro_rules! sorted {
        ($fast:ty) => {{
            let hits: Vec<(Option<$fast>, DocAddress)> = searcher
                .search(
                    query,
                    &TopDocs::with_limit(limit.max(1))
                        .and_offset(start)
                        .order_by_fast_field::<$fast>(field.to_string(), order),
                )?;
            let scores = hits.iter().map(|(v, _)| fast_to_score(v)).collect();
            let addresses = hits.into_iter().map(|(_, a)| a).collect();
            Ok((total, addresses, scores))
        }};
    }

    match ty {
        Some(FieldType::Integer) => sorted!(i64),
        Some(FieldType::Float) => sorted!(f64),
        Some(FieldType::Boolean) => sorted!(u64),
        Some(FieldType::Date) | Some(FieldType::DateTime) => sorted!(TantivyDate),
        _ => {
            let hits: Vec<(Option<String>, DocAddress)> = searcher
                .search(
                    query,
                    &TopDocs::with_limit(limit.max(1))
                        .and_offset(start)
                        .order_by_string_fast_field(field.to_string(), order),
                )?;
            let scores = hits.iter().map(|_| 0.0).collect();
            let addresses = hits.into_iter().map(|(_, a)| a).collect();
            Ok((total, addresses, scores))
        }
    }
}

fn fast_to_score<T: FastScore>(value: &Option<T>) -> f64 {
    value.as_ref().map(FastScore::to_score).unwrap_or(0.0)
}

trait FastScore {
    fn to_score(&self) -> f64;
}

impl FastScore for i64 {
    fn to_score(&self) -> f64 {
        *self as f64
    }
}

impl FastScore for u64 {
    fn to_score(&self) -> f64 {
        *self as f64
    }
}

impl FastScore for f64 {
    fn to_score(&self) -> f64 {
        *self
    }
}

impl FastScore for TantivyDate {
    fn to_score(&self) -> f64 {
        self.into_timestamp_secs() as f64
    }
}

fn parse_sort(spec: &str) -> Result<(&str, Order)> {
    if let Some(field) = spec.strip_prefix('-') {
        Ok((field, Order::Desc))
    } else if let Some(field) = spec.strip_prefix('+') {
        Ok((field, Order::Asc))
    } else {
        Ok((spec, Order::Asc))
    }
}

// ---------------------------------------------------------------------------
// Query building
// ---------------------------------------------------------------------------

fn build_query(
    _searcher: &Searcher,
    compiled: &CompiledIndex,
    ast: &QueryNode,
) -> Result<Box<dyn Query>> {
    Ok(match ast {
        QueryNode::MatchAll => Box::new(tantivy::query::AllQuery),
        QueryNode::MatchNone => Box::new(EmptyQuery),
        QueryNode::And { children } => {
            let subs: Vec<(Occur, Box<dyn Query>)> = children
                .iter()
                .map(|c| build_query(_searcher, compiled, c).map(|q| (Occur::Must, q)))
                .collect::<Result<_>>()?;
            Box::new(BooleanQuery::from(subs))
        }
        QueryNode::Or { children } => {
            let subs: Vec<(Occur, Box<dyn Query>)> = children
                .iter()
                .map(|c| build_query(_searcher, compiled, c).map(|q| (Occur::Should, q)))
                .collect::<Result<_>>()?;
            Box::new(BooleanQuery::from(subs))
        }
        QueryNode::Not { child } => {
            let inner = build_query(_searcher, compiled, child)?;
            Box::new(BooleanQuery::new(vec![
                (Occur::Must, Box::new(tantivy::query::AllQuery) as Box<dyn Query>),
                (Occur::MustNot, inner),
            ]))
        }
        QueryNode::Filter { field, op, value } => build_filter(compiled, field, *op, value)?,
    })
}

fn field_lookup(compiled: &CompiledIndex, name: &str) -> Result<tantivy::schema::Field> {
    compiled.tantivy_schema.get_field(name).map_err(|_| {
        EngineError::Query(format!("unknown field '{name}' in index schema"))
    })
}

fn build_filter(
    compiled: &CompiledIndex,
    field: &str,
    op: FilterOp,
    value: &WireValue,
) -> Result<Box<dyn Query>> {
    let tfield = field_lookup(compiled, field)?;
    let def: Option<&FieldDef> = compiled.pitchfork_schema.get(field);
    let ty = def.map(|d| d.field_type);

    let basic = IndexRecordOption::Basic;
    let with_freqs = IndexRecordOption::WithFreqs;

    let range = |lo: Option<Term>, hi: Option<Term>, lo_inc: bool, hi_inc: bool| -> Box<dyn Query> {
        let lower = match lo {
            Some(t) => {
                if lo_inc {
                    Bound::Included(t)
                } else {
                    Bound::Excluded(t)
                }
            }
            None => Bound::Unbounded,
        };
        let upper = match hi {
            Some(t) => {
                if hi_inc {
                    Bound::Included(t)
                } else {
                    Bound::Excluded(t)
                }
            }
            None => Bound::Unbounded,
        };
        Box::new(RangeQuery::new(lower, upper))
    };

    Ok(match op {
        FilterOp::Exact | FilterOp::Contains => match ty {
            Some(FieldType::Boolean) => {
                let b = value_truthy(value);
                Box::new(TermQuery::new(Term::from_field_bool(tfield, b), basic))
            }
            Some(FieldType::Integer) => {
                let n = wire_to_i64(value)?;
                Box::new(TermQuery::new(Term::from_field_i64(tfield, n), basic))
            }
            Some(FieldType::Float) => {
                let n = wire_to_f64(value)?;
                Box::new(TermQuery::new(Term::from_field_f64(tfield, n), basic))
            }
            Some(FieldType::Date) => {
                // Date exact: match the whole calendar day.
                let (lo, hi) = day_terms(tfield, wire_to_date(value)?);
                range(Some(lo), Some(hi), true, false)
            }
            Some(FieldType::DateTime) => {
                let d = wire_to_date(value)?;
                Box::new(TermQuery::new(
                    Term::from_field_date_for_search(tfield, d),
                    basic,
                ))
            }
            _ => {
                // The document field is analyzed: match the default tokenizer
                // (strip punctuation, lowercase, one term per word).
                let raw = wire_to_str(value)?;
                if field == compiled.document_field_name() {
                    let subs: Vec<(Occur, Box<dyn Query>)> = raw
                        .split_whitespace()
                        .filter_map(|w| {
                            let cleaned: String = w
                                .chars()
                                .filter(|c| c.is_alphanumeric())
                                .collect::<String>()
                                .to_lowercase();
                            if cleaned.is_empty() {
                                None
                            } else {
                                let q: Box<dyn Query> = Box::new(TermQuery::new(
                                    Term::from_field_text(tfield, &cleaned),
                                    with_freqs,
                                ));
                                Some((Occur::Should, q))
                            }
                        })
                        .collect();
                    if subs.is_empty() {
                        Box::new(EmptyQuery)
                    } else {
                        Box::new(BooleanQuery::from(subs))
                    }
                } else {
                    Box::new(TermQuery::new(
                        Term::from_field_text(tfield, &raw),
                        with_freqs,
                    ))
                }
            }
        },
        FilterOp::StartsWith => {
            let escaped = regex_escape(&wire_to_str(value)?);
            Box::new(
                RegexQuery::from_pattern(&format!("{escaped}.*"), tfield)
                    .map_err(|e| EngineError::Query(format!("startswith regex: {e}")))?,
            )
        }
        FilterOp::EndsWith => {
            let escaped = regex_escape(&wire_to_str(value)?);
            Box::new(
                RegexQuery::from_pattern(&format!(".*{escaped}"), tfield)
                    .map_err(|e| EngineError::Query(format!("endswith regex: {e}")))?,
            )
        }
        FilterOp::Gt => range(Some(type_term(ty, tfield, value)?), None, false, false),
        FilterOp::Gte => range(Some(type_term(ty, tfield, value)?), None, true, false),
        FilterOp::Lt => range(None, Some(type_term(ty, tfield, value)?), false, false),
        FilterOp::Lte => range(None, Some(type_term(ty, tfield, value)?), false, true),
        FilterOp::In => {
            let items: Vec<WireValue> = match value {
                WireValue::List(items) => items.clone(),
                other => vec![other.clone()],
            };
            if matches!(ty, Some(FieldType::Date)) {
                // Dates match the whole calendar day: union of day ranges.
                let subs: Vec<(Occur, Box<dyn Query>)> = items
                    .iter()
                    .map(|item| {
                        let (lo, hi) = day_terms(tfield, wire_to_date(item)?);
                        let q: Box<dyn Query> = Box::new(RangeQuery::new(
                            Bound::Included(lo),
                            Bound::Excluded(hi),
                        ));
                        Ok((Occur::Should, q))
                    })
                    .collect::<Result<_>>()?;
                Box::new(BooleanQuery::from(subs))
            } else {
                let terms: Vec<Term> = items
                    .iter()
                    .map(|item| type_term(ty, tfield, item))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Box::new(TermSetQuery::new(terms))
            }
        }
        FilterOp::Range => {
            let items: Vec<WireValue> = match value {
                WireValue::List(items) if items.len() == 2 => items.clone(),
                _ => {
                    return Err(EngineError::Query(
                        "range filter requires a two-element list".to_string(),
                    ))
                }
            };
            // Inclusive on both ends, like haystack's whoosh/solr backends.
            range(
                Some(type_term(ty, tfield, &items[0])?),
                Some(type_term(ty, tfield, &items[1])?),
                true,
                true,
            )
        }
        FilterOp::Fuzzy => {
            let term = wire_to_str(value)?;
            Box::new(FuzzyTermQuery::new(
                Term::from_field_text(tfield, &term),
                2,
                true,
            ))
        }
        FilterOp::Content => {
            // Haystack's bare-field lookups encode as `content`. On the
            // document (analyzed) field that means full-text; on raw-indexed
            // char/identifier fields it is an exact whole-value match.
            let is_document = field == compiled.document_field_name();
            let tfield = field_lookup(compiled, field)?;
            let words = wire_to_str(value)?;

            if !is_document {
                return Ok(Box::new(TermQuery::new(
                    Term::from_field_text(tfield, &words),
                    basic,
                )));
            }

            let subs: Vec<(Occur, Box<dyn Query>)> = words
                .split_whitespace()
                .filter_map(|w| {
                    // Match the default tokenizer: strip punctuation, lowercase.
                    let cleaned: String = w
                        .chars()
                        .filter(|c| c.is_alphanumeric())
                        .collect::<String>()
                        .to_lowercase();
                    if cleaned.is_empty() {
                        None
                    } else {
                        let q: Box<dyn Query> = Box::new(TermQuery::new(
                            Term::from_field_text(tfield, &cleaned),
                            with_freqs,
                        ));
                        Some((Occur::Should, q))
                    }
                })
                .collect();
            if subs.is_empty() {
                Box::new(tantivy::query::AllQuery)
            } else {
                Box::new(BooleanQuery::from(subs))
            }
        }
    })
}

fn type_term(ty: Option<FieldType>, tfield: tantivy::schema::Field, value: &WireValue) -> Result<Term> {
    Ok(match ty {
        Some(FieldType::Integer) => Term::from_field_i64(tfield, wire_to_i64(value)?),
        Some(FieldType::Float) => Term::from_field_f64(tfield, wire_to_f64(value)?),
        Some(FieldType::Date) | Some(FieldType::DateTime) => {
            Term::from_field_date_for_search(tfield, wire_to_date(value)?)
        }
        Some(FieldType::Boolean) => Term::from_field_bool(tfield, value_truthy(value)),
        _ => Term::from_field_text(tfield, &wire_to_str(value)?),
    })
}

fn value_truthy(value: &WireValue) -> bool {
    match value {
        WireValue::Bool(b) => *b,
        WireValue::Int(n) => *n != 0,
        WireValue::Str(s) => s.eq_ignore_ascii_case("true") || s == "1",
        _ => false,
    }
}

fn wire_to_str(value: &WireValue) -> Result<String> {
    Ok(match value {
        WireValue::Str(s) => s.clone(),
        WireValue::Int(n) => n.to_string(),
        WireValue::Float(n) => n.to_string(),
        WireValue::Bool(b) => b.to_string(),
        other => {
            return Err(EngineError::Query(format!(
                "expected a scalar value, got {other:?}"
            )))
        }
    })
}

fn wire_to_i64(value: &WireValue) -> Result<i64> {
    Ok(match value {
        WireValue::Int(n) => *n,
        WireValue::Float(n) => *n as i64,
        WireValue::Str(s) => {
            s.parse()
                .map_err(|_| EngineError::Query(format!("cannot parse '{s}' as an integer")))?
        }
        other => {
            return Err(EngineError::Query(format!(
                "expected a numeric value, got {other:?}"
            )))
        }
    })
}

fn wire_to_f64(value: &WireValue) -> Result<f64> {
    Ok(match value {
        WireValue::Int(n) => *n as f64,
        WireValue::Float(n) => *n,
        WireValue::Str(s) => {
            s.parse()
                .map_err(|_| EngineError::Query(format!("cannot parse '{s}' as a float")))?
        }
        other => {
            return Err(EngineError::Query(format!(
                "expected a numeric value, got {other:?}"
            )))
        }
    })
}

fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

/// Parse ISO-8601 date / datetime strings the way haystack's
/// DATE_REGEX / DATETIME_REGEX accept them: `YYYY-MM-DD`,
/// `YYYY-MM-DDTHH:MM:SS[.f][Z]`, and the space-separated variant.
/// Returns (unix seconds, has_time_component).
pub fn parse_iso8601(s: &str) -> Option<(i64, bool)> {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    let year: i32 = s.get(0..4)?.parse().ok()?;
    if bytes.get(4) != Some(&b'-') {
        return None;
    }
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let days = days_from_civil(year, month, day);

    let rest = s.get(10..).unwrap_or("").trim_start_matches(['T', 't', ' ']);
    if rest.is_empty() || rest == "Z" {
        return Some((days * 86400, false));
    }

    // HH:MM:SS[.ffffff][Z][+HH:MM]
    let hour: i64 = rest.get(0..2)?.parse().ok()?;
    if !(0..=23).contains(&hour) || rest.as_bytes().get(2) != Some(&b':') {
        return None;
    }
    let minute: i64 = rest.get(3..5)?.parse().ok()?;
    let second: i64 = rest.get(6..8)?.parse().ok()?;
    if !(0..=59).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }
    let mut secs = days * 86400 + hour * 3600 + minute * 60 + second;

    // Optional fractional seconds and timezone offset.
    let tail = &rest[8..];
    let tail = tail.trim_start_matches('.');
    let offset_start = tail
        .find(['Z', '+', '-'])
        .unwrap_or(tail.len());
    let tail = &tail[offset_start..];
    if let Some(stripped) = tail.strip_prefix('Z') {
        let _ = stripped;
    } else if tail.starts_with('+') || tail.starts_with('-') {
        let sign: i64 = if tail.starts_with('-') { -1 } else { 1 };
        let digits: String = tail[1..].chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() >= 4 {
            let oh: i64 = digits[0..2].parse().ok()?;
            let om: i64 = digits[2..4].parse().ok()?;
            secs -= sign * (oh * 3600 + om * 60);
        }
    }

    Some((secs, true))
}

fn wire_to_date(value: &WireValue) -> Result<TantivyDate> {
    let s = wire_to_str(value)?;
    let (secs, _) = parse_iso8601(&s).ok_or_else(|| {
        EngineError::Query(format!("cannot parse '{s}' as an ISO-8601 date/datetime"))
    })?;
    Ok(TantivyDate::from_timestamp_secs(secs))
}

fn day_terms(tfield: tantivy::schema::Field, d: TantivyDate) -> (Term, Term) {
    let secs = d.into_timestamp_secs();
    let day_start = secs - secs.rem_euclid(86400);
    let lo = Term::from_field_date_for_search(tfield, TantivyDate::from_timestamp_secs(day_start));
    let hi =
        Term::from_field_date_for_search(tfield, TantivyDate::from_timestamp_secs(day_start + 86400));
    (lo, hi)
}

/// Days since 1970-01-01 (Howard Hinnant's days_from_civil).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Inverse of days_from_civil (Howard Hinnant's civil_from_days).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

// ---------------------------------------------------------------------------
// Schema compilation
// ---------------------------------------------------------------------------

fn build_tantivy_schema(schema: &PitchforkSchema) -> Result<Schema> {
    let mut builder = SchemaBuilder::new();

    // Reserved haystack identifier fields: exact-match, stored.
    for reserved in RESERVED_FIELDS {
        let options = TextOptions::default().set_stored().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("raw")
                .set_index_option(IndexRecordOption::Basic),
        );
        builder.add_text_field(reserved, options);
    }

    let mut location_fields: Vec<String> = Vec::new();

    for def in &schema.fields {
        match def.field_type {
            FieldType::Text => {
                let options = TextOptions::default().set_stored().set_indexing_options(
                    TextFieldIndexing::default().set_index_option(IndexRecordOption::WithFreqs),
                );
                builder.add_text_field(&def.name, options);
            }
            FieldType::Char => {
                let options = TextOptions::default().set_stored().set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer("raw")
                        .set_index_option(IndexRecordOption::Basic),
                );
                builder.add_text_field(&def.name, options);
            }
            FieldType::Ngram => {
                let options = TextOptions::default().set_stored().set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer("ngram")
                        .set_index_option(IndexRecordOption::WithFreqs),
                );
                builder.add_text_field(&def.name, options);
            }
            FieldType::EdgeNgram => {
                let options = TextOptions::default().set_stored().set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer("edge_ngram")
                        .set_index_option(IndexRecordOption::WithFreqs),
                );
                builder.add_text_field(&def.name, options);
            }
            FieldType::Integer => {
                let options = NumericOptions::default()
                    .set_stored()
                    .set_indexed()
                    .set_fast();
                builder.add_i64_field(&def.name, options);
            }
            FieldType::Boolean => {
                let options = NumericOptions::default()
                    .set_stored()
                    .set_indexed()
                    .set_fast();
                builder.add_u64_field(&def.name, options);
            }
            FieldType::Float => {
                let options = NumericOptions::default()
                    .set_stored()
                    .set_indexed()
                    .set_fast();
                builder.add_f64_field(&def.name, options);
            }
            FieldType::Date | FieldType::DateTime => {
                let options = DateOptions::default()
                    .set_stored()
                    .set_indexed()
                    .set_fast();
                builder.add_date_field(&def.name, options);
            }
            FieldType::Location => {
                // Stored/indexed as a raw "lat,lng" string; spatial queries
                // use hidden numeric fast fields (see below).
                let options = TextOptions::default().set_stored().set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer("raw")
                        .set_index_option(IndexRecordOption::Basic),
                );
                builder.add_text_field(&def.name, options);
                location_fields.push(def.name.clone());
            }
        }
    }

    // Hidden numeric fast fields for location spatial queries:
    // `{field}__pitchfork_lat` / `{field}__pitchfork_lng`.
    let location_options = NumericOptions::default().set_indexed().set_fast();
    for name in &location_fields {
        builder.add_f64_field(&format!("{name}{LAT_FIELD_SUFFIX}"), location_options.clone());
        builder.add_f64_field(&format!("{name}{LNG_FIELD_SUFFIX}"), location_options.clone());
    }
    Ok(builder.build())
}

pub(crate) const LAT_FIELD_SUFFIX: &str = "__pitchfork_lat";
pub(crate) const LNG_FIELD_SUFFIX: &str = "__pitchfork_lng";

// ---------------------------------------------------------------------------
// Documents
// ---------------------------------------------------------------------------

fn build_document(
    compiled: &CompiledIndex,
    doc_map: &BTreeMap<String, WireValue>,
) -> std::result::Result<TantivyDocument, String> {
    let schema = &compiled.tantivy_schema;
    let mut document = TantivyDocument::new();

    for (name, value) in doc_map {
        let field = schema.get_field(name).map_err(|_| {
            format!("document field '{name}' is not in the registered schema")
        })?;
        let field_type = compiled.pitchfork_schema.get(name).map(|d| d.field_type);
        match value {
            WireValue::Null => continue,
            WireValue::List(items) => {
                for item in items {
                    add_scalar(&mut document, field, item, name, field_type)?;
                }
            }
            scalar => add_scalar(&mut document, field, scalar, name, field_type)?,
        }
        // Location values also fill the hidden numeric fast fields used by
        // spatial queries.
        if field_type == Some(FieldType::Location) {
            if let Some((lat, lng)) = parse_point(value) {
                if let (Ok(lat_field), Ok(lng_field)) = (
                    schema.get_field(&format!("{name}{LAT_FIELD_SUFFIX}")),
                    schema.get_field(&format!("{name}{LNG_FIELD_SUFFIX}")),
                ) {
                    document.add_field_value(lat_field, &lat);
                    document.add_field_value(lng_field, &lng);
                }
            }
        }
    }

    for reserved in RESERVED_FIELDS {
        if !doc_map.contains_key(reserved) {
            return Err(format!("document is missing required field '{reserved}'"));
        }
    }

    Ok(document)
}

fn add_scalar(
    document: &mut TantivyDocument,
    field: tantivy::schema::Field,
    value: &WireValue,
    name: &str,
    field_type: Option<FieldType>,
) -> std::result::Result<(), String> {
    let owned = match (field_type, value) {
        // Type-aware coercion: dates travel as ISO-8601 strings.
        (Some(FieldType::Date) | Some(FieldType::DateTime), WireValue::Str(s)) => {
            let (secs, _) = parse_iso8601(s).ok_or_else(|| {
                format!("cannot parse '{s}' as an ISO-8601 date/datetime for field '{name}'")
            })?;
            OwnedValue::Date(TantivyDate::from_timestamp_secs(secs))
        }
        (Some(FieldType::Boolean), other) => OwnedValue::U64(u64::from(value_truthy(other))),
        // Location values stay strings; spatial queries are phase 5.
        (Some(FieldType::Location), WireValue::Str(s)) => OwnedValue::Str(s.clone()),
        (_, WireValue::Null) => return Ok(()),
        (_, WireValue::Str(s)) => OwnedValue::Str(s.clone()),
        (_, WireValue::Int(n)) => OwnedValue::I64(*n),
        (_, WireValue::Float(n)) => OwnedValue::F64(*n),
        (_, WireValue::Bool(b)) => OwnedValue::U64(u64::from(*b)),
        (_, WireValue::List(_)) => {
            return Err(format!(
                "field '{name}' received a nested list; flatten the value first"
            ))
        }
    };
    document.add_field_value(field, &owned);
    Ok(())
}

fn to_hit(compiled: &CompiledIndex, retrieved: &TantivyDocument, score: f64) -> Hit {
    let schema = &compiled.tantivy_schema;
    let named = retrieved.to_named_doc(schema);
    let NamedFieldDocument(map) = &named;

    let take_first = |values: &Vec<OwnedValue>| values.first().cloned().unwrap_or(OwnedValue::Null);

    let mut id = String::new();
    let mut django_ct = String::new();
    let mut django_id = String::new();
    let mut stored: BTreeMap<String, WireValue> = BTreeMap::new();

    for (name, values) in map {
        let owned = take_first(values);
        let def = compiled.pitchfork_schema.get(name);
        let field_type = def.map(|d| d.field_type);
        let wire = if def.map(|d| d.multi).unwrap_or(false) {
            // Multi-valued fields cross the wire as lists (haystack's
            // MultiValueField.convert expects an iterable).
            WireValue::List(values.iter().map(owned_to_wire_untyped).collect())
        } else {
            owned_to_wire_typed(&owned, field_type)
        };
        match name.as_str() {
            "id" => {
                if let OwnedValue::Str(s) = owned {
                    id = s;
                }
            }
            "django_ct" => {
                if let OwnedValue::Str(s) = owned {
                    django_ct = s;
                }
            }
            "django_id" => {
                if let OwnedValue::Str(s) = owned {
                    django_id = s;
                }
            }
            _ => {
                stored.insert(name.clone(), wire);
            }
        }
    }

    Hit {
        id,
        django_ct,
        django_id,
        score,
        stored,
        snippets: BTreeMap::new(),
    }
}

fn owned_to_wire(value: &OwnedValue) -> WireValue {
    owned_to_wire_typed(value, None)
}

/// Schema-aware conversion: boolean fields are u64 in tantivy but must cross
/// the wire as booleans (haystack's BooleanField.convert expects a bool).
fn owned_to_wire_typed(value: &OwnedValue, field_type: Option<FieldType>) -> WireValue {
    match (field_type, value) {
        (Some(FieldType::Boolean), OwnedValue::U64(n)) => WireValue::Bool(*n != 0),
        (Some(FieldType::Boolean), OwnedValue::I64(n)) => WireValue::Bool(*n != 0),
        (_, value) => owned_to_wire_untyped(value),
    }
}

fn owned_to_wire_untyped(value: &OwnedValue) -> WireValue {
    match value {
        OwnedValue::Null => WireValue::Null,
        OwnedValue::Str(s) => WireValue::Str(s.clone()),
        OwnedValue::U64(n) => WireValue::Int(*n as i64),
        OwnedValue::I64(n) => WireValue::Int(*n),
        OwnedValue::F64(n) => WireValue::Float(*n),
        OwnedValue::Bool(b) => WireValue::Bool(*b),
        OwnedValue::Date(d) => WireValue::Str(format_date(*d)),
        OwnedValue::Array(items) => WireValue::List(items.iter().map(owned_to_wire).collect()),
        _ => WireValue::Null,
    }
}

fn format_date(d: TantivyDate) -> String {
    // Always emit the full datetime format: haystack's DateTimeField.convert
    // requires a datetime string, and its DateField.convert accepts the
    // zeroed-time variant (see haystack DATE_REGEX / DATETIME_REGEX).
    let secs = d.into_timestamp_secs();
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, da) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{da:02}T{h:02}:{m:02}:{s:02}Z")
}

// ---------------------------------------------------------------------------
// Spatial (phase 5): haystack within = bounding box, dwithin = radius
// ---------------------------------------------------------------------------

/// Parse a "lat,lng" wire point into (lat, lng).
fn parse_point(value: &WireValue) -> Option<(f64, f64)> {
    let s = match value {
        WireValue::Str(s) => s,
        _ => return None,
    };
    let (lat, lng) = s.split_once(',')?;
    let lat: f64 = lat.trim().parse().ok()?;
    let lng: f64 = lng.trim().parse().ok()?;
    Some((lat, lng))
}

/// Haversine distance in kilometers.
fn haversine_km(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    const EARTH_RADIUS_KM: f64 = 6371.0;
    let (lat1, lng1, lat2, lng2) = (
        lat1.to_radians(),
        lng1.to_radians(),
        lat2.to_radians(),
        lng2.to_radians(),
    );
    let dlat = lat2 - lat1;
    let dlng = lng2 - lng1;
    let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlng / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * a.sqrt().asin()
}

/// Run a spatial search (within = bbox range queries; dwithin = bbox
/// prefilter + haversine post-filter). Returns the full response.
fn spatial_search(
    searcher: &Searcher,
    compiled: &CompiledIndex,
    req: &SearchRequestV1,
    spatial: &pitchfork_core::wire::SpatialFilterV1,
) -> Result<SearchResponseV1> {
    use pitchfork_core::wire::SpatialFilterV1;
    use std::ops::Bound as StdBound;

    let (field, p1, p2, radius_km, is_dwithin) = match spatial {
        SpatialFilterV1::Within {
            field,
            point_1,
            point_2,
        } => (field, point_1, point_2, None, false),
        SpatialFilterV1::DWithin {
            field,
            point,
            radius_km,
        } => (field, point, point, Some(*radius_km), true),
    };

    let (lat1, lng1) = parse_point(&WireValue::Str(p1.clone())).ok_or_else(|| {
        EngineError::Query(format!("cannot parse spatial point '{p1}' as 'lat,lng'"))
    })?;
    let (lat2, lng2) = parse_point(&WireValue::Str(p2.clone())).ok_or_else(|| {
        EngineError::Query(format!("cannot parse spatial point '{p2}' as 'lat,lng'"))
    })?;

    // The field must be a registered location field with hidden fast fields.
    let def = compiled
        .pitchfork_schema
        .get(field)
        .ok_or_else(|| EngineError::Query(format!("unknown spatial field '{field}'")))?;
    if def.field_type != FieldType::Location {
        return Ok(SearchResponseV1::error(format!(
            "spatial filter field '{field}' is not a location field"
        )));
    }

    let lat_field = compiled
        .tantivy_schema
        .get_field(&format!("{field}{LAT_FIELD_SUFFIX}"))
        .map_err(|_| {
            EngineError::Query(format!("missing hidden lat field for '{field}'"))
        })?;
    let lng_field = compiled
        .tantivy_schema
        .get_field(&format!("{field}{LNG_FIELD_SUFFIX}"))
        .map_err(|_| {
            EngineError::Query(format!("missing hidden lng field for '{field}'"))
        })?;

    let (min_lat, max_lat) = if is_dwithin {
        let radius_deg = radius_km.expect("dwithin has a radius") / 111.32;
        (lat1 - radius_deg, lat1 + radius_deg)
    } else {
        (lat1.min(lat2), lat1.max(lat2))
    };
    let (min_lng, max_lng) = if is_dwithin {
        let radius_deg =
            radius_km.expect("dwithin has a radius") / (111.32 * lat1.to_radians().cos().abs().max(0.01));
        (lng1 - radius_deg, lng1 + radius_deg)
    } else {
        (lng1.min(lng2), lng1.max(lng2))
    };

    let lat_range = RangeQuery::new(
        StdBound::Included(Term::from_field_f64(lat_field, min_lat)),
        StdBound::Included(Term::from_field_f64(lat_field, max_lat)),
    );
    let lng_range = RangeQuery::new(
        StdBound::Included(Term::from_field_f64(lng_field, min_lng)),
        StdBound::Included(Term::from_field_f64(lng_field, max_lng)),
    );
    let bbox = BooleanQuery::new(vec![
        (Occur::Must, Box::new(lat_range) as Box<dyn Query>),
        (Occur::Must, Box::new(lng_range) as Box<dyn Query>),
    ]);

    // Scan the bbox matches (V1 caps at 10k like the non-spatial path).
    let raw: Vec<(f64, DocAddress)> = searcher
        .search(&bbox, &TopDocs::with_limit(10_000).order_by_score())?
        .into_iter()
        .map(|(s, a)| (s as f64, a))
        .collect();

    let mut matched: Vec<(f64, DocAddress)> = Vec::new();
    for (score, addr) in raw {
        if is_dwithin {
            let doc: TantivyDocument = searcher.doc(addr)?;
            let loc_field = compiled
                .tantivy_schema
                .get_field(field)
                .map_err(|_| EngineError::Query(format!("unknown field '{field}'")))?;
            let point = doc
                .get_all(loc_field)
                .find_map(|v| match OwnedValue::from(v) {
                    OwnedValue::Str(s) => Some(s),
                    _ => None,
                })
                .and_then(|s| parse_point(&WireValue::Str(s)));
            match point {
                Some((lat, lng)) => {
                    let d = haversine_km(lat1, lng1, lat, lng);
                    if d <= radius_km.expect("dwithin has a radius") {
                        matched.push((score, addr));
                    }
                }
                None => continue,
            }
        } else {
            matched.push((score, addr));
        }
    }

    let total = matched.len() as i64;
    let start = req.start;
    let end = req.end.unwrap_or(matched.len());
    let window: Vec<(f64, DocAddress)> = matched
        .into_iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();

    let id_field = compiled.tantivy_schema.get_field("id")?;
    let mut hits = Vec::with_capacity(window.len());
    for (score, addr) in window {
        let retrieved: TantivyDocument = searcher.doc(addr)?;
        hits.push(to_hit(compiled, &retrieved, score));
    }
    let _ = id_field;

    Ok(SearchResponseV1 {
        v: pitchfork_core::WIRE_VERSION,
        kind: if hits.is_empty() { 1 } else { 0 },
        total,
        hits,
        facets: None,
        spelling: None,
        error: None,
    })
}

// ---------------------------------------------------------------------------
// Facets + snippets
// ---------------------------------------------------------------------------

fn aggregate_facets(
    searcher: &Searcher,
    compiled: &CompiledIndex,
    facets: &[String],
    addresses: &[DocAddress],
) -> Result<FacetCounts> {
    let mut counts: FacetCounts = FacetCounts::new();

    for facet_field in facets {
        let tfield = field_lookup(compiled, facet_field)?;
        let mut per_value: HashMap<String, (WireValue, i64)> = HashMap::new();

        for addr in addresses {
            let doc: TantivyDocument = searcher.doc(*addr)?;
            let mut seen: HashSet<String> = HashSet::new();
            for value in doc.get_all(tfield) {
                let wire = owned_to_wire(&OwnedValue::from(value));
                let key = format!("{wire:?}");
                if seen.insert(key.clone()) {
                    per_value
                        .entry(key)
                        .and_modify(|(_, count)| *count += 1)
                        .or_insert((wire, 1));
                }
            }
        }

        let mut list: Vec<(WireValue, i64)> = per_value.into_values().collect();
        // Sort by descending count then ascending value, like whoosh's
        // Count maptype ordering in haystack's _process_results.
        list.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| format!("{:?}", a.0).cmp(&format!("{:?}", b.0)))
        });
        counts.insert(facet_field.clone(), list);
    }
    Ok(counts)
}

fn build_snippets(
    searcher: &Searcher,
    compiled: &CompiledIndex,
    query: &dyn Query,
    hits: &[Hit],
) -> Result<BTreeMap<String, Vec<String>>> {
    let field_name = compiled.document_field_name().to_string();
    let field = field_lookup(compiled, &field_name)?;
    let id_field = field_lookup(compiled, "id")?;

    let mut snippets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Ok(generator) = tantivy::snippet::SnippetGenerator::create(searcher, query, field) {
        for hit in hits {
            let tq = TermQuery::new(Term::from_field_text(id_field, &hit.id), IndexRecordOption::Basic);
            if let Ok(found) = searcher.search(&tq, &TopDocs::with_limit(1).order_by_score()) {
                if let Some((_, addr)) = found.first() {
                    if let Ok(doc) = searcher.doc::<TantivyDocument>(*addr) {
                        let snippet = generator.snippet_from_doc(&doc).to_html();
                        snippets.entry(field_name.clone()).or_default().push(snippet);
                    }
                }
            }
        }
    }
    Ok(snippets)
}

use tantivy::schema::NamedFieldDocument;

#[cfg(test)]
mod tests {
    use super::*;
    use pitchfork_core::schema::FieldDef;
    use tempfile::TempDir;

    fn fixture_schema() -> PitchforkSchema {
        PitchforkSchema::new(vec![
            FieldDef {
                document: true,
                ..FieldDef::new("text", FieldType::Text)
            },
            FieldDef::new("name", FieldType::Char),
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

    fn doc(
        id: &str,
        name: &str,
        text: &str,
        pub_date: &str,
        views: i64,
    ) -> BTreeMap<String, WireValue> {
        let mut m = BTreeMap::new();
        m.insert("id".into(), WireValue::Str(id.into()));
        m.insert("django_ct".into(), WireValue::Str("core.mockmodel".into()));
        m.insert(
            "django_id".into(),
            WireValue::Str(id.rsplit('.').next().unwrap().into()),
        );
        m.insert("name".into(), WireValue::Str(name.into()));
        m.insert("text".into(), WireValue::Str(text.into()));
        m.insert("pub_date".into(), WireValue::Str(pub_date.into()));
        m.insert("views".into(), WireValue::Int(views));
        m.insert(
            "tags".into(),
            WireValue::List(vec![WireValue::Str("a".into()), WireValue::Str("b".into())]),
        );
        m.insert("active".into(), WireValue::Bool(true));
        m.insert("rating".into(), WireValue::Float(3.5));
        m
    }

    struct Fixture {
        engine: TantivyEngine,
        schema: PitchforkSchema,
        #[allow(dead_code)]
        dir: TempDir,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().unwrap();
        let engine = TantivyEngine::new(dir.path()).unwrap();
        let schema = fixture_schema();
        engine.setup(&schema).unwrap();
        engine
            .index(&IndexRequestV1 {
                v: pitchfork_core::WIRE_VERSION,
                schema_fp: Some(schema.fingerprint()),
                schema: schema.clone(),
                docs: vec![
                    doc("core.mockmodel.1", "daniel", "Indexed! one two three", "2020-01-01T00:00:00", 10),
                    doc("core.mockmodel.2", "david", "Indexed! four five six", "2021-06-15T12:30:00", 20),
                    doc("core.mockmodel.3", "clay", "seven eight nine", "2022-12-31T23:59:59", 30),
                ],
            })
            .unwrap();
        Fixture { engine, schema, dir }
    }

    fn search_req(fp: &str, ast: QueryNode) -> SearchRequestV1 {
        SearchRequestV1 {
            v: pitchfork_core::WIRE_VERSION,
            schema_fp: fp.to_string(),
            ast,
            start: 0,
            end: Some(10),
            sort: vec![],
            facets: vec![],
            highlight: false,
            spellcheck: false,
            mlt_doc_id: None,
            spelling_query: None,
            spatial: None,
        }
    }

    #[test]
    fn match_all_returns_total() {
        let f = fixture();
        let res = f
            .engine
            .search(&search_req(&f.schema.fingerprint(), QueryNode::MatchAll))
            .unwrap();
        assert_eq!(res.total, 3);
        assert_eq!(res.hits.len(), 3);
        assert!(res.hits.iter().all(|h| h.django_ct == "core.mockmodel"));
    }

    #[test]
    fn fingerprint_mismatch_is_hard_error() {
        let f = fixture();
        let res = f
            .engine
            .search(&search_req("deadbeefdeadbeef", QueryNode::MatchAll))
            .unwrap();
        assert_eq!(res.kind, 2);
        assert!(res.error.unwrap().contains("mismatch"));
    }

    #[test]
    fn exact_and_contains_filters() {
        let f = fixture();
        let fp = f.schema.fingerprint();

        let exact = QueryNode::filter("name", FilterOp::Exact, WireValue::Str("daniel".into()));
        assert_eq!(f.engine.search(&search_req(&fp, exact)).unwrap().total, 1);

        let contains = QueryNode::filter("text", FilterOp::Contains, WireValue::Str("four".into()));
        assert_eq!(f.engine.search(&search_req(&fp, contains)).unwrap().total, 1);

        let content = QueryNode::filter("text", FilterOp::Content, WireValue::Str("indexed!".into()));
        assert_eq!(f.engine.search(&search_req(&fp, content)).unwrap().total, 2);
    }

    #[test]
    fn numeric_range_and_comparison() {
        let f = fixture();
        let fp = f.schema.fingerprint();

        let gt = QueryNode::filter("views", FilterOp::Gt, WireValue::Int(10));
        assert_eq!(f.engine.search(&search_req(&fp, gt)).unwrap().total, 2);

        let lte = QueryNode::filter("views", FilterOp::Lte, WireValue::Int(20));
        assert_eq!(f.engine.search(&search_req(&fp, lte)).unwrap().total, 2);

        let range = QueryNode::filter(
            "views",
            FilterOp::Range,
            WireValue::List(vec![WireValue::Int(10), WireValue::Int(20)]),
        );
        assert_eq!(f.engine.search(&search_req(&fp, range)).unwrap().total, 2);

        let float = QueryNode::filter("rating", FilterOp::Exact, WireValue::Float(3.5));
        assert_eq!(f.engine.search(&search_req(&fp, float)).unwrap().total, 3);
    }

    #[test]
    fn date_filters() {
        let f = fixture();
        let fp = f.schema.fingerprint();

        let gt = QueryNode::filter("pub_date", FilterOp::Gt, WireValue::Str("2021-01-01".into()));
        assert_eq!(f.engine.search(&search_req(&fp, gt)).unwrap().total, 2);

        let exact_day = QueryNode::filter("pub_date", FilterOp::Exact, WireValue::Str("2020-01-01".into()));
        assert_eq!(f.engine.search(&search_req(&fp, exact_day)).unwrap().total, 1);

        let datetime = QueryNode::filter("pub_date", FilterOp::Exact, WireValue::Str("2021-06-15T12:30:00".into()));
        assert_eq!(f.engine.search(&search_req(&fp, datetime)).unwrap().total, 1);

        let range = QueryNode::filter(
            "pub_date",
            FilterOp::Range,
            WireValue::List(vec![WireValue::Str("2020-01-01".into()), WireValue::Str("2021-12-31".into())]),
        );
        assert_eq!(f.engine.search(&search_req(&fp, range)).unwrap().total, 2);

        // DateTime __in uses exact-term semantics (whoosh-consistent): a
        // date-only string means exactly midnight, so only the midnight doc
        // matches.
        let in_list = QueryNode::filter(
            "pub_date",
            FilterOp::In,
            WireValue::List(vec![WireValue::Str("2020-01-01T00:00:00".into()), WireValue::Str("2022-12-31".into())]),
        );
        assert_eq!(f.engine.search(&search_req(&fp, in_list)).unwrap().total, 1);
    }

    #[test]
    fn boolean_and_multi() {
        let f = fixture();
        let fp = f.schema.fingerprint();

        let active = QueryNode::filter("active", FilterOp::Exact, WireValue::Bool(true));
        assert_eq!(f.engine.search(&search_req(&fp, active)).unwrap().total, 3);

        let in_list = QueryNode::filter(
            "name",
            FilterOp::In,
            WireValue::List(vec![WireValue::Str("daniel".into()), WireValue::Str("clay".into())]),
        );
        assert_eq!(f.engine.search(&search_req(&fp, in_list)).unwrap().total, 2);
    }

    #[test]
    fn startswith_endswith_fuzzy() {
        let f = fixture();
        let fp = f.schema.fingerprint();

        let starts = QueryNode::filter("name", FilterOp::StartsWith, WireValue::Str("da".into()));
        assert_eq!(f.engine.search(&search_req(&fp, starts)).unwrap().total, 2);

        let ends = QueryNode::filter("name", FilterOp::EndsWith, WireValue::Str("vid".into()));
        assert_eq!(f.engine.search(&search_req(&fp, ends)).unwrap().total, 1);

        let fuzzy = QueryNode::filter("name", FilterOp::Fuzzy, WireValue::Str("danisl".into()));
        assert_eq!(f.engine.search(&search_req(&fp, fuzzy)).unwrap().total, 1);
    }

    #[test]
    fn and_or_not_composition() {
        let f = fixture();
        let fp = f.schema.fingerprint();

        let and = QueryNode::And {
            children: vec![
                QueryNode::filter("views", FilterOp::Gt, WireValue::Int(5)),
                QueryNode::filter("name", FilterOp::Exact, WireValue::Str("clay".into())),
            ],
        };
        assert_eq!(f.engine.search(&search_req(&fp, and)).unwrap().total, 1);

        let or = QueryNode::Or {
            children: vec![
                QueryNode::filter("name", FilterOp::Exact, WireValue::Str("daniel".into())),
                QueryNode::filter("name", FilterOp::Exact, WireValue::Str("clay".into())),
            ],
        };
        assert_eq!(f.engine.search(&search_req(&fp, or)).unwrap().total, 2);

        let not = QueryNode::Not {
            child: Box::new(QueryNode::filter("name", FilterOp::Exact, WireValue::Str("daniel".into()))),
        };
        assert_eq!(f.engine.search(&search_req(&fp, not)).unwrap().total, 2);
    }

    #[test]
    fn sort_by_field() {
        let f = fixture();
        let fp = f.schema.fingerprint();

        let mut req = search_req(&fp, QueryNode::MatchAll);
        req.sort = vec!["views".into()];
        let res = f.engine.search(&req).unwrap();
        assert_eq!(res.total, 3);

        let mut req_desc = search_req(&fp, QueryNode::MatchAll);
        req_desc.sort = vec!["-views".into()];
        let res_desc = f.engine.search(&req_desc).unwrap();
        let views: Vec<i64> = res_desc
            .hits
            .iter()
            .map(|h| match h.stored.get("views") {
                Some(WireValue::Int(n)) => *n,
                _ => 0,
            })
            .collect();
        assert_eq!(views, vec![30, 20, 10]);
    }

    #[test]
    fn pagination() {
        let f = fixture();
        let fp = f.schema.fingerprint();

        let mut req = search_req(&fp, QueryNode::MatchAll);
        req.start = 1;
        req.end = Some(2);
        let res = f.engine.search(&req).unwrap();
        assert_eq!(res.total, 3);
        assert_eq!(res.hits.len(), 1);
    }

    #[test]
    fn facets_count_values() {
        let f = fixture();
        let fp = f.schema.fingerprint();

        let mut req = search_req(&fp, QueryNode::MatchAll);
        req.facets = vec!["tags".into()];
        let res = f.engine.search(&req).unwrap();
        let facets = res.facets.expect("facet counts");
        let tags = &facets["tags"];
        let a_count = tags
            .iter()
            .find(|(v, _)| matches!(v, WireValue::Str(s) if s == "a"))
            .unwrap()
            .1;
        assert_eq!(a_count, 3);
    }

    #[test]
    fn remove_and_clear() {
        let f = fixture();
        let fp = f.schema.fingerprint();

        f.engine
            .remove(&RemoveRequestV1 {
                v: pitchfork_core::WIRE_VERSION,
                schema_fp: fp.clone(),
                ids: vec!["core.mockmodel.1".into()],
            })
            .unwrap();
        let res = f.engine.search(&search_req(&fp, QueryNode::MatchAll)).unwrap();
        assert_eq!(res.total, 2);

        f.engine.clear().unwrap();
        let res = f.engine.search(&search_req(&fp, QueryNode::MatchAll)).unwrap();
        assert_eq!(res.total, 0);
        assert_eq!(res.kind, 1);
    }

    #[test]
    fn more_like_this_finds_similar() {
        let f = fixture();
        let fp = f.schema.fingerprint();

        let req = search_req(&fp, QueryNode::MatchAll);
        let res = f.engine.more_like_this("core.mockmodel.1", &req).unwrap();
        assert!(res.total >= 1, "expected similar docs, got {res:?}");
        assert!(res.hits.iter().all(|h| h.id != "core.mockmodel.1"));
    }

    #[test]
    fn persistence_across_reopen() {
        let dir = TempDir::new().unwrap();
        let schema = fixture_schema();
        {
            let engine = TantivyEngine::new(dir.path()).unwrap();
            engine.setup(&schema).unwrap();
            engine
                .index(&IndexRequestV1 {
                    v: pitchfork_core::WIRE_VERSION,
                    schema_fp: Some(schema.fingerprint()),
                    schema: schema.clone(),
                    docs: vec![doc("core.mockmodel.1", "daniel", "hello", "2020-01-01", 1)],
                })
                .unwrap();
        }
        let engine = TantivyEngine::new(dir.path()).unwrap();
        engine.setup(&schema).unwrap();
        let res = engine
            .search(&search_req(&schema.fingerprint(), QueryNode::MatchAll))
            .unwrap();
        assert_eq!(res.total, 1);
    }

    #[test]
    fn reindex_same_fingerprint_is_noop() {
        let f = fixture();
        assert!(f.engine.setup(&f.schema).is_ok());
        let res = f
            .engine
            .search(&search_req(&f.schema.fingerprint(), QueryNode::MatchAll))
            .unwrap();
        assert_eq!(res.total, 3, "no-op setup must not clear documents");
    }

    #[test]
    fn stored_values_round_trip() {
        let f = fixture();
        let res = f
            .engine
            .search(&search_req(&f.schema.fingerprint(), QueryNode::MatchAll))
            .unwrap();
        let hit = &res.hits[0];
        assert!(matches!(hit.stored.get("views"), Some(WireValue::Int(_))));
        assert!(matches!(hit.stored.get("active"), Some(WireValue::Bool(true))));
        assert!(matches!(
            hit.stored.get("pub_date"),
            Some(WireValue::Str(_))
        ));
        match hit.stored.get("tags") {
            Some(WireValue::List(items)) => assert_eq!(items.len(), 2),
            other => panic!("expected list of tags, got {other:?}"),
        }
    }
}


#[cfg(test)]
mod date_semantics {
    use super::*;
    use pitchfork_core::schema::FieldDef;
    use tempfile::TempDir;

    #[test]
    fn datetime_exact_semantics() {
        let schema = PitchforkSchema::new(vec![
            FieldDef {
                document: true,
                ..FieldDef::new("text", FieldType::Text)
            },
            FieldDef::new("pub_date", FieldType::DateTime),
            FieldDef::new("day", FieldType::Date),
        ]);
        let dir = TempDir::new().unwrap();
        let engine = TantivyEngine::new(dir.path()).unwrap();
        engine.setup(&schema).unwrap();

        let mkdoc = |id: &str, date: &str| {
            let mut m = BTreeMap::new();
            m.insert("id".to_string(), WireValue::Str(id.into()));
            m.insert("django_ct".to_string(), WireValue::Str("a.m".into()));
            m.insert("django_id".to_string(), WireValue::Str("1".into()));
            m.insert("text".to_string(), WireValue::Str("hello".into()));
            m.insert("pub_date".to_string(), WireValue::Str(date.into()));
            m.insert("day".to_string(), WireValue::Str("2022-12-31".into()));
            m
        };

        engine
            .index(&IndexRequestV1 {
                v: pitchfork_core::WIRE_VERSION,
                schema_fp: Some(schema.fingerprint()),
                schema: schema.clone(),
                docs: vec![mkdoc("a.1", "2022-12-31T23:59:59")],
            })
            .unwrap();

        let fp = schema.fingerprint();
        let mkreq = |ast: QueryNode| SearchRequestV1 {
            v: pitchfork_core::WIRE_VERSION,
            schema_fp: fp.clone(),
            ast,
            start: 0,
            end: Some(10),
            sort: vec![],
            facets: vec![],
            highlight: false,
            spellcheck: false,
            mlt_doc_id: None,
            spelling_query: None,
            spatial: None,
        };

        // Date fields match the whole calendar day.
        let day_exact = mkreq(QueryNode::filter("day", FilterOp::Exact, WireValue::Str("2022-12-31".into())));
        assert_eq!(engine.search(&day_exact).unwrap().total, 1);

        // DateTime fields: date-only means exactly midnight (whoosh semantics).
        let dt_exact = mkreq(QueryNode::filter("pub_date", FilterOp::Exact, WireValue::Str("2022-12-31".into())));
        assert_eq!(engine.search(&dt_exact).unwrap().total, 0);
        let dt_full = mkreq(QueryNode::filter("pub_date", FilterOp::Exact, WireValue::Str("2022-12-31T23:59:59".into())));
        assert_eq!(engine.search(&dt_full).unwrap().total, 1);
    }
}

#[cfg(test)]
mod phase5_tests {
    use super::*;
    use pitchfork_core::schema::FieldDef;
    use tempfile::TempDir;

    fn location_schema() -> PitchforkSchema {
        PitchforkSchema::new(vec![
            FieldDef {
                document: true,
                ..FieldDef::new("text", FieldType::Text)
            },
            FieldDef::new("name", FieldType::Char),
            FieldDef::new("location", FieldType::Location),
        ])
    }

    fn loc_doc(id: &str, name: &str, text: &str, lat: f64, lng: f64) -> BTreeMap<String, WireValue> {
        let mut m = BTreeMap::new();
        m.insert("id".into(), WireValue::Str(id.into()));
        m.insert("django_ct".into(), WireValue::Str("core.mockmodel".into()));
        m.insert("django_id".into(), WireValue::Str("1".into()));
        m.insert("name".into(), WireValue::Str(name.into()));
        m.insert("text".into(), WireValue::Str(text.into()));
        m.insert(
            "location".into(),
            WireValue::Str(format!("{lat},{lng}")),
        );
        m
    }

    struct SpatialFixture {
        engine: TantivyEngine,
        schema: PitchforkSchema,
        #[allow(dead_code)]
        dir: TempDir,
    }

    fn spatial_fixture() -> SpatialFixture {
        let dir = TempDir::new().unwrap();
        let engine = TantivyEngine::new(dir.path()).unwrap();
        let schema = location_schema();
        engine.setup(&schema).unwrap();
        engine
            .index(&IndexRequestV1 {
                v: pitchfork_core::WIRE_VERSION,
                schema_fp: Some(schema.fingerprint()),
                schema: schema.clone(),
                docs: vec![
                    loc_doc("a.1", "london", "city one", 51.5074, -0.1278),
                    loc_doc("a.2", "paris", "city two", 48.8566, 2.3522),
                    loc_doc("a.3", "berlin", "city three", 52.5200, 13.4050),
                ],
            })
            .unwrap();
        SpatialFixture { engine, schema, dir }
    }

    fn req_with(fp: &str, spatial: pitchfork_core::wire::SpatialFilterV1) -> SearchRequestV1 {
        SearchRequestV1 {
            v: pitchfork_core::WIRE_VERSION,
            schema_fp: fp.to_string(),
            ast: QueryNode::MatchAll,
            start: 0,
            end: Some(10),
            sort: vec![],
            facets: vec![],
            highlight: false,
            spellcheck: false,
            spelling_query: None,
            spatial: Some(spatial),
            mlt_doc_id: None,
        }
    }

    #[test]
    fn within_bounding_box() {
        use pitchfork_core::wire::SpatialFilterV1;
        let f = spatial_fixture();
        let fp = f.schema.fingerprint();

        // Bounding box covering London and Paris (lat/lng corners).
        let spatial = SpatialFilterV1::Within {
            field: "location".into(),
            point_1: "47.0,-1.0".into(),
            point_2: "53.0,3.0".into(),
        };
        let res = f.engine.search(&req_with(&fp, spatial)).unwrap();
        assert_eq!(res.total, 2, "london + paris expected, got {res:?}");
        let names: Vec<String> = res
            .hits
            .iter()
            .map(|h| h.stored.get("name").cloned().unwrap().to_json().to_string())
            .collect();
        assert!(names.iter().any(|n| n.contains("london")));
        assert!(names.iter().any(|n| n.contains("paris")));
        assert!(!names.iter().any(|n| n.contains("berlin")));
    }

    #[test]
    fn dwithin_radius() {
        use pitchfork_core::wire::SpatialFilterV1;
        let f = spatial_fixture();
        let fp = f.schema.fingerprint();

        // 300 km around London: Paris (~344 km) is out, London is in.
        let spatial = SpatialFilterV1::DWithin {
            field: "location".into(),
            point: "51.5074,-0.1278".into(),
            radius_km: 300.0,
        };
        let res = f.engine.search(&req_with(&fp, spatial)).unwrap();
        assert_eq!(res.total, 1);
        assert_eq!(res.hits[0].id, "a.1");
    }

    #[test]
    fn spatial_requires_location_field() {
        use pitchfork_core::wire::SpatialFilterV1;
        let f = spatial_fixture();
        let fp = f.schema.fingerprint();

        let spatial = SpatialFilterV1::Within {
            field: "name".into(),
            point_1: "0.0,0.0".into(),
            point_2: "1.0,1.0".into(),
        };
        let res = f.engine.search(&req_with(&fp, spatial)).unwrap();
        assert_eq!(res.kind, 2);
        assert!(res.error.unwrap().contains("not a location field"));
    }

    #[test]
    fn spelling_suggests_correction() {
        let f = spatial_fixture();
        let fp = f.schema.fingerprint();

        // "ctiy" is a transposition away from the dictionary term "city"
        // (which lives in the analyzed document field).
        let mut req = search_req_basic(&fp);
        req.spellcheck = true;
        req.spelling_query = Some("ctiy one".into());
        let res = f.engine.search(&req).unwrap();
        assert_eq!(res.spelling.as_deref(), Some("city one"), "got {res:?}");
    }

    #[test]
    fn spelling_none_when_dictionary_clean() {
        let f = spatial_fixture();
        let fp = f.schema.fingerprint();

        let mut req = search_req_basic(&fp);
        req.spellcheck = true;
        req.spelling_query = Some("london city".into());
        let res = f.engine.search(&req).unwrap();
        assert_eq!(res.spelling, None);
    }

    fn search_req_basic(fp: &str) -> SearchRequestV1 {
        SearchRequestV1 {
            v: pitchfork_core::WIRE_VERSION,
            schema_fp: fp.to_string(),
            ast: QueryNode::MatchAll,
            start: 0,
            end: Some(10),
            sort: vec![],
            facets: vec![],
            highlight: false,
            spellcheck: false,
            spelling_query: None,
            spatial: None,
            mlt_doc_id: None,
        }
    }

    #[test]
    fn damerau_levenshtein_distances() {
        assert_eq!(spell::damerau_levenshtein("londn", "london"), 1);
        assert_eq!(spell::damerau_levenshtein("london", "london"), 0);
        assert_eq!(spell::damerau_levenshtein("abc", "abd"), 1);
        // Transposition costs one (optimal string alignment).
        assert_eq!(spell::damerau_levenshtein("abcd", "abdc"), 1);
    }
}
