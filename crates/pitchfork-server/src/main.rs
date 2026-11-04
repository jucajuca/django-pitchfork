//! Standalone axum HTTP search server speaking PitchforkWireV1 (Strategy C).
//!
//! Endpoints: /setup, /search, /index, /remove, /clear, /more_like_this,
//! /health. Every request/return uses `PitchforkWireV1` types; the wire
//! version is checked both in the body (`v`) and the `X-Pitchfork-Wire`
//! header. A schema fingerprint mismatch is a hard error, never a rebuild.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use pitchfork_core::wire::{
    AckResponseV1, ClearRequestV1, HealthResponseV1, IndexRequestV1, RemoveRequestV1,
    SearchRequestV1, SearchResponseV1, WIRE_VERSION,
};
use pitchfork_engine::{SearchEngine, TantivyEngine};
use std::sync::Arc;
use tokio::sync::RwLock;

pub const WIRE_HEADER: &str = "X-Pitchfork-Wire";

type SharedEngine = Arc<RwLock<Option<Arc<TantivyEngine>>>>;

#[derive(Clone)]
pub struct AppState {
    engine: SharedEngine,
    data_dir: Arc<std::path::PathBuf>,
}

pub fn build_router(data_dir: impl Into<std::path::PathBuf>) -> Router {
    let data_dir = Arc::new(data_dir.into());
    let state = AppState {
        engine: Arc::new(RwLock::new(None)),
        data_dir,
    };
    Router::new()
        .route("/health", get(health))
        .route("/setup", post(setup))
        .route("/search", post(search))
        .route("/index", post(index))
        .route("/remove", post(remove))
        .route("/clear", post(clear))
        .route("/more_like_this", post(more_like_this))
        .with_state(state)
}

fn check_wire(
    headers: &HeaderMap,
    body_v: u32,
) -> Result<(), (StatusCode, Json<SearchResponseV1>)> {
    if body_v != WIRE_VERSION {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(SearchResponseV1::error(format!(
                "unsupported wire version {body_v} (server speaks {WIRE_VERSION})"
            ))),
        ));
    }
    match headers.get(WIRE_HEADER).and_then(|h| h.to_str().ok()) {
        Some(v) if v == WIRE_VERSION.to_string() => Ok(()),
        Some(other) => Err((
            StatusCode::BAD_REQUEST,
            Json(SearchResponseV1::error(format!(
                "wire version header mismatch: header '{other}', body {body_v}, server {WIRE_VERSION}"
            ))),
        )),
        None => Ok(()), // header is optional; the body check is authoritative
    }
}

async fn health(State(state): State<AppState>) -> Json<HealthResponseV1> {
    let registered = state.engine.read().await.is_some();
    Json(HealthResponseV1 {
        v: WIRE_VERSION,
        status: if registered { "ok" } else { "no-schema" }.to_string(),
    })
}

async fn setup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<IndexRequestV1>,
) -> Result<Json<AckResponseV1>, (StatusCode, Json<AckResponseV1>)> {
    if let Err((status, Json(resp))) = check_wire(&headers, req.v) {
        let ack = AckResponseV1 {
            v: resp.v,
            ok: false,
            schema_fp: None,
            error: resp.error,
        };
        return Err((status, Json(ack)));
    }

    let mut guard = state.engine.write().await;
    if let Some(existing) = guard.as_ref() {
        let existing_fp = existing.schema_fp().unwrap_or_default();
        let sent_fp = req.schema_fp.clone().unwrap_or_else(|| existing_fp.clone());
        if existing_fp == sent_fp {
            return Ok(Json(AckResponseV1 {
                v: WIRE_VERSION,
                ok: true,
                schema_fp: Some(existing_fp),
                error: None,
            }));
        }
        return Err((
            StatusCode::CONFLICT,
            Json(AckResponseV1::error(format!(
                "schema fingerprint mismatch: server '{}', adapter '{}'",
                existing_fp, sent_fp
            ))),
        ));
    }

    let engine = TantivyEngine::new((*state.data_dir).clone())
        .and_then(|e| {
            e.setup(&req.schema)?;
            Ok(e)
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AckResponseV1::error(e.to_string())),
            )
        })?;
    *guard = Some(Arc::new(engine));
    Ok(Json(AckResponseV1 {
        v: WIRE_VERSION,
        ok: true,
        schema_fp: Some(req.schema.fingerprint()),
        error: None,
    }))
}

async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SearchRequestV1>,
) -> Result<Json<SearchResponseV1>, (StatusCode, Json<SearchResponseV1>)> {
    check_wire(&headers, req.v)?;
    let engine = state.engine.read().await.clone();
    match engine {
        Some(engine) => {
            Ok(Json(engine.search(&req).unwrap_or_else(|e| {
                SearchResponseV1::error(e.to_string())
            })))
        }
        None => Ok(Json(SearchResponseV1::error(
            "no schema registered; POST /setup first",
        ))),
    }
}

async fn index(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<IndexRequestV1>,
) -> Result<Json<AckResponseV1>, (StatusCode, Json<AckResponseV1>)> {
    check_wire_ack(&headers, req.v)?;
    let engine = state.engine.read().await.clone();
    match engine {
        Some(engine) => Ok(Json(
            engine
                .index(&req)
                .unwrap_or_else(|e| AckResponseV1::error(e.to_string())),
        )),
        None => {
            // An index request before setup registers the schema itself
            // (P1-4: the result of that first indexing must propagate —
            // a failed batch must NOT ack ok, and must NOT register the
            // engine, or later searches would silently run against a
            // half-registered schema).
            let engine = TantivyEngine::new((*state.data_dir).clone())
                .and_then(|e| {
                    e.setup(&req.schema)?;
                    Ok(e)
                })
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(AckResponseV1::error(e.to_string())),
                    )
                })?;
            // Register the engine first so `engine.index` sees its own
            // compiled state; on failure, roll the registration back
            // (P1-4: never ack ok on a failed first batch, never keep a
            // half-registered schema).
            let engine = Arc::new(engine);
            *state.engine.write().await = Some(engine.clone());
            let mut ack = engine
                .index(&req)
                .unwrap_or_else(|e| AckResponseV1::error(e.to_string()));
            // The engine — not this handler — owns the fingerprint
            // (P1-4: return the engine-computed fingerprint).
            if ack.ok {
                ack.schema_fp = engine.schema_fp();
                Ok(Json(ack))
            } else {
                *state.engine.write().await = None;
                Ok(Json(ack))
            }
        }
    }
}

async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RemoveRequestV1>,
) -> Result<Json<AckResponseV1>, (StatusCode, Json<AckResponseV1>)> {
    check_wire_ack(&headers, req.v)?;
    let engine = state.engine.read().await.clone();
    match engine {
        Some(engine) => Ok(Json(
            engine
                .remove(&req)
                .unwrap_or_else(|e| AckResponseV1::error(e.to_string())),
        )),
        None => Ok(Json(AckResponseV1::error(
            "no schema registered; POST /setup first",
        ))),
    }
}

async fn clear(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ClearRequestV1>,
) -> Result<Json<AckResponseV1>, (StatusCode, Json<AckResponseV1>)> {
    check_wire_ack(&headers, req.v)?;
    let engine = state.engine.read().await.clone();
    match engine {
        Some(engine) => Ok(Json(
            engine
                .clear(&req)
                .unwrap_or_else(|e| AckResponseV1::error(e.to_string())),
        )),
        None => Ok(Json(AckResponseV1::error(
            "no schema registered; POST /setup first",
        ))),
    }
}

async fn more_like_this(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SearchRequestV1>,
) -> Result<Json<SearchResponseV1>, (StatusCode, Json<SearchResponseV1>)> {
    check_wire(&headers, req.v)?;
    let engine = state.engine.read().await.clone();
    match engine {
        Some(engine) => {
            let doc_id = req.mlt_doc_id.clone().unwrap_or_default();
            Ok(Json(engine.more_like_this(&doc_id, &req).unwrap_or_else(
                |e| SearchResponseV1::error(e.to_string()),
            )))
        }
        None => Ok(Json(SearchResponseV1::error(
            "no schema registered; POST /setup first",
        ))),
    }
}

fn check_wire_ack(
    headers: &HeaderMap,
    body_v: u32,
) -> Result<(), (StatusCode, Json<AckResponseV1>)> {
    if body_v != WIRE_VERSION {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(AckResponseV1::error(format!(
                "unsupported wire version {body_v} (server speaks {WIRE_VERSION})"
            ))),
        ));
    }
    match headers.get(WIRE_HEADER).and_then(|h| h.to_str().ok()) {
        Some(v) if v == WIRE_VERSION.to_string() => Ok(()),
        Some(other) => Err((
            StatusCode::BAD_REQUEST,
            Json(AckResponseV1::error(format!(
                "wire version header mismatch: header '{other}', body {body_v}, server {WIRE_VERSION}"
            ))),
        )),
        None => Ok(()),
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data_dir = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "./pitchfork-data".to_string());
    let addr = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "127.0.0.1:8117".to_string());

    let app = build_router(&data_dir);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");
    eprintln!("pitchfork-server listening on {addr} (data: {data_dir})");
    axum::serve(listener, app).await.expect("server error");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use pitchfork_core::query_ast::{FilterOp, QueryNode};
    use pitchfork_core::schema::{FieldDef, FieldType, PitchforkSchema};
    use pitchfork_core::wire::WireValue;
    use serde_json::json;
    use std::collections::BTreeMap;
    use tower::ServiceExt;

    fn fixture_schema() -> PitchforkSchema {
        PitchforkSchema::new(vec![
            FieldDef {
                document: true,
                ..FieldDef::new("text", FieldType::Text)
            },
            FieldDef::new("name", FieldType::Char),
        ])
    }

    fn doc(id: &str, name: &str) -> BTreeMap<String, WireValue> {
        let mut m = BTreeMap::new();
        m.insert("id".to_string(), WireValue::Str(id.into()));
        m.insert(
            "django_ct".to_string(),
            WireValue::Str("core.mockmodel".into()),
        );
        m.insert("django_id".to_string(), WireValue::Str("1".into()));
        m.insert("name".to_string(), WireValue::Str(name.into()));
        m.insert("text".to_string(), WireValue::Str("hello world".into()));
        m
    }

    async fn setup_app() -> (Router, PitchforkSchema, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = fixture_schema();
        let app = build_router(dir.path());

        let req = IndexRequestV1 {
            v: WIRE_VERSION,
            schema_fp: Some(schema.fingerprint()),
            schema: schema.clone(),
            docs: vec![
                doc("core.mockmodel.1", "daniel"),
                doc("core.mockmodel.2", "david"),
            ],
        };
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/index")
                    .header("content-type", "application/json")
                    .header(WIRE_HEADER, WIRE_VERSION.to_string())
                    .body(Body::from(serde_json::to_vec(&req).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        (app, schema, dir)
    }

    async fn post_json(app: &Router, path: &str, body: serde_json::Value) -> serde_json::Value {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .header(WIRE_HEADER, WIRE_VERSION.to_string())
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn index_then_search_over_http() {
        let (app, schema, _dir) = setup_app().await;

        let search_req = SearchRequestV1 {
            v: WIRE_VERSION,
            schema_fp: schema.fingerprint(),
            ast: QueryNode::filter("name", FilterOp::Exact, WireValue::Str("daniel".into())),
            start: 0,
            end: Some(10),
            sort: vec![],
            facets: vec![],
            highlight: false,
            spellcheck: false,
            facet_scan_limit: None,
            spelling_query: None,
            spatial: None,
            mlt_doc_id: None,
        };
        let resp = post_json(&app, "/search", serde_json::to_value(&search_req).unwrap()).await;
        eprintln!("search response: {resp:?}");
        assert_eq!(resp["kind"], 0);
        assert_eq!(resp["total"], 1);
        assert_eq!(resp["hits"][0]["id"], "core.mockmodel.1");
        assert_eq!(resp["hits"][0]["django_ct"], "core.mockmodel");
    }

    #[tokio::test]
    async fn wire_version_mismatch_is_rejected() {
        let (app, _schema, _dir) = setup_app().await;

        let mut body = json!({"v": 99, "schema_fp": "x", "ast": {"node": "match_all"}});
        body["v"] = json!(99);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/search")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn fingerprint_mismatch_is_hard_error() {
        let (app, _schema, _dir) = setup_app().await;

        let search_req = SearchRequestV1 {
            v: WIRE_VERSION,
            schema_fp: "deadbeefdeadbeef".into(),
            ast: QueryNode::MatchAll,
            start: 0,
            end: Some(10),
            sort: vec![],
            facets: vec![],
            highlight: false,
            spellcheck: false,
            facet_scan_limit: None,
            spelling_query: None,
            spatial: None,
            mlt_doc_id: None,
        };
        let resp = post_json(&app, "/search", serde_json::to_value(&search_req).unwrap()).await;
        assert_eq!(resp["kind"], 2);
        assert!(resp["error"].as_str().unwrap().contains("mismatch"));
    }

    #[tokio::test]
    async fn remove_then_clear_over_http() {
        let (app, schema, _dir) = setup_app().await;

        let remove_req = RemoveRequestV1 {
            v: WIRE_VERSION,
            schema_fp: schema.fingerprint(),
            ids: vec!["core.mockmodel.1".into()],
        };
        let resp = post_json(&app, "/remove", serde_json::to_value(&remove_req).unwrap()).await;
        assert_eq!(resp["ok"], true);

        let mut search_req = SearchRequestV1 {
            v: WIRE_VERSION,
            schema_fp: schema.fingerprint(),
            ast: QueryNode::MatchAll,
            start: 0,
            end: Some(10),
            sort: vec![],
            facets: vec![],
            highlight: false,
            spellcheck: false,
            facet_scan_limit: None,
            spelling_query: None,
            spatial: None,
            mlt_doc_id: None,
        };
        let resp = post_json(&app, "/search", serde_json::to_value(&search_req).unwrap()).await;
        assert_eq!(resp["total"], 1);

        let clear_req = ClearRequestV1 {
            v: WIRE_VERSION,
            schema_fp: Some(schema.fingerprint()),
        };
        let resp = post_json(&app, "/clear", serde_json::to_value(&clear_req).unwrap()).await;
        assert_eq!(resp["ok"], true);

        search_req.ast = QueryNode::MatchAll;
        let resp = post_json(&app, "/search", serde_json::to_value(&search_req).unwrap()).await;
        assert_eq!(resp["total"], 0);
    }

    #[tokio::test]
    async fn health_reports_schema_state() {
        let (app, _schema, _dir) = setup_app().await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let health: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(health["v"], 1);
        assert_eq!(health["status"], "ok");
    }

    /// P1-4: a first `/index` batch that fails must NOT ack ok and must
    /// NOT register the schema (previously: `ok: true` plus a registered
    /// engine, i.e. silent data loss).
    #[tokio::test]
    async fn failed_first_index_batch_is_not_ok_and_not_registered() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = fixture_schema();
        let app = build_router(dir.path());

        // A document missing the required reserved identifier fields makes
        // the batch fail inside the engine.
        let mut bad_doc: BTreeMap<String, WireValue> = BTreeMap::new();
        bad_doc.insert("name".into(), WireValue::Str("daniel".into()));
        let req = IndexRequestV1 {
            v: WIRE_VERSION,
            schema_fp: Some(schema.fingerprint()),
            schema: schema.clone(),
            docs: vec![doc("core.mockmodel.1", "daniel"), bad_doc],
        };
        let resp = post_json(&app, "/index", serde_json::to_value(&req).unwrap()).await;
        assert_eq!(resp["ok"], false, "failed batch must not ack ok: {resp}");
        assert!(
            resp["error"]
                .as_str()
                .unwrap()
                .contains("missing required field"),
            "engine error must surface: {resp}"
        );

        // The engine must not stay registered with the failed schema.
        let search_req = SearchRequestV1 {
            v: WIRE_VERSION,
            schema_fp: schema.fingerprint(),
            ast: QueryNode::MatchAll,
            start: 0,
            end: Some(10),
            sort: vec![],
            facets: vec![],
            highlight: false,
            spellcheck: false,
            facet_scan_limit: None,
            spelling_query: None,
            spatial: None,
            mlt_doc_id: None,
        };
        let resp = post_json(&app, "/search", serde_json::to_value(&search_req).unwrap()).await;
        assert!(
            resp["error"]
                .as_str()
                .unwrap()
                .contains("no schema registered"),
            "engine must not be registered after a failed first batch: {resp}"
        );

        // A good batch afterwards still works end-to-end.
        let req = IndexRequestV1 {
            v: WIRE_VERSION,
            schema_fp: Some(schema.fingerprint()),
            schema: schema.clone(),
            docs: vec![doc("core.mockmodel.1", "daniel")],
        };
        let resp = post_json(&app, "/index", serde_json::to_value(&req).unwrap()).await;
        assert_eq!(resp["ok"], true, "{resp}");
        // P1-4: the ack carries the engine-computed fingerprint.
        assert_eq!(
            resp["schema_fp"].as_str().unwrap(),
            schema.fingerprint(),
            "engine-computed fingerprint must be returned: {resp}"
        );
    }
}
