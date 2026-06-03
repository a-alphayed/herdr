# Federated Remote Agent Control Design

Status: draft proposal  
Owner: TBD  
Created: 2026-05-31  
Updated: 2026-06-01

## Summary

Add first-class multi-host agent control to Herdr so agents running on remote machines appear in the normal local Herdr sidebar agent panel and can be controlled through the same Herdr agent APIs as local agents.

The design is **authoritative remote Herdr nodes with local aggregation and proxying**:

```text
Every host runs its own authoritative Herdr server.
The local Herdr server aggregates remote AgentInfo and proxies agent control over SSH.
Remote hooks report to their own host's local Herdr socket, unchanged.
Clicking a remote agent focuses/attaches to the exact remote session/tab/pane where it lives.
Durability remains per authoritative Herdr server.
```

This is **not** a "local server owns a remote PTY" design. Remote panes and agents are owned by the Herdr server on the machine where they run. The local Herdr instance acts as a unified control plane: it displays remote agents, routes commands, and opens direct attach/proxy views for interaction.

## Core Invariants

- A host owns the PTYs and child processes it runs.
- A Herdr server owns the workspaces, tabs, panes, terminals, hooks, and persistence inside its own session.
- Other Herdr servers may aggregate, route commands to, and proxy views of those panes, but they do not own the remote PTYs.
- Remote hooks report to the remote host's local Herdr socket. They do not relay to the aggregating host.
- Remote targets are always host-qualified in MVP.
- Existing `herdr --remote <target>` remains a remote TUI attach mode. Federated agents use a remote API bridge, not the render/client bridge.

## Goals

- Remote agents appear in the normal Herdr sidebar Agents panel alongside local agents.
- Clicking a remote agent focuses the exact remote session/tab/pane where that agent lives.
- `herdr agent list/read/send/focus/wait/start` can target local and remote agents.
- Agents running in any Herdr pane can use one local API surface to inspect/control agents across configured hosts.
- Remote agent hooks stay local to the remote host and continue using normal Herdr integration semantics.
- Durability inherits existing Herdr semantics:
  - local agents persist/restore through the local Herdr server;
  - remote agents persist/restore through their own remote Herdr server;
  - the aggregating local server persists remote host configuration and reconnects/re-aggregates on startup.
- Reuse the existing `src/remote.rs` SSH/bootstrap/version machinery where it fits.

## Non-goals

- Plain `ssh host` panes automatically exposing remote agents.
- Adopting externally-started remote processes into Herdr. `ssh jafar && codex` is unmanaged unless Codex is started inside a Herdr-managed pane on Jafar.
- Local Herdr owning remote PTYs in the MVP.
- Relaying remote hooks back to the local Herdr socket.
- Replacing each remote host's Herdr server with a dumb remote helper.
- Browser proxying or remote webview support.
- Transitive federation/mesh routing in MVP. A node routes only to local agents and explicitly configured remote hosts.
- Embedded splittable remote panes in the local layout as an MVP. Direct remote terminal attach/proxy is the MVP focus behavior; embedded proxy panes remain an optional later phase.
- Persisting remote proxy placement in MVP. The aggregator persists remote config only and re-aggregates on restart.

## Terminology

- **Local node / aggregator**: the Herdr server whose sidebar/UI the user is currently using, e.g. SteamDeck.
- **Remote node**: a configured remote Herdr server/session, e.g. Jafar's default Herdr session.
- **Remote host alias**: local configured name used in targets such as `jafar/codex`.
- **SSH target**: the value passed to `ssh`, e.g. `jafar`, `afayed@host`, or a Tailscale DNS name.
- **Herdr session**: the named Herdr server/socket context on a host. This is not a workspace/space.
- **Workspace/space**: a workspace inside one Herdr session.
- **Remote API bridge**: an SSH-backed bridge from the local node to the remote node's Herdr API socket.
- **Remote render/attach bridge**: a separate bridge used for interactive terminal attach/focus. This is distinct from the API bridge.

## User Workflows

### 1. Register a remote node

```bash
herdr remote add jafar --target jafar --session default
```

Meaning:

```text
remote alias:          jafar
SSH target:            jafar
remote Herdr session:  default
```

`remote add` is allowed to be interactive. It may SSH to the host, check/install/bootstrap a compatible Herdr binary, start the remote Herdr session if needed, and validate the remote API bridge.

### 2. Start a remote agent

```bash
herdr agent start --host jafar \
  --name codex \
  --cwd /Users/afayed/projects/mentat \
  -- codex
```

The command is routed to Jafar's Herdr server. Jafar creates a real workspace/tab/pane/terminal and starts Codex there. The local sidebar then shows a remote entry such as:

```text
jafar/codex    working
```

`--cwd` is a path on Jafar. It must not be locally expanded or locally validated.

### 3. Control a remote agent

```bash
herdr agent read jafar/codex --lines 80
herdr agent send jafar/codex "continue"
herdr agent wait jafar/codex --status done
```

The local Herdr server parses the host-qualified target and proxies the API request to the owning remote node.

### 4. Click/focus a remote agent

Clicking `jafar/codex` in the local sidebar should:

1. route `agent.focus` to Jafar's API socket so Jafar's own session focuses the correct workspace/tab/pane;
2. open a direct remote terminal attach/proxy to that exact remote terminal;
3. return to the local Herdr UI on detach.

This is a direct remote terminal attach/proxy, not an embedded local split pane in MVP.

### 5. Agent-to-agent control

An agent running on SteamDeck can control Jafar agents through the local SteamDeck Herdr API:

```bash
herdr agent list
herdr agent read jafar/codex
herdr agent send jafar/claude "continue"
```

An agent running on Jafar can control SteamDeck agents only if Jafar's Herdr session also has SteamDeck configured as a remote host. MVP has no transitive routing; each node routes only to local agents and explicitly configured remotes.

### 6. Plain SSH is unmanaged

This does not create a managed remote Herdr agent:

```bash
ssh jafar
codex
```

From the SteamDeck Herdr server's perspective, that is just an SSH process. From Jafar's Herdr server's perspective, Codex was not started in a Jafar Herdr pane.

This can be managed if the user starts Herdr on Jafar and launches Codex inside a Jafar Herdr pane, or uses the local federated start command shown above.

## Current Architecture Notes

Relevant files:

```text
src/app/state.rs              AppState, UI/config state
src/ui/sidebar.rs             native sidebar agent panel
src/app/agents.rs             collect_agent_infos(), agent_info(), agent start/name conflict logic
src/app/terminal_targets.rs   local terminal/agent target resolution
src/app/api.rs                agent/pane API handlers and AppEvent dispatch
src/api/schema.rs             AgentInfo, PaneInfo, API request/response schema
src/api/client.rs             Unix-socket API client
src/api/server.rs             one-request-per-connection API server loop
src/api/subscriptions.rs      events.subscribe and wait behavior
src/terminal/state.rs         TerminalState, hook authority, effective agent state
src/remote.rs                 existing `herdr --remote` SSH thin-client bootstrap/bridge
src/client/mod.rs             terminal attach / remote client rendering paths
```

The sidebar currently derives agents from local workspaces and panes:

```text
AppState.workspaces
  -> Workspace.pane_details(AppState.terminals)
  -> AgentPanelEntry
```

This design adds a second agent entry source for remote authoritative nodes rather than pretending remote agents are local panes or building a separate dashboard outside the sidebar.

The existing `herdr --remote <host>` mode is related but not sufficient. It runs a local thin client connected to a full remote Herdr server; the remote server owns the whole UI and the local side blits rendered frames. It cannot merge local and remote agents into one sidebar because the render stream is opaque rendered cells, not structured `AgentInfo`.

The structured substrate for federation is the Herdr JSON socket API:

```text
agent.list
agent.read
agent.send
agent.focus
agent.start
events.subscribe
```

Important API transport detail: the API socket is one-request-per-connection. The server reads one JSON line, dispatches, writes one response, and closes the socket. `events.subscribe` and output waits (`pane.wait_for_output`) are the exceptions: they hold a connection open while streaming/waiting. (`agent.wait` is not an API method — it is a CLI-level composition over those wait primitives; see §9.) There is no request multiplexing on a single API socket connection today.

## Proposed Architecture

### High-level diagram

```text
SteamDeck Herdr server / UI
  local workspaces, panes, agents
  remote source: jafar/default
    ssh jafar herdr remote-api-bridge --session default
      -> Jafar Herdr API socket
  remote source: work-mini/default
    ssh work-mini herdr remote-api-bridge --session default
      -> Work Mini Herdr API socket

Unified local sidebar
  pi
  codex
  jafar/codex
  jafar/claude
  work-mini/pi
```

Each remote host remains authoritative for its own panes:

```text
Jafar Herdr server
  Workspace -> Tab -> Pane -> Terminal -> local PTY -> agent
  local hooks -> Jafar Herdr socket
  Jafar persistence/restore/handoff semantics
```

The local host aggregates and routes:

```text
SteamDeck Herdr server
  fetches/subscribes to remote AgentInfo
  displays remote agents in local sidebar
  proxies allowed agent/pane commands to owning node
  focuses remote agents by attaching/proxying to their remote terminal
```

## Components

### 1. Remote configuration and lifecycle

Add remote config under `config.toml` or equivalent persisted settings:

```toml
[remote]
enabled = true

[[remote.hosts]]
name = "jafar"
target = "jafar"
session = "default"
auto_connect = true

[[remote.hosts]]
name = "work-mini"
target = "work-mini"
session = "default"
auto_connect = true
```

Rules:

- `name` is the local alias used in targets. It must be unique and must not contain `/` or `:`.
- `target` is the SSH target.
- `session` is the Herdr session name on the remote host, not a workspace/space.
- Multiple sessions on the same SSH target should be configured with distinct aliases, e.g. `jafar-default` and `jafar-agents`.
- The remote source key is `(host_alias, session_name)`, not just host.

Provisioning split:

- `remote add` / `remote connect` may be interactive and may prompt to install/bootstrap a compatible Herdr binary.
- The background supervisor/watchdog reconnect path must be non-interactive and must never attempt install/bootstrap repeatedly. If the remote binary/session is missing or incompatible during reconnect, mark the remote as `incompatible` or `needs_setup` and surface the error.

### 2. Remote API bridge

Add a remote API bridge parallel to the existing remote client bridge.

Existing `herdr --remote` bridge shape:

```text
local client <-> SSH stdio bridge <-> remote Herdr client/render socket
```

New API bridge shape:

```text
local Herdr server <-> SSH stdio bridge <-> remote Herdr API socket
```

Contract:

- `remote-api-bridge` is a **1:1 pipe for one remote API socket connection**.
- Because the Herdr API is one-request-per-connection, the local aggregator opens one remote API bridge per request, wait, or subscription.
- A long-lived `events.subscribe` connection occupies its own bridge.
- A blocking `agent.wait` or output wait occupies its own bridge for the wait duration.
- Heartbeat pings use their own short-lived bridge and must not rely on the subscription bridge.
- No custom multiplexing protocol is part of MVP.
- The implementation should work without user SSH multiplexing configuration.
- It may use a Herdr-owned SSH `ControlMaster`/`ControlPath`/`ControlPersist` optimization internally to amortize authentication and TCP setup, but correctness must not depend on it.
- The implementation must bound per-host bridge/process count. MVP should assume a small configured fleet (single-digit remote hosts, not dozens) and enforce a per-host concurrent request/wait cap (for example 4 active short-lived/wait bridges), queueing or rejecting excess work instead of spawning unbounded SSH processes. A normally connected idle host should use roughly one long-lived subscription bridge plus occasional heartbeat/request bridges; a focused remote terminal uses a separate attach/render bridge.

Implementation direction:

- Reuse/refactor from `src/remote.rs`:
  - SSH target validation and shell quoting.
  - remote platform detection.
  - remote binary lookup/version/protocol checks.
  - remote install/bootstrap flow.
  - `SshStdioBridge` pattern.
- Add a sibling bridge command such as:

```bash
herdr remote-api-bridge --session <name>
```

which connects to the remote `api::socket_path()` instead of the remote client/render socket.

### 3. Protocol/capability handshake

Before routing remote commands, the aggregator must verify compatibility.

Existing API support:

- `ResponseResult::Pong` already carries `version`, `protocol`, and optional `ServerCapabilities`.
- Existing `ServerCapabilities` is currently narrow (for example `live_handoff`) and does not enumerate all API methods needed for federation.
- Phase 0 may rely on `Pong.version` and `Pong.protocol` for compatibility checks.
- If method-level federation capabilities are required, that is a prerequisite schema change, not a check that exists today.

Required checks:

- remote Herdr binary is reachable;
- remote Herdr server/session is running or can be started;
- remote API socket responds;
- remote protocol version is compatible;
- required federation methods are supported by protocol version or by a new capability field if added.

Note: `Pong.protocol` is the shared render wire `PROTOCOL_VERSION` (`src/protocol/wire.rs`), not a JSON-API-specific version. It moves when the render protocol changes and does not track JSON-API schema changes, so treat it as a coarse gate only. A federation-specific capability/method set (the prerequisite schema change above) is the durable compatibility contract.

On mismatch:

- `remote add/connect` may offer an interactive bootstrap/update path;
- background reconnect marks the remote `incompatible` and surfaces an actionable error;
- do not silently half-enable a remote that cannot support required methods.

### 4. Remote source supervisor / watchdog

Add a local subsystem, e.g. `src/remote_source.rs` or `src/app/remote_agents.rs`, responsible for configured remote hosts.

Responsibilities:

- maintain connection state for each `(host_alias, session_name)`;
- perform heartbeat pings over short-lived API bridges;
- maintain an event subscription bridge when connected;
- keep a cache of remote `AgentInfo` records;
- track reachability, staleness, last error, and protocol compatibility;
- reconnect with bounded exponential backoff and jitter;
- enforce per-host bridge concurrency limits;
- expose remote agents to sidebar/API layers through main-loop state updates.

State machine:

```text
Disabled
Connecting
Connected
Stale
Reconnecting
Disconnected
AuthFailed
Incompatible
NeedsSetup
```

The SSH supervisor/watchdog tasks live outside pure `AppState`. Cached remote state may live in pure state. Supervisors send updates to the main app loop through existing event patterns; they must not mutate UI state directly off-thread.

### 5. Cache consistency and resync

Do not use `snapshot -> subscribe` as the reconnect algorithm. It has a race: agent state can change after the snapshot and before the subscription begins.

MVP resync algorithm:

1. Open remote `events.subscribe` for the subscription set below.
2. Buffer incoming events for that host/session.
3. Pull a full **local-only** snapshot using the internal `agent.list_local` API on the remote node, plus any required pane/workspace metadata calls.
4. Replace or reconcile that host/session's cache from the snapshot.
5. Replay buffered events idempotently, applying an event only if its `revision` is newer than the cached entry for that identity.
6. Continue applying subscription events live.
7. Run a periodic full refresh as a safety net for missed events or subscription loss.

No snapshot cursor/generation exists in the API today. Do not rely on a cursor-based protocol in MVP unless a separate schema change adds one.

Periodic refresh constraints:

- It must not be a tight loop. Default interval should be conservative (for example 60 seconds or slower) with jitter.
- It should run only while the host/session is connected.
- It should fetch only the metadata needed for aggregation (`agent.list_local` and any required pane/workspace metadata), never pane output. Public `agent.list` may aggregate cached remotes for users, so supervisors must not poll it or they can ingest remote-of-remote entries.
- The interval should be configurable or at least centralized as a named constant so it is not accidentally shipped as a 1s loop.

Subscription scope:

- Subscribe to `pane.agent_status_changed` and `pane.agent_detected`, plus the workspace/tab/pane lifecycle subscriptions (`workspace.*`, `tab.*`, `pane.created` / `closed` / `focused` / `exited`) needed to maintain the remote cache.
- Subscription streams must preserve the same local-only boundary as snapshots. A local aggregator should subscribe only to events for the remote node's own panes/agents, not to that remote node's aggregated remote-of-remote view.
- There is no raw-output subscription to avoid: the API exposes no `PaneOutputChanged` subscription. The only output-oriented subscription is `pane.output_matched`, which is pattern-filtered. Do **not** use `pane.output_matched` for aggregation — aggregation needs status and lifecycle, not output text.

Cache deletion rules:

- Connected snapshot missing an old remote agent: remove it from that host/session cache.
- Disconnected host/session: keep last-known entries as stale/disconnected; do not remove them until reconnection confirms absence or the remote is removed.
- Remote server restart/protocol generation change: replace that host/session cache from the fresh snapshot, then apply buffered events.
- Remote agent exits or remote pane closes: remove or mark done/closed according to the remote event/snapshot state.

### 6. Remote identity model

Remote IDs are scoped to their owning host/session. Local and remote IDs may collide textually, e.g. local pane `1-1` and Jafar pane `1-1`.

Use an explicit remote location wrapper rather than treating remote IDs as local IDs:

```rust
struct RemoteHostKey {
    alias: String,
    session: String,
}

struct RemoteAgentLocation {
    host: RemoteHostKey,
    workspace_id: String,
    tab_id: String,
    pane_id: String,
    terminal_id: String,
}

struct RemoteAgentCacheEntry {
    location: RemoteAgentLocation,
    info: AgentInfo,
    reachable: bool,
    stale: bool,
    last_seen_ms: Option<u64>,
    last_error: Option<String>,
}

enum AgentRoute {
    Local { /* existing resolved local target */ },
    Remote { host: RemoteHostKey, target: String, entry: RemoteAgentCacheEntry },
}
```

Stable user-facing target identity:

- `host/name` resolves by remote agent `name`/manual label/effective agent label within the remote host/session snapshot.
- `host/pane:<id>` and `host/terminal:<id>` are typed transient handles.
- `terminal_id`/`pane_id` are not durable across remote node restart; they are refreshed on snapshot.
- Named agents are the preferred stable user target. Unnamed agents can still be targeted by typed handles while connected.

### 7. Target grammar and parsing

Canonical remote target grammar:

```text
<host>/<target>
```

Examples:

```text
jafar/codex
jafar/claude-reviewer
jafar/pane:1-1
jafar/terminal:term_abc123
jafar/workspace:w123
```

Rules:

- The first `/` separates host alias from target.
- Host aliases cannot contain `/` or `:`.
- The target remainder is parsed verbatim; remote agent labels may contain `/` because only the first slash is structural.
- Typed handles use prefixes such as `pane:` and `terminal:`.
- Bare targets are local-only in MVP. Remote targets must be host-qualified.
- If no remote hosts are configured, host-qualified parsing is not activated; slash-containing local labels keep the pre-federation local behavior.
- Later versions may add global unique shorthand, but MVP should not search remotes for bare names.

### 8. Unified sidebar entries

Extend the native agent panel to append remote agents from the aggregator.

Current local path:

```text
agent_panel_entries_with_runtimes(app, runtimes)
  -> local Workspace.pane_details(...)
```

New path:

```text
local entries
+ remote entries from RemoteSource cache
```

Remote entries must carry location data, not just display text:

```rust
enum AgentPanelEntryLocation {
    Local { ws_idx, tab_idx, pane_id },
    Remote { host, session, workspace_id, tab_id, pane_id, terminal_id },
}
```

This lets click-to-focus route precisely to the owning remote node.

Display examples:

```text
Agents
  pi                 working
  codex              blocked
  jafar/codex        working
  jafar/claude       idle
  work-mini/pi       disconnected
```

Remote sidebar rows are native sidebar entries, not a static dashboard pane.

### 9. Cross-host command routing

Local targets continue resolving through existing local target logic.

Remote targets resolve to a remote owner and are proxied over a remote API bridge.

Allowed command categories for MVP:

```text
ping/status/capability checks
agent.list        # public aggregated local + cached configured remotes
agent.list_local  # hidden/internal local-only snapshot primitive for supervisors
agent.get
agent.read
agent.send
agent.rename
agent.focus
agent.wait (composed via events.wait / pane.wait_for_output; not a single API method)
agent.start
pane.list
pane.get
pane.read
pane.send_text
pane.send_keys
pane.send_input
events.subscribe for cache/wait state
```

Denied by default over federation:

```text
server.stop
server.live_handoff
server.reload_config
integration.install
integration.uninstall
broad workspace/tab destructive mutation
```

Workspace/tab creation or pane placement may be allowed only through explicit remote `agent.start` placement features.

`agent.wait` is not a routable API method — the API exposes no such op. The CLI's `agent wait` is composed from the long-held wait primitives (`events.wait` / `pane.wait_for_output`). Over federation it is realized by proxying those primitives to the owning node on a dedicated bridge and running the wait/match logic locally, not by forwarding an `agent.wait` call.

Current spike API shape: aggregated `agent.list` host-qualifies label fields so they can be passed back to `agent.get/read/send`, but it keeps raw remote `terminal_id`, `pane_id`, `workspace_id`, and `tab_id` values. Those raw IDs are authoritative on the owning host but not globally unique. A future schema should add machine-readable host/session fields instead of encoding location into labels.

Non-idempotent retry rule:

- `agent.send`, `pane.send_text`, `pane.send_keys`, and `pane.send_input` are not idempotent.
- If the bridge drops after dispatch but before acknowledgement, surface an uncertain-delivery error and do not auto-retry.
- `agent.list`, `agent.get`, `pane.read`, and status/ping calls are safe to retry within bounded timeout rules.

### 10. Remote agent start placement

Remote start must have deterministic placement. Do not use "remote active pane" as the default because the remote Herdr session can have multiple clients or no active client.

MVP default:

```text
agent.start --host <host> with no remote placement creates a new remote workspace for that agent.
```

Future/optional placement flags may route to a specific remote workspace/tab:

```bash
herdr agent start --host jafar --workspace <remote-workspace-id-or-name> --name codex --cwd /remote/path -- codex
herdr agent start --host jafar --tab <remote-tab-id> --split right --name codex --cwd /remote/path -- codex
```

Placement flags, when present with `--host`, refer to remote workspace/tab/pane IDs on the remote host, not local IDs.

### 11. Cross-node focus / direct remote terminal attach

Click-to-focus is two operations:

1. API operation: proxy `agent.focus` to the remote node so the remote session's own focus is consistent.
2. Render/terminal operation: open a direct remote terminal attach/proxy to the remote terminal.

MVP attach behavior:

```text
focus jafar/codex
  -> remote API bridge: agent.focus codex on Jafar
  -> remote render/terminal attach bridge: attach to Jafar terminal_id
  -> user interacts with remote terminal
  -> detach returns to local Herdr UI
```

The attach/proxy path rides the remote render/client/terminal attach machinery, not the remote API bridge.

Open decisions for implementation:

- Whether attach temporarily replaces the local UI or opens a special local proxy surface.
- How duplicate attaches are shown.
- How writable ownership/takeover works if the remote terminal already has a writable attach owner. The protocol primitive already exists — `ClientMessage::AttachTerminal { terminal_id, takeover }` (`src/protocol/wire.rs`) carries a `takeover` flag — so this is a policy/UX decision, not new protocol.
- How resize is forwarded and how detach returns to previous local focus.

MVP should prefer direct terminal attach over nested full remote Herdr UI. Embedded split-pane proxy views are deferred.

### 12. Remote hooks and integrations

Remote hooks remain unchanged in principle.

```text
Remote agent on Jafar
  -> Jafar hook
  -> Jafar HERDR_SOCKET_PATH Unix socket
  -> Jafar Herdr server
  -> local SteamDeck aggregator learns state through remote API/events
```

Do not expose the SteamDeck local API socket to Jafar for hook reporting. Do not set remote `HERDR_SOCKET_PATH` to point at SteamDeck.

### 13. Durability model

Durability remains per authoritative Herdr node.

```text
Local agents -> local Herdr persistence/restore/handoff.
Jafar agents -> Jafar Herdr persistence/restore/handoff.
Work Mini agents -> Work Mini Herdr persistence/restore/handoff.
```

The local aggregating server persists:

- remote host configuration;
- remote session names;
- user preferences for auto-connect and display;
- last-known cache only if useful for startup UI, but authoritative state must be refreshed from remote nodes.

It does **not** persist or own remote PTY lifecycle.

### Local aggregator restart

If SteamDeck Herdr restarts:

1. Jafar agents continue running on Jafar if Jafar Herdr remains up.
2. SteamDeck reloads remote host config.
3. SteamDeck reconnects remote API bridges non-interactively.
4. SteamDeck resyncs each host/session using subscribe/buffer/snapshot/replay.
5. Sidebar remote entries reappear with current state.
6. Any active remote attach/proxy view is not auto-restored in MVP; the user can click the remote agent again.

### Remote node restart

If Jafar Herdr restarts:

1. Jafar handles its own session restore using existing Herdr semantics.
2. SteamDeck marks Jafar stale/disconnected during restart.
3. On reconnect, SteamDeck refreshes the Jafar snapshot.
4. Remote terminal/pane IDs may change; named remote targets are refreshed from the new snapshot.

### Network drop

If SteamDeck loses network/Tailscale/SSH access to Jafar:

- SteamDeck marks `jafar` as stale/disconnected after heartbeat/read/write failure.
- Last-known Jafar entries remain visible as stale/disconnected.
- Jafar agents keep running on Jafar if Jafar remains healthy.
- On reconnect, SteamDeck resyncs and clears stale state.

## Commands

Remote host management:

```bash
herdr remote add jafar --target jafar --session default
herdr remote list
herdr remote status jafar
herdr remote connect jafar
herdr remote reconnect jafar
herdr remote disconnect jafar
herdr remote remove jafar
```

Semantics:

- `remote add` saves config and may validate/provision interactively.
- `remote connect` ensures a bridge now, interactive if setup is required.
- `remote reconnect` tears down and reopens bridges.
- `remote disconnect` stops local aggregation/bridges only; it does not stop the remote Herdr server.
- `remote remove` removes local config and tears down local bridges/attaches only; it does not stop the remote Herdr server.

Remote agent commands:

```bash
herdr agent list
herdr agent list --host jafar
herdr agent read jafar/codex --lines 80
herdr agent send jafar/codex "continue"
herdr agent focus jafar/codex
herdr agent wait jafar/codex --status done
herdr agent start --host jafar --name codex --cwd /Users/afayed/projects/mentat -- codex
```

Avoid using subcommand `--remote` for this feature because root-level `--remote <ssh-target>` already means remote TUI attach.

## Security Model

Federation uses the existing SSH trust boundary.

- The local aggregator can drive allowed API methods on each configured remote node.
- `agent.start` runs arbitrary commands on the remote host, intentionally, through SSH-authenticated Herdr control.
- Compromise of the aggregating local node can control configured remote nodes within the allowed method set and SSH account permissions.
- The remote API bridge grants no privilege beyond what the SSH user could already do by logging in and using that user's `~/.config/herdr/.../herdr.sock`.
- Configured SSH targets must be validated before invoking `ssh`; leading-dash targets are rejected so a host value cannot be interpreted as an SSH option.
- The local API socket is not exposed to remote hosts.
- Remote hooks report to their own local node only.
- Do not persist proxied remote pane contents in local snapshots in MVP.

## Relationship to Existing `herdr --remote`

Existing:

```bash
herdr --remote jafar
```

means:

```text
Run a local thin client connected to Jafar's Herdr server/render socket.
Jafar renders the whole UI.
The local terminal displays Jafar's UI.
```

New federation:

```bash
herdr remote add jafar --target jafar
```

means:

```text
Keep using the local Herdr UI.
Connect to Jafar's Herdr API socket over SSH.
Aggregate Jafar agents into the local sidebar.
Route host-qualified commands to Jafar.
Open a direct remote terminal attach/proxy when focusing a Jafar agent.
```

Both can reuse SSH/bootstrap/version code. They are different UX modes and should remain distinct.

## `src/remote.rs` Reuse

Reusable:

- SSH target validation.
- shell quoting.
- remote platform detection.
- remote binary discovery/version/protocol matching.
- remote binary bootstrap/install.
- stdio bridge pattern.
- remote server status/version checks.

Not directly reusable as the core model:

- `run_remote` as-is, because it attaches a local thin client to a remote-rendered UI.
- `remote-client-bridge` as-is, because it targets the remote client/render socket, not the remote API socket.
- remote server handoff logic as a control-plane primitive for proxying agents, though it remains useful for managing remote Herdr server version compatibility.

New reusable extraction target:

```text
remote bootstrap utilities
remote socket bridge utilities
remote API bridge command
remote render/terminal attach bridge reuse for focus
```

## Failure Scenarios

### SSH auth failure / host key change

- Mark remote `AuthFailed` or `HostKeyFailed`.
- Do not retry aggressively; backoff will not fix it.
- Surface actionable error in `remote status` and sidebar.

### Remote unreachable

- Mark host/session `Disconnected` or `Reconnecting`.
- Keep last-known entries as stale/disconnected.
- Retry with bounded exponential backoff and jitter.

### Protocol mismatch / remote upgraded mid-session

- Mark host/session `Incompatible`.
- Stop applying events from old bridge.
- Require explicit reconnect/update path.

### Remote agent exits / pane closes

- If connected and event/snapshot confirms absence, remove entry or mark done/closed according to remote state.
- If disconnected, keep stale until reconnect confirms absence.

### Active proxy attach disconnects

- Detach/proxy view shows disconnected.
- Remote process remains owned by remote Herdr.
- User can retry focus after reconnect.

### Command in flight when bridge drops

- For read/list/status: retry if safe and within timeout.
- For send/input/start: do not auto-retry after uncertain dispatch. Surface uncertain-delivery error.

## Testing Strategy

Do not require two physical machines for core tests.

Recommended test layers:

1. Unit tests for target grammar and host-qualified resolution.
2. Unit tests for cache merge/revision/stale behavior.
3. Unit tests for RemoteSource supervisor state transitions with a mock remote API.
4. Integration tests using a second local Herdr named session as a stand-in remote.
5. Optional SSH integration tests for real remote bridge behavior.

Example local test topology:

```text
Herdr session A = aggregator
Herdr session B = fake remote node
remote-api-bridge connects A to B over a local bridge or mocked SSH transport
```

## Implementation Phases

### Phase 0 — Remote API bridge and provisioning spike

Goal: prove the local Herdr process can invoke the remote Herdr API socket over SSH using existing remote bootstrap patterns.

Non-goals:

- sidebar UI;
- cross-host target routing;
- event subscription;
- remote focus;
- remote agent start.

Expected modules/files:

```text
src/remote.rs
src/main.rs
src/api/client.rs
src/cli.rs or src/cli/remote.rs
src/config/model.rs (if config is added in this phase)
tests for remote bridge/target parsing where feasible
```

Data model sketch:

```rust
struct RemoteHostConfig {
    name: String,
    target: String,
    session: String,
    auto_connect: bool,
}
```

Control flow:

```text
herdr remote ping jafar
  -> load remote config or use CLI target
  -> ensure compatible remote Herdr binary/session if interactive
  -> open one remote-api-bridge for one API request
  -> send ping/status request
  -> print result
```

CLI/API shape:

```bash
herdr remote add jafar --target jafar --session default
herdr remote ping jafar
herdr remote status jafar
```

Acceptance criteria:

- `herdr remote ping jafar` succeeds when Jafar is reachable and Herdr is running or can be started.
- Failure cases for unreachable host and protocol mismatch are surfaced clearly.
- Existing `herdr --remote jafar` behavior remains unchanged.

Tests:

- parse/validate remote config names (`/` and `:` rejected in alias);
- one-request bridge can round-trip a `ping` against a mock/local socket;
- incompatible protocol produces explicit error.

Constraints / do-not-do:

- Do not implement a custom bridge multiplexer in MVP.
- Do not expose the local API socket to the remote host.
- Do not change existing `--remote` semantics.
- Do not start remote PTY backend work.

### Phase 1 — RemoteSource supervisor and read-only sidebar aggregation

Goal: show remote agents in the local sidebar with correct live/stale state.

Non-goals:

- remote read/send/wait;
- remote focus/attach;
- remote start;
- embedded panes.

Expected modules/files:

```text
src/app/state.rs
src/events.rs
src/ui/sidebar.rs
src/api/schema.rs or local wrapper types
src/remote_source.rs or src/app/remote_agents.rs
src/config/model.rs
```

Data model sketch:

```rust
enum RemoteConnectionState {
    Disabled,
    Connecting,
    Connected,
    Stale,
    Reconnecting,
    Disconnected,
    AuthFailed,
    Incompatible,
    NeedsSetup,
}

struct RemoteSourceState {
    host: RemoteHostKey,
    state: RemoteConnectionState,
    last_seen: Option<Instant>,
    last_error: Option<String>,
}

struct RemoteAgentCacheEntry {
    location: RemoteAgentLocation,
    info: AgentInfo,
    stale: bool,
}
```

Control flow:

```text
on startup/config reload
  -> start RemoteSource supervisors for auto_connect hosts
  -> each supervisor opens subscription bridge
  -> buffer events
  -> pull snapshot
  -> merge snapshot and buffered events
  -> send AppEvent to update pure state cache
  -> sidebar renders local + remote entries
```

Acceptance criteria:

- Start an agent in the remote Herdr session.
- Local sidebar shows `host/agent` with current state.
- Public local `agent.list` shows cached configured remote agents as host-qualified entries; the supervisor snapshot path remains local-only via `agent.list_local`.
- Disconnect remote host; sidebar/API list marks entries stale/disconnected.
- Reconnect remote host; sidebar/API list resyncs without duplicate entries.

Tests:

- cache merge removes missing agents only when connected;
- disconnected keeps stale entries;
- buffered event with older revision is ignored;
- subscription set excludes output-firehose events.

Constraints / do-not-do:

- Do not route control commands yet.
- Do not persist proxy placement.
- Do not subscribe to output events (`pane.output_matched`) for aggregation; there is no raw `PaneOutputChanged` subscription anyway.
- Do not mutate AppState from supervisor threads directly.

### Phase 2 — Cross-host read/send/wait routing

Goal: local Herdr API can control remote agents by host-qualified target.

Non-goals:

- remote focus/attach;
- remote start;
- workspace/tab mutation beyond allowlist.

Expected modules/files:

```text
src/app/terminal_targets.rs
src/app/api.rs
src/api/wait.rs
src/cli/agent.rs
src/api/schema.rs or routing wrapper types
```

Control flow:

```text
herdr agent read jafar/codex
  -> parse host-qualified target
  -> resolve against RemoteSource cache
  -> open one request bridge to Jafar
  -> send remote agent.read
  -> return remote response
```

Spike limitation: the first implementation performs the one-request SSH bridge synchronously in the App API handler. That proves the routing path, but it can block the local UI/server loop while SSH probes and request execution run. A shipping implementation should move remote request execution off the main loop and reuse supervisor compatibility/preparation state instead of probing the remote binary on every read/send.

Acceptance criteria:

```bash
herdr agent read jafar/codex --lines 50
herdr agent send jafar/codex "continue"
herdr agent wait jafar/codex --status done
```

work from the local machine and from agents running in local panes.

Tests:

- host-qualified target routes remote;
- bare target routes local only;
- public `agent.list` aggregates cached configured remotes while hidden `agent.list_local` stays local-only for supervisor snapshots;
- non-idempotent send is not retried after uncertain delivery;
- denied methods are rejected;
- Docker/local SSH smoke proves configured remote -> supervisor cache -> API list/get -> host-qualified send/read -> disconnect stale -> reconnect without duplicates.

### Phase 3 — Cross-node focus / direct remote terminal attach

Goal: clicking/focusing a remote agent takes the user to the exact remote terminal.

Non-goals:

- embedded splittable local proxy pane;
- persistent proxy placement.

Expected modules/files:

```text
src/ui/sidebar.rs
src/client/mod.rs
src/remote.rs
src/app/api.rs
src/cli/agent.rs
```

Control flow:

```text
click jafar/codex
  -> route remote agent.focus to Jafar API
  -> open remote render/terminal attach bridge to Jafar terminal_id
  -> user interacts
  -> detach returns to local UI
```

Acceptance criteria:

- Clicking a remote sidebar agent opens an interactive view of the exact remote terminal.
- Detach returns to local Herdr.
- If bridge drops, view reports disconnected and remote process remains running.

Open decisions to resolve before implementation:

- writable ownership/takeover if remote terminal already has an attach owner (the `AttachTerminal { takeover }` primitive already exists in `src/protocol/wire.rs`; this is a policy/UX choice, not new protocol);
- exact detach key/path;
- whether attach replaces the local TUI temporarily or opens a special local proxy surface.

### Phase 4 — Remote agent start

Goal: start a remote agent from the local Herdr command/API and have it appear in the remote Herdr session and local sidebar.

Non-goals:

- complex remote placement across arbitrary remote layouts in MVP;
- embedded local panes.

Default placement:

```text
new remote workspace for the agent unless explicit remote placement flags are provided
```

Acceptance criteria:

```bash
herdr agent start --host jafar --name codex --cwd /Users/afayed/projects/mentat -- codex
```

creates a Jafar Herdr pane/terminal and local sidebar entry.

Constraints:

- `--cwd` is a remote path.
- Do not use ambiguous remote active pane as default.
- Placement IDs, if supplied, are remote IDs.

### Phase 5 — Resilience, docs, and polish

Goal: make federation reliable and documented.

Tasks:

- remote list/status/remove/connect/disconnect polish;
- protocol mismatch messaging;
- auth/host-key failure messaging;
- stale/disconnected UI styling;
- docs for sessions vs workspaces;
- tests for restart/reconnect flows;
- optional internal SSH ControlMaster optimization.

Production validation should go beyond the Docker/localhost SSH smoke. Before shipping, test at least one real SSH-reachable remote host, multiple configured hosts, slow/unreachable SSH, sleep/offline/wake reconnect behavior, and a Tailscale/MagicDNS target if that is a supported deployment path. The Docker smoke proves the controlled one-hop federation path, but it does not prove real-network behavior or tailnet naming/reachability assumptions.

Acceptance criteria:

- SteamDeck restart re-aggregates remote agents.
- Jafar restart is handled by Jafar Herdr and then resynced.
- Network drop does not kill remote agents.
- `herdr remote status` clearly explains disconnected/auth/incompatible states.

### Phase 6 — Optional embedded remote panes

Only if later required.

This phase would revisit the earlier remote-backed local pane idea:

```text
local pane -> remote PTY runtime
```

It would require:

- real terminal runtime backend abstraction;
- remote PTY helper;
- remote hook relay or local proxy strategy;
- handoff/persistence special cases.

This is intentionally not part of the MVP because authoritative remote nodes satisfy the current click-to-focus and agent-to-agent control requirements with far less architectural risk.

## Why Not Local-Owned Remote PTYs?

The earlier cmux-style idea was:

```text
local Herdr owns pane/sidebar
remote machine owns raw PTY process
remote hooks relay back to local Herdr
```

That design fits if the requirement is an embedded remote process inside the local pane layout. It is not the best fit for Ahmed's clarified requirements:

- Remote agent should focus the exact remote session/tab/pane where it lives.
- All hosts can run Herdr.
- Durability should inherit existing Herdr behavior.
- Agents need cross-host Herdr API control.

Authoritative remote nodes satisfy these directly. Local-owned remote PTYs would duplicate Herdr's hardest existing features on the wrong host: PTY lifecycle, hooks, process detection, session restore, handoff, and persistence.

## Recommended Next Step

Implement Phase 0 on a feature branch:

```bash
git checkout -b feature/federated-remote-agents
```

First milestone: local Herdr can connect to Jafar's Herdr API socket over SSH and call `ping`/`agent.list` through a one-request `remote-api-bridge` built from existing `src/remote.rs` primitives.
