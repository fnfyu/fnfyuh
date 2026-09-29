# WSL2 + Docker 成品环境

## 推荐环境

- Windows 11/10 with WSL2
- Debian 12 或 Ubuntu 22.04+ WSL distribution
- Docker Desktop with **Use the WSL 2 based engine** enabled
- Docker Desktop Settings → Resources → WSL Integration → enable the working distro
- 4 CPU / 8 GB RAM recommended; at least 10 GB free disk
- Git checkout preferably under the WSL filesystem (`~/src/...`) for better bind-mount performance

The **builder stage** contains Node 22/npm and Rust 1.85.1; it runs SDK tests, SDK build, web asset checks, `cargo test --workspace`, and release compilation before producing the slim runtime image. The final runtime keeps Node plus the daemon, CLI, gateway, Web/IDE client, and SDK, not Cargo or npm development dependencies. The runtime uses the standard Node UID 1000 so WSL bind-mounted workspaces remain writable in the usual WSL user setup.

## First run from WSL

```bash
cd ~/src/fnfyuh
cp -n .env.example .env || true
docker info >/dev/null 2>&1 || sudo service docker start
docker info >/dev/null
export HARNESS_WORKSPACE="$PWD"

docker compose build --pull harnessd
bash scripts/wsl-docker-verify.sh
```

If the repository is on a Windows drive, use the WSL path (`/mnt/d/...`) for `HARNESS_WORKSPACE`; do not pass `D:\...` to Compose.

## Use the finished image

```bash
# Health
HARNESS_WORKSPACE="$PWD" docker compose --profile cli run --rm cli health

# Create a session rooted at the mounted workspace
HARNESS_WORKSPACE="$PWD" docker compose --profile cli run --rm cli create /workspace

# Replay/resume with the returned session id
HARNESS_WORKSPACE="$PWD" docker compose --profile cli run --rm cli replay <session-id>

# Start the daemon directly for another client
HARNESS_WORKSPACE="$PWD" docker compose run --rm harnessd

# Start the browser gateway (open http://127.0.0.1:8787)
HARNESS_WORKSPACE="$PWD" docker compose --profile web up gateway
```

The SQLite event database is stored in the named `harness-data` volume. To inspect or reset local data:

```bash
docker volume ls | grep harness-data
docker compose down -v   # destructive: removes the local event database
```

## Useful commands

```bash
# Rebuild after Rust or protocol changes
docker compose build --pull --no-cache harnessd

# Show the image metadata
docker image inspect local-first-harness:dev

# Open a shell in the runtime image
docker compose run --rm --entrypoint /bin/bash harnessd
```

## Security posture

This image makes builds reproducible; the default trusted-host adapter is still **not** a hostile-agent sandbox. To use the real container adapter, set `HARNESS_EXECUTION_BACKEND=container`, provide an engine-visible immutable image, and ensure the daemon can reach that engine. The standard runtime image intentionally has no Docker CLI/socket; prefer a dedicated/rootless runner. `vm` means Docker Hyper-V isolation and requires a compatible Windows-container image/engine. Backend preflight failure is fail-closed, never a host fallback. The event database, leased outbox, artifacts, and recovery state are durable; delivery remains at-least-once.
