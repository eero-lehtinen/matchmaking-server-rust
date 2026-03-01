# Matchmaking Server

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
