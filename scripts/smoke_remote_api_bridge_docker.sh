#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HERDR_BIN="${HERDR_BIN:-$ROOT/target/debug/herdr}"
BASE_WAS_SET=0
if [[ -n "${BASE:-}" ]]; then
  BASE_WAS_SET=1
else
  BASE="$(mktemp -d /tmp/herdr-remote-api-smoke.XXXXXX)"
fi

ALIAS="herdr-fed-docker"
SESSION="fed-remote-api"
LOCAL_HOME="$BASE/local-home"
LOCAL_CONFIG="$BASE/local-config"
LOCAL_STATE="$BASE/local-state"
LOCAL_RUNTIME="$BASE/runtime"
SSH_DIR="$LOCAL_HOME/.ssh"
WRAPPER_DIR="$BASE/bin"
KEY_PATH="$BASE/id_ed25519"
IMAGE_TAG="herdr-remote-api-smoke:$(basename "$BASE" | tr -c '[:alnum:]_.-' '-')"
CONTAINER_NAME="herdr-remote-api-smoke-$$"
PORT=""

require_command() {
  local name="$1"
  if ! command -v "$name" >/dev/null 2>&1; then
    echo "error: required command not found: $name" >&2
    exit 1
  fi
}

case "$BASE" in
  /tmp/herdr-remote-api-smoke.*)
    ;;
  *)
    if [[ "$BASE_WAS_SET" -eq 1 ]]; then
      if [[ -z "$BASE" || "$BASE" == "/" || "$BASE" == "$HOME" || "$BASE" == "$HOME/"* ]]; then
        echo "error: refusing unsafe BASE override: $BASE" >&2
        exit 1
      fi
      echo "warning: using explicit BASE override outside /tmp/herdr-remote-api-smoke.*: $BASE" >&2
    else
      echo "error: refusing unsafe generated BASE path: $BASE" >&2
      exit 1
    fi
    ;;
esac

for path in "$LOCAL_HOME" "$LOCAL_CONFIG" "$LOCAL_STATE" "$LOCAL_RUNTIME" "$WRAPPER_DIR"; do
  case "$path" in
    "$BASE"/*)
      ;;
    *)
      echo "error: refusing local path outside BASE: $path" >&2
      exit 1
      ;;
  esac
done

cleanup() {
  set +e
  if [[ -n "${CONTAINER_STARTED:-}" ]]; then
    docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
  fi
  if [[ -n "${IMAGE_BUILT:-}" ]]; then
    docker image rm "$IMAGE_TAG" >/dev/null 2>&1 || true
  fi
  if [[ "$BASE_WAS_SET" -eq 0 && "$BASE" == /tmp/herdr-remote-api-smoke.* ]]; then
    rm -rf "$BASE"
  fi
}
trap cleanup EXIT

require_command cargo
require_command docker
require_command ssh
REAL_SSH="$(command -v ssh)"
require_command ssh-keygen
require_command python3

mkdir -p "$BASE" "$SSH_DIR" "$WRAPPER_DIR" "$LOCAL_CONFIG" "$LOCAL_STATE" "$LOCAL_RUNTIME"
chmod 700 "$SSH_DIR" "$WRAPPER_DIR" "$LOCAL_RUNTIME"

if [[ "$HERDR_BIN" != "$ROOT/"* && "$HERDR_BIN" != /tmp/herdr-remote-api-smoke.*/* ]]; then
  echo "warning: HERDR_BIN is outside repo/temp paths: $HERDR_BIN" >&2
fi

cargo build --locked --manifest-path "$ROOT/Cargo.toml"
if [[ ! -x "$HERDR_BIN" ]]; then
  echo "error: Herdr binary is missing or not executable: $HERDR_BIN" >&2
  exit 1
fi

rm -f "$KEY_PATH" "$KEY_PATH.pub"
ssh-keygen -q -t ed25519 -N "" -f "$KEY_PATH" -C "herdr-remote-api-smoke" >/dev/null
chmod 600 "$KEY_PATH"

cat >"$BASE/Dockerfile" <<'DOCKERFILE'
FROM debian:unstable-slim
RUN apt-get update \
  && apt-get install -y --no-install-recommends openssh-server ca-certificates bash \
  && rm -rf /var/lib/apt/lists/*
RUN useradd -m -s /bin/bash fed \
  && mkdir -p /run/sshd /home/fed/.ssh \
  && chmod 700 /home/fed/.ssh
COPY id_ed25519.pub /home/fed/.ssh/authorized_keys
RUN chown -R fed:fed /home/fed/.ssh \
  && chmod 600 /home/fed/.ssh/authorized_keys
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
DOCKERFILE

docker build -q -t "$IMAGE_TAG" "$BASE" >/dev/null
IMAGE_BUILT=1

PORT="$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)"

docker run -d --rm \
  --name "$CONTAINER_NAME" \
  -p "127.0.0.1:$PORT:22" \
  --mount "type=bind,src=$HERDR_BIN,dst=/usr/local/bin/herdr,readonly" \
  "$IMAGE_TAG" >/dev/null
CONTAINER_STARTED=1

cat >"$SSH_DIR/config" <<EOF
Host $ALIAS
  HostName 127.0.0.1
  Port $PORT
  User fed
  IdentityFile $KEY_PATH
  IdentitiesOnly yes
  BatchMode yes
  StrictHostKeyChecking no
  UserKnownHostsFile $BASE/known_hosts
  ConnectTimeout 5
  ConnectionAttempts 1
  LogLevel ERROR
EOF
chmod 600 "$SSH_DIR/config"

cat >"$WRAPPER_DIR/ssh" <<EOF
#!/usr/bin/env bash
exec "$REAL_SSH" -F "$SSH_DIR/config" "\$@"
EOF
chmod 700 "$WRAPPER_DIR/ssh"

for _ in {1..120}; do
  if PATH="$WRAPPER_DIR:$PATH" HOME="$LOCAL_HOME" ssh -T "$ALIAS" true >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done
if ! PATH="$WRAPPER_DIR:$PATH" HOME="$LOCAL_HOME" ssh -T "$ALIAS" true >/dev/null 2>&1; then
  echo "error: SSH server in Docker container did not become reachable" >&2
  exit 1
fi

ping_output="$(env \
  -u HERDR_SOCKET_PATH \
  -u HERDR_CLIENT_SOCKET_PATH \
  -u HERDR_SESSION \
  PATH="$WRAPPER_DIR:$PATH" \
  HOME="$LOCAL_HOME" \
  XDG_CONFIG_HOME="$LOCAL_CONFIG" \
  XDG_STATE_HOME="$LOCAL_STATE" \
  XDG_RUNTIME_DIR="$LOCAL_RUNTIME" \
  "$HERDR_BIN" --session "$SESSION" remote-api-ping "$ALIAS")"

python3 - "$ping_output" <<'PY'
import json
import sys

payload = json.loads(sys.argv[1])
if payload.get("id") != "remote-api-ping":
    raise SystemExit(f"unexpected response id: {payload.get('id')!r}")
result = payload.get("result") or {}
if result.get("type") != "pong":
    raise SystemExit(f"unexpected response result.type: {result.get('type')!r}")
PY

workspace_json="$(PATH="$WRAPPER_DIR:$PATH" HOME="$LOCAL_HOME" ssh -T "$ALIAS" \
  "/usr/local/bin/herdr --session $SESSION workspace create --cwd /tmp --focus")"

pane_id="$(python3 - "$workspace_json" <<'PY'
import json
import sys

payload = json.loads(sys.argv[1])
try:
    print(payload["result"]["root_pane"]["pane_id"])
except KeyError as exc:
    raise SystemExit(f"workspace create response missing {exc}: {payload}")
PY
)"

PATH="$WRAPPER_DIR:$PATH" HOME="$LOCAL_HOME" ssh -T "$ALIAS" \
  "/usr/local/bin/herdr --session $SESSION pane report-agent $pane_id --source smoke --agent smoke-agent --state working" \
  >/dev/null

agent_list_output="$(env \
  -u HERDR_SOCKET_PATH \
  -u HERDR_CLIENT_SOCKET_PATH \
  -u HERDR_SESSION \
  PATH="$WRAPPER_DIR:$PATH" \
  HOME="$LOCAL_HOME" \
  XDG_CONFIG_HOME="$LOCAL_CONFIG" \
  XDG_STATE_HOME="$LOCAL_STATE" \
  XDG_RUNTIME_DIR="$LOCAL_RUNTIME" \
  "$HERDR_BIN" --session "$SESSION" remote-api-agent-list "$ALIAS")"

python3 - "$agent_list_output" <<'PY'
import json
import sys

payload = json.loads(sys.argv[1])
if payload.get("id") != "remote-api-agent-list":
    raise SystemExit(f"unexpected response id: {payload.get('id')!r}")
result = payload.get("result") or {}
if result.get("type") != "agent_list":
    raise SystemExit(f"unexpected response result.type: {result.get('type')!r}")
agents = result.get("agents") or []
for agent in agents:
    labels = {
        agent.get("agent"),
        agent.get("display_agent"),
        agent.get("name"),
        agent.get("title"),
    }
    status = agent.get("agent_status") or agent.get("status")
    if "smoke-agent" in labels and status == "working":
        break
else:
    raise SystemExit(f"smoke-agent working entry not found in agents: {agents}")
PY

echo "remote API bridge Docker smoke passed: ping and agent-list probes succeeded"
