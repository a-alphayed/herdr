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
REMOTE_SESSION="fed-remote-api"
LOCAL_SESSION="fed-local-api"
LOCAL_HOME="$BASE/local-home"
LOCAL_CONFIG="$BASE/local-config"
LOCAL_CONFIG_FILE="$BASE/local-config.toml"
LOCAL_STATE="$BASE/local-state"
LOCAL_RUNTIME="$BASE/runtime"
LOCAL_SERVER_LOG="$BASE/local-server.log"
SSH_DIR="$LOCAL_HOME/.ssh"
WRAPPER_DIR="$BASE/bin"
KEY_PATH="$BASE/id_ed25519"
IMAGE_TAG="herdr-remote-api-smoke:$(basename "$BASE" | tr -c '[:alnum:]_.-' '-')"
CONTAINER_NAME="herdr-remote-api-smoke-$$"
PORT=""
LOCAL_SERVER_PID=""

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

for path in "$LOCAL_HOME" "$LOCAL_CONFIG" "$LOCAL_STATE" "$LOCAL_RUNTIME" "$WRAPPER_DIR" "$LOCAL_CONFIG_FILE" "$LOCAL_SERVER_LOG"; do
  case "$path" in
    "$BASE"/* | "$BASE")
      ;;
    *)
      echo "error: refusing local path outside BASE: $path" >&2
      exit 1
      ;;
  esac
done

run_herdr() {
  local session="$1"
  shift
  env \
    -u HERDR_SOCKET_PATH \
    -u HERDR_CLIENT_SOCKET_PATH \
    -u HERDR_SESSION \
    PATH="$WRAPPER_DIR:$PATH" \
    HOME="$LOCAL_HOME" \
    XDG_CONFIG_HOME="$LOCAL_CONFIG" \
    XDG_STATE_HOME="$LOCAL_STATE" \
    XDG_RUNTIME_DIR="$LOCAL_RUNTIME" \
    HERDR_CONFIG_PATH="$LOCAL_CONFIG_FILE" \
    "$HERDR_BIN" --session "$session" "$@"
}

run_remote_ssh() {
  PATH="$WRAPPER_DIR:$PATH" HOME="$LOCAL_HOME" ssh -T "$ALIAS" "$@"
}

cleanup() {
  set +e
  if [[ -n "$LOCAL_SERVER_PID" ]]; then
    run_herdr "$LOCAL_SESSION" server stop >/dev/null 2>&1 || true
    for _ in {1..40}; do
      if ! kill -0 "$LOCAL_SERVER_PID" >/dev/null 2>&1; then
        break
      fi
      sleep 0.05
    done
    if kill -0 "$LOCAL_SERVER_PID" >/dev/null 2>&1; then
      kill "$LOCAL_SERVER_PID" >/dev/null 2>&1 || true
    fi
    wait "$LOCAL_SERVER_PID" >/dev/null 2>&1 || true
  fi
  docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
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

start_container() {
  docker run -d --rm \
    --name "$CONTAINER_NAME" \
    -p "127.0.0.1:$PORT:22" \
    --mount "type=bind,src=$HERDR_BIN,dst=/usr/local/bin/herdr,readonly" \
    "$IMAGE_TAG" >/dev/null
}

wait_for_ssh() {
  for _ in {1..120}; do
    if run_remote_ssh true >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  echo "error: SSH server in Docker container did not become reachable" >&2
  return 1
}

start_container

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

wait_for_ssh

cat >"$LOCAL_CONFIG_FILE" <<EOF
[remote]
enabled = true

[[remote.hosts]]
name = "$ALIAS"
target = "$ALIAS"
session = "$REMOTE_SESSION"
auto_connect = true
EOF

create_remote_smoke_agent() {
  local workspace_json pane_id
  workspace_json="$(run_remote_ssh "/usr/local/bin/herdr --session $REMOTE_SESSION workspace create --cwd /tmp --focus")"
  pane_id="$(python3 - "$workspace_json" <<'PY'
import json
import sys

text = sys.argv[1]
decoder = json.JSONDecoder()
idx = 0
payload = None
while idx < len(text):
    idx = text.find("{", idx)
    if idx == -1:
        break
    try:
        value, idx = decoder.raw_decode(text, idx)
    except json.JSONDecodeError:
        idx += 1
        continue
    if value.get("id") == "cli:workspace:create" or value.get("result", {}).get("root_pane"):
        payload = value
if payload is None:
    raise SystemExit(f"workspace create response not found in: {text!r}")
try:
    print(payload["result"]["root_pane"]["pane_id"])
except KeyError as exc:
    raise SystemExit(f"workspace create response missing {exc}: {payload}")
PY
)"

  run_remote_ssh "/usr/local/bin/herdr --session $REMOTE_SESSION pane report-agent $pane_id --source smoke --agent smoke-agent --state working" >/dev/null
}

AGENT_STATUS_HELPER="$BASE/agent_status_helper.py"
cat >"$AGENT_STATUS_HELPER" <<'PY'
import json
import sys

# Shared non-connected classifier: any custom_status Herdr surfaces for a
# stale/non-connected cached remote agent entry (see
# RemoteConnectionStatus::stale_label in src/remote_source.rs). Both the
# shutdown-phase wait (accept any of these) and the reconnect-phase wait
# (reject any of these) must use this same set so a status like
# "unreachable" is never mistaken for connected.
NON_CONNECTED_STATUSES = {"disconnected", "unreachable", "needs update"}


def parse_payload(text):
    decoder = json.JSONDecoder()
    idx = 0
    payload = None
    while idx < len(text):
        idx = text.find("{", idx)
        if idx == -1:
            break
        try:
            payload, idx = decoder.raw_decode(text, idx)
        except json.JSONDecodeError:
            idx += 1
            continue
    return payload


def find_agent_custom_status(text, expected_name):
    # Returns (found, custom_status). "found" is True only when an agent
    # entry matching expected_name exists; custom_status is whatever value
    # (including None/absent) that entry carries. Callers must not conflate
    # "not found" with "found but custom_status is None" -- a truly
    # connected agent typically has no custom_status at all.
    payload = parse_payload(text)
    if payload is None:
        return False, None
    for agent in (payload.get("result") or {}).get("agents") or []:
        labels = {
            agent.get("agent"),
            agent.get("display_agent"),
            agent.get("name"),
            agent.get("title"),
        }
        if expected_name in labels:
            return True, agent.get("custom_status")
    return False, None


if __name__ == "__main__":
    mode = sys.argv[1]
    text = sys.argv[2]
    expected_name = sys.argv[3]
    found, status = find_agent_custom_status(text, expected_name)
    if mode == "non-connected":
        # Shutdown phase: succeed only once the agent is present AND its
        # cached status is one of the recognized non-connected statuses.
        raise SystemExit(0 if found and status in NON_CONNECTED_STATUSES else 1)
    elif mode == "connected":
        # Reconnect phase: succeed only once the agent is present AND its
        # cached status is absent/None or any value NOT in the recognized
        # non-connected set. Symmetric with "non-connected" above so
        # "unreachable" is never wrongly accepted as connected.
        raise SystemExit(0 if found and status not in NON_CONNECTED_STATUSES else 1)
    else:
        raise SystemExit(f"unknown agent_status_helper mode: {mode!r}")
PY

json_has_agent() {
  local payload="$1"
  local expected_name="$2"
  python3 - "$payload" "$expected_name" <<'PY'
import json
import sys

text = sys.argv[1]
expected_name = sys.argv[2]
decoder = json.JSONDecoder()
idx = 0
payload = None
while idx < len(text):
    idx = text.find("{", idx)
    if idx == -1:
        break
    try:
        payload, idx = decoder.raw_decode(text, idx)
    except json.JSONDecodeError:
        idx += 1
        continue
if payload is None:
    raise SystemExit(1)
result = payload.get("result") or {}
agents = result.get("agents") or []
for agent in agents:
    labels = {
        agent.get("agent"),
        agent.get("display_agent"),
        agent.get("name"),
        agent.get("title"),
    }
    if expected_name in labels:
        raise SystemExit(0)
raise SystemExit(1)
PY
}

json_agent_count() {
  local payload="$1"
  local expected_name="$2"
  python3 - "$payload" "$expected_name" <<'PY'
import json
import sys

text = sys.argv[1]
expected_name = sys.argv[2]
decoder = json.JSONDecoder()
idx = 0
payload = None
while idx < len(text):
    idx = text.find("{", idx)
    if idx == -1:
        break
    try:
        payload, idx = decoder.raw_decode(text, idx)
    except json.JSONDecodeError:
        idx += 1
        continue
if payload is None:
    raise SystemExit(1)
result = payload.get("result") or {}
agents = result.get("agents") or []
count = 0
for agent in agents:
    labels = {
        agent.get("agent"),
        agent.get("display_agent"),
        agent.get("name"),
        agent.get("title"),
    }
    if expected_name in labels:
        count += 1
print(count)
PY
}

wait_for_local_server() {
  for _ in {1..100}; do
    local status_json
    status_json="$(run_herdr "$LOCAL_SESSION" status server --json 2>/dev/null || true)"
    if python3 - "$status_json" <<'PY' >/dev/null 2>&1
import json
import sys
text = sys.argv[1]
decoder = json.JSONDecoder()
idx = 0
payload = None
while idx < len(text):
    idx = text.find("{", idx)
    if idx == -1:
        break
    try:
        payload, idx = decoder.raw_decode(text, idx)
    except json.JSONDecodeError:
        idx += 1
        continue
raise SystemExit(0 if payload and payload.get("status") == "running" else 1)
PY
    then
      return 0
    fi
    sleep 0.1
  done
  echo "error: local Herdr server did not become reachable; log follows" >&2
  sed -n '1,200p' "$LOCAL_SERVER_LOG" >&2 || true
  return 1
}

wait_for_local_agent() {
  local expected_name="$1"
  local timeout_seconds="$2"
  local deadline=$((SECONDS + timeout_seconds))
  while (( SECONDS < deadline )); do
    local list_json
    list_json="$(run_herdr "$LOCAL_SESSION" agent list 2>/dev/null || true)"
    if [[ -n "$list_json" ]] && json_has_agent "$list_json" "$expected_name"; then
      printf '%s' "$list_json"
      return 0
    fi
    sleep 1
  done
  echo "error: local agent list did not contain $expected_name within ${timeout_seconds}s" >&2
  return 1
}

wait_for_local_agent_disconnected() {
  # Despite the name, this accepts any recognized non-connected custom_status
  # (disconnected, unreachable, needs update): a forcibly removed container
  # can surface any of these depending on how the SSH bridge observes the
  # teardown, and all three are equally valid proof the shutdown phase took
  # effect.
  local expected_name="$1"
  local timeout_seconds="$2"
  local deadline=$((SECONDS + timeout_seconds))
  while (( SECONDS < deadline )); do
    local list_json
    list_json="$(run_herdr "$LOCAL_SESSION" agent list 2>/dev/null || true)"
    if [[ -n "$list_json" ]] && python3 "$AGENT_STATUS_HELPER" non-connected "$list_json" "$expected_name"
    then
      printf '%s' "$list_json"
      return 0
    fi
    sleep 1
  done
  echo "error: local agent list did not mark $expected_name non-connected (disconnected/unreachable/needs update) within ${timeout_seconds}s" >&2
  return 1
}

wait_for_single_connected_local_agent() {
  local expected_name="$1"
  local timeout_seconds="$2"
  local deadline=$((SECONDS + timeout_seconds))
  while (( SECONDS < deadline )); do
    local list_json count
    list_json="$(run_herdr "$LOCAL_SESSION" agent list 2>/dev/null || true)"
    if [[ -n "$list_json" ]]; then
      count="$(json_agent_count "$list_json" "$expected_name")"
      # Symmetric with wait_for_local_agent_disconnected's non-connected
      # check above: reject ANY recognized non-connected status here, not
      # only the literal "disconnected" string, so "unreachable" is never
      # wrongly accepted as connected.
      if [[ "$count" == "1" ]] && python3 "$AGENT_STATUS_HELPER" connected "$list_json" "$expected_name"
      then
        printf '%s' "$list_json"
        return 0
      fi
    fi
    sleep 1
  done
  echo "error: local agent list did not contain one connected $expected_name within ${timeout_seconds}s" >&2
  return 1
}

json_read_contains() {
  local payload="$1"
  local needle="$2"
  python3 - "$payload" "$needle" <<'PY'
import json
import sys

raw = sys.argv[1]
needle = sys.argv[2]
decoder = json.JSONDecoder()
idx = 0
payload = None
while idx < len(raw):
    idx = raw.find("{", idx)
    if idx == -1:
        break
    try:
        payload, idx = decoder.raw_decode(raw, idx)
    except json.JSONDecodeError:
        idx += 1
        continue
if payload is None:
    raise SystemExit(1)
text = ((payload.get("result") or {}).get("read") or {}).get("text") or ""
raise SystemExit(0 if needle in text else 1)
PY
}

read_agent_until_contains() {
  local target="$1"
  local needle="$2"
  local timeout_seconds="$3"
  local deadline=$((SECONDS + timeout_seconds))
  local read_output=""
  while (( SECONDS < deadline )); do
    read_output="$(run_herdr "$LOCAL_SESSION" agent read "$target" --source recent-unwrapped --lines 40 2>/dev/null || true)"
    if [[ -n "$read_output" ]] && json_read_contains "$read_output" "$needle"; then
      printf '%s' "$read_output"
      return 0
    fi
    sleep 0.5
  done
  echo "error: remote read for $target did not contain $needle within ${timeout_seconds}s" >&2
  return 1
}

ping_output="$(run_herdr "$REMOTE_SESSION" remote-api-ping "$ALIAS")"

python3 - "$ping_output" <<'PY'
import json
import sys

text = sys.argv[1]
decoder = json.JSONDecoder()
idx = 0
payload = None
while idx < len(text):
    idx = text.find("{", idx)
    if idx == -1:
        break
    try:
        value, idx = decoder.raw_decode(text, idx)
    except json.JSONDecodeError:
        idx += 1
        continue
    if value.get("id") == "remote-api-ping":
        payload = value
if payload is None:
    raise SystemExit(f"remote-api-ping response not found in: {text!r}")
result = payload.get("result") or {}
if result.get("type") != "pong":
    raise SystemExit(f"unexpected response result.type: {result.get('type')!r}")
PY

create_remote_smoke_agent

agent_list_output="$(run_herdr "$REMOTE_SESSION" remote-api-agent-list "$ALIAS")"

python3 - "$agent_list_output" <<'PY'
import json
import sys

text = sys.argv[1]
decoder = json.JSONDecoder()
idx = 0
payload = None
while idx < len(text):
    idx = text.find("{", idx)
    if idx == -1:
        break
    try:
        value, idx = decoder.raw_decode(text, idx)
    except json.JSONDecodeError:
        idx += 1
        continue
    if value.get("id") == "remote-api-agent-list":
        payload = value
if payload is None:
    raise SystemExit(f"remote-api-agent-list response not found in: {text!r}")
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

run_herdr "$LOCAL_SESSION" server >"$LOCAL_SERVER_LOG" 2>&1 &
LOCAL_SERVER_PID="$!"
wait_for_local_server

HOST_AGENT="$ALIAS/smoke-agent"
wait_for_local_agent "$HOST_AGENT" 90 >/dev/null

get_output="$(run_herdr "$LOCAL_SESSION" agent get "$HOST_AGENT")"
python3 - "$get_output" "$HOST_AGENT" <<'PY'
import json
import sys

text = sys.argv[1]
expected_name = sys.argv[2]
decoder = json.JSONDecoder()
idx = 0
payload = None
while idx < len(text):
    idx = text.find("{", idx)
    if idx == -1:
        break
    try:
        payload, idx = decoder.raw_decode(text, idx)
    except json.JSONDecodeError:
        idx += 1
        continue
if payload is None:
    raise SystemExit(f"agent.get response not found in: {text!r}")
agent = (payload.get("result") or {}).get("agent") or {}
labels = {agent.get("agent"), agent.get("display_agent"), agent.get("name"), agent.get("title")}
if expected_name not in labels:
    raise SystemExit(f"host-qualified agent label not found in get response: {agent}")
if not agent.get("terminal_id") or not agent.get("pane_id"):
    raise SystemExit(f"remote ids missing in get response: {agent}")
PY

SMOKE_TEXT="herdr-fed-smoke-$RANDOM"
SEND_TEXT="$(printf "printf '%%s\\\\n' %q\n" "$SMOKE_TEXT")"
run_herdr "$LOCAL_SESSION" agent send "$HOST_AGENT" "$SEND_TEXT" >/dev/null
read_agent_until_contains "$HOST_AGENT" "$SMOKE_TEXT" 20 >/dev/null

STARTED_NAME="started-smoke"
STARTED_HOST_AGENT="$ALIAS/$STARTED_NAME"
STARTED_TEXT="herdr-fed-start-smoke-$RANDOM"
start_output="$(run_herdr "$LOCAL_SESSION" agent start --host "$ALIAS" --name "$STARTED_NAME" --cwd /tmp -- sh -lc "printf '%s\n' '$STARTED_TEXT'; sleep 300")"
python3 - "$start_output" "$STARTED_HOST_AGENT" <<'PY'
import json
import sys

text = sys.argv[1]
expected_name = sys.argv[2]
decoder = json.JSONDecoder()
idx = 0
payload = None
while idx < len(text):
    idx = text.find("{", idx)
    if idx == -1:
        break
    try:
        payload, idx = decoder.raw_decode(text, idx)
    except json.JSONDecodeError:
        idx += 1
        continue
if payload is None:
    raise SystemExit(f"agent.start response not found in: {text!r}")
result = payload.get("result") or {}
if result.get("type") != "agent_started":
    raise SystemExit(f"unexpected agent.start result.type: {result.get('type')!r}")
agent = result.get("agent") or {}
labels = {agent.get("agent"), agent.get("display_agent"), agent.get("name"), agent.get("title")}
if expected_name not in labels:
    raise SystemExit(f"host-qualified started agent label not found in response: {agent}")
if not agent.get("terminal_id") or not agent.get("pane_id"):
    raise SystemExit(f"started agent response missing remote ids: {agent}")
PY
wait_for_local_agent "$STARTED_HOST_AGENT" 90 >/dev/null

focus_output="$(run_herdr "$LOCAL_SESSION" agent focus "$STARTED_HOST_AGENT")"
python3 - "$focus_output" <<'PY'
import json
import sys

text = sys.argv[1]
decoder = json.JSONDecoder()
idx = 0
payload = None
while idx < len(text):
    idx = text.find("{", idx)
    if idx == -1:
        break
    try:
        payload, idx = decoder.raw_decode(text, idx)
    except json.JSONDecodeError:
        idx += 1
        continue
if payload is None:
    raise SystemExit(f"agent.focus response not found in: {text!r}")
result = payload.get("result") or {}
if result.get("type") != "agent_info":
    raise SystemExit(f"unexpected agent.focus result.type: {result.get('type')!r}")
agent = result.get("agent") or {}
if not agent.get("terminal_id") or not agent.get("pane_id"):
    raise SystemExit(f"focused agent response missing remote ids: {agent}")
PY
read_agent_until_contains "$STARTED_HOST_AGENT" "$STARTED_TEXT" 30 >/dev/null

# This single-container smoke verifies the configured remote path. The
# no-transitive remote-of-remote supervisor boundary is covered by unit tests
# around the hidden agent.list_local snapshot path.
docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
wait_for_local_agent_disconnected "$HOST_AGENT" 120 >/dev/null

start_container
wait_for_ssh
run_herdr "$REMOTE_SESSION" remote-api-ping "$ALIAS" >/dev/null
create_remote_smoke_agent
wait_for_single_connected_local_agent "$HOST_AGENT" 120 >/dev/null

# ---- explicit runtime bridge lifecycle (connect/reconnect/disconnect) ----
# These control ONLY the running local server's aggregation/supervisor/bridge
# state. The remote Herdr server/agent in the container must stay alive and
# authoritative throughout: a disconnect makes the LOCAL aggregated entry
# stale/disconnected while the remote keeps running (verified by querying the
# remote server directly over SSH, bypassing the local server).

LIFECYCLE_HELPER="$BASE/lifecycle_helper.py"
cat >"$LIFECYCLE_HELPER" <<'PY'
import json
import sys


def parse(text):
    decoder = json.JSONDecoder()
    idx = 0
    payload = None
    while idx < len(text):
        idx = text.find("{", idx)
        if idx == -1:
            break
        try:
            payload, idx = decoder.raw_decode(text, idx)
        except json.JSONDecodeError:
            idx += 1
            continue
    return payload


mode = sys.argv[1]
text = sys.argv[2]
payload = parse(text)
if payload is None:
    raise SystemExit(f"no JSON payload found in: {text!r}")
if payload.get("error") is not None:
    raise SystemExit(f"unexpected error response: {payload}")
result = payload.get("result") or {}
if result.get("type") != "remote_lifecycle":
    raise SystemExit(f"unexpected result.type: {result.get('type')!r}")
inner = result.get("result") or {}
if mode == "status":
    print(inner.get("status", ""))
elif mode == "changed":
    print("true" if inner.get("changed") else "false")
elif mode == "remote_authoritative":
    print("true" if inner.get("remote_authoritative") else "false")
elif mode == "action":
    print(inner.get("action", ""))
else:
    raise SystemExit(f"unknown mode: {mode!r}")
PY

run_lifecycle_json() {
  local action="$1"
  run_herdr "$LOCAL_SESSION" remote "$action" "$ALIAS" --json
}

assert_lifecycle() {
  local action="$1"
  local expected_status="$2"
  local expected_changed="$3"
  local output status changed authoritative
  output="$(run_lifecycle_json "$action")"
  status="$(python3 "$LIFECYCLE_HELPER" status "$output")"
  changed="$(python3 "$LIFECYCLE_HELPER" changed "$output")"
  authoritative="$(python3 "$LIFECYCLE_HELPER" remote_authoritative "$output")"
  [[ "$status" == "$expected_status" ]] || {
    echo "error: remote $action expected status $expected_status, got $status" >&2
    exit 1
  }
  [[ "$changed" == "$expected_changed" ]] || {
    echo "error: remote $action expected changed $expected_changed, got $changed" >&2
    exit 1
  }
  [[ "$authoritative" == "true" ]] || {
    echo "error: remote $action did not report remote_authoritative=true" >&2
    exit 1
  }
}

remote_agent_listed() {
  # The remote Herdr server in the container is independent of local
  # aggregation; prove it stays alive/authoritative after a local disconnect
  # by querying it directly over SSH (bypassing the local server entirely).
  local out
  out="$(run_remote_ssh "/usr/local/bin/herdr --session $REMOTE_SESSION agent list 2>/dev/null" || true)"
  [[ -n "$out" ]] && json_has_agent "$out" "smoke-agent"
}

# Precondition: the supervisor-driven reconnect above left the host connected
# with exactly one smoke-agent.
wait_for_single_connected_local_agent "$HOST_AGENT" 120 >/dev/null

# (2) disconnect: local entry goes stale/disconnected while the remote server/
# agent in the container stays alive and authoritative.
assert_lifecycle disconnect disconnected true
wait_for_local_agent_disconnected "$HOST_AGENT" 120 >/dev/null
remote_agent_listed || {
  echo "error: remote agent must remain alive after a local disconnect" >&2
  exit 1
}

# (3) repeated disconnect is idempotent (changed=false).
assert_lifecycle disconnect disconnected false

# (4) connect restores local aggregation without setup/update (changed=true).
assert_lifecycle connect connected true
wait_for_single_connected_local_agent "$HOST_AGENT" 120 >/dev/null
# A second healthy connect preserves state (idempotent, changed=false).
assert_lifecycle connect connected false

# (5) reconnect yields fresh local bridge/supervisor state with a single
# connected agent (no duplicates).
assert_lifecycle reconnect connected true
wait_for_single_connected_local_agent "$HOST_AGENT" 120 >/dev/null

# (6) stopping the fake SSH endpoint: connect/reconnect return non-zero with a
# non-connected (unreachable/disconnected/needs_update) LOCAL status, never an
# install/bootstrap attempt. Wait for the supervisor to observe the dead
# endpoint first so the cached status is non-connected and connect probes a
# fresh generation rather than taking the idempotent Connected path.
docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
wait_for_local_agent_disconnected "$HOST_AGENT" 120 >/dev/null

set +e
connect_dead_out="$(run_lifecycle_json connect)"
connect_dead_rc=$?
set -e
[[ $connect_dead_rc -ne 0 ]] || {
  echo "error: connect against a dead endpoint must exit non-zero (got $connect_dead_rc)" >&2
  exit 1
}
connect_dead_status="$(python3 "$LIFECYCLE_HELPER" status "$connect_dead_out")"
[[ "$connect_dead_status" != "connected" ]] || {
  echo "error: connect against a dead endpoint must not report connected" >&2
  exit 1
}

set +e
reconnect_dead_out="$(run_lifecycle_json reconnect)"
reconnect_dead_rc=$?
set -e
[[ $reconnect_dead_rc -ne 0 ]] || {
  echo "error: reconnect against a dead endpoint must exit non-zero (got $reconnect_dead_rc)" >&2
  exit 1
}
reconnect_dead_status="$(python3 "$LIFECYCLE_HELPER" status "$reconnect_dead_out")"
[[ "$reconnect_dead_status" != "connected" ]] || {
  echo "error: reconnect against a dead endpoint must not report connected" >&2
  exit 1
}

echo "remote API bridge Docker smoke passed: probes, supervisor cache, host-qualified get/read/send/focus/start, supervisor disconnect/reconnect, and explicit connect/reconnect/disconnect lifecycle (local-only, idempotent, dead-endpoint non-zero) succeeded"
