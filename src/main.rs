use axum::{
    Json,
    Router,
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post}, //get,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool, sqlite::SqlitePoolOptions};
use tokio::time::Duration;
use tower_http::trace::TraceLayer;

#[derive(Debug, Deserialize, Serialize, FromRow)]
struct Structure {
    // DB-managed
    id: Option<i64>,         // AUTOINCREMENT PK
    created_at: Option<i64>, // epoch millis

    // From client
    user_id: i64,
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
    /// Produce the SQLx query to insert and return the stored row.
    fn insert_query() -> &'static str {
        r#"
        INSERT INTO structures (
            user_id, map_id, scene, segment, prefab,
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
            ?,?,?,?,?,
            ?,?,?,
            ?,?,?,?,
            ?,?,?,
            ?,?,?,
            ?,
            ?,?,?,
            ?,?,?,?,
            ?,
            strftime('%s','now')*1000
        ) RETURNING *;
        "#
    }
}

#[derive(Clone)]
struct AppState {
    db: SqlitePool,
}

async fn post_structure(
    State(state): State<AppState>,
    Json(s): Json<Structure>,
) -> Result<Json<Structure>, (StatusCode, String)> {
    let rec: Structure = sqlx::query_as::<_, Structure>(Structure::insert_query())
        .bind(s.user_id)
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
        .fetch_one(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(rec))
}

#[derive(Deserialize)]
struct RandomParams {
    map_id: i32,
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_limit() -> i64 {
    5
}

async fn get_random(
    State(state): State<AppState>,
    Query(p): Query<RandomParams>,
) -> Result<Json<Vec<Structure>>, (StatusCode, String)> {
    let rows: Vec<Structure> = sqlx::query_as::<_, Structure>(
        "SELECT * FROM structures WHERE map_id = ? ORDER BY RANDOM() LIMIT ?;",
    )
    .bind(p.map_id)
    .bind(p.limit)
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(rows))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

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
            user_id   INTEGER NOT NULL,
            map_id    INTEGER NOT NULL,
            scene     TEXT,
            segment   INTEGER,
            prefab    TEXT NOT NULL,
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

    let state = AppState { db };

    let app = Router::new()
        .route("/api/v1/structures", get(get_random))
        .route("/api/v1/structures", post(post_structure))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    tracing::info!("Server listening on {:?}", listener);
    axum::serve(listener, app).await.unwrap();

    Ok(())
}
