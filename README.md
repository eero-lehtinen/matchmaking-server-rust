## Init with Dokku

```sh
## Init
git remote add prod dokku@94.177.9.119:gaming-gamers-matchmaking
git remote add staging dokku@94.177.9.119:gaming-gamers-matchmaking-staging
```

## Deploy to Dokku

```sh
git push staging master
```

## Run

```sh
IP_SOURCE=ConnectInfo RATE_LIMIT=false cargo run
```

```sh
curl -X POST -H "Content-Type: application/json" -d '{"external_address": "127.0.0.1:123", "local_address": "127.0.0.1:123"}' http://localhost:3000/game
```

## Turn server

Put `coturn.service` in `/etc/systemd/system/`, `turnserver.conf` in `/etc/` and run:

```sh
sudo apt install coturn
sudo systemctl start coturn
```

Actually I use the server in `stun-only` mode, because I don't want use TURN.
