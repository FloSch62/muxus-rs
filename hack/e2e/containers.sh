#!/usr/bin/env bash
# Test servers for hack/e2e, as Docker containers named muxusrs-e2e-*, with
# every published port bound to 127.0.0.1.
#
#   hack/e2e/containers.sh up <authorized-public-key-file>
#   hack/e2e/containers.sh down [--images]
#
# Servers (host port → service):
#   22201  ssh      OpenSSH: alice (password alice-pass or key), kbd
#                   (keyboard-interactive only, kbd-pass), nofwd (no TCP
#                   forwarding); web page on the container's 127.0.0.1:8080
#   22202  jump     OpenSSH jump host (same users); also on 22203, which the
#                   scenarios leave out of known_hosts (the host-key question)
#   -      inner    OpenSSH reachable only through the jump host (hostname
#                   "inner" on the e2e network), web page on every interface
#   22323  telnet   busybox telnetd, login tuser / tuser-pass
#   25900  vnc      TigerVNC, password vnc-pass
#   23389  rdp      xrdp, login rdpuser / rdp-pass
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
net=muxusrs-e2e-net
images=(sshd telnet vnc rdp)
containers=(muxusrs-e2e-ssh muxusrs-e2e-jump muxusrs-e2e-inner muxusrs-e2e-telnet muxusrs-e2e-vnc muxusrs-e2e-rdp)
# Builds write to DOCKER_CONFIG; give them a writable one when the default is not.
if [[ -z "${DOCKER_CONFIG:-}" ]] && ! { mkdir -p "$HOME/.docker/buildx" 2>/dev/null && [[ -w "$HOME/.docker/buildx" ]]; }; then
  export DOCKER_CONFIG="${TMPDIR:-/tmp}/muxusrs-e2e-docker"
  mkdir -p "$DOCKER_CONFIG"
fi

build() {
  local name
  for name in "${images[@]}"; do
    [[ -n "${E2E_SKIP_RDP:-}" && $name == rdp ]] && continue
    docker build -q -t "muxusrs-e2e-$name:latest" "$here/containers/$name" >/dev/null
  done
}

wait_port() {
  local port=$1 deadline=$((SECONDS + 60))
  until (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; do
    ((SECONDS < deadline)) || { echo "port $port did not open" >&2; return 1; }
    sleep 0.5
  done
}

up() {
  local key="$1"
  [[ -f "$key" ]] || { echo "no public key at $key" >&2; exit 2; }
  down_containers
  build
  docker network create "$net" >/dev/null
  local keys
  keys="$(cat "$key")"
  docker run -d --name muxusrs-e2e-ssh --hostname e2e-ssh --network "$net" \
    -p 127.0.0.1:22201:22 -e AUTHORIZED_KEYS="$keys" -e HTTP=1 muxusrs-e2e-sshd:latest >/dev/null
  docker run -d --name muxusrs-e2e-jump --hostname e2e-jump --network "$net" \
    -p 127.0.0.1:22202:22 -p 127.0.0.1:22203:22 -e AUTHORIZED_KEYS="$keys" muxusrs-e2e-sshd:latest >/dev/null
  docker run -d --name muxusrs-e2e-inner --hostname inner --network "$net" --network-alias inner \
    -e AUTHORIZED_KEYS="$keys" -e HTTP=all muxusrs-e2e-sshd:latest >/dev/null
  docker run -d --name muxusrs-e2e-telnet --network "$net" -p 127.0.0.1:22323:23 muxusrs-e2e-telnet:latest >/dev/null
  docker run -d --name muxusrs-e2e-vnc --network "$net" -p 127.0.0.1:25900:5900 muxusrs-e2e-vnc:latest >/dev/null
  if [[ -z "${E2E_SKIP_RDP:-}" ]]; then
    docker run -d --name muxusrs-e2e-rdp --network "$net" -p 127.0.0.1:23389:3389 muxusrs-e2e-rdp:latest >/dev/null
  fi
  local port
  for port in 22201 22202 22323 25900; do wait_port "$port"; done
  [[ -n "${E2E_SKIP_RDP:-}" ]] || wait_port 23389
}

down_containers() {
  local name
  for name in "${containers[@]}"; do
    docker rm -f "$name" >/dev/null 2>&1 || true
  done
  docker network rm "$net" >/dev/null 2>&1 || true
}

case "${1:-}" in
  up) up "${2:-}" ;;
  down)
    down_containers
    if [[ "${2:-}" == --images ]]; then
      for name in "${images[@]}"; do docker rmi "muxusrs-e2e-$name:latest" >/dev/null 2>&1 || true; done
    fi
    ;;
  build) build ;;
  *) sed -n '2,20p' "$0"; exit 2 ;;
esac
