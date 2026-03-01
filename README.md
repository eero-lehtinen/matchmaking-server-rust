# Matchmaking Server

A lightweight peer-to-peer game matchmaking server written in Rust. It acts as a signaling service that helps game clients discover each other's network addresses so they can establish direct peer-to-peer connections (e.g. UDP hole-punching).

## How It Works

1. **Host creates a game** — sends their external and local addresses (gathered from a STUN service) to `POST /game` and receives a `game_id` (for heartbeat polling) and a `token` (to share with other players).
2. **Player joins** — sends `POST /join/{token}` with their address. The server returns the host's address. The joiner is queued for the host to pick up.
3. **Host polls for joiners** — sends `POST /heartbeat/{game_id}` periodically to get the list of pending joiners and keep the game alive.
4. **Direct connection** — once both sides have each other's addresses, they connect peer-to-peer and the matchmaking server is no longer involved.

All state is held in-memory (no database). Games expire after 60 seconds without a heartbeat. The server supports rate limiting, Prometheus metrics, and is designed to run behind Cloudflare.

### API

| Method | Path                   | Description                        |
| ------ | ---------------------- | ---------------------------------- |
| `GET`  | `/ping`                | Health check                       |
| `POST` | `/game`                | Create a new game                  |
| `POST` | `/join/{token}`        | Join an existing game              |
| `POST` | `/heartbeat/{game_id}` | Host polls for new joiners         |
| `GET`  | `/metrics`             | Prometheus metrics (requires auth) |

## Run

```sh
IP_SOURCE=ConnectInfo RATE_LIMIT=false cargo run
```

```sh
curl -X POST -H "Content-Type: application/json" -d '{"external_address": "127.0.0.1:123", "local_address": "127.0.0.1:123"}' http://localhost:3000/game

xh post localhost:3000/game external_address=127.0.0.1:123 local_address=127.0.0.1:123
```

## STUN server

See [coturn-docker](https://github.com/eero-lehtinen/coturn-docker) for running a STUN server alongside this matchmaking server.
