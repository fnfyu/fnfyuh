#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

: "${HARNESS_WORKSPACE:=$ROOT_DIR}"
: "${NPM_REGISTRY:=https://registry.npmmirror.com}"
: "${CARGO_MIRROR:=sparse+https://rsproxy.cn/index/}"
export HARNESS_WORKSPACE NPM_REGISTRY CARGO_MIRROR

if ! docker info >/dev/null 2>&1; then
  if command -v service >/dev/null 2>&1 && sudo -n service docker start >/dev/null 2>&1; then
    :
  else
    printf '%s\n' 'Docker is unavailable. Start Docker Desktop WSL integration or a native WSL Docker daemon.' >&2
    exit 1
  fi
fi

docker info >/dev/null
printf '%s\n' '==> Building and testing the pinned Rust/Node image'
docker compose build --pull harnessd

printf '%s\n' '==> Running daemon health check through the CLI container'
docker compose --profile cli run --rm cli health

printf '%s\n' 'Container build already ran SDK and Rust workspace tests in the builder stage.'
printf '%s\n' 'Container verification completed.'
