use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use axum::{
    debug_handler,
    extract::{self, Path, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use axum_client_ip::{SecureClientIp, SecureClientIpSource};
use dashmap::{mapref::one::RefMut, DashMap};
use nanoid::nanoid;
use serde::{Deserialize, Serialize};
use tracing::log::*;

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Debug, Default)]
struct MyState {
    games: DashMap<String, Game>,
    total_games_created: AtomicU64,
}

impl MyState {
    fn get_game_mut(&self, token: &str) -> Option<RefMut<String, Game>> {
        let now = unix_time();
        self.games
            .get_mut(token)
            .filter(|game| now - game.timestamp <= GAME_STALE)
    }

    fn cleanup(&self) {
        let now = unix_time();
        self.games
            .retain(|_, game| now - game.timestamp <= GAME_STALE);
    }
}

#[derive(Debug)]
struct Game {
    timestamp: u64,
    external_address: SocketAddr,
    local_address: SocketAddr,
    clients_to_join: HashMap<SocketAddr, u64>,
}

#[derive(Deserialize)]
struct Config {
    ip_source: SecureClientIpSource,
    port: Option<u16>,
}

#[tokio::main]
async fn main() {
    let config: Config = envy::from_env().unwrap();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "matchmaking_server_rust=info".into()),
        )
        .compact()
        .without_time()
        .init();

    let state = Arc::new(MyState::default());

    let task_state = state.clone();
    tokio::task::spawn(async { cleanup(task_state).await });

    let app = Router::new()
        .route("/game", post(create_game))
        .route("/game/:token/join", post(join_game))
        .route("/game/:token/heartbeat", post(heartbeat))
        .with_state(state)
        .layer(config.ip_source.into_extension());

    let ip = Ipv4Addr::UNSPECIFIED;
    let port = config.port.unwrap_or(3000);
    let addr = SocketAddrV4::new(ip, port).into();
    info!("Starting server in {addr}");

    axum::Server::bind(&addr)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .unwrap();
}

#[derive(Deserialize)]
struct CreateGameRequest {
    external_address: SocketAddr,
    local_address: SocketAddr,
}

#[derive(Serialize)]
struct CreateGameResponse {
    token: String,
}

#[debug_handler]
async fn create_game(
    client_ip: SecureClientIp,
    State(state): State<Arc<MyState>>,
    extract::Json(payload): extract::Json<CreateGameRequest>,
) -> Result<Json<CreateGameResponse>, (StatusCode, &'static str)> {
    if client_ip.0 != payload.external_address.ip() {
        info!(
            "IPs {:?} and {:?} don't match",
            client_ip.0, payload.external_address
        );
        return Err((StatusCode::BAD_REQUEST, "IPs don't match"));
    }

    let token = loop {
        let token = nanoid!(10);
        if !state.games.contains_key(&token) {
            break token;
        }
    };

    let game = Game {
        timestamp: unix_time(),
        external_address: payload.external_address,
        local_address: payload.local_address,
        clients_to_join: HashMap::new(),
    };
    state.games.insert(token.clone(), game);
    state.total_games_created.fetch_add(1, Ordering::Relaxed);

    Ok(Json(CreateGameResponse { token }))
}

#[derive(Deserialize)]
struct JoinGameRequest {
    external_address: SocketAddr,
}

#[derive(Serialize)]
struct JoinGameResponse {
    join: SocketAddr,
}

#[debug_handler]
async fn join_game(
    client_ip: SecureClientIp,
    State(state): State<Arc<MyState>>,
    Path(token): Path<String>,
    extract::Json(payload): extract::Json<JoinGameRequest>,
) -> Result<Json<JoinGameResponse>, (StatusCode, &'static str)> {
    if client_ip.0 != payload.external_address.ip() {
        info!(
            "IPs {:?} and {:?} don't match",
            client_ip, payload.external_address
        );
        return Err((StatusCode::BAD_REQUEST, "IPs don't match"));
    }

    let mut game = state
        .get_game_mut(&token)
        .ok_or((StatusCode::NOT_FOUND, "Game not found"))?;

    if payload.external_address.ip() == game.external_address.ip() {
        return Ok(Json(JoinGameResponse {
            join: game.local_address,
        }));
    }

    game.clients_to_join
        .insert(payload.external_address, unix_time());

    Ok(Json(JoinGameResponse {
        join: game.external_address,
    }))
}

#[derive(Serialize)]
struct HeartbeatResponse {
    clients: Vec<SocketAddr>,
}

#[debug_handler]
async fn heartbeat(
    client_ip: SecureClientIp,
    State(state): State<Arc<MyState>>,
    Path(token): Path<String>,
) -> Result<Json<HeartbeatResponse>, (StatusCode, &'static str)> {
    let mut game = state
        .get_game_mut(&token)
        .ok_or((StatusCode::NOT_FOUND, "Game not found"))?;

    if client_ip.0 != game.external_address.ip() {
        info!(
            "IPs {:?} and {:?} don't match",
            client_ip, game.external_address
        );

        return Err((StatusCode::BAD_REQUEST, "IPs don't match"));
    }

    let now = unix_time();
    game.timestamp = now;
    game.clients_to_join
        .retain(|_, timestamp| now - *timestamp <= CLIENT_JOIN_STALE);

    let clients = game.clients_to_join.keys().copied().collect();

    Ok(Json(HeartbeatResponse { clients }))
}

// 10 seconds
const CLIENT_JOIN_STALE: u64 = 10_000;

// 1 minute
const GAME_STALE: u64 = 60_000;

// 5 minutes
const CLEANUP_INTERVAL: u64 = 5 * 60_000;

fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

async fn cleanup(state: Arc<MyState>) {
    let mut interval = tokio::time::interval(Duration::from_millis(CLEANUP_INTERVAL));
    let mut last_total_games_created = 0;
    loop {
        interval.tick().await;
        state.cleanup();
        let diff_games_created =
            state.total_games_created.load(Ordering::Relaxed) - last_total_games_created;
        if diff_games_created > 0 {
            last_total_games_created += diff_games_created;
            info!(
                "Total games created: {}, in the last 5 mins: {}",
                last_total_games_created, diff_games_created
            );
        }
    }
}
