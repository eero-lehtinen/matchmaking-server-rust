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
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tower_governor::{governor::GovernorConfig, GovernorLayer};
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
        let now = unix_time_secs();
        self.games
            .get_mut(token)
            .filter(|game| now - game.timestamp <= GAME_STALE.as_secs())
    }

    fn cleanup(&self) {
        let now = unix_time_secs();
        self.games
            .retain(|_, game| now - game.timestamp <= GAME_STALE.as_secs());
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
                .unwrap_or_else(|_| "info,matchmaking_server_rust=debug".into()),
        )
        .compact()
        .without_time()
        .init();

    let state: &'static MyState = Box::leak(Box::default());
    tokio::task::spawn(free_old_state(state));

    let governor_conf = Arc::new(GovernorConfig::default());
    let limiter = governor_conf.limiter().clone();
    tokio::task::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            limiter.retain_recent();
        }
    });

    let app = Router::new()
        .route("/game", post(create_game))
        .route("/game/:token/join", post(join_game))
        .route("/game/:token/heartbeat", post(heartbeat))
        .with_state(state)
        .layer(config.ip_source.into_extension())
        .layer(GovernorLayer {
            config: governor_conf,
        });

    let ip = Ipv4Addr::UNSPECIFIED;
    let port = config.port.unwrap_or(3000);
    let addr = SocketAddrV4::new(ip, port);
    info!("Starting server in {addr}");

    let listener = TcpListener::bind(addr).await.unwrap();

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
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

static BAD_WORDS: Lazy<Vec<&'static str>> =
    Lazy::new(|| include_str!("badwords.txt").lines().collect());

fn contains_bad_words(token: &str) -> bool {
    let token = token.to_ascii_lowercase();
    for bad_word in BAD_WORDS.iter() {
        if token.contains(bad_word) {
            return true;
        }
    }
    false
}

// Test bad words
#[test]
fn test_contains_bad_words() {
    assert!(contains_bad_words("ASDF-CuMJ_K"));
    assert!(!contains_bad_words("AdDF-aFcx"));
}

// Same as nanoid::alphabet::SAFE but dash, underscore and capital letters removed
pub const TOKEN_ALPHABET: [char; 36] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i',
    'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z',
];

#[debug_handler]
async fn create_game(
    client_ip: SecureClientIp,
    State(state): State<&'static MyState>,
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
        // https://zelark.github.io/nano-id-cc/
        let token = nanoid!(11, &TOKEN_ALPHABET);
        if !state.games.contains_key(&token) && !contains_bad_words(&token) {
            break token;
        }
    };
    debug!(
        "Created game {}, addr: {}, local_addr: {}",
        token, payload.external_address, payload.local_address
    );

    let game = Game {
        timestamp: unix_time_secs(),
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
    State(state): State<&'static MyState>,
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
        debug!(
            "Joining game {} from {} to local_addr: {}",
            token, payload.external_address, game.local_address
        );

        return Ok(Json(JoinGameResponse {
            join: game.local_address,
        }));
    }

    game.clients_to_join
        .insert(payload.external_address, unix_time_secs());

    debug!(
        "Joining game {} from {} to external_addr: {}",
        token, payload.external_address, game.external_address
    );

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
    State(state): State<&'static MyState>,
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

    let now = unix_time_secs();
    game.timestamp = now;
    game.clients_to_join
        .retain(|_, timestamp| now - *timestamp <= CLIENT_JOIN_STALE.as_secs());

    let clients = game.clients_to_join.keys().copied().collect();

    Ok(Json(HeartbeatResponse { clients }))
}

const CLIENT_JOIN_STALE: Duration = Duration::from_secs(10);
const GAME_STALE: Duration = Duration::from_secs(60);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(5 * 60);

fn unix_time_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

async fn free_old_state(state: &'static MyState) {
    let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
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
