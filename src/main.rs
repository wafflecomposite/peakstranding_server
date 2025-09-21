use axum::{
    Json, Router,
    extract::{FromRequestParts, OriginalUri, Path, Query, State},
    http::{HeaderName, Method, StatusCode},
    routing::{get, post},
};
use dashmap::DashMap;
use dotenvy::dotenv;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::{
    FromRow, Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{env, str::FromStr};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::time::Instant;
use tracing_subscriber::{EnvFilter, fmt};

static STEAM_HEADER: HeaderName = HeaderName::from_static("x-steam-auth"); // Header for Steam auth ticket
static CONFIG: OnceLock<Arc<Config>> = OnceLock::new();

#[derive(Debug, Clone)]
struct Config {
    steam_appid: u64,
    max_user_structs_saved_per_scene: i64,
    max_requested_structs: i64,
    post_structure_rate_limit: Duration,
    get_structure_rate_limit: Duration,
    post_like_rate_limit: Duration,
    default_random_limit: i64,
    max_scene_length: usize,
    database_url: String,
    server_port: u16,
    skip_steam_ticket_validation: bool,
}

impl Config {
    fn from_env() -> Self {
        fn parse_env<T>(key: &str, default: T) -> T
        where
            T: FromStr,
        {
            env::var(key)
                .ok()
                .and_then(|value| value.parse::<T>().ok())
                .unwrap_or(default)
        }

        let database_url = env::var("DATABASE_URL")
            .unwrap_or_else(|_| "sqlite://peakstranding.db?mode=rwc".to_string());

        Self {
            steam_appid: parse_env("STEAM_APPID", 3527290_u64),
            max_user_structs_saved_per_scene: parse_env(
                "MAX_USER_STRUCTS_SAVED_PER_SCENE",
                100_i64,
            ),
            max_requested_structs: parse_env("MAX_REQUESTED_STRUCTS", 400_i64),
            post_structure_rate_limit: Duration::from_secs(parse_env(
                "POST_STRUCTURE_RATE_LIMIT",
                2_u64,
            )),
            get_structure_rate_limit: Duration::from_secs(parse_env(
                "GET_STRUCTURE_RATE_LIMIT",
                6_u64,
            )),
            post_like_rate_limit: Duration::from_secs(parse_env("POST_LIKE_RATE_LIMIT", 1_u64)),
            default_random_limit: parse_env("DEFAULT_RANDOM_LIMIT", 40_i64),
            max_scene_length: parse_env("MAX_SCENE_LENGTH", 50_usize),
            database_url,
            server_port: parse_env("SERVER_PORT", 3000_u16),
            skip_steam_ticket_validation: parse_env("SKIP_STEAM_TICKET_VALIDATION", false),
        }
    }
}

fn config() -> &'static Config {
    CONFIG
        .get()
        .map(|cfg| cfg.as_ref())
        .expect("Config not initialized")
}
struct VerifiedUser(u64); // steam_id

#[derive(Debug, Clone)]
struct AppState {
    db: SqlitePool,
    cache: Arc<DashMap<String, u64>>,
    http: Client,
    steam_key: String,
    config: Arc<Config>,
    post_structure_rate_limiter: Arc<DashMap<u64, Instant>>,
    get_structure_rate_limiter: Arc<DashMap<u64, Instant>>,
    post_like_rate_limiter: Arc<DashMap<u64, Instant>>,
}

//#[async_trait] // not needed for axum 0.7's FromRequestParts
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


        if state.config.skip_steam_ticket_validation {
            let parsed_id = header
                .parse::<u64>()
                .map_err(|_| (StatusCode::BAD_REQUEST, "invalid steam ticket override".into()))?;
            state.cache.insert(header, parsed_id);
            return Ok(VerifiedUser(parsed_id));
        }
        // Not cached – verify with Steam
        let url = format!(
            "https://api.steampowered.com/ISteamUserAuth/AuthenticateUserTicket/v1?key={}&appid={}&ticket={}",
            state.steam_key, state.config.steam_appid, header
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

        let start = Instant::now();
        let resp = match state.http.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "steam_auth called result=transport_error error={} duration_ms={}",
                    e,
                    start.elapsed().as_millis()
                );
                return Err((StatusCode::BAD_GATEWAY, e.to_string()));
            }
        };
        let res: SteamResp = match resp.json().await {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(
                    "steam_auth called result=bad_json error={} duration_ms={}",
                    e,
                    start.elapsed().as_millis()
                );
                return Err((StatusCode::BAD_GATEWAY, e.to_string()));
            }
        };

        if res.response.params.result != "OK" {
            tracing::warn!(
                "steam_auth called result={} steamid={} duration_ms={}",
                res.response.params.result,
                res.response.params.steamid,
                start.elapsed().as_millis()
            );
            return Err((StatusCode::UNAUTHORIZED, "ticket rejected".into()));
        }

        let id = res
            .response
            .params
            .steamid
            .parse::<u64>()
            .map_err(|_| (StatusCode::BAD_GATEWAY, "bad steamid".into()))?;

        tracing::info!(
            "steam_auth called result=OK steamid={} duration_ms={}",
            id,
            start.elapsed().as_millis()
        );

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

    likes: i32,
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
    OriginalUri(uri): OriginalUri,
    method: Method,
    Json(s): Json<NewStructure>,
) -> Result<Json<Structure>, (StatusCode, String)> {
    let started = Instant::now();

    // Rate limiting check for posting structures (configurable)
    if let Some(last_post_time) = state.post_structure_rate_limiter.get(&steamid) {
        if last_post_time.elapsed() < state.config.post_structure_rate_limit {
            let dur = started.elapsed().as_millis();
            let url = uri.to_string();
            tracing::warn!(
                "request user_id={} method={} url={} status=429 duration_ms={} level={} map_id={}",
                steamid,
                method.as_str(),
                url,
                dur,
                s.scene,
                s.map_id
            );
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
    let mut tx = state.db.begin().await.map_err(|e| {
        let dur = started.elapsed().as_millis();
        tracing::error!(
            "request user_id={} method={} url={} status=500 duration_ms={} error=like_tx_begin_failed",
            steamid,
            method.as_str(),
            uri.to_string(),
            dur
        );
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;

    // 0. Ensure the posting user exists in users table
    sqlx::query(
        r#"INSERT OR IGNORE INTO users (user_id, upload_banned, likes_received, likes_send)
           VALUES (?, 0, 0, 0);"#,
    )
    .bind(steamid as i64)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        let dur = started.elapsed().as_millis();
        tracing::error!(
            "request user_id={} method={} url={} status=500 duration_ms={} error=ensure_user_failed",
            steamid,
            method.as_str(),
            uri.to_string(),
            dur
        );
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;

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
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| {
            let dur = started.elapsed().as_millis();
            tracing::error!(
                "request user_id={} method={} url={} status=500 duration_ms={} error=insert_structure_failed",
                steamid,
                method.as_str(),
                uri.to_string(),
                dur
            );
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

    // 2. Count how many structures this user already has in this scene.
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM structures WHERE user_id = ? AND scene = ?")
            .bind(steamid as i64)
            .bind(&s.scene)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| {
                let dur = started.elapsed().as_millis();
                tracing::error!(
                    "request user_id={} method={} url={} status=500 duration_ms={} error=count_structures_failed",
                    steamid,
                    method.as_str(),
                    uri.to_string(),
                    dur
                );
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            })?;

    // 3. If over the limit, delete the oldest one.
    if count > state.config.max_user_structs_saved_per_scene {
        let delete_query = r#"
            DELETE FROM structures
            WHERE id = (
                SELECT id FROM structures
                WHERE user_id = ? AND scene = ?
                ORDER BY created_at ASC, id ASC
                LIMIT 1
            );
        "#;

        let _ = sqlx::query(delete_query)
            .bind(steamid as i64)
            .bind(&s.scene)
            .execute(&mut *tx)
            .await;
    }

    // Commit the transaction to finalize all changes.
    tx.commit().await.map_err(|e| {
        let dur = started.elapsed().as_millis();
        tracing::error!(
            "request user_id={} method={} url={} status=500 duration_ms={} error=tx_commit_failed",
            steamid,
            method.as_str(),
            uri.to_string(),
            dur
        );
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;

    let dur = started.elapsed().as_millis();
    tracing::info!(
        "request user_id={} method={} url={} status=200 duration_ms={} level={} map_id={}",
        steamid,
        method.as_str(),
        uri.to_string(),
        dur,
        s.scene,
        s.map_id
    );

    Ok(Json(rec))
}

#[derive(Deserialize)]
struct RandomParams {
    scene: String,
    map_id: Option<i32>,
    #[serde(default = "default_limit")]
    limit: i64,
    exclude_prefabs: Option<String>,
}
fn default_limit() -> i64 {
    config().default_random_limit
}

async fn get_random(
    State(state): State<AppState>,
    VerifiedUser(steamid): VerifiedUser,
    OriginalUri(uri): OriginalUri,
    method: Method,
    Query(p): Query<RandomParams>,
) -> Result<Json<Vec<Structure>>, (StatusCode, String)> {
    let started = Instant::now();

    if let Some(last_get_time) = state.get_structure_rate_limiter.get(&steamid) {
        if last_get_time.elapsed() < state.config.get_structure_rate_limit {
            let dur = started.elapsed().as_millis();
            tracing::warn!(
                "request user_id={} method={} url={} status=429 duration_ms={}",
                steamid,
                method.as_str(),
                uri.to_string(),
                dur
            );
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "You are requesting structures too frequently.".into(),
            ));
        }
    }
    state
        .get_structure_rate_limiter
        .insert(steamid, Instant::now());

    if p.scene.len() > state.config.max_scene_length {
        let dur = started.elapsed().as_millis();
        tracing::warn!(
            "request user_id={} method={} url={} status=400 duration_ms={} reason=scene_too_long",
            steamid,
            method.as_str(),
            uri.to_string(),
            dur
        );
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "scene must be <= {} characters",
                state.config.max_scene_length
            ),
        ));
    }
    let limit = p.limit.clamp(0, state.config.max_requested_structs);

    let base_query = r#"
        WITH RankedStructures AS (
            SELECT
                *,
                ROW_NUMBER() OVER (PARTITION BY user_id, segment ORDER BY RANDOM()) as diversity_rank
            FROM structures
    "#;

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
            antigrav,
            likes
        FROM RankedStructures
        ORDER BY diversity_rank, RANDOM()
        LIMIT ?;
    "#;

    let mut where_conditions = vec!["scene = ?".to_string(), "deleted = 0".to_string()];

    if p.map_id.is_some() {
        where_conditions.push("map_id = ?".to_string());
    }

    let prefabs_to_exclude: Vec<String> = p
        .exclude_prefabs
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    if !prefabs_to_exclude.is_empty() {
        let placeholders = format!("({})", vec!["?"; prefabs_to_exclude.len()].join(","));
        where_conditions.push(format!("prefab NOT IN {}", placeholders));
    }

    let full_query = format!(
        "{} WHERE {} {}",
        base_query,
        where_conditions.join(" AND "),
        final_select
    );

    let mut query = sqlx::query_as::<_, Structure>(&full_query).bind(&p.scene);
    if let Some(id) = p.map_id {
        query = query.bind(id);
    }
    for prefab_name in &prefabs_to_exclude {
        query = query.bind(prefab_name);
    }
    query = query.bind(limit);

    let rows = query.fetch_all(&state.db).await.map_err(|e| {
        let dur = started.elapsed().as_millis();
        tracing::error!(
            "request user_id={} method={} url={} status=500 duration_ms={} error=query_failed",
            steamid,
            method.as_str(),
            uri.to_string(),
            dur
        );
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;

    let dur = started.elapsed().as_millis();
    tracing::info!(
        "request user_id={} method={} url={} status=200 duration_ms={}",
        steamid,
        method.as_str(),
        uri.to_string(),
        dur
    );

    Ok(Json(rows))
}

#[derive(Deserialize)]
struct LikeBody {
    count: Option<i32>,
}

async fn like_structure(
    State(state): State<AppState>,
    VerifiedUser(steamid): VerifiedUser,
    OriginalUri(uri): OriginalUri,
    method: Method,
    Path(id): Path<i64>,
    Json(body): Json<LikeBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let started = Instant::now();
    let requested = body.count.unwrap_or(1); // log before clamp

    // Per-user rate limit for likes (configurable)
    if let Some(last) = state.post_like_rate_limiter.get(&steamid) {
        if last.elapsed() < state.config.post_like_rate_limit {
            let dur = started.elapsed().as_millis();
            tracing::warn!(
                "request user_id={} method={} url={} status=429 duration_ms={} like_requested={}",
                steamid,
                method.as_str(),
                uri.to_string(),
                dur,
                requested
            );
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "You are liking too frequently.".into(),
            ));
        }
    }
    state.post_like_rate_limiter.insert(steamid, Instant::now());

    let mut tx = state.db.begin().await.map_err(|e| {
        let dur = started.elapsed().as_millis();
        tracing::error!(
            "request user_id={} method={} url={} status=500 duration_ms={} like_requested={} error=tx_begin_failed",
            steamid,
            method.as_str(),
            uri.to_string(),
            dur,
            requested
        );
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;

    // Validate structure and get owner
    let owner: Option<(i64,)> =
        sqlx::query_as("SELECT user_id FROM structures WHERE id = ? AND deleted = 0")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| {
                let dur = started.elapsed().as_millis();
                tracing::error!(
                    "request user_id={} method={} url={} status=500 duration_ms={} like_requested={} error=select_owner_failed",
                    steamid,
                    method.as_str(),
                    uri.to_string(),
                    dur,
                    requested
                );
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            })?;

    let Some((owner_user_id,)) = owner else {
        tx.rollback().await.ok();
        let dur = started.elapsed().as_millis();
        tracing::warn!(
            "request user_id={} method={} url={} status=404 duration_ms={} like_requested={}",
            steamid,
            method.as_str(),
            uri.to_string(),
            dur,
            requested
        );
        return Err((StatusCode::NOT_FOUND, "Structure not found".into()));
    };

    // Forbid self-like attempts
    if owner_user_id == steamid as i64 {
        tx.rollback().await.ok();
        let dur = started.elapsed().as_millis();
        tracing::warn!(
            "request user_id={} method={} url={} status=400 duration_ms={} like_requested={} reason=self_like",
            steamid,
            method.as_str(),
            uri.to_string(),
            dur,
            requested
        );
        return Err((
            StatusCode::BAD_REQUEST,
            "Cannot like your own structure.".into(),
        ));
    }

    // Normalize count AFTER logging requested
    let count = requested.clamp(1, 100);

    // Ensure liker and owner exist in users
    sqlx::query(
        r#"INSERT OR IGNORE INTO users (user_id, upload_banned, likes_received, likes_send)
           VALUES (?, 0, 0, 0);"#,
    )
    .bind(steamid as i64)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        let dur = started.elapsed().as_millis();
        tracing::error!(
            "request user_id={} method={} url={} status=500 duration_ms={} like_requested={} error=ensure_liker_failed",
            steamid,
            method.as_str(),
            uri.to_string(),
            dur,
            requested
        );
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;
    sqlx::query(
        r#"INSERT OR IGNORE INTO users (user_id, upload_banned, likes_received, likes_send)
           VALUES (?, 0, 0, 0);"#,
    )
    .bind(owner_user_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        let dur = started.elapsed().as_millis();
        tracing::error!(
            "request user_id={} method={} url={} status=500 duration_ms={} like_requested={} error=ensure_owner_failed",
            steamid,
            method.as_str(),
            uri.to_string(),
            dur,
            requested
        );
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;

    // Update structure likes
    let updated =
        sqlx::query("UPDATE structures SET likes = likes + ? WHERE id = ? AND deleted = 0")
            .bind(count)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                let dur = started.elapsed().as_millis();
                tracing::error!(
                    "request user_id={} method={} url={} status=500 duration_ms={} like_requested={} error=update_structure_failed",
                    steamid,
                    method.as_str(),
                    uri.to_string(),
                    dur,
                    requested
                );
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            })?;
    if updated.rows_affected() == 0 {
        tx.rollback().await.ok();
        let dur = started.elapsed().as_millis();
        tracing::warn!(
            "request user_id={} method={} url={} status=404 duration_ms={} like_requested={}",
            steamid,
            method.as_str(),
            uri.to_string(),
            dur,
            requested
        );
        return Err((StatusCode::NOT_FOUND, "Structure not found".into()));
    }

    // Update users metrics
    sqlx::query("UPDATE users SET likes_send = likes_send + ? WHERE user_id = ?")
        .bind(count)
        .bind(steamid as i64)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            let dur = started.elapsed().as_millis();
            tracing::error!(
                "request user_id={} method={} url={} status=500 duration_ms={} like_requested={} error=update_liker_metrics_failed",
                steamid,
                method.as_str(),
                uri.to_string(),
                dur,
                requested
            );
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;
    sqlx::query("UPDATE users SET likes_received = likes_received + ? WHERE user_id = ?")
        .bind(count)
        .bind(owner_user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            let dur = started.elapsed().as_millis();
            tracing::error!(
                "request user_id={} method={} url={} status=500 duration_ms={} like_requested={} error=update_owner_metrics_failed",
                steamid,
                method.as_str(),
                uri.to_string(),
                dur,
                requested
            );
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

    tx.commit().await.map_err(|e| {
        let dur = started.elapsed().as_millis();
        tracing::error!(
            "request user_id={} method={} url={} status=500 duration_ms={} like_requested={} error=tx_commit_failed",
            steamid,
            method.as_str(),
            uri.to_string(),
            dur,
            requested
        );
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;

    let dur = started.elapsed().as_millis();
    tracing::info!(
        "request user_id={} method={} url={} status=204 duration_ms={} like_requested={}",
        steamid,
        method.as_str(),
        uri.to_string(),
        dur,
        requested
    );

    Ok(StatusCode::NO_CONTENT)
}


fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/structures", get(get_random))
        .route("/api/v1/structures", post(post_structure))
        .route("/api/v1/structures/{id}/like", post(like_structure))
        // .layer(TraceLayer::new_for_http()) // intentionally removed to avoid extra logs
        .with_state(state)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Only WARN/ERROR from deps, but INFO from this crate.
    let crate_name = env!("CARGO_PKG_NAME");
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("warn,{crate_name}=info")));

    fmt().with_env_filter(filter).init();

    dotenv().ok();

    let config = Arc::new(Config::from_env());
    CONFIG
        .set(config.clone())
        .expect("Config already initialized");

    let connect_opts = SqliteConnectOptions::from_str(&config.database_url)?
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(std::time::Duration::from_secs(5));

    let db = SqlitePoolOptions::new()
        .max_connections(4)
        .idle_timeout(Duration::from_secs(30))
        .connect_with(connect_opts)
        .await?;

    let structures_ddl = format!(
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
            created_at INTEGER NOT NULL
        );
        "#,
        max_scene_length = config.max_scene_length
    );

    sqlx::query(&structures_ddl).execute(&db).await?;

    // apply non-destructive migrations if needed
    apply_migrations(&db).await?;

    let state = AppState {
        db,
        cache: Arc::new(DashMap::new()),
        http: Client::builder()
            .pool_max_idle_per_host(0)
            .timeout(Duration::from_secs(5))
            .build()?,
        steam_key: env::var("STEAM_WEB_API_KEY").expect("STEAM_WEB_API_KEY missing"),
        config: config.clone(),
        post_structure_rate_limiter: Arc::new(DashMap::new()),
        get_structure_rate_limiter: Arc::new(DashMap::new()),
        post_like_rate_limiter: Arc::new(DashMap::new()),
    };

    let app = build_router(state.clone());

    let bind_addr = format!("0.0.0.0:{}", config.server_port);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await.unwrap();
    tracing::info!("Server listening on {}", bind_addr);
    axum::serve(listener, app).await.unwrap();

    Ok(())
}

// --- migrations ---
async fn apply_migrations(db: &SqlitePool) -> Result<(), sqlx::Error> {
    // Ensure users table exists
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            user_id       INTEGER PRIMARY KEY,
            upload_banned BOOLEAN NOT NULL DEFAULT 0,
            likes_received INTEGER NOT NULL DEFAULT 0,
            likes_send     INTEGER NOT NULL DEFAULT 0
        );
        "#,
    )
    .execute(db)
    .await?;

    // Add columns to structures if missing
    if !column_exists(db, "structures", "likes").await? {
        sqlx::query("ALTER TABLE structures ADD COLUMN likes INTEGER NOT NULL DEFAULT 0;")
            .execute(db)
            .await?;
    }
    if !column_exists(db, "structures", "deleted").await? {
        sqlx::query("ALTER TABLE structures ADD COLUMN deleted BOOLEAN NOT NULL DEFAULT 0;")
            .execute(db)
            .await?;
    }
    // Create helpful indexes (idempotent)
    // Filter path in get_random: WHERE scene = ? AND deleted = 0 [AND map_id = ?]
    sqlx::query(
        r#"CREATE INDEX IF NOT EXISTS idx_structures_scene_deleted_map
           ON structures(scene, map_id, deleted);"#,
    )
    .execute(db)
    .await?;

    // Oldest-per-user-per-scene pruning: ORDER BY created_at, id WHERE user_id = ? AND scene = ?
    sqlx::query(
        r#"CREATE INDEX IF NOT EXISTS idx_structures_user_scene_created
           ON structures(user_id, scene, created_at, id);"#,
    )
    .execute(db)
    .await?;

    // Exclusion by prefab (NOT IN ...) can benefit from an index on prefab
    sqlx::query(
        r#"CREATE INDEX IF NOT EXISTS idx_structures_prefab
           ON structures(prefab);"#,
    )
    .execute(db)
    .await?;

    Ok(())
}

async fn column_exists(db: &SqlitePool, table: &str, column: &str) -> Result<bool, sqlx::Error> {
    let mut rows = sqlx::query(&format!("PRAGMA table_info({});", table))
        .fetch_all(db)
        .await?;

    // PRAGMA table_info columns: cid, name, type, notnull, dflt_value, pk
    for row in rows.drain(..) {
        let name: String = row.try_get("name").unwrap_or_default();
        if name.eq_ignore_ascii_case(column) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests;
