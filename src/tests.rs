#![cfg(test)]

use super::*;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use serde_json::{json, Value};
use std::sync::Arc;
use http_body_util::BodyExt;
use tower::ServiceExt;

const OWNER_TICKET: &str = "owner-ticket";
const LIKER_TICKET: &str = "liker-ticket";
const OTHER_TICKET: &str = "other-ticket";

const OWNER_ID: u64 = 111;
const LIKER_ID: u64 = 222;
const OTHER_ID: u64 = 333;

struct TestContext {
    state: AppState,
    app: Router,
}

impl TestContext {
    async fn new() -> Self {
        let config = shared_test_config();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("failed to create test pool");

        let ddl = format!(
            r#"
            CREATE TABLE IF NOT EXISTS structures (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username  TEXT CHECK (length(username) <= 50),
                user_id   INTEGER NOT NULL,
                map_id    INTEGER NOT NULL,
                scene     TEXT NOT NULL CHECK (length(scene) <= {max_scene_length}),
                segment   INTEGER,
                prefab    TEXT NOT NULL CHECK (length(prefab) <= 50),
                pos_x REAL, pos_y REAL, pos_z REAL,
                rot_x REAL, rot_y REAL, rot_z REAL, rot_w REAL,
                rope_start_x REAL, rope_start_y REAL, rope_start_z REAL,
                rope_end_x   REAL, rope_end_y   REAL, rope_end_z   REAL,
                rope_length  REAL,
                rope_flying_rotation_x REAL, rope_flying_rotation_y REAL, rope_flying_rotation_z REAL,
                rope_anchor_rotation_x REAL, rope_anchor_rotation_y REAL, rope_anchor_rotation_z REAL, rope_anchor_rotation_w REAL,
                antigrav BOOLEAN NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                likes INTEGER NOT NULL DEFAULT 0,
                deleted BOOLEAN NOT NULL DEFAULT 0
            );
            "#,
            max_scene_length = config.max_scene_length
        );
        sqlx::query(&ddl)
            .execute(&pool)
            .await
            .expect("failed to run ddl");
        apply_migrations(&pool).await.expect("failed to run migrations");

        let cache = Arc::new(DashMap::new());
        cache.insert(OWNER_TICKET.to_string(), OWNER_ID);
        cache.insert(LIKER_TICKET.to_string(), LIKER_ID);
        cache.insert(OTHER_TICKET.to_string(), OTHER_ID);

        let state = AppState {
            db: pool.clone(),
            cache,
            http: Client::builder().build().expect("failed to build client"),
            steam_key: "test".to_string(),
            config: config.clone(),
            post_structure_rate_limiter: Arc::new(DashMap::new()),
            get_structure_rate_limiter: Arc::new(DashMap::new()),
            post_like_rate_limiter: Arc::new(DashMap::new()),
        };

        let app = build_router(state.clone());

        Self { state, app }
    }

    async fn post_structure(&self, ticket: &str, body: Value) -> axum::http::Response<Body> {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/structures")
                    .header(&STEAM_HEADER, ticket)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("failed to build POST request"),
            )
            .await
            .expect("POST /structures request failed")
    }

    async fn get_random(&self, ticket: &str, query: &str) -> axum::http::Response<Body> {
        let uri = format!("/api/v1/structures{query}");
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .header(&STEAM_HEADER, ticket)
                    .body(Body::empty())
                    .expect("failed to build GET request"),
            )
            .await
            .expect("GET /structures request failed")
    }

    async fn like_structure(&self, ticket: &str, id: i64, body: Value) -> axum::http::Response<Body> {
        let uri = format!("/api/v1/structures/{id}/like");
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(uri)
                    .header(&STEAM_HEADER, ticket)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("failed to build like request"),
            )
            .await
            .expect("POST /like request failed")
    }

    fn clear_post_rate_limit(&self, steam_id: u64) {
        self.state.post_structure_rate_limiter.remove(&steam_id);
    }

    fn clear_get_rate_limit(&self, steam_id: u64) {
        self.state.get_structure_rate_limiter.remove(&steam_id);
    }

}

fn shared_test_config() -> Arc<Config> {
    CONFIG
        .get_or_init(|| {
            Arc::new(Config {
                steam_appid: 0,
                max_user_structs_saved_per_scene: 2,
                max_requested_structs: 4,
                post_structure_rate_limit: Duration::from_millis(100),
                get_structure_rate_limit: Duration::from_millis(100),
                post_like_rate_limit: Duration::from_millis(100),
                default_random_limit: 3,
                max_scene_length: 16,
                database_url: "sqlite::memory:".to_string(),
                server_port: 0,
                skip_steam_ticket_validation: true,
            })
        })
        .clone()
}

fn structure_payload(username: &str, scene: &str, map_id: i32, segment: i32, prefab: &str) -> Value {
    json!({
        "username": username,
        "map_id": map_id,
        "scene": scene,
        "segment": segment,
        "prefab": prefab,
        "pos_x": 1.0,
        "pos_y": 2.0,
        "pos_z": 3.0,
        "rot_x": 0.0,
        "rot_y": 0.0,
        "rot_z": 0.0,
        "rot_w": 1.0,
        "rope_start_x": 0.0,
        "rope_start_y": 0.0,
        "rope_start_z": 0.0,
        "rope_end_x": 1.0,
        "rope_end_y": 1.0,
        "rope_end_z": 1.0,
        "rope_length": 5.0,
        "rope_flying_rotation_x": 0.0,
        "rope_flying_rotation_y": 0.0,
        "rope_flying_rotation_z": 0.0,
        "rope_anchor_rotation_x": 0.0,
        "rope_anchor_rotation_y": 0.0,
        "rope_anchor_rotation_z": 0.0,
        "rope_anchor_rotation_w": 1.0,
        "antigrav": false
    })
}

async fn response_json(response: axum::http::Response<Body>) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("failed to collect body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("failed to parse json")
}

async fn create_structure(
    ctx: &TestContext,
    ticket: &str,
    steam_id: u64,
    username: &str,
    scene: &str,
    map_id: i32,
    segment: i32,
    prefab: &str,
) -> i64 {
    let payload = structure_payload(username, scene, map_id, segment, prefab);
    let response = ctx.post_structure(ticket, payload).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    ctx.clear_post_rate_limit(steam_id);
    body["id"].as_i64().expect("structure id present")
}

#[tokio::test]
async fn post_structure_stores_and_returns_payload() {
    let ctx = TestContext::new().await;
    let payload = structure_payload("Sam", "SceneA", 1, 0, "prefab_a");
    let response = ctx.post_structure(OWNER_TICKET, payload).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["username"], "Sam");
    assert_eq!(body["user_id"].as_i64().unwrap(), OWNER_ID as i64);
    assert_eq!(body["likes"].as_i64().unwrap(), 0);
    let id = body["id"].as_i64().expect("id");
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM structures WHERE id = ?")
        .bind(id)
        .fetch_one(&ctx.state.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn post_structure_blocks_when_rate_limited() {
    let ctx = TestContext::new().await;
    let payload = structure_payload("Sam", "SceneRate", 1, 0, "prefab_rate");
    let first = ctx.post_structure(OWNER_TICKET, payload.clone()).await;
    assert_eq!(first.status(), StatusCode::OK);
    let second = ctx.post_structure(OWNER_TICKET, payload).await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn post_structure_prunes_oldest_per_user_scene() {
    let ctx = TestContext::new().await;
    for segment in 0..3 {
        let prefab = format!("prefab_{segment}");
        let _ = create_structure(
            &ctx,
            OWNER_TICKET,
            OWNER_ID,
            "Sam",
            "ScenePrune",
            1,
            segment,
            &prefab,
        )
        .await;
    }
    let prefabs: Vec<String> = sqlx::query_scalar(
        "SELECT prefab FROM structures WHERE scene = ? ORDER BY id",
    )
    .bind("ScenePrune")
    .fetch_all(&ctx.state.db)
    .await
    .unwrap();
    assert_eq!(prefabs, vec!["prefab_1".to_string(), "prefab_2".to_string()]);
}

#[tokio::test]
async fn requests_missing_steam_header_are_rejected() {
    let ctx = TestContext::new().await;
    let payload = structure_payload("Sam", "SceneNoAuth", 1, 0, "prefab_noauth");
    let response = ctx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/structures")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .expect("failed to build unauthenticated request"),
        )
        .await
        .expect("unauthenticated request failed");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn get_random_applies_limits_and_filters() {
    let ctx = TestContext::new().await;
    let users = [
        (OWNER_TICKET, OWNER_ID, "Owner"),
        (LIKER_TICKET, LIKER_ID, "Liker"),
        (OTHER_TICKET, OTHER_ID, "Other"),
    ];
    let mut prefabs = Vec::new();
    for (ticket, steam_id, prefix) in users {
        for segment in 0..2 {
            let prefab = format!("{prefix}_prefab_{segment}");
            prefabs.push(prefab.clone());
            let _ = create_structure(
                &ctx,
                ticket,
                steam_id,
                &format!("{prefix}_user"),
                "SceneRandom",
                1,
                segment,
                &prefab,
            )
            .await;
        }
    }

    let response = ctx.get_random(OWNER_TICKET, "?scene=SceneRandom").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let items = body.as_array().expect("array response");
    assert_eq!(items.len(), ctx.state.config.default_random_limit as usize);
    for item in items {
        assert_eq!(item["scene"], "SceneRandom");
    }

    ctx.clear_get_rate_limit(OWNER_ID);
    let response = ctx
        .get_random(OWNER_TICKET, "?scene=SceneRandom&map_id=1&limit=10")
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let items = body.as_array().expect("array response");
    assert_eq!(items.len(), ctx.state.config.max_requested_structs as usize);
    for item in items {
        assert_eq!(item["map_id"].as_i64().unwrap(), 1);
    }

    ctx.clear_get_rate_limit(OWNER_ID);
    let keep = prefabs.last().unwrap().clone();
    let exclude = prefabs
        .iter()
        .filter(|name| **name != keep)
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    let response = ctx
        .get_random(
            OWNER_TICKET,
            &format!("?scene=SceneRandom&map_id=1&limit=10&exclude_prefabs={exclude}"),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let items = body.as_array().expect("array response");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["prefab"].as_str().unwrap(), keep);

    ctx.clear_get_rate_limit(OWNER_ID);
    let too_long_scene = "X".repeat((ctx.state.config.max_scene_length + 1) as usize);
    let response = ctx
        .get_random(OWNER_TICKET, &format!("?scene={too_long_scene}"))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn get_random_enforces_rate_limit() {
    let ctx = TestContext::new().await;
    let _ = create_structure(
        &ctx,
        OWNER_TICKET,
        OWNER_ID,
        "RateUser",
        "SceneRate",
        1,
        0,
        "prefab_rate",
    )
    .await;

    let first = ctx.get_random(OWNER_TICKET, "?scene=SceneRate").await;
    assert_eq!(first.status(), StatusCode::OK);
    let second = ctx.get_random(OWNER_TICKET, "?scene=SceneRate").await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn like_structure_updates_counts_and_clamps() {
    let ctx = TestContext::new().await;
    let structure_id = create_structure(
        &ctx,
        OWNER_TICKET,
        OWNER_ID,
        "Owner",
        "SceneLike",
        1,
        0,
        "prefab_like",
    )
    .await;

    let response = ctx
        .like_structure(LIKER_TICKET, structure_id, json!({ "count": 150 }))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let likes = sqlx::query_scalar::<_, i64>("SELECT likes FROM structures WHERE id = ?")
        .bind(structure_id)
        .fetch_one(&ctx.state.db)
        .await
        .unwrap();
    assert_eq!(likes, 100);

    let (likes_send,) = sqlx::query_as::<_, (i64,)>(
        "SELECT likes_send FROM users WHERE user_id = ?",
    )
    .bind(LIKER_ID as i64)
    .fetch_one(&ctx.state.db)
    .await
    .unwrap();
    assert_eq!(likes_send, 100);

    let (likes_received,) = sqlx::query_as::<_, (i64,)>(
        "SELECT likes_received FROM users WHERE user_id = ?",
    )
    .bind(OWNER_ID as i64)
    .fetch_one(&ctx.state.db)
    .await
    .unwrap();
    assert_eq!(likes_received, 100);
}

#[tokio::test]
async fn like_structure_rejects_self_likes() {
    let ctx = TestContext::new().await;
    let structure_id = create_structure(
        &ctx,
        OWNER_TICKET,
        OWNER_ID,
        "Owner",
        "SceneSelf",
        1,
        0,
        "prefab_self",
    )
    .await;

    let response = ctx
        .like_structure(OWNER_TICKET, structure_id, json!({ "count": 1 }))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn like_structure_enforces_rate_limit() {
    let ctx = TestContext::new().await;
    let structure_id = create_structure(
        &ctx,
        OWNER_TICKET,
        OWNER_ID,
        "Owner",
        "SceneLikeLimit",
        1,
        0,
        "prefab_like_limit",
    )
    .await;

    let first = ctx
        .like_structure(LIKER_TICKET, structure_id, json!({ "count": 1 }))
        .await;
    assert_eq!(first.status(), StatusCode::NO_CONTENT);
    let second = ctx
        .like_structure(LIKER_TICKET, structure_id, json!({ "count": 1 }))
        .await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn like_structure_fails_for_missing_structure() {
    let ctx = TestContext::new().await;
    let response = ctx
        .like_structure(LIKER_TICKET, 999, json!({ "count": 1 }))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

