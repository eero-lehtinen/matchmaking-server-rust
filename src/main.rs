use axum::{
    debug_handler,
    extract::{self, Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use axum_client_ip::{SecureClientIp, SecureClientIpSource};
use governor::make_governor_layer;
use mimalloc::MiMalloc;
use serde::{Deserialize, Serialize};
use state::{state_cleanup, JoinClient, MyState};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tracing::log::*;

mod governor;
mod state;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

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
    tokio::task::spawn(state_cleanup(state));

    let app = Router::new()
        .route("/ping", get(ping))
        .route("/game", post(create_game))
        .route("/join/:token", post(join_game))
        .route("/heartbeat/:game_id", post(heartbeat))
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(make_governor_layer())
                .layer(config.ip_source.into_extension()),
        );

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

#[debug_handler]
async fn ping() -> &'static str {
    "pong"
}

#[derive(Deserialize)]
struct CreateGameRequest {
    external_address: SocketAddr,
    local_address: SocketAddr,
}

#[derive(Serialize)]
struct CreateGameResponse {
    game_id: String,
    token: String,
}

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

    let (game_id, token) = state.create_game(payload.external_address, payload.local_address);

    Ok(Json(CreateGameResponse { token, game_id }))
}

#[derive(Deserialize)]
struct JoinGameRequest {
    external_address: SocketAddr,
    #[serde(default)]
    hard_nat: bool,
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
        .get_game_mut_by_join_token(&token)
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

    game.add_joiner(JoinClient {
        addr: payload.external_address,
        hard_nat: payload.hard_nat,
    });

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
    clients: Vec<JoinClient>,
}

#[debug_handler]
async fn heartbeat(
    client_ip: SecureClientIp,
    State(state): State<&'static MyState>,
    Path(game_id): Path<String>,
) -> Result<Json<HeartbeatResponse>, (StatusCode, &'static str)> {
    let mut game = state
        .get_game_mut(&game_id)
        .ok_or((StatusCode::NOT_FOUND, "Game not found"))?;

    if client_ip.0 != game.external_address.ip() {
        info!(
            "IPs {:?} and {:?} don't match",
            client_ip, game.external_address
        );

        return Err((StatusCode::BAD_REQUEST, "IPs don't match"));
    }

    Ok(Json(HeartbeatResponse {
        clients: game.drain_joiners(),
    }))
}
