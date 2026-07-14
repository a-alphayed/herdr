#!/usr/bin/env bash
#
# Committed, reusable multi-host sleep/offline/wake soak harness for the
# Herdr remote API bridge.
#
# This drives two Docker-backed localhost SSH fake remote hosts plus one
# isolated local Herdr aggregator server/session, and alternates down/wake
# cycles between the two hosts while asserting bounded offline detection,
# online-host isolation, reconnect, host-qualified `agent get` identity, and
# no duplicate host-qualified agent rows.
#
# Everything live (HOME/XDG config/state/runtime, SSH wrapper files, keys,
# Dockerfile) stays under a harness base dir; durable logs are written under
# the (gitignored) repo `.local/reviews/` tree or a caller-provided
# ARTIFACT_DIR. No real remote hosts are touched.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HERDR_BIN="${HERDR_BIN:-$ROOT/target/debug/herdr}"
DURATION_SECONDS="${DURATION_SECONDS:-1800}"
MIN_CYCLES="${MIN_CYCLES:-4}"
WAIT_SECONDS="${WAIT_SECONDS:-120}"
POLL_TIMEOUT="${POLL_TIMEOUT:-15}"
HARD_CAP_SECONDS="${HARD_CAP_SECONDS:-2400}"
RUN_ID="multi-host-local-soak-$(date -u +%Y%m%dT%H%M%SZ)-$$"

# Live runtime base. Generated default is a fresh /tmp/herdr-fed-soak.* dir.
# Explicit BASE overrides are allowed only for safe-looking values and are
# NEVER removed by this harness -- cleanup removes only harness-generated
# /tmp/herdr-fed-soak.* bases (see cleanup()).
BASE_WAS_SET=0
if [[ -n "${BASE:-}" ]]; then
  BASE_WAS_SET=1
else
  BASE="$(mktemp -d /tmp/herdr-fed-soak.XXXXXX)"
fi

# Durable artifact dir. Default is script-relative under the gitignored repo
# .local/reviews tree; never a user-absolute path.
ARTIFACT_DIR="${ARTIFACT_DIR:-$ROOT/.local/reviews/$RUN_ID}"
LOG="$ARTIFACT_DIR/soak.log"
SUMMARY="$ARTIFACT_DIR/summary.env"

LOCAL_SESSION="fed-local-soak"
LOCAL_HOME="$BASE/local-home"
LOCAL_CONFIG="$BASE/local-config"
LOCAL_CONFIG_FILE="$BASE/local-config.toml"
LOCAL_STATE="$BASE/local-state"
LOCAL_RUNTIME="$BASE/runtime"
LOCAL_SERVER_LOG="$ARTIFACT_DIR/local-server.log"
SSH_DIR="$LOCAL_HOME/.ssh"
WRAPPER_DIR="$BASE/bin"
KEY_PATH="$BASE/id_ed25519"
IMAGE_TAG="herdr-fed-soak:${RUN_ID//[^[:alnum:]_.-]/-}"
LOCAL_SERVER_PID=""
IMAGE_BUILT=0
CYCLES_COMPLETED=0
FAIL_REASON=""

ALIASES=("herdr-fed-soak-a" "herdr-fed-soak-b")
REMOTE_SESSIONS=("fed-soak-a" "fed-soak-b")
AGENTS=("smoke-a" "smoke-b")
PORTS=("" "")
CONTAINERS=("herdr-fed-soak-a-$$" "herdr-fed-soak-b-$$")

log() {
  printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" | tee -a "$LOG"
}

fail() {
  FAIL_REASON="$*"
  log "FAIL: $*"
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

run_herdr() {
  # Non-timeout local Herdr CLI call. Used only for server start/stop, which
  # are not aggregator polls and must not be time-bounded.
  local session="$1"; shift
  env \
    -u HERDR_SOCKET_PATH \
    -u HERDR_CLIENT_SOCKET_PATH \
    -u HERDR_SESSION \
    -u HERDR_PANE_ID \
    -u HERDR_ENV \
    -u HERDR_TAB_ID \
    -u HERDR_WORKSPACE_ID \
    PATH="$WRAPPER_DIR:$PATH" \
    HOME="$LOCAL_HOME" \
    XDG_CONFIG_HOME="$LOCAL_CONFIG" \
    XDG_STATE_HOME="$LOCAL_STATE" \
    XDG_RUNTIME_DIR="$LOCAL_RUNTIME" \
    HERDR_CONFIG_PATH="$LOCAL_CONFIG_FILE" \
    "$HERDR_BIN" --session "$session" "$@"
}

run_herdr_poll() {
  # Time-bounded local Herdr CLI call. Every aggregator poll / API call that
  # could hang on a wedged server or bridge MUST go through this so it is
  # bounded by POLL_TIMEOUT seconds.
  local session="$1"; shift
  timeout "$POLL_TIMEOUT"s env \
    -u HERDR_SOCKET_PATH \
    -u HERDR_CLIENT_SOCKET_PATH \
    -u HERDR_SESSION \
    -u HERDR_PANE_ID \
    -u HERDR_ENV \
    -u HERDR_TAB_ID \
    -u HERDR_WORKSPACE_ID \
    PATH="$WRAPPER_DIR:$PATH" \
    HOME="$LOCAL_HOME" \
    XDG_CONFIG_HOME="$LOCAL_CONFIG" \
    XDG_STATE_HOME="$LOCAL_STATE" \
    XDG_RUNTIME_DIR="$LOCAL_RUNTIME" \
    HERDR_CONFIG_PATH="$LOCAL_CONFIG_FILE" \
    "$HERDR_BIN" --session "$session" "$@"
}

run_remote_ssh() {
  local alias="$1"; shift
  PATH="$WRAPPER_DIR:$PATH" HOME="$LOCAL_HOME" ssh -T "$alias" "$@"
}

cleanup() {
  set +e
  log "cleanup starting"
  if [[ -n "$LOCAL_SERVER_PID" ]]; then
    run_herdr "$LOCAL_SESSION" server stop >>"$LOG" 2>&1 || true
    for _ in {1..40}; do
      if ! kill -0 "$LOCAL_SERVER_PID" >/dev/null 2>&1; then break; fi
      sleep 0.1
    done
    if kill -0 "$LOCAL_SERVER_PID" >/dev/null 2>&1; then
      kill "$LOCAL_SERVER_PID" >/dev/null 2>&1 || true
    fi
    wait "$LOCAL_SERVER_PID" >/dev/null 2>&1 || true
  fi
  for container in "${CONTAINERS[@]}"; do
    docker rm -f "$container" >>"$LOG" 2>&1 || true
  done
  if [[ "$IMAGE_BUILT" -eq 1 ]]; then
    docker image rm "$IMAGE_TAG" >>"$LOG" 2>&1 || true
  fi
  # Only ever remove a harness-generated base. Explicit caller-provided BASE
  # overrides are left in place even when they happen to match the generated
  # pattern, so we never rm -rf a path the caller handed us.
  if [[ "$BASE_WAS_SET" -eq 0 && -n "${BASE:-}" ]]; then
    case "$BASE" in
      /tmp/herdr-fed-soak.*) rm -rf "$BASE" ;;
      *) log "refusing to remove unexpected generated BASE=$BASE" ;;
    esac
  fi
  log "cleanup complete"
  {
    echo "RUN_ID=$RUN_ID"
    echo "CYCLES_COMPLETED=$CYCLES_COMPLETED"
    echo "FAIL_REASON=$FAIL_REASON"
  } > "$SUMMARY"
}
trap cleanup EXIT INT TERM

parse_json_agent_py='import json, sys
mode=sys.argv[1]
text=sys.argv[2]
expected=sys.argv[3]
non_connected={"disconnected","unreachable","needs update"}
decoder=json.JSONDecoder(); idx=0; payload=None
while idx < len(text):
    idx=text.find("{", idx)
    if idx == -1: break
    try:
        payload, idx = decoder.raw_decode(text, idx)
    except json.JSONDecodeError:
        idx += 1
if payload is None: raise SystemExit(1)
result=payload.get("result") or {}
agents=result.get("agents") or []
matched=[]
for agent in agents:
    labels={agent.get("agent"), agent.get("display_agent"), agent.get("name"), agent.get("title")}
    if expected in labels:
        matched.append(agent)
if mode == "count":
    print(len(matched)); raise SystemExit(0)
if mode == "connected":
    raise SystemExit(0 if len(matched)==1 and matched[0].get("custom_status") not in non_connected else 1)
if mode == "non-connected":
    raise SystemExit(0 if len(matched)>=1 and all(a.get("custom_status") in non_connected for a in matched) else 1)
raise SystemExit(2)'

check_get_py='import json, sys
text=sys.argv[1]
expected=sys.argv[2]
decoder=json.JSONDecoder(); idx=0; payload=None
while idx < len(text):
    idx=text.find("{", idx)
    if idx == -1: break
    try:
        payload, idx = decoder.raw_decode(text, idx)
    except json.JSONDecodeError:
        idx += 1
if payload is None: raise SystemExit("no json payload")
agent=(payload.get("result") or {}).get("agent") or {}
labels={agent.get("agent"), agent.get("display_agent"), agent.get("name"), agent.get("title")}
if expected not in labels:
    raise SystemExit(f"expected {expected!r} in labels {labels!r}; agent={agent!r}")
if not agent.get("terminal_id") or not agent.get("pane_id"):
    raise SystemExit(f"missing remote ids in agent={agent!r}")
'

alloc_port() {
  python3 - <<'PY'
import socket
s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()
PY
}

start_container() {
  local idx="$1"
  docker rm -f "${CONTAINERS[$idx]}" >/dev/null 2>&1 || true
  docker run -d --rm \
    --name "${CONTAINERS[$idx]}" \
    -p "127.0.0.1:${PORTS[$idx]}:22" \
    --mount "type=bind,src=$HERDR_BIN,dst=/usr/local/bin/herdr,readonly" \
    "$IMAGE_TAG" >/dev/null
  log "container started alias=${ALIASES[$idx]} container=${CONTAINERS[$idx]} port=${PORTS[$idx]}"
}

wait_for_ssh() {
  local idx="$1"
  local deadline=$((SECONDS + WAIT_SECONDS))
  while (( SECONDS < deadline )); do
    if run_remote_ssh "${ALIASES[$idx]}" true >/dev/null 2>&1; then
      log "ssh reachable alias=${ALIASES[$idx]}"
      return 0
    fi
    sleep 1
  done
  fail "ssh did not become reachable for ${ALIASES[$idx]} within ${WAIT_SECONDS}s"
}

remote_api_ping() {
  local idx="$1"
  local alias="${ALIASES[$idx]}"
  local session="${REMOTE_SESSIONS[$idx]}"
  run_herdr_poll "$session" remote-api-ping "$alias" >/dev/null || fail "remote-api-ping failed alias=$alias session=$session"
  log "remote-api-ping ok alias=$alias session=$session"
}

create_remote_agent() {
  local idx="$1"
  local alias="${ALIASES[$idx]}"
  local session="${REMOTE_SESSIONS[$idx]}"
  local agent="${AGENTS[$idx]}"
  local workspace_json pane_id
  workspace_json="$(run_remote_ssh "$alias" "SHELL=/bin/bash HOME=/home/fed /usr/local/bin/herdr --session $session workspace create --cwd /tmp --focus" 2>&1)" || fail "remote workspace create failed alias=$alias output=$workspace_json"
  pane_id="$(python3 - "$workspace_json" <<'PY'
import json, sys
text=sys.argv[1]; decoder=json.JSONDecoder(); idx=0; payload=None
while idx < len(text):
    idx=text.find('{', idx)
    if idx == -1: break
    try: value, idx = decoder.raw_decode(text, idx)
    except json.JSONDecodeError: idx += 1; continue
    if value.get('id') == 'cli:workspace:create' or value.get('result', {}).get('root_pane'):
        payload=value
if payload is None: raise SystemExit(f'workspace create response not found: {text!r}')
print(payload['result']['root_pane']['pane_id'])
PY
)"
  run_remote_ssh "$alias" "SHELL=/bin/bash HOME=/home/fed /usr/local/bin/herdr --session $session pane report-agent $pane_id --source soak --agent $agent --state working" >/dev/null || fail "remote report-agent failed alias=$alias agent=$agent pane=$pane_id"
  log "remote agent reported alias=$alias agent=$agent pane=$pane_id"
}

wait_for_local_server() {
  for _ in {1..100}; do
    local status_json
    status_json="$(run_herdr_poll "$LOCAL_SESSION" status server --json 2>/dev/null || true)"
    if python3 - "$status_json" <<'PY' >/dev/null 2>&1
import json, sys
text=sys.argv[1]; decoder=json.JSONDecoder(); idx=0; payload=None
while idx < len(text):
    idx=text.find('{', idx)
    if idx == -1: break
    try: payload, idx = decoder.raw_decode(text, idx)
    except json.JSONDecodeError: idx += 1
raise SystemExit(0 if payload and payload.get('status') == 'running' else 1)
PY
    then
      log "local server reachable"
      return 0
    fi
    sleep 0.2
  done
  fail "local server did not become reachable"
}

agent_identity() {
  local idx="$1"
  printf '%s/%s' "${ALIASES[$idx]}" "${AGENTS[$idx]}"
}

wait_agent_state() {
  local idx="$1" mode="$2" timeout_seconds="$3" expected
  expected="$(agent_identity "$idx")"
  local deadline=$((SECONDS + timeout_seconds)) list_json count
  while (( SECONDS < deadline )); do
    list_json="$(run_herdr_poll "$LOCAL_SESSION" agent list 2>/dev/null || true)"
    if [[ -n "$list_json" ]] && python3 -c "$parse_json_agent_py" "$mode" "$list_json" "$expected"; then
      count="$(python3 -c "$parse_json_agent_py" count "$list_json" "$expected")"
      if [[ "$mode" == "connected" && "$count" != "1" ]]; then
        fail "expected exactly one connected $expected but count=$count"
      fi
      printf '%s' "$list_json" > "$ARTIFACT_DIR/agent-list-${mode}-${expected//\//-}-cycle-${CYCLES_COMPLETED}.json"
      log "agent state ok mode=$mode expected=$expected count=$count"
      return 0
    fi
    sleep 1
  done
  fail "agent $expected did not reach state $mode within ${timeout_seconds}s"
}

agent_get_check() {
  local idx="$1" expected output
  expected="$(agent_identity "$idx")"
  output="$(run_herdr_poll "$LOCAL_SESSION" agent get "$expected")" || fail "agent get failed for $expected"
  python3 -c "$check_get_py" "$output" "$expected" || fail "agent get identity check failed for $expected"
  printf '%s' "$output" > "$ARTIFACT_DIR/agent-get-${expected//\//-}-cycle-${CYCLES_COMPLETED}.json"
  log "agent get identity ok expected=$expected"
}

# Validate the base dir: a generated base MUST be under /tmp/herdr-fed-soak.*.
# An explicit caller override is allowed for safe-looking values (warned) but
# is never removed; outright unsafe overrides (empty, /, $HOME) are refused.
case "$BASE" in
  /tmp/herdr-fed-soak.*)
    ;;
  *)
    if [[ "$BASE_WAS_SET" -eq 1 ]]; then
      if [[ -z "$BASE" || "$BASE" == "/" || "$BASE" == "$HOME" || "$BASE" == "$HOME/"* ]]; then
        echo "error: refusing unsafe BASE override: $BASE" >&2
        exit 2
      fi
      echo "warning: using explicit BASE override outside /tmp/herdr-fed-soak.*: $BASE" >&2
    else
      echo "error: refusing unsafe generated BASE path: $BASE" >&2
      exit 2
    fi
    ;;
esac

mkdir -p "$ARTIFACT_DIR" "$BASE" "$LOCAL_HOME" "$LOCAL_CONFIG" "$LOCAL_STATE" "$LOCAL_RUNTIME" "$WRAPPER_DIR" "$SSH_DIR"
chmod 700 "$SSH_DIR" "$WRAPPER_DIR" "$LOCAL_RUNTIME"
: > "$LOG"
log "starting multi-host local soak run_id=$RUN_ID duration=${DURATION_SECONDS}s min_cycles=$MIN_CYCLES base=$BASE herdr_bin=$HERDR_BIN artifact_dir=$ARTIFACT_DIR"

require_command cargo
require_command docker
require_command ssh
require_command ssh-keygen
require_command python3
require_command timeout
REAL_SSH="$(command -v ssh)"

cd "$ROOT"
cargo build --locked --manifest-path "$ROOT/Cargo.toml" 2>&1 | tee -a "$LOG"
[[ -x "$HERDR_BIN" ]] || fail "HERDR_BIN missing or not executable: $HERDR_BIN"
sha256sum "$HERDR_BIN" | tee "$ARTIFACT_DIR/herdr-bin.sha256" | tee -a "$LOG"

rm -f "$KEY_PATH" "$KEY_PATH.pub"
ssh-keygen -q -t ed25519 -N "" -f "$KEY_PATH" -C "herdr-fed-soak" >/dev/null
chmod 600 "$KEY_PATH"

cat > "$BASE/Dockerfile" <<'DOCKERFILE'
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
log "docker image built tag=$IMAGE_TAG"

for idx in 0 1; do
  PORTS[$idx]="$(alloc_port)"
done

cat > "$SSH_DIR/config" <<EOF
Host ${ALIASES[0]}
  HostName 127.0.0.1
  Port ${PORTS[0]}
  User fed
  IdentityFile $KEY_PATH
  IdentitiesOnly yes
  BatchMode yes
  StrictHostKeyChecking no
  UserKnownHostsFile $BASE/known_hosts
  ConnectTimeout 5
  ConnectionAttempts 1
  LogLevel ERROR

Host ${ALIASES[1]}
  HostName 127.0.0.1
  Port ${PORTS[1]}
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

cat > "$WRAPPER_DIR/ssh" <<EOF
#!/usr/bin/env bash
exec "$REAL_SSH" -F "$SSH_DIR/config" "\$@"
EOF
chmod 700 "$WRAPPER_DIR/ssh"

cat > "$LOCAL_CONFIG_FILE" <<EOF
[remote]
enabled = true

[[remote.hosts]]
name = "${ALIASES[0]}"
target = "${ALIASES[0]}"
session = "${REMOTE_SESSIONS[0]}"
connection_policy = "auto"
connect_timeout_secs = 5

[[remote.hosts]]
name = "${ALIASES[1]}"
target = "${ALIASES[1]}"
session = "${REMOTE_SESSIONS[1]}"
connection_policy = "auto"
connect_timeout_secs = 5
EOF
cp "$LOCAL_CONFIG_FILE" "$ARTIFACT_DIR/local-config.toml"

for idx in 0 1; do
  start_container "$idx"
  wait_for_ssh "$idx"
  remote_api_ping "$idx"
  create_remote_agent "$idx"
done

run_herdr "$LOCAL_SESSION" server >"$LOCAL_SERVER_LOG" 2>&1 &
LOCAL_SERVER_PID="$!"
wait_for_local_server

for idx in 0 1; do
  wait_agent_state "$idx" connected "$WAIT_SECONDS"
  agent_get_check "$idx"
done

SOAK_START=$SECONDS
SOAK_TARGET=$((SOAK_START + DURATION_SECONDS))
SOAK_HARD=$((SOAK_START + HARD_CAP_SECONDS))
log "cyclic soak starting target_end=${DURATION_SECONDS}s hard_cap=${HARD_CAP_SECONDS}s"

while { (( SECONDS < SOAK_TARGET )) || (( CYCLES_COMPLETED < MIN_CYCLES )); } && (( SECONDS < SOAK_HARD )); do
  down_idx=$(( CYCLES_COMPLETED % 2 ))
  up_idx=$(( 1 - down_idx ))
  cycle_num=$(( CYCLES_COMPLETED + 1 ))
  log "cycle=$cycle_num down_alias=${ALIASES[$down_idx]} online_alias=${ALIASES[$up_idx]} starting"

  docker rm -f "${CONTAINERS[$down_idx]}" >/dev/null 2>&1 || true
  log "cycle=$cycle_num stopped container alias=${ALIASES[$down_idx]}"
  wait_agent_state "$down_idx" non-connected "$WAIT_SECONDS"
  wait_agent_state "$up_idx" connected "$WAIT_SECONDS"
  agent_get_check "$up_idx"

  start_container "$down_idx"
  wait_for_ssh "$down_idx"
  remote_api_ping "$down_idx"
  create_remote_agent "$down_idx"
  wait_agent_state "$down_idx" connected "$WAIT_SECONDS"
  agent_get_check "$down_idx"
  wait_agent_state "$up_idx" connected "$WAIT_SECONDS"

  CYCLES_COMPLETED=$cycle_num
  elapsed=$((SECONDS - SOAK_START))
  log "cycle=$cycle_num complete elapsed=${elapsed}s"
done

if (( CYCLES_COMPLETED < MIN_CYCLES )); then
  fail "completed only $CYCLES_COMPLETED cycles, required $MIN_CYCLES"
fi
for idx in 0 1; do
  wait_agent_state "$idx" connected "$WAIT_SECONDS"
  agent_get_check "$idx"
done
elapsed=$((SECONDS - SOAK_START))
if (( elapsed < DURATION_SECONDS )); then
  fail "cyclic soak elapsed ${elapsed}s below target ${DURATION_SECONDS}s"
fi
log "PASS: multi-host local soak completed elapsed=${elapsed}s cycles=$CYCLES_COMPLETED hosts=${ALIASES[*]}"
FAIL_REASON=""
