# Herdr roadmap

This is the committed operational roadmap and default agent read-set for Herdr.
It records where the project is and the accepted next-unit queue so an agent
landing cold (or a future lane auto-continue boundary) can prove provenance
without relying on agent-written continuity text.

This file is **not** release/user documentation. For release notes read
`CHANGELOG.md` and the published website docs; for the next-release staging
read `docs/next/`. The detailed federated remote-agent design lives in
`docs/next/remote-agent-control-design.md`, which is the authoritative design
reference this roadmap points at, not replaces.

Agents working on Herdr should also read root `AGENTS.md` and
`.local/continuity.md` before acting (see the global preflight and default
read-set policy).

## Current state

- `origin/master` tip: `ca7c4fc93da9dcdd9edeadb18762c3c2c6b876af`
  (`feat: add dedicated host selection rail`).
- `origin/roadmap/herdr-follow-on-roadmap` tip:
  `ca7c4fc93da9dcdd9edeadb18762c3c2c6b876af` (same commit). The prior
  follow-on roadmap is complete at this tip. The older
  `herdr-follow-on-roadmap` token is **not active for the current manually
  sourced unit**, and this file records no valid roadmap-push/succession token
  for the in-progress branch. Do not reuse or invent a roadmap token from this
  task branch's own diff.
- Lane workflow default: on; lane mode default: non-interactive. No standing
  project trunk/shared auto-land opt-in. Runtime `lane auto-continue on/off`
  state is tracked in `.local/agent-lanes.md` per global/project policy and is
  re-checked at every boundary, but this ROADMAP records **no approved next
  unit**; auto-continue therefore holds at this unit's future closeout unless
  Ahmed supplies a separate accepted queue/token.
- Current manually sourced work unit (in progress on
  `feat/remote-projection-control-surface`): **remote machine projection
  control surface**, sourced from Ahmed's explicit 2026-07-21 manual handoff
  goal and clarification. Selecting a machine is now an authority-routing
  boundary for the same Herdr workspace control surface: `local` shows/controls
  only local state; a connected remote host shows only that host's spaces/
  agents, immediately projects that host's authoritative focused workspace (or
  deterministic first-space fallback), renders live terminal-session frames in
  the projected layout, and routes supported controls/input to the selected
  host in place. Remote hosts remain authoritative for PTYs, panes, hooks,
  focus/layout, child processes, and persistence; stale/disconnected/
  capability-mismatched projections remain remote-scoped read-only; no local
  pane, split, PTY, hook relay, takeover affordance, or persistence record is
  created by projection.

The remote-agent control sequence that lets a local Herdr controller reach an
authoritative remote host's agents/spaces/panes over an SSH-bridged JSON API is
landed and exercised end to end:

- Source projection foundation and status markers (`62e6346`, `de19f89`):
  the source rail and one-source sidebar projection model, plus source rail
  status markers.
- Legacy projected read-only view and direct attach (`541baba`, `8cbf6c3`):
  this shipped the first active-tab geometry projection and explicit direct
  terminal attach escape hatch. It is superseded as the projected primary path
  by the current manually sourced in-place observe/control unit; explicit CLI
  direct attach remains separate.
- Headless prompt submission (`5fce1f7`): explicit `agent.submit` /
  `herdr agent submit` over federation, so a controller can place a remote
  agent, submit a prompt, and read the reaction without manual SSH or attach.
- Teardown and projected pane/tab/workspace editing
  (`6025132`, `067bec2`, `41f39f3`, `8b1611a`, `79bd4a2`, `4d398a2`):
  controller-side federation teardown plus routed remote `pane.split` /
  `pane.close` / tab create-focus-close / rename and inter-pane/inter-tab
  focus, all resolved against the projection cache.
- Resilience G.1-G.10 (`1cda856`, `fdda7e9`, `30f2788`, `b0640b8`,
  `5cad839`, `5d6fc71`, `c65f318`, `e422cbd`, `8e68e33`, `780f892`):
  stale/non-connected mutation fail-fast, bounded configured-host SSH connect
  timeout, bounded transient backoff + deterministic jitter for source probes,
  per-host connection policy (`auto`/`on_demand`/`manual`), remote-agent bridge
  dispatch off the App/headless loops, bounded per-host bridge dispatch
  concurrency, structured `remote.route.*` tracing of configured routed
  agent actions, G.9 supervisor-state reuse that keeps routed agent
  bridge dispatch from redoing per-request remote binary prep/probes, and
  G.10 bounded persistent remote-API bridge pool that reuses bridges across
  requests.
- Dedicated host rail (`ca7c4fc`, `feat: add dedicated host selection rail`):
  the Ahmed-sourced 2026-07-20 correction is complete and landed. It restored
  the narrow left host rail with an explicit `hosts` header, selectable local/
  remote rows, status markers, persistent divider, and selected-host-scoped
  adjacent spaces/agents sidebar. The prior full-width Hosts-section direction
  remains historical/superseded.

Remote hosts remain authoritative for PTYs, panes, hooks, persistence, and
child processes; the local node only aggregates, caches, routes/proxies allowed
commands, and renders the selected source's control surface.

## Completed history (remote roadmap)

Landed resilience/control units from the prior `herdr-remote-roadmap` roadmap
(now landed to `origin/master@5217edc4c55008155fac101b41a9c04544284770`; the
`refs/heads/roadmap/herdr-remote-roadmap` ref was deleted after landing),
newest first:

- **Setup/update orchestration (`remote setup`, explicit configured-host
  setup/update)** — added the explicit live `herdr remote setup <HOST>
  [--handoff]` command. It resolves exactly one configured host and rejects
  disabled federation, unknown hosts, and invalid config before any SSH (these
  are hard errors, exit 1, not the successful no-ops `remote list`/`status`/
  `check` use), then reuses the existing interactive remote preparation pipeline:
  SSH target/session/timeout validation via `RemoteHostConfig`/
  `RemoteHostRegistry`/`RemoteSsh`, remote platform detection, find/install/
  update of a compatible Herdr binary through the existing confirmation prompts,
  `ensure_remote_server_ready` over the existing confirmed-stop/live-handoff
  path, and a final capability-gated federation `ping`. `--handoff` threads the
  existing live-handoff path when the remote server advertises it. It preserves
  the existing non-interactive TTY safety, does not edit local config
  (`remote add`/`remove` remain the config mutation commands), does not stop a
  remote server except via the existing per-run confirmation/live-handoff path,
  and adds no new SSH shell-command shapes, protocol, or JSON API schema. The
  Unix implementation is a thin wrapper over `remote_ssh_for_host`/
  `prepare_remote_herdr`/`ensure_remote_server_ready` and an existing
  capability-gated ping; the Windows stub returns the existing unsupported-
  remote error. Runtime bridge lifecycle commands (`remote connect`/
  `reconnect`/`disconnect`) were not shipped in this slice; they later landed
  separately as the runtime bridge lifecycle commands unit (`355894f`, `feat:
  add remote bridge lifecycle commands` — see below), distinct from `remote
  setup`. No protocol change. No commit SHA is invented here; the closeout
  commit SHA is recorded in the closeout receipt when the work unit lands.

- **Remote management/ops polish — mutating config commands (`remote add`/
  `remote remove`, config-only)** — added the local-config-only `herdr remote
  add <alias> --target <ssh-target>` and confirmed `herdr remote remove <alias>
  --confirm` commands. `remote add` writes a `[[remote.hosts]]` entry and
  enables `[remote]`, resolving `connection_policy`/`connect_timeout_secs`/
  `session` directly from CLI options and validating the combined host registry
  before writing; `remote remove` requires `--confirm`, rejects unknown aliases,
  and leaves the file unchanged on any failure. Both are line-preserving
  local-config mutations: they open no SSH bridge, probe nothing, install/start/
  update nothing, kill no processes, close no panes, and delete no remote state.
  Runtime bridge lifecycle commands (`remote connect`/`reconnect`/`disconnect`)
  and interactive provisioning are explicitly **not** shipped in this slice;
  they later landed separately as the runtime bridge lifecycle commands unit
  (`355894f`, `feat: add remote bridge lifecycle commands` — see below). No
  protocol change. No commit SHA is invented here; the closeout commit SHA is
  recorded in the closeout receipt when the work unit lands.

- **Remote management/ops polish — read-only command/diagnostic surface
  (`remote list`)** — added the no-probe `herdr remote list [HOST]`
  configured-host inventory command. It prints host alias, SSH target, remote
  session, `connection_policy` (via `as_toml_str`), and `connect_timeout_secs`
  from local config only, sharing the `status`/`check` config-validation and
  host-filter path without opening an SSH bridge, probing a remote server, or
  mutating local/remote state; the existing `remote status`/`remote check`
  read-only diagnostics are unchanged. No protocol change, no config write, no
  remote lifecycle change. No commit SHA is invented here; the closeout commit
  SHA is recorded in the closeout receipt when the work unit lands.

- **Capability/protocol negotiation cleanup** — `a885999` (`refactor:
  centralize remote capability gates`): centralizes advertised federation ->
  cached remote-source capability mapping, adds cache-side route-method checks,
  and deduplicates `remote_capability_unavailable`; behavior preserved.
- **Stale projection reconciliation / master repair** — `aa9fcaa` (`fix: refresh
  stale projected pane cache`) plus reconciliation `90fa69f` (`chore: reconcile
  remote roadmap branch`) that lands the master
  commit onto the roadmap branch without touching `master`/shared.
- **Safe command queue/retry policy** — `e5bc124` (`docs: codify remote command
  retry policy`): preserves non-idempotent/uncertain-delivery rules (no
  auto-retry of non-idempotent mutating commands).
- **Scheduler/orchestrator availability policy** — `82dbf66` (`feat: codify
  remote scheduler availability policy`): availability policy for
  sleeping/offline hosts.
- **Phase G.10 bounded bridge pool** — `780f892` (`feat: pool persistent remote
  api bridges`): the formerly conditional connection-reuse unit, now landed as
  a bounded persistent remote-API bridge pool.
- **Phase G.9 supervisor-state reuse** — `8e68e33` (`feat: reuse remote
  supervisor bridge state`): routed agent bridge dispatch reuses
  `remote_supervisor` compatibility/preparation state.

Projection UX / Meta-Herdr R6.2 status: `bb7d717` (`feat: polish projected
remote space UI`) polished the projected remote space UI and `898e6d4` (`feat:
add projected remote command copy affordance`) added a projected remote command
copy affordance. The Meta-Herdr R6.2 projected-UX gap audit is **complete and
landed** at `origin/master@bd37a2151d999b90d4679d8dc84f57faf9de688b` (`bd37a21`,
`docs: audit projected host ux gaps`). The audit's bounded full-width
Hosts-section implementation also landed at `5507f671cf395822526f6ced9de6aae3a5f3ab06`
(`feat: add expanded desktop hosts section`) and was then superseded by Ahmed's
2026-07-20 correction. The corrected dedicated host rail landed at
`ca7c4fc93da9dcdd9edeadb18762c3c2c6b876af` (`feat: add dedicated host selection
rail`) and completed the prior roadmap/queue. The current remote machine
projection control-surface unit is a separate Ahmed 2026-07-21 manual handoff
unit in progress; it is not sourced from that old roadmap token and does not
create an approved successor queue.

- **Meta-Herdr R6.2 bounded Hosts-section implementation (landed, structure
  superseded)** — `5507f671cf395822526f6ced9de6aae3a5f3ab06` (`feat: add
  expanded desktop hosts section`) landed the bounded, audit-sourced
  full-width Hosts-section work. It is historical only now: Ahmed's 2026-07-20
  correction replaced it with the dedicated host rail.
- **Runtime bridge lifecycle commands (landed)** —
  `355894faeea7f88e8ac9889232611e0542eb3d7c` (`feat: add remote bridge
  lifecycle commands`) implemented the already-designed `herdr remote connect
  <HOST>`, `herdr remote reconnect <HOST>`, and `herdr remote disconnect
  <HOST>` surface: `connect` ensures the configured host's local
  aggregation/API bridge is connected now without unnecessarily replacing a
  healthy bridge; `reconnect` explicitly discards local bridge/supervisor
  state for that host and establishes a fresh bridge; `disconnect` stops only
  local aggregation/bridges for that host and exposes the disconnected state.
  Remote authority was preserved: none of these stops the remote Herdr server,
  kills processes, closes remote panes/workspaces, deletes remote state, or
  performs setup/update; `remote setup` remains the separate provisioning/
  setup-update path.
- **Dedicated host rail (landed, roadmap complete)** —
  `ca7c4fc93da9dcdd9edeadb18762c3c2c6b876af` (`feat: add dedicated host
  selection rail`) restored the dedicated left host rail requested by Ahmed's
  2026-07-20 correction and completed the prior follow-on roadmap at both
  `origin/master` and `origin/roadmap/herdr-follow-on-roadmap`.

Existing validation coverage: the controlled one-hop Docker federation smoke
was exercised; the Jafar real-host (localhost-SSH) smoke was exercised at
runtime/manually where supported, but no landed Jafar smoke test/script is
claimed here without a separate committed artifact citing it. A
validation-only local/fake multi-host sleep/offline/wake soak is recorded
below, followed by the committed reusable multi-host soak harness and its
short isolated PASS. No full real-host multi-host sleep/offline/wake soak has
been run yet.

- **Validation-only local/fake multi-host soak — PASS (2026-07-13)**:
  receipt `.local/reviews/multi-host-local-soak-closeout-receipt.md`. Scope
  was validation-only — no source/doc/config implementation change, no commit,
  no push (no commit SHA is invented for this unit). Ahmed-approved path: a
  30-minute local/fake multi-host soak using Docker/localhost-SSH fake remotes
  only, run against the validation binary built from
  `origin/roadmap/herdr-remote-roadmap@e5bc1242ec5b82d2010fab7906ef82c07a9c9581`.
  PASS line: `PASS: multi-host local soak completed elapsed=1840s cycles=47
  hosts=herdr-fed-soak-a herdr-fed-soak-b`. It cycled two fake hosts alternately
  downed and validated bounded offline detection, online-host isolation,
  wake/reconnect, and final host-qualified identity checks. Auto-continue held:
  this unit produced evidence only and there is no source commit to
  roadmap-push; the hold was specifically before any real-host soak or next
  roadmap slice.
- **Committed reusable multi-host soak harness + short isolated PASS
  (2026-07-14)**: this unit added `scripts/soak_remote_api_bridge_multi_host.sh`
  — a reusable Docker/localhost-SSH multi-host soak harness for the federated
  remote-API bridge (multi-host sleep/offline/wake cycling, bounded offline
  detection, online-host isolation, wake/reconnect, host-qualified identity
  checks). It was validated with a short isolated run (no real hosts; temp
  `herdr-fed-soak-*` Docker/localhost-SSH fake remotes only) against the
  validation binary (sha256
  `be2631e6e29a767f4af2b03b20c4df9fb39928cf0abc39bb9c7c393177fb1af9`); PASS
  line: `PASS: multi-host local soak completed elapsed=47s cycles=1
  hosts=herdr-fed-soak-a herdr-fed-soak-b`. Artifact dir:
  `.local/reviews/real-configured-host-soak-short-20260714T205244Z/`. Scope was
  non-destructive/validation-only: **no** real-host install/update or
  destructive remote ops were run, and fake-remote cleanup was confirmed (no
  `herdr-fed-soak-*` containers or images left). No commit SHA is invented for
  this unit's own commit.

## Next queue

No approved next unit is recorded in this ROADMAP.

The only active work described here is the **current in-progress** remote
machine projection control-surface unit, manually sourced from Ahmed's explicit
2026-07-21 handoff/clarification and implemented on
`feat/remote-projection-control-surface`. It is not a queued successor and its
own diff does not authorize a future unit, roadmap token, roadmap-push, trunk
landing, cleanup, or auto-continue succession.

If Ahmed wants more work after this unit, he must provide a separate accepted
source/queue/token. Until then, the correct boundary state is: no approved next
unit; auto-continue held.

## Auto-continue provenance guidance

This section exists so a future lane auto-continue boundary can prove
next-unit provenance instead of trusting continuity prose. It does not by
itself authorize anything; the global auto-continue policy and preconditions
still govern.

- **Prior roadmap complete.** The earlier `herdr-follow-on-roadmap` queue is
  complete at `origin/master@ca7c4fc93da9dcdd9edeadb18762c3c2c6b876af` and
  `origin/roadmap/herdr-follow-on-roadmap@ca7c4fc93da9dcdd9edeadb18762c3c2c6b876af`.
  Its old token is not active for the current manually sourced projection
  control-surface unit.
- **No active roadmap-push authority for this unit.** This ROADMAP records no
  valid active roadmap ref token or successor base for
  `feat/remote-projection-control-surface`. Do not reuse the prior completed
  token, do not invent a token from this task branch's diff, and do not push a
  reviewed result to `refs/heads/roadmap/*` unless Ahmed separately supplies an
  accepted queue/token and every global precondition passes.
- **No approved next unit.** A next unit that exists only in this unit's own
  diff does not count as provenance. With no accepted next-unit source here,
  auto-continue holds at closeout.
- **Standing constraints.** No `master`/shared/trunk mutation without the
  active global/project authorization; no force-push; no release/publish; no
  broad destructive remote ops, PID kill, server stop, real-host setup/update,
  or host management beyond separately approved scope. Local remote projection
  work must preserve remote authority, capability gates, no-takeover, stale/
  disconnected read-only behavior, no transitive routing, no local-owned remote
  PTY, and no projection frame/stream persistence.
