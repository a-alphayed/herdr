# Federated Remote Agent Control Design

Status: draft proposal  
Owner: TBD  
Created: 2026-05-31  
Updated: 2026-07-05

## Summary

This document records Herdr's federated remote-agent control design and the next sidebar presentation direction.

The current shipped federated MVP adds first-class multi-host agent control to Herdr: agents running on configured remote machines can be aggregated with local agents in the normal local Herdr sidebar/API surfaces and controlled through the same Herdr agent APIs as local agents.

The forward UI direction is source/machine projection. The local Herdr node still aggregates configured remote sources, but the proposed next sidebar presentation selects one source at a time (`local`, `jafar`, `work-mini`) and renders that source through the vanilla Herdr `Spaces` and `Agents` panel chrome plus a source rail. This is a presentation/read-model layer, not a change in remote authority and not current shipped behavior.

The design is **authoritative remote Herdr nodes with local aggregation and proxying**:

```text
Every host runs its own authoritative Herdr server.
The local Herdr server aggregates remote AgentInfo and proxies agent control over SSH.
Remote hooks report to their own host's local Herdr socket, unchanged.
Clicking a remote agent focuses/attaches to the exact remote session/tab/pane where it lives.
Durability remains per authoritative Herdr server.
```

This is **not** a "local server owns a remote PTY" design. Remote panes and agents are owned by the Herdr server on the machine where they run. The local Herdr instance acts as an aggregation/control plane: it displays remote metadata, routes allowed commands, and opens direct attach/proxy views for interaction. The current MVP can show local and remote agents together; the next presentation layer should project one selected source at a time without changing ownership.

## Headless prompt submit and teardown

Verified on 2026-07-03 with both the controller and remote host running Herdr 0.7.1 / protocol 15:

- `agent start --host ...` can place a remote lane and the controller can detect it as `<node>/<name>`.
- `agent read <node>/<target>` can read remote pane content.
- `agent send <node>/<target> "text"` writes text into a composer-style target, but does not submit the composer prompt; the remote prompt-processing agent remains idle. Readback of text sitting in the composer proves only text injection, not agent processing. `agent send` remains literal text injection by design.
- Before Phase F, no federation-only controller teardown path existed for remote panes/workspaces/lane surfaces placed by the controller.

### Submit-capable route (Phase E.0)

`agent.submit` (`herdr agent submit <target> <text>`) is the submit-capable controller route for composer-style agent targets. Submit writes the text followed by an encoded Enter so the prompt is actually submitted, without an interactive attach. It is distinct from `agent send`:

- `agent send` = literal text injection (no Enter).
- `agent submit` = text plus Enter (prompt submission).

Remote submit is federation-routed like `agent.send`: the controller resolves a host-qualified target against its `RemoteSourceCache`, rewrites it to the remote terminal id, and sends an `agent.submit` request to the authoritative remote Herdr over the SSH-bridged JSON API. It is gated by a new `agent_submit` federation capability: a remote that does not advertise `agent_submit` fails through the existing needs-update path and does not fall back to `agent.send`. Submit is non-idempotent and is not retried after uncertain delivery; the remote host remains the PTY/process authority.

The controller-only headless orchestration path is `place -> submit prompt -> agent reacts -> controller reads reaction`, with no SSH/manual attach step. `agent.submit` provides the submit primitive; full end-to-end controller-only runtime proof for composer-style agents is the E.0 validation step.

### Interactive projected pane attach (Phase E.1)

Phase E.1 makes remote workspace projection interactive: a projected pane that has a live `terminal_id` can be clicked or focused-and-Enter'd to open a local attach split, replacing the previous read-only view.

Mechanism:

- `layout.export` pane nodes now carry `terminal_id` (the server-assigned transient id; omitted by older remotes).
- `compute_view_internal` computes `RemoteProjectionHitArea` geometry for each projected pane, carrying `host`, `session`, `terminal_id`, `focused`, and `live` state.
- Mouse click on a live hit area, or plain Enter on the focused live pane, sets `request_remote_attach_in_new_split` to a `RemoteAttachTarget { host, session, terminal_id, label }` and clears `selected_remote_space`.
- `precheck_remote_attach_target` validates host connectivity via `host_status()` only; it does not require the `terminal_id` to be in the local agent cache — the remote server validates it.
- Paste and all non-Enter keys continue to be swallowed while a remote projection is selected. The plain-Enter exception is narrow: plain Enter (no modifiers) only, and only when the hit area is both focused and live with a `terminal_id`.
- Stale projections (last-known snapshots from a disconnected host) remain fully read-only; their panes render without the live border style and do not respond to click or Enter.
- `<host>/terminal:<id>` is also exposed as a CLI target form: `herdr agent attach <host>/terminal:<id> [--takeover]`.

Invariants preserved:

- `RemoteSpaceKey { host, session, workspace_id }` is SELECTION-ONLY — it is not a `RemoteAttachTarget`.
- No-takeover default; `--takeover` is CLI-only.
- The runtime-only attach split is not persisted.
- Projection hit areas take precedence in mouse routing when a projection is selected.
- Remote source authority, hook relay, and PTY ownership are unchanged.

### Teardown (first narrow path)

The first narrow, explicitly destructive, capability-gated controller-side teardown path is now in place for federation-placed agent/lane surfaces. It is deliberately NOT broad remote pane/workspace/server/process management.

- API: `agent.teardown` with `{ target, confirm }`. `confirm` defaults to `false` and the controller rejects the request with `confirmation_required` before route planning or remote forwarding when it is absent or false.
- CLI: `herdr agent teardown <host>/<target> --confirm`. The CLI rejects a missing `--confirm` before sending; the API enforces `confirm` as well.
- Capability: the route requires the remote host to advertise `agent_teardown` (in addition to `remote_api_bridge`). A remote that does not advertise it fails with `does not advertise federation method agent_teardown` and does not fall back.
- Target resolution: host/session-qualified. The controller resolves the projection-derived target against `RemoteSourceCache` and forwards `agent.teardown` to the authoritative host with the resolved `terminal_id` and `confirm: true`. Workspace selectors remain unsupported.
- Local authoritative close: the close flows through the existing `close_pane` path, preserving `pane.closed` / `workspace.closed` events, session save, and runtime cleanup. The target must carry agent identity (`agent_info_for_target`); non-agent terminal/pane ids fail as an agent-target error and never become a general pane close.
- The worktree-group `confirmation_required` guard on `close_pane` is NOT bypassed by teardown's `confirm: true`; a teardown that would collapse a worktree group still surfaces `confirmation_required`.
- No PID kill, process/host/server stop, provisioning, arbitrary SSH shell commands, or local-owned remote PTY behavior is exposed. The operation is non-idempotent and performs no uncertain retry: a remote `close_pane` that happens to close the workspace (last pane) is a consequence of the authoritative close path, not an exposed raw workspace close.

Broad remote pane/workspace/server/process teardown remains out of scope except for this narrow federation-placed agent/lane surface and the separate confirmed projected-pane close described below.

### Remote pane split (Phase F.2)

`pane.split` is federation-routed for host/session-qualified pane targets. It is deliberately narrow: only the existing split-right/down operation is routed, and it creates a pane through the authoritative remote Herdr rather than exposing broad remote layout management.

- API: existing `pane.split` with `{ workspace_id, target_pane_id, direction, ratio, cwd, env, focus }`; no schema change and no protocol bump.
- CLI: `herdr pane split <host>/<target> --direction right|down`, where `<target>` is `terminal:<id>`, `pane:<id>`, or `workspace:<id>`. Bare/local targets keep the local path.
- Capability: the route requires the remote host to advertise `pane_split` (in addition to `remote_api_bridge`). A remote that does not advertise it fails with `does not advertise federation method pane_split` and does not fall back.
- Target resolution: the controller resolves against live active-tab projections from `layout.export`, not against the agent cache. Stale/unavailable projections and older projections without terminal ids are rejected before mutation.
- Remote authority: the forwarded request carries the resolved remote `workspace_id` and `target_pane_id`; `cwd` and `env` are remote-scoped and pass through without local expansion.
- The operation is non-destructive but non-idempotent. It has no confirmation gate and no uncertain-delivery retry.

### Confirmed projected-pane close (Phase F.2)

`pane.close` now has a narrow federation route for closing one pane from a live source projection. This is the carve-out from the broader remote teardown ban: it is an explicit, confirmed close of the projected pane/process on its authoritative host, not a general remote workspace/tab/server/process management API.

- API/schema: `pane.close` uses `{ pane_id, confirm }`. `confirm` defaults to `false` and is omitted when false. Local `pane.close` remains backward-compatible; the existing worktree-group `confirmation_required` guard on the authoritative close path remains separate and is not bypassed by `confirm: true`.
- CLI: `herdr pane close <host>/<target> --confirm`, where `<target>` is `terminal:<id>`, `pane:<id>`, or `workspace:<id>`. Bare/local targets keep the local path; a configured host-qualified remote target without `--confirm` is rejected before sending.
- Capability: the route requires the remote host to advertise `pane_close` (in addition to `remote_api_bridge`). A remote that does not advertise it fails clearly with `does not advertise federation method pane_close` and does not fall back.
- Target resolution: the controller requires `confirm: true` before host status checks, projection-cache resolution, or bridge send. It resolves host/session-qualified terminal, pane, and workspace selectors against live active-tab projections from `layout.export`; workspace selectors resolve to the focused live projected pane, not to broad workspace close. Stale/unavailable projections and older projections without terminal ids are rejected before mutation.
- Remote authority: the forwarded request carries the resolved authoritative remote `pane_id` and `confirm: true`. The remote node runs its normal local `close_pane` path, preserving `pane.closed` / `workspace.closed` events, session save, runtime cleanup, and the separate worktree-group confirmation guard. If the closed pane is the last pane, closing the workspace is a consequence of the authoritative local close path, not a separate workspace-close operation.
- UI: a live projected pane context menu exposes **Close pane**. The confirmation overlay names the host, session, and target label and states that it closes the remote pane/process on that host. Confirm dispatches `tui.remote_projection.pane.close` as `pane.close { pane_id: "<host>/terminal:<terminal_id>", confirm: true }` and returns to the prior local terminal/navigate mode; cancel mutates neither local nor remote panes.
- Non-goals remain: no agent-label close, no broad workspace/tab destructive mutation, no PID kill, no process/host/server stop, no arbitrary SSH command, no local-owned remote PTY behavior, and no uncertain-delivery retry.

### Remote tab creation, switching, and confirmed close (Phase F.2)

Projected remote tabs are now federation-routed to the authoritative remote Herdr host. The local node renders cached remote tab metadata and forwards only capability-gated tab mutations; it still does not own remote PTYs, tabs, hooks, persistence, or child processes.

- API/schema: `tab.create` may target `workspace_id: "<host>/workspace:<remote-workspace-id>"`; `tab.focus` may target `tab_id: "<host>/tab:<remote-tab-id>"`; `tab.close` uses `{ tab_id, confirm }`, with `confirm` defaulting to `false` and omitted when false. Bare/local tab calls remain backward-compatible.
- CLI: `herdr tab close <host>/tab:<id> --confirm` is required for configured remote hosts. A configured host-qualified remote tab close without `--confirm` is rejected before sending. Bare/local tab close keeps its prior behavior.
- Capability: remote tab routes require `tab_create`, `tab_focus`, or `tab_close` respectively, in addition to `remote_api_bridge`; `tab_list` remains the metadata capability for cached remote tab lists.
- Target/cache resolution: `tab.create` resolves the host-qualified workspace target against a connected direct remote workspace snapshot. `tab.focus` and `tab.close` resolve `tab:<id>` against connected, fresh per-workspace `tab.list` snapshots. Stale, unavailable, disconnected, or missing metadata is rejected before forwarding.
- Confirmation: remote `tab.close` requires `confirm: true` before host status checks, cache resolution, or remote forwarding. Closing the last remote tab is not special-cased by the controller; the authoritative remote tab close path returns its normal `cannot close the last tab` error.
- Remote authority: forwarded requests carry raw authoritative remote ids only. `tab.create` rewrites `workspace_id` to the remote workspace id and passes `cwd`, `env`, `focus`, and `label` through unchanged as remote-scoped user input. The local node does not expand local paths, forward local env/secrets, create local-owned remote PTYs, relay hooks, perform transitive routing, or expose broad host/process/server management.
- Cache/UI freshness: the remote supervisor caches per-workspace `TabInfo` snapshots and uses them for the projected tab strip and active-tab label; `layout.export` remains scoped to the active tab. After successful create/focus/close, the controller patches or refreshes the tab/projection cache promptly instead of waiting for the normal supervisor loop.
- UI: a live projected remote workspace shows a compact remote tab strip, a new-tab affordance when `tab_create` is advertised, click-to-switch when `tab_focus` is advertised, and a close affordance when `tab_close` is advertised. Confirmed close names the host/session/workspace/tab. Remote tab hit areas are separate from suppressed local tab hit areas, and no remote tab controls are enabled for stale, unavailable, or disconnected projections.
- Non-goals remain: no broad remote workspace close, no split dragging/resize expansion, no PID kill, no arbitrary SSH command, no transitive remote-of-remote routing, no local-owned remote PTY behavior, and no uncertain-delivery retry for non-idempotent tab create/close.

### Remote workspace, tab, and pane rename (Phase F.2)

`workspace.rename`, `tab.rename`, and `pane.rename` are federation-routed for host-qualified targets. Each route is individually capability-gated; a remote host can advertise rename for workspaces without advertising it for tabs or panes.

- API: existing `workspace.rename`, `tab.rename`, and `pane.rename` with their existing params. No schema change and no protocol bump. Bare/local targets keep their local paths.
- Capability: `workspace.rename` requires the remote host to advertise `workspace_rename`; `tab.rename` requires `tab_rename`; `pane.rename` requires `pane_rename` (each in addition to `remote_api_bridge`). A remote that does not advertise the required method fails with `does not advertise federation method <method>` and does not fall back.
- Target resolution: `workspace.rename` resolves the host-qualified workspace id against the connected remote workspace snapshot. `tab.rename` resolves the host-qualified tab id against connected per-workspace tab list snapshots. `pane.rename` resolves the host-qualified terminal id against live active-tab projections from `layout.export`. Stale, unavailable, or disconnected cache snapshots are rejected before mutation.
- Remote authority: the forwarded request carries the resolved authoritative remote id and the new label. The local node does not expand or transform the label. After a successful `tab.rename` the controller patches the tab cache and refreshes the tab/projection metadata promptly. After a successful `workspace.rename` the controller updates the workspace metadata cache entry. After a successful `pane.rename` the projected pane label updates on the next supervisor refresh cycle (Phase F.2 limitation; no immediate projection patch).
- UI: the context menu for a live projected pane exposes **Rename pane** and **Clear pane name**. **Rename pane** opens the rename modal with the current label pre-filled; saving dispatches `tui.remote_projection.pane.rename` as `pane.rename { pane_id: "<host>/terminal:<terminal_id>", label: <new_name> }`. **Clear pane name** dispatches `tui.remote_projection.pane.clear_name` as `pane.rename { pane_id: "<host>/terminal:<terminal_id>", label: null }` immediately without a confirmation step. Remote tab rename and remote workspace rename are API/CLI-only in this slice; projected tab strip and workspace sidebar rename affordances are deferred to a later phase.
- Non-goals remain: no broad remote workspace close, no PID kill, no arbitrary SSH command, no transitive remote-of-remote routing, no local-owned remote PTY behavior, and no uncertain-delivery retry.

### Remote pane focus and focus direction (Phase F.2)

`pane.focus` and `pane.focus_direction` are federation-routed for host-qualified targets. These let the controller move the focused pane on the remote host without creating a local attach.

- API: existing `pane.focus` with `{ pane_id }` and `pane.focus_direction` with `{ pane_id, direction }`. No schema change and no protocol bump. Bare/local targets keep their local paths.
- Capability: `pane.focus` requires the remote host to advertise `pane_focus`; `pane.focus_direction` requires `pane_focus_direction` (each in addition to `remote_api_bridge`). A remote that does not advertise the required method fails clearly and does not fall back.
- Target resolution: resolves the host-qualified terminal id against live active-tab projections from `layout.export`. Stale, unavailable, or disconnected projections are rejected before forwarding.
- Remote authority: the forwarded request carries the resolved authoritative remote `pane_id`. The local node does not create a local-owned remote PTY or relay any remote hooks.
- UI: left-clicking a live projected pane dispatches `tui.remote_projection.pane.focus` (`pane.focus { pane_id: "<host>/terminal:<terminal_id>" }`) instead of opening a local attach split. The existing left-click attach path is preserved as **Attach in new split** in the context menu (right-click). The context menu also exposes **Focus pane** to reach the same remote focus action from the context menu.
- Non-goals remain: no embedded remote pane proxy, no local-owned remote PTY, no broad workspace/tab management, no arbitrary SSH command, no transitive routing, and no uncertain-delivery retry.

## Core Invariants

- A host owns the PTYs and child processes it runs.
- A Herdr server owns the workspaces, tabs, panes, terminals, hooks, and persistence inside its own session.
- Other Herdr servers may aggregate, route commands to, and proxy views of those panes, but they do not own the remote PTYs.
- Remote hooks report to the remote host's local Herdr socket. They do not relay to the aggregating host.
- Remote targets are always host-qualified in MVP.
- Existing `herdr --remote <target>` remains a remote TUI attach mode. Federated agents use a remote API bridge, not the render/client bridge.
- Source/machine projection is a UI/read-model selection. Selecting a source must not change local workspace ownership, focused PTY ownership, remote PTY lifecycle, or hook authority.
- The source-projection direction described here does not require a protocol bump by itself. Protocol/version changes are required only when server/client wire or API contracts change.

## Goals

- Current MVP: remote agents can appear in the normal Herdr sidebar/API surfaces alongside local agents.
- Next presentation layer: one selected source (`local`, `jafar`, `work-mini`) is projected through vanilla Herdr `Spaces` and `Agents` sidebar chrome.
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
- Treating a flat unified local+remote sidebar list as the required final UI. It remains valid current aggregation behavior, but source/machine projection is the forward presentation direction.
- Claiming source projection is shipped before the corresponding UI slice lands.

## Terminology

- **Local node / aggregator**: the Herdr server whose sidebar/UI the user is currently using, e.g. SteamDeck.
- **Remote node**: a configured remote Herdr server/session, e.g. Jafar's default Herdr session.
- **Remote host alias**: local configured name used in targets such as `jafar/codex`.
- **Source / machine source**: a selectable authority whose state can be projected into the local Herdr UI, e.g. `local`, `jafar`, or `work-mini`.
- **Source projection**: a UI/read-model selection that renders one source's spaces and agents through normal local Herdr chrome while that source remains authoritative for its own state.
- **Source rail**: the sidebar affordance for switching the active source projection.
- **All-machines pseudo-projection**: a possible future overview that intentionally shows multiple sources together; it is deferred and is not the default projection model.
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
herdr agent send jafar/codex "continue"    # text injection only; does not submit the composer prompt
herdr agent submit jafar/codex "continue"  # text plus Enter; submits composer-style prompts
herdr agent wait jafar/codex --status done
```

The local Herdr server parses the host-qualified target and proxies the API request to the owning remote node. `agent send` reaches the remote pane and writes text to a composer-style agent, but it does not submit the prompt. Use `agent submit` (`agent.submit`) for headless controller-only prompt submission; `agent send` stays literal text injection.

### 4. Click/focus a remote agent

Clicking `jafar/codex` in the local sidebar should:

1. route `agent.focus` to Jafar's API socket so Jafar's own session focuses the correct workspace/tab/pane;
2. open a direct remote terminal attach/proxy to that exact remote terminal;
3. return to the local Herdr UI on detach.

This is a direct remote terminal attach/proxy, not an embedded local split pane in MVP.

### 5. Agent-to-agent control

An agent running on SteamDeck can inspect Jafar agents and route allowed commands through the local SteamDeck Herdr API:

```bash
herdr agent list
herdr agent read jafar/codex
herdr agent send jafar/claude "continue"    # text injection only; does not submit the composer prompt
herdr agent submit jafar/claude "continue"  # text plus Enter; submits composer-style prompts
```

An agent running on Jafar can inspect/control SteamDeck agents only if Jafar's Herdr session also has SteamDeck configured as a remote host. MVP has no transitive routing; each node routes only to local agents and explicitly configured remotes. For composer-style agents, `agent submit` provides the federation-routed prompt-submission route; full controller-only runtime proof is the E.0 validation step.

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

The current MVP adds a second agent entry source for remote authoritative nodes rather than pretending remote agents are local panes or building a separate dashboard outside the sidebar. Source projection keeps that structured substrate but changes the presentation: the sidebar read model is filtered to one selected source instead of requiring local and remote rows to stay visible side by side.

The existing `herdr --remote <host>` mode is related but not sufficient. It runs a local thin client connected to a full remote Herdr server; the remote server owns the whole UI and the local side blits rendered frames. It cannot merge local and remote agents into one sidebar because the render stream is opaque rendered cells, not structured `AgentInfo`.

The structured substrate for federation is the Herdr JSON socket API:

```text
agent.list
agent.read
agent.send
agent.submit
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
```

Current shipped aggregation can expose local and configured remote agents together in sidebar/API surfaces:

```text
Aggregated agent view
  pi
  codex
  jafar/codex
  jafar/claude
  work-mini/pi
```

The proposed next sidebar presentation keeps the same aggregation substrate but projects one selected source at a time:

```text
Source rail
  local
  jafar
  work-mini

Active source: jafar

Spaces
  herdr
  fleet-api
  footer: new / menu

Agents
  codex
  claude
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
connection_policy = "auto"
connect_timeout_secs = 10

[[remote.hosts]]
name = "work-mini"
target = "work-mini"
session = "default"
connection_policy = "auto"

# A sleeping/roaming remote that must not be woken or auto-probed just
# because it is configured: it is never started automatically, and explicit
# mutating commands (`agent start --host steamdeck`) fail locally before any
# SSH/API dispatch. Read-only diagnostics (`herdr remote status steamdeck`)
# still probe it on demand.
[[remote.hosts]]
name = "steamdeck"
target = "steamdeck"
session = "default"
connection_policy = "manual"
```

Rules:

- `name` is the local alias used in targets. It must be unique and must not contain `/` or `:`.
- `target` is the SSH target.
- `session` is the Herdr session name on the remote host, not a workspace/space.
- `connection_policy` controls whether the local aggregator connects to a host automatically:
  - `auto` (default): the host is probed/connected automatically at startup and on config reload, seeded as a disconnected remote source, and treated as a configured auto source event sender. Explicit mutating commands still fail fast when its cached status is non-connected.
  - `on_demand`: the host is not probed automatically, but an explicit mutating command such as `agent start --host` may attempt a live bridge dispatch when there is no cached non-connected status; a cached disconnected/unreachable/needs-update status still fails fast before forwarding.
  - `manual`: the host is never reached implicitly. It is not probed automatically, and an explicit mutating `agent.start --host` fails locally before dispatch with a distinct policy error. Use this for sleeping/roaming remotes (e.g. a laptop or handheld that sleeps, roams between networks, or is often offline) so they are not auto-probed or woken merely because they are configured. Read-only diagnostics (`herdr remote status`/`check`) still probe named hosts regardless of policy.
  - The legacy `auto_connect` boolean remains a backward-compatible alias. Omitted fields resolve to `auto`; `auto_connect = true` resolves to `auto`; `auto_connect = false` resolves to `on_demand`. An explicit `connection_policy` may be combined with `auto_connect` only when consistent (`true` only with `auto`; `false` only with `on_demand` or `manual`); an inconsistent combination is rejected as a config error. `connection_policy` is the single stored source of truth; `auto_connect` is not retained as a separate stored field.
- Multiple sessions on the same SSH target should be configured with distinct aliases, e.g. `jafar-default` and `jafar-agents`.
- The remote source key is `(host_alias, session_name)`, not just host.
- `connect_timeout_secs` bounds the SSH `ConnectTimeout` (whole seconds) used for connection attempts to this host, for both interactive and noninteractive configured-host SSH invocations. Optional; defaults to 10 seconds (matching the previous hardcoded noninteractive default). Must be a non-zero value no greater than 300 seconds; config with an out-of-range value is rejected.

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

#### Probe timing and circuit-breaker / status policy

The supervisor runs two probes per host/session: a lightweight `ping` on a short cadence and a deeper `agent.list_local` / workspace / tab / layout `snapshot`, each with its own next-due timestamp. Each failing probe tick computes a single retry interval that consumes the shared transient counter at most once, so one failure never double-escalates the shared counter in a single tick. On a failed **ping** tick that one interval is reused for both the next ping and the next snapshot deferral (the loop sets `next_ping` and defers `next_snapshot` to at least the same interval, so an offline host does not immediately run the deeper probes in the same tick); on a failed **snapshot** tick one interval is computed for the snapshot only, while ping keeps running on its own cadence. Failure intervals are chosen by failure class:

- **Transient failures** (`Unreachable` / `Unknown`): the cached state for the host/session is preserved as stale, the supervisor emits an `unreachable` (`Unreachable`) or `disconnected` (`Unknown`) status, and the probe retries with bounded exponential backoff **plus deterministic jitter**. The pure base sequence is `15s, 30s, 60s, 120s, 240s`, capped at `300s`; jitter is layered on deterministically per `(host, session, failure-index)` via an in-tree FNV-1a hash so hosts de-synchronize instead of all retrying on the same wall-clock ticks, without randomness, wall-clock time, global state, or new dependencies. Below the cap the base is the lower bound and a small additive window (`0..=min(30, max(1, base/4))`) is added, then clamped to the cap; at the cap a subtractive window keeps hosts in `[270s, 300s]` so they never synchronize forever on exactly `300s`.
- **Setup / compatibility failures** (`NeedsUpdate`, e.g. missing/incompatible binary or invalid federation data): this is a **circuit-breaker** state, not a transient one. It keeps a fixed long retry interval (matching the backoff cap, currently `300s`), it does **not** consume or reset the transient counter, and a failed ping defers the deeper snapshot/projection probes until a future ping succeeds. A transient flap right after a `NeedsUpdate` therefore still starts the transient ladder from its base.
- **Circuit-breaker versus transient across ping and snapshot**: if `NeedsUpdate` surfaces during snapshot after a successful ping, ping continues on its normal cadence while only the failing deeper probe (snapshot) is held to the fixed long retry — ping is not stopped. If `NeedsUpdate` surfaces on ping, the deeper probes stay deferred until ping recovers.
- **Recovery**: any successful ping **or** snapshot clears the accumulated transient backoff, so the next transient failure restarts from the first jittered base interval for that host/session.

Probe timing does not change mutating command behavior. Commands still fail fast through the existing cache/status checks (stale, unavailable, disconnected, or missing-metadata targets are rejected before forwarding); there is no command queuing and no uncertain non-idempotent retry.

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

### 8. Sidebar aggregation and source projection

Current shipped behavior: Herdr aggregates local and configured remote agent metadata, so sidebar/API surfaces can show local and remote agents together. Remote entries are native structured entries, not local pane impostors and not a static dashboard.

Current local path:

```text
agent_panel_entries_with_runtimes(app, runtimes)
  -> local Workspace.pane_details(...)
```

Current aggregation path:

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

Forward presentation direction: source/machine projection. The same cached local/remote entry sources should feed a projected sidebar read model:

```text
Source rail
  local
  jafar
  work-mini

Active source: local

Spaces
  herdr
  fleet-api
  footer: new / menu

Agents
  pi
  codex
```

```text
Source rail
  local
  jafar
  work-mini

Active source: jafar

Spaces
  herdr
  fleet-api
  footer: new / menu

Agents
  codex
  claude
```

Projection rules:

- `local` projection preserves vanilla Herdr sidebar layout/navigation.
- Only one source projection is expanded at a time.
- Switching projected source changes the UI/read model only; it must not change active local workspace, focused PTY, or remote PTY lifecycle.
- Remote projection uses cached metadata from `RemoteSource`/`RemoteSourceCache` and routes only capability-gated commands to the authoritative remote host.
- The Spaces footer `new` action operates on the active projection: local creates local; remote creates remote only when connected and capable.
- Adding/provisioning a machine is a remote config/provisioning action, not the Spaces `new` action.
- A future all-machines/unified overview may be added as an explicit pseudo-projection; it is deferred and should not be confused with the default projected-source model.

This is a presentation-layer direction. It does not remove the current ability for API/list surfaces to aggregate local and configured remote agents, and it does not require a protocol bump unless the implementation changes wire/API contracts.

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
agent.submit
agent.rename
agent.focus
agent.wait (composed via events.wait / pane.wait_for_output; not a single API method)
agent.start
pane.list
pane.get
pane.read
pane.split
pane.close (confirmed projected-pane close only)
pane.send_text
pane.send_keys
pane.send_input
events.subscribe for cache/wait state
```

Being in the allowlist means the operation may be routed to the authoritative host; it does not prove higher-level composer semantics. Routed `agent.send` writes text into a composer-style agent but does not submit the prompt; routed `agent.submit` (gated by the `agent_submit` federation capability) writes the text plus an encoded Enter to submit it. The lower-level `pane.send_keys` and `pane.send_input` primitives are listed as routable categories; end-to-end controller-only runtime proof for composer-style submit is the E.0 validation step.

Denied by default over federation:

```text
server.stop
server.live_handoff
server.reload_config
integration.install
integration.uninstall
broad workspace/tab destructive mutation
```

Remote workspace creation is allowed only through explicit capability-gated routes to the owning host, and only when the active source/projection is remote and connected. It must not be confused with adding/provisioning a machine. Broader workspace/tab destructive mutation remains denied by default until confirmations, capabilities, and UX language are settled. Workspace/tab creation or pane placement for agent start may be allowed through explicit remote `agent.start` placement features. The narrow federation-placed agent/lane teardown route (`agent.teardown`) is host/session-qualified, capability-gated, and explicit about destructive semantics, and the confirmed projected-pane `pane.close` route is limited to one projection-resolved pane/process on the authoritative host. Both stay much narrower than broad host/process management — broad pane/workspace/server/process teardown remains out of scope.

`agent.wait` is not a routable API method — the API exposes no such op. The CLI's `agent wait` is composed from the long-held wait primitives (`events.wait` / `pane.wait_for_output`). Over federation it is realized by proxying those primitives to the owning node on a dedicated bridge and running the wait/match logic locally, not by forwarding an `agent.wait` call.

Current spike API shape: aggregated `agent.list` host-qualifies label fields so they can be passed back to `agent.get/read/send`, but it keeps raw remote `terminal_id`, `pane_id`, `workspace_id`, and `tab_id` values. Those raw IDs are authoritative on the owning host but not globally unique. A future schema should add machine-readable host/session fields instead of encoding location into labels.

Non-idempotent retry rule:

- `agent.send`, `agent.submit`, `pane.split`, `pane.send_text`, `pane.send_keys`, and `pane.send_input` are not idempotent.
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

Connection policy gate: `agent.start --host` respects the host's `connection_policy`. A host with `connection_policy = "manual"` (e.g. a sleeping/roaming remote) fails locally before dispatch with a distinct policy error, so it is never woken implicitly. An `on_demand` host with no cached non-connected status may still dispatch on demand; a cached disconnected/unreachable/needs-update status fails fast before forwarding for any host.

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
3. SteamDeck reconnects remote API bridges non-interactively — but only for hosts whose `connection_policy = "auto"`. `on_demand` and `manual` hosts (e.g. sleeping/roaming remotes) are not probed or seeded as sources at startup; they are reached only through explicit commands (`manual` refuses implicit mutating dispatch entirely).
4. SteamDeck resyncs each auto host/session using subscribe/buffer/snapshot/replay.
5. Sidebar remote entries reappear with current state (auto hosts only).
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

Remote host management and diagnostics:

```bash
# Implemented read-only diagnostics for configured hosts:
herdr remote status
herdr remote status jafar
herdr remote check
herdr remote check jafar

# Planned host management commands, not part of the current MVP implementation:
herdr remote add jafar --target jafar --session default
herdr remote list
herdr remote connect jafar
herdr remote reconnect jafar
herdr remote disconnect jafar
herdr remote remove jafar
```

Implemented diagnostic semantics:

- `remote status [HOST]` is a compact read-only status table for all configured hosts or one host alias. It validates remote config before SSH and classifies hosts as connected, not running, needs update, unreachable, or error.
- `remote check [HOST]` is a deeper read-only diagnostic. It separates SSH/binary compatibility, federation capability support, and no-spawn API server status. It does not install/update/restart/spawn remote Herdr.
- Unknown host filters fail before probing. Invalid remote config, including leading-dash SSH targets, fails before SSH.

Planned host management semantics:

- `remote list` lists configured hosts without mutating them.
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
herdr agent send jafar/codex "continue"    # text injection for composer-style agents; does not submit
herdr agent submit jafar/codex "continue"  # text plus Enter; submits composer-style prompts
herdr agent focus jafar/codex
herdr agent wait jafar/codex --status done
herdr agent start --host jafar --name codex --cwd /Users/afayed/projects/mentat -- codex
```

`agent send` is a routed text-write primitive for composer-style agents: readback can show the text in the composer, but the target agent may not process it. `agent submit` is the federation-routed submit primitive (gated by `agent_submit`); full controller-only runtime proof is the E.0 validation step.

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
- Retry with bounded exponential backoff and deterministic jitter.

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
    /// SSH `ConnectTimeout` in whole seconds. Default: 10.
    connect_timeout_secs: u32,
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

Phase G.6 status: remote AGENT CONTROL bridge dispatch for host-qualified `agent.read`/`focus`/`send`/`submit`/`teardown` and `agent.start --host` now leaves the App and headless server request loops *after* the existing in-memory route/cache/policy gates pass. A pure planner (`plan_deferred_remote_agent_request`) performs route planning, cached target resolution, the `agent.teardown` confirm gate, the `agent.start --host` connection-policy guard and cached non-connected precheck, and the remote-mutating connected checks without spawning a thread or touching SSH; only when those gates pass does an owned dispatch descriptor run the SSH bridge on a background worker and send the response through a one-shot channel. Slow or sleeping remote hosts therefore can no longer stall unrelated local UI or headless request handling. The guard ordering and semantics are unchanged: `manual` rejects `agent.start --host` before SSH/API dispatch, `on_demand` is not auto-probed/seeded, cached non-connected statuses fail mutating commands before forwarding, and `agent.get`/`agent.list` stay cache-only.

Phase G.7 status: the G.6 deferred remote-agent request bridge dispatch is now bounded per configured `(host alias, session)`. A small in-process limiter (`RemoteAgentBridgeLimiter`) caps active in-flight bridge dispatches at `REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT = 4` per `(host alias, session)`, acquired in the default dispatch starter after the descriptor is built and before the worker thread is spawned. When the cap is saturated the starter sends an immediate `remote_bridge_busy` JSON API error response (preserving the local request id, naming the host/session and active limit) on the existing `respond_to` channel, spawns no worker, and queues nothing. An acquired permit is moved into the worker closure as an RAII guard, so the slot releases on bridge success, bridge error, rewrite error, and panic unwind. This bounds only the G.6 deferred remote-agent request bridge path; supervisor subscription/heartbeat bridges and terminal attach/render bridges are not affected. The planner stays pure (no limiter state, no spawn, no SSH), `agent.get`/`agent.list` stay cache-only, local/bare agent paths stay synchronous, and the Phase G.5/G.6 guard ordering and fail-fast connection-policy semantics are unchanged: the limiter is consulted only after the manual connection-policy guard, on-demand cached non-connected precheck, teardown confirm, route/cache resolution, and remote-mutating connected checks have already passed.

Still future hardening (outside G.6 and G.7): reusing `remote_supervisor` compatibility/preparation state to avoid per-request remote binary prep/probes; command queueing/retry for mutating commands; and cache-mutating remote tab/workspace/pane layout operations.

Acceptance criteria for current Phase 2 routing:

- `herdr agent read jafar/codex --lines 50` returns remote pane content from the local machine and from agents running in local panes.
- `herdr agent send jafar/codex "continue"` routes to the owning host and the injected text can be observed by subsequent readback, but verified Herdr 0.7.1 / protocol 15 behavior does not submit composer-style prompts.
- `herdr agent wait jafar/codex --status done` composes the remote wait primitives without introducing a separate routable `agent.wait` method.

Future controller-only headless orchestration acceptance must prove `place -> submit prompt -> agent reacts -> controller reads reaction` with no SSH/manual attach.

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

### Spike implementation status

Implemented in this spike:

- remote API bridge and configured remote host/target grammar;
- `RemoteSource` cache, supervisors, and sidebar/list aggregation;
- host-qualified `agent get/read/send/focus`;
- CLI direct remote terminal attach via the existing remote client/render bridge;
- CLI/API remote `agent.start`, defaulting to a new remote workspace when no remote workspace/tab placement is supplied;
- federation capability/method negotiation for the hidden remote API bridge and routed agent methods;
- read-only `herdr remote status [HOST]` and `herdr remote check [HOST]` diagnostics;
- live remote source failure reasons (`disconnected`, `needs update`, `unreachable`) shared between the supervisor, CLI status, and sidebar labels;
- sidebar host-state rows for configured auto-connect remotes when no cached agents exist;
- Docker smoke coverage for the configured one-hop path: get/read/send/focus/start, disconnect, and reconnect;
- real-host Jafar smoke for capability/status commands and isolated `fed-*` host-qualified control after updating the remote binary;
- verified real-host gap (0.7.1/protocol 15): `agent send` writes text into a composer-style remote agent but does not submit the prompt. Phase E.0 adds the submit-capable `agent.submit` route (`herdr agent submit`), capability-gated by `agent_submit`; full controller-only runtime proof is the E.0 validation step. Phase F adds the first narrow federation-placed teardown route (`agent.teardown` / `herdr agent teardown <host>/<target> --confirm`), capability-gated by `agent_teardown`; Phase F.2 adds capability-gated remote `pane.split` and confirmed projected-pane `pane.close` for projection-resolved pane/terminal/workspace selectors. Full controller-only runtime proof is the F validation step.

Still intentionally out of MVP scope:

- embedded remote panes or local-owned remote PTYs;
- broad destructive remote operations (broad pane/workspace/server/process teardown remains out of scope; only the narrow federation-placed agent/lane `agent.teardown` path and the confirmed projection-resolved single-pane `pane.close` carve-out are supported);
- transitive remote-of-remote routing;
- automatic remote update/setup orchestration for configured hosts;
- multi-host production soak/chaos validation beyond the Docker and Jafar smoke coverage.

Direct remote terminal attach is terminal-interactive and is covered by unit tests and review, not by the Docker smoke.

### Phase 5 — Resilience, docs, and polish

Goal: make federation reliable and documented.

Implemented Phase 5 hardening:

- Federation capability/method negotiation gates hidden bridge use and routed agent methods. Version/protocol-compatible binaries that do not advertise federation support now fail clearly as `needs update` instead of later with `unknown command: remote-api-bridge`.
- `herdr remote status [HOST]` gives a compact read-only status view and distinguishes connected, not running, needs update, unreachable, and error cases.
- `herdr remote check [HOST]` gives layered read-only diagnostics for SSH/binary compatibility, federation capabilities, and no-spawn API server status.
- Remote failure classification is shared between CLI status/check and the live `RemoteSource` supervisor, including longer retry backoff for missing/incompatible binaries.
- The sidebar preserves stale remote agents and labels them with the specific failure reason. Configured auto-connect hosts with no cached agents now appear as host-only rows when disconnected, unreachable, or needing update.
- Remote status/check validate configuration before SSH, including rejecting leading-dash SSH targets.
- Jafar real-host smoke covered updated remote capability/status commands, isolated `fed-*` status, and host-qualified list/get/read/send/focus/start routing paths, with `agent send` documented as text injection and `agent submit` providing the composer prompt-submission route added in Phase E.0.

Remaining non-MVP or later polish:

- planned `remote list` and mutating `remote add/connect/reconnect/disconnect/remove` management commands;
- automatic remote update/setup orchestration;
- optional internal SSH ControlMaster optimization;
- broader multi-host and sleep/offline/wake soak testing.
- source/machine projection sidebar presentation; current MVP aggregation remains the shipped behavior until that UI layer lands.

Production validation should go beyond the Docker/localhost SSH smoke. Before wider release, test multiple configured hosts, slow/unreachable SSH, sleep/offline/wake reconnect behavior, and a Tailscale/MagicDNS target if that is a supported deployment path. The Docker smoke proves the controlled one-hop federation path, and the Jafar smoke proves one real SSH-reachable host, but neither is a multi-host soak.

Acceptance criteria for the MVP hardening slice:

- SteamDeck restart re-aggregates configured auto-connect remote agents.
- Jafar restart is handled by Jafar Herdr and then resynced.
- Network drop does not kill remote agents.
- `herdr remote status` clearly explains disconnected/auth/incompatible states.
- The sidebar clearly shows stale remote agents and host-level failure state even when no cached agents exist.

### Next presentation layer — Source/machine projection

Goal: keep the current aggregation/control substrate while changing the sidebar presentation from mixed local/remote sections to one selected source rendered through vanilla Herdr chrome.

Recommended slice order:

1. Restore vanilla local projection so `local` uses the normal `Spaces` and `Agents` panel layout, footer actions, and navigation semantics.
2. Introduce source rail/projected source state for `local`, `jafar`, `work-mini` style machine selection. Switching sources changes only the sidebar read model.
3. Render a selected remote source through the same panel shape using existing `RemoteSourceCache`, `RemoteHostKey`, workspace snapshots, remote agent rows, and workspace-create capability gating.
4. Defer keyboard source cycling, projection persistence fallback, and an all-machines/unified overview until the base projection is stable.

Constraints:

- no local-owned remote PTYs;
- no remote hook relay;
- no transitive routing;
- no protocol bump unless implementation changes a wire/API contract;
- no claim that source projection is current shipped behavior until the UI slice lands.

### Phase 6 — Optional embedded remote panes

Only if later required, and not as a substitute for source/machine projection.

This phase would revisit the earlier remote-backed local pane idea:

```text
local pane -> remote PTY runtime
```

It would require:

- real terminal runtime backend abstraction;
- remote PTY helper;
- a hook authority strategy that still avoids relaying remote hooks to the local node;
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

Implement the source/machine projection presentation layer in narrow UI slices:

1. restore the vanilla local sidebar projection;
2. add source rail/projected source state;
3. render one selected remote source through the same `Spaces` and `Agents` chrome using the existing remote cache and capability gates.

Keep the shipped aggregation/API behavior intact while doing this. Source projection is the forward UI direction, not an already-shipped behavior claim.
