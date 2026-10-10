#!/bin/bash
# Deploys (or updates) a Drawlycool server with Docker.
#
#   With HTTPS on your own domain (DNS pointing here, ports 80 and 443 open):
#     curl -fsSL https://raw.githubusercontent.com/rafa-rrayes/Drawlycool/main/deploy.sh | sudo bash -s -- --domain draw.example.com
#
#   Plain HTTP on port 4180, for a LAN or VPN:
#     curl -fsSL https://raw.githubusercontent.com/rafa-rrayes/Drawlycool/main/deploy.sh | sudo bash
#
# Run it again to update to the latest server. Canvases and the access token are kept.
set -euo pipefail

usage() {
  cat <<'EOF'
Options:
  --domain <name>   Serve HTTPS on this domain (automatic certificate via Caddy)
  --name <name>     Server name people see in the app (default: Drawlycool)
  --url <url>       Address people use to reach the server, if not the detected one
  --port <port>     HTTP port when not using --domain (default: 4180)
  --dir <dir>       Where the deployment lives (default: /opt/drawlycool)
  --reset-token     Issue a new access token; old invite links stop working
EOF
}

DOMAIN=""
NAME="Drawlycool"
URL=""
PORT="4180"
DIR="/opt/drawlycool"
RESET_TOKEN=0
IMAGE="${DRAWLYCOOL_IMAGE:-ghcr.io/rafa-rrayes/drawlycool-server:latest}"

fail() { echo "error: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --domain) DOMAIN="${2:?--domain needs a value}"; shift 2 ;;
    --name) NAME="${2:?--name needs a value}"; shift 2 ;;
    --url) URL="${2:?--url needs a value}"; shift 2 ;;
    --port) PORT="${2:?--port needs a value}"; shift 2 ;;
    --dir) DIR="${2:?--dir needs a value}"; shift 2 ;;
    --reset-token) RESET_TOKEN=1; shift ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; fail "unknown option $1" ;;
  esac
done

# ------------------------------------------------------------------ docker

if ! command -v docker >/dev/null 2>&1; then
  if [ "$(uname -s)" = Linux ] && [ "$(id -u)" = 0 ]; then
    echo "Installing Docker (get.docker.com)…"
    curl -fsSL https://get.docker.com | sh
  else
    fail "Docker is required. Install it first: https://docs.docker.com/engine/install/"
  fi
fi
docker info >/dev/null 2>&1 || fail "can't talk to Docker. Is it running, and are you root (sudo)?"
docker compose version >/dev/null 2>&1 || fail "the Docker Compose plugin is required (docker compose)"

# ------------------------------------------------------------------ config

if [ -n "$DOMAIN" ]; then
  URL="https://$DOMAIN"
elif [ -z "$URL" ]; then
  ip="$(hostname -I 2>/dev/null | awk '{print $1}')"
  [ -n "$ip" ] || ip="$(ipconfig getifaddr en0 2>/dev/null || true)"
  URL="http://${ip:-127.0.0.1}:$PORT"
fi
URL="${URL%/}"

mkdir -p "$DIR"
cd "$DIR"

cat > .env <<EOF
DRAWLYCOOL_NAME=$NAME
DRAWLYCOOL_PUBLIC_URL=$URL
DOMAIN=$DOMAIN
PORT=$PORT
EOF

{
  cat <<EOF
# Written by deploy.sh; edit the options and re-run it instead.
name: drawlycool
services:
  server:
    image: $IMAGE
    restart: unless-stopped
    environment:
      DRAWLYCOOL_NAME: \${DRAWLYCOOL_NAME}
      DRAWLYCOOL_PUBLIC_URL: \${DRAWLYCOOL_PUBLIC_URL}
    volumes:
      - data:/data
EOF
  if [ -n "$DOMAIN" ]; then
    cat <<'EOF'

  caddy:
    image: caddy:2
    restart: unless-stopped
    environment:
      DOMAIN: ${DOMAIN}
    ports:
      - "80:80"
      - "443:443"
      - "443:443/udp"
    volumes:
      - ./Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy_data:/data
      - caddy_config:/config
    depends_on:
      - server

volumes:
  data:
  caddy_data:
  caddy_config:
EOF
  else
    cat <<'EOF'
    ports:
      - "${PORT}:4180"

volumes:
  data:
EOF
  fi
} > compose.yml

if [ -n "$DOMAIN" ]; then
  cat > Caddyfile <<'EOF'
{$DOMAIN} {
	reverse_proxy server:4180
}
EOF
else
  rm -f Caddyfile
fi

# ------------------------------------------------------------------ run

case "$IMAGE" in
  */*) echo "Pulling the latest images…"; docker compose pull --quiet ;;
esac

if [ "$RESET_TOKEN" = 1 ]; then
  echo "Resetting the access token…"
  docker compose stop server >/dev/null 2>&1 || true
  docker run --rm -v drawlycool_data:/data busybox rm -f /data/token
fi

docker compose up -d --remove-orphans

invite=""
for _ in $(seq 1 30); do
  invite="$(docker compose logs server 2>/dev/null | grep -o 'drawlycool://[^[:space:]]*' | tail -1 || true)"
  [ -n "$invite" ] && break
  sleep 1
done
[ -n "$invite" ] || fail "the server didn't start; see: docker compose -f $DIR/compose.yml logs server"
( umask 077; echo "$invite" > invite.txt )

cat <<EOF

Drawlycool server is running at $URL

Invite link (anyone with it can open and edit every canvas; share it like a password):

  $invite

In the app: Welcome screen → Connect… → paste the link.

  Update:        re-run this command
  Invite link:   cat $DIR/invite.txt
  Logs:          docker compose -f $DIR/compose.yml logs -f
  Stop:          docker compose -f $DIR/compose.yml down
EOF
if [ -n "$DOMAIN" ]; then
  echo
  echo "HTTPS: $DOMAIN must point at this machine, with ports 80 and 443 reachable, for the certificate to be issued."
else
  echo
  echo "Traffic is plain HTTP. Keep it on a trusted network or VPN, or re-run with --domain for HTTPS."
fi
