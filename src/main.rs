use axum::{
    Json, Router,
    extract::{FromRequestParts, Query, State},
    http::{HeaderName, StatusCode},
    routing::{get, post},
};
use dashmap::DashMap;
use dotenvy::dotenv;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool, sqlite::SqlitePoolOptions};
use std::env;
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;
use tower_http::trace::TraceLayer;

static STEAM_HEADER: HeaderName = HeaderName::from_static("x-steam-auth"); // Header for Steam auth ticket
static STEAM_APPID: u64 = 3527290; // Peak Stranding AppID
const MAX_USER_STRUCTS_SAVED_PER_SCENE: i64 = 100;
const MAX_REQUESTED_STRUCTS: i64 = 150;

static POST_STRUCTURE_RATE_LIMIT: Duration = Duration::from_secs(2);
static GET_STRUCTURE_RATE_LIMIT: Duration = Duration::from_secs(6);
struct VerifiedUser(u64); // steam_id

#[derive(Clone)]
struct AppState {
    db: SqlitePool,
    cache: Arc<DashMap<String, u64>>,
    http: Client,
    steam_key: String,
    post_structure_rate_limiter: Arc<DashMap<u64, Instant>>,
    get_structure_rate_limiter: Arc<DashMap<u64, Instant>>,
}

//#[async_trait] // ???
impl FromRequestParts<AppState> for VerifiedUser {
    type Rejection = (StatusCode, String);

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(&STEAM_HEADER)
            .ok_or((StatusCode::UNAUTHORIZED, "X-Steam-Auth missing".into()))?
            .to_str()
            .map_err(|_| (StatusCode::BAD_REQUEST, "bad header".into()))?
            .to_owned();

        if let Some(id) = state.cache.get(&header) {
            return Ok(VerifiedUser(*id));
        }

        // Not cached – verify with Steam
        let url = format!(
            "https://api.steampowered.com/ISteamUserAuth/AuthenticateUserTicket/v1?key={}&appid={}&ticket={}",
            state.steam_key, STEAM_APPID, header
        );

        #[derive(Deserialize)]
        struct SteamResp {
            response: SteamResponseInner,
        }
        #[derive(Deserialize)]
        struct SteamResponseInner {
            params: SteamParams,
        }
        #[derive(Deserialize)]
        struct SteamParams {
            result: String,
            steamid: String,
        }

        let res: SteamResp = state
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))? // we haven't got a response
            .json()
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?; // we haven't got a _proper_ response

        if res.response.params.result != "OK" {
            return Err((StatusCode::UNAUTHORIZED, "ticket rejected".into())); // ticket is trash
        }

        let id = res
            .response
            .params
            .steamid
            .parse::<u64>()
            .map_err(|_| (StatusCode::BAD_GATEWAY, "bad steamid".into()))?; // invalid steamid in steam response (???)

        state.cache.insert(header, id);
        Ok(VerifiedUser(id))
    }
}

// in-game structure representation in the database
#[derive(Debug, Serialize, FromRow)]
struct Structure {
    // DB-managed
    id: Option<i64>,         // AUTOINCREMENT PK
    created_at: Option<i64>, // epoch millis (seconds actually)

    // getting that from steam
    user_id: i64,

    // from client
    // bro is trusting client data
    username: String,
    map_id: i32,
    scene: String,
    segment: i32,
    prefab: String,

    pos_x: f32,
    pos_y: f32,
    pos_z: f32,

    rot_x: f32,
    rot_y: f32,
    rot_z: f32,
    rot_w: f32,

    rope_start_x: f32,
    rope_start_y: f32,
    rope_start_z: f32,

    rope_end_x: f32,
    rope_end_y: f32,
    rope_end_z: f32,

    rope_length: f32,

    rope_flying_rotation_x: f32,
    rope_flying_rotation_y: f32,
    rope_flying_rotation_z: f32,

    rope_anchor_rotation_x: f32,
    rope_anchor_rotation_y: f32,
    rope_anchor_rotation_z: f32,
    rope_anchor_rotation_w: f32,

    antigrav: bool,
}

// in-game structure representation we receive as the payload for POST request
#[derive(Debug, Deserialize)]
struct NewStructure {
    username: String,
    map_id: i32,
    scene: String,
    segment: i32,
    prefab: String,
    pos_x: f32,
    pos_y: f32,
    pos_z: f32,
    rot_x: f32,
    rot_y: f32,
    rot_z: f32,
    rot_w: f32,
    rope_start_x: f32,
    rope_start_y: f32,
    rope_start_z: f32,
    rope_end_x: f32,
    rope_end_y: f32,
    rope_end_z: f32,
    rope_length: f32,
    rope_flying_rotation_x: f32,
    rope_flying_rotation_y: f32,
    rope_flying_rotation_z: f32,
    rope_anchor_rotation_x: f32,
    rope_anchor_rotation_y: f32,
    rope_anchor_rotation_z: f32,
    rope_anchor_rotation_w: f32,
    antigrav: bool,
}

impl Structure {
    fn insert_query() -> &'static str {
        r#"
        INSERT INTO structures (
            user_id,
            username,
            map_id, scene, segment, prefab,
            pos_x, pos_y, pos_z,
            rot_x, rot_y, rot_z, rot_w,
            rope_start_x, rope_start_y, rope_start_z,
            rope_end_x,   rope_end_y,   rope_end_z,
            rope_length,
            rope_flying_rotation_x, rope_flying_rotation_y, rope_flying_rotation_z,
            rope_anchor_rotation_x, rope_anchor_rotation_y, rope_anchor_rotation_z, rope_anchor_rotation_w,
            antigrav,
            created_at
        ) VALUES (
            ?, ?, ?, ?, ?, ?,
            ?, ?, ?,
            ?, ?, ?, ?,
            ?, ?, ?,
            ?, ?, ?,
            ?,
            ?, ?, ?,
            ?, ?, ?, ?,
            ?,
            strftime('%s','now')*1000
        ) RETURNING *;
        "#
    }
}

async fn post_structure(
    State(state): State<AppState>,
    VerifiedUser(steamid): VerifiedUser,
    Json(s): Json<NewStructure>,
) -> Result<Json<Structure>, (StatusCode, String)> {
    // Rate limiting check for posting structures (2 seconds)
    if let Some(last_post_time) = state.post_structure_rate_limiter.get(&steamid) {
        if last_post_time.elapsed() < POST_STRUCTURE_RATE_LIMIT {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "You are posting structures too frequently.".into(),
            ));
        }
    }
    state
        .post_structure_rate_limiter
        .insert(steamid, Instant::now());

    // Begin a transaction to perform all database operations at once.
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // 1. Insert the new structure.
    let rec: Structure = sqlx::query_as::<_, Structure>(Structure::insert_query())
        .bind(steamid as i64)
        .bind(&s.username)
        .bind(s.map_id)
        .bind(&s.scene)
        .bind(s.segment)
        .bind(&s.prefab)
        // position
        .bind(s.pos_x)
        .bind(s.pos_y)
        .bind(s.pos_z)
        // rotation
        .bind(s.rot_x)
        .bind(s.rot_y)
        .bind(s.rot_z)
        .bind(s.rot_w)
        // rope start
        .bind(s.rope_start_x)
        .bind(s.rope_start_y)
        .bind(s.rope_start_z)
        // rope end
        .bind(s.rope_end_x)
        .bind(s.rope_end_y)
        .bind(s.rope_end_z)
        // length
        .bind(s.rope_length)
        // flying rot
        .bind(s.rope_flying_rotation_x)
        .bind(s.rope_flying_rotation_y)
        .bind(s.rope_flying_rotation_z)
        // anchor rot
        .bind(s.rope_anchor_rotation_x)
        .bind(s.rope_anchor_rotation_y)
        .bind(s.rope_anchor_rotation_z)
        .bind(s.rope_anchor_rotation_w)
        // antigrav
        .bind(s.antigrav)
        .fetch_one(&mut *tx) // Use the transaction object 'tx'
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // 2. Count how many structures this user already has in this scene.
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM structures WHERE user_id = ? AND scene = ?")
            .bind(steamid as i64)
            .bind(&s.scene)
            .fetch_one(&mut *tx) // Use the transaction object 'tx'
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // 3. If over the limit, delete the oldest one.
    if count > MAX_USER_STRUCTS_SAVED_PER_SCENE {
        // This can be optimized further by combining the SELECT and DELETE
        // into a single query using a Common Table Expression (CTE).
        let delete_query = r#"
            DELETE FROM structures
            WHERE id = (
                SELECT id FROM structures
                WHERE user_id = ? AND scene = ?
                ORDER BY created_at ASC, id ASC
                LIMIT 1
            );
        "#;

        // Best-effort delete; ignore failure within the transaction for this specific logic.
        let _ = sqlx::query(delete_query)
            .bind(steamid as i64)
            .bind(&s.scene)
            .execute(&mut *tx) // Use the transaction object 'tx'
            .await;
    }

    // Commit the transaction to finalize all changes.
    tx.commit()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(rec))
}

#[derive(Deserialize)]
struct RandomParams {
    scene: String,
    map_id: Option<i32>,
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_limit() -> i64 {
    30
}

async fn get_random(
    State(state): State<AppState>,
    VerifiedUser(steamid): VerifiedUser,
    Query(p): Query<RandomParams>,
) -> Result<Json<Vec<Structure>>, (StatusCode, String)> {
    if let Some(last_get_time) = state.get_structure_rate_limiter.get(&steamid) {
        if last_get_time.elapsed() < GET_STRUCTURE_RATE_LIMIT {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "You are requesting structures too frequently.".into(),
            ));
        }
    }
    state
        .get_structure_rate_limiter
        .insert(steamid, Instant::now());
    if p.scene.len() > 50 {
        return Err((
            StatusCode::BAD_REQUEST,
            "scene must be ≤ 50 characters".into(),
        ));
    }
    let limit = p.limit.clamp(0, MAX_REQUESTED_STRUCTS);

    // We use a Common Table Expression (CTE) to rank structures.
    // The ranking is partitioned by user_id and segment to ensure diversity.
    let base_query = r#"
        WITH RankedStructures AS (
            SELECT
                *,
                ROW_NUMBER() OVER (PARTITION BY user_id, segment ORDER BY RANDOM()) as diversity_rank
            FROM structures
    "#;

    // The final SELECT statement orders by our new diversity rank,
    // ensuring we get a varied selection first.
    // We must explicitly list columns because the CTE adds `diversity_rank`,
    // which is not part of the `Structure` struct.
    let final_select = r#"
        )
        SELECT
            id, created_at, user_id, username, map_id, scene, segment, prefab,
            pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, rot_w,
            rope_start_x, rope_start_y, rope_start_z,
            rope_end_x, rope_end_y, rope_end_z,
            rope_length,
            rope_flying_rotation_x, rope_flying_rotation_y, rope_flying_rotation_z,
            rope_anchor_rotation_x, rope_anchor_rotation_y, rope_anchor_rotation_z, rope_anchor_rotation_w,
            antigrav
        FROM RankedStructures
        ORDER BY diversity_rank, RANDOM()
        LIMIT ?;
    "#;

    let rows: Vec<Structure> = if let Some(id) = p.map_id {
        let full_query = format!(
            "{} WHERE scene = ? AND map_id = ? {}",
            base_query, final_select
        );
        sqlx::query_as::<_, Structure>(&full_query)
            .bind(&p.scene)
            .bind(id)
            .bind(limit)
            .fetch_all(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    } else {
        let full_query = format!("{} WHERE scene = ? {}", base_query, final_select);
        sqlx::query_as::<_, Structure>(&full_query)
            .bind(&p.scene)
            .bind(limit)
            .fetch_all(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    };

    Ok(Json(rows))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    dotenv().ok();

    // create pool
    let db = SqlitePoolOptions::new()
        .max_connections(4)
        .idle_timeout(Duration::from_secs(30))
        .connect("sqlite://peakstranding.db?mode=rwc")
        .await?;

    // one‑shot schema (extra NULL columns allowed)
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS structures (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username  TEXT CHECK (length(username) <= 50),
            user_id   INTEGER NOT NULL,
            map_id    INTEGER NOT NULL,
            scene     TEXT NOT NULL CHECK (length(scene) <= 50),
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
            created_at INTEGER NOT NULL
        );
        "#,
    )
    .execute(&db)
    .await?;

    let state = AppState {
        db,
        cache: Arc::new(DashMap::new()),
        http: Client::builder()
            .pool_max_idle_per_host(0)
            .timeout(Duration::from_secs(5))
            .build()?,
        steam_key: env::var("STEAM_WEB_API_KEY").expect("STEAM_WEB_API_KEY missing"),
        post_structure_rate_limiter: Arc::new(DashMap::new()),
        get_structure_rate_limiter: Arc::new(DashMap::new()),
    };

    let app = Router::new()
        .route("/api/v1/structures", get(get_random))
        .route("/api/v1/structures", post(post_structure))
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    tracing::info!("Server listening on {:?}", listener);
    axum::serve(listener, app).await.unwrap();

    Ok(())
}
