use axum::{
    debug_handler,
    extract::{self, connect_info::IntoMakeServiceWithConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use axum_client_ip::{ClientIp, ClientIpSource};
use governor::make_governor_layer;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use mimalloc::MiMalloc;
use serde::{Deserialize, Serialize};
use state::{state_cleanup, JoinClient, MyState};
use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    time::Duration,
};
use tokio::net::TcpListener;
use tracing::log::*;

mod governor;
mod state;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Deserialize)]
struct Config {
    ip_source: ClientIpSource,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default = "default_rate_limit")]
    rate_limit: bool,
}
fn default_port() -> u16 {
    3000
}
fn default_rate_limit() -> bool {
    true
}

#[tokio::main]
async fn main() {
    let config: Config = envy::from_env().unwrap();

    init_tracing();

    let ip = Ipv4Addr::UNSPECIFIED;
    let addr = SocketAddrV4::new(ip, config.port);
    info!("Starting server in {addr}");

    let listener = TcpListener::bind(addr).await.unwrap();

    axum::serve(listener, app(config)).await.unwrap();
}

fn app(config: Config) -> IntoMakeServiceWithConnectInfo<Router, SocketAddr> {
    let state: &'static MyState = Box::leak(Box::new(MyState::new(make_prometheus())));
    tokio::task::spawn(state_cleanup(state));

    let mut app = Router::new()
        .route("/ping", get(ping))
        .route("/game", post(create_game))
        .route("/join/{token}", post(join_game))
        .route("/heartbeat/{game_id}", post(heartbeat))
        .route("/metrics", get(metrics))
        .with_state(state)
        .layer(config.ip_source.into_extension())
        .layer(axum_metrics::MetricLayer::default());

    if config.rate_limit {
        app = app.layer(make_governor_layer());
    }

    app.into_make_service_with_connect_info()
}

fn make_prometheus() -> PrometheusHandle {
    let prometheus_handle = PrometheusBuilder::new().install_recorder().unwrap();
    let prometheus_handle2 = prometheus_handle.clone();
    tokio::task::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            prometheus_handle2.run_upkeep();
        }
    });
    prometheus_handle
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,matchmaking_server_rust=debug".into()),
        )
        .compact()
        .without_time()
        .init();
}

#[debug_handler]
async fn metrics(
    State(state): State<&'static MyState>,
    headers: HeaderMap,
) -> Result<String, StatusCode> {
    dbg!(headers.iter().collect::<Vec<_>>());
    if headers
        .get("Authorization")
        .and_then(|auth| auth.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .is_none_or(|s| s != "kEtjcINjG4lhkdCF2ot1h")
    {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(state.prometheus_handle.render())
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
    client_ip: ClientIp,
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
    client_ip: ClientIp,
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
    client_ip: ClientIp,
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
