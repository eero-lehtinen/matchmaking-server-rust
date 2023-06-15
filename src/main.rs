use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    debug_handler,
    extract::{self, Path, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use axum_client_ip::{SecureClientIp, SecureClientIpSource};
use base64::prelude::*;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tower_http::trace::TraceLayer;
use tracing::log::*;

#[derive(Debug, Default)]
struct Games(HashMap<String, Game>);

#[derive(Debug)]
struct Game {
    timestamp: u64,
    external_address: SocketAddr,
    clients_to_join: HashMap<SocketAddr, u64>,
}

#[derive(serde::Deserialize)]
struct Config {
    ip_source: SecureClientIpSource,
}

#[tokio::main]
async fn main() {
    let config: Config = envy::from_env().unwrap();

    tracing_subscriber::fmt()
        .with_env_filter("matchmaking_server_rust=debug,tower_http=debug")
        .with_target(false)
        .compact()
        .without_time()
        .init();

    let state = Arc::new(Mutex::new(Games::default()));

    let task_state = state.clone();
    tokio::task::spawn(async { cleanup(task_state).await });

    let app = Router::new()
        .route("/game", post(create_game))
        .route("/game/:token/join", post(join_game))
        .route("/game/:token/heartbeat", post(heartbeat))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(config.ip_source.into_extension());

    info!("Starting server");

    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .unwrap();
}

#[derive(Deserialize)]
struct CreateGameRequest {
    external_address: SocketAddr,
}

#[derive(Serialize)]
struct CreateGameResponse {
    token: String,
}

#[debug_handler]
async fn create_game(
    client_ip: SecureClientIp,
    State(state): State<Arc<Mutex<Games>>>,
    extract::Json(payload): extract::Json<CreateGameRequest>,
) -> Result<Json<CreateGameResponse>, (StatusCode, &'static str)> {
    trace!("Client IP: {:?}", client_ip);
    if client_ip.0 != payload.external_address.ip() {
        debug!(
            "IPs {:?} and {:?} don't match",
            client_ip.0, payload.external_address
        );
        return Err((StatusCode::BAD_REQUEST, "IPs don't match"));
    }

    let mut games = state.lock().await;

    let mut random_data = [0u8; 7];
    let token = loop {
        rand::thread_rng().fill_bytes(&mut random_data);
        let token = BASE64_URL_SAFE_NO_PAD.encode(random_data);
        if !games.0.contains_key(&token) {
            break token;
        }
    };

    let game = Game {
        timestamp: unix_time(),
        external_address: payload.external_address,
        clients_to_join: HashMap::new(),
    };
    games.0.insert(token.clone(), game);

    Ok(Json(CreateGameResponse { token }))
}

#[derive(Deserialize)]
struct JoinGameRequest {
    external_address: SocketAddr,
}

#[derive(Serialize)]
struct JoinGameResponse {
    /// The address of the game server.
    join: SocketAddr,
}

#[debug_handler]
async fn join_game(
    client_ip: SecureClientIp,
    State(state): State<Arc<Mutex<Games>>>,
    Path(token): Path<String>,
    extract::Json(payload): extract::Json<JoinGameRequest>,
) -> Result<Json<JoinGameResponse>, (StatusCode, &'static str)> {
    trace!("Client IP: {:?}", client_ip);
    if client_ip.0 != payload.external_address.ip() {
        debug!(
            "IPs {:?} and {:?} don't match",
            client_ip, payload.external_address
        );
        return Err((StatusCode::BAD_REQUEST, "IPs don't match"));
    }

    let mut games = state.lock().await;

    let game = games
        .0
        .get_mut(&token)
        .ok_or((StatusCode::NOT_FOUND, "Game not found"))?;

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
    State(state): State<Arc<Mutex<Games>>>,
    Path(token): Path<String>,
) -> Result<Json<HeartbeatResponse>, (StatusCode, &'static str)> {
    let mut games = state.lock().await;

    let game = games
        .0
        .get_mut(&token)
        .ok_or((StatusCode::NOT_FOUND, "Game not found"))?;

    trace!("Client IP: {:?}", client_ip);
    if client_ip.0 != game.external_address.ip() {
        debug!(
            "IPs {:?} and {:?} don't match",
            client_ip, game.external_address
        );

        return Err((StatusCode::BAD_REQUEST, "IPs don't match"));
    }

    game.timestamp = unix_time();

    let clients = game.clients_to_join.keys().copied().collect();

    Ok(Json(HeartbeatResponse { clients }))
}

// 10 seconds
const CLIENT_JOIN_STALE: u64 = 10_000;

// 1 minute
const GAME_STALE: u64 = 60_000;

// 2 seconds
const CLEANUP_INTERVAL: u64 = 2_000;

fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

async fn cleanup(state: Arc<Mutex<Games>>) {
    let mut interval = tokio::time::interval(Duration::from_millis(CLEANUP_INTERVAL));
    loop {
        interval.tick().await;

        let mut games = state.lock().await;

        let now = unix_time();

        games.0.retain(|_, game| {
            if now - game.timestamp > GAME_STALE {
                trace!("Game {:?} is stale", game);
                return false;
            }

            game.clients_to_join.retain(|addr, timestamp| {
                if now - *timestamp > CLIENT_JOIN_STALE {
                    trace!("Client {} is stale", addr);
                    return false;
                }

                true
            });

            true
        });
    }
}
