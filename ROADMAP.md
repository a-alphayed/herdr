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

- Active roadmap ref/tip: `origin/roadmap/herdr-remote-roadmap@a8859995f82d0f697f371b42f84a19f15bf9ba9b`
  (`refactor: centralize remote capability gates`).
- `origin/master@aa9fcaacb36febf55d3399f917cc182f9e08ecb7` is unchanged by the
  roadmap-push regime; the roadmap branch contains that master commit via
  reconciliation `90fa69f` (`chore: reconcile remote roadmap branch`). The
  roadmap regime never fast-forwards or pushes `master`/shared refs.
- Lane workflow default: on; lane mode default: non-interactive. No standing
  project trunk/shared auto-land opt-in; `lane auto-continue on` is active for
  the committed `herdr-remote-roadmap` roadmap per recorded runtime state and
  the global auto-continue preconditions. See `AGENTS.md` "Local lane closeout
  policy" and the global lane policy.

The remote-agent control sequence that lets a local Herdr controller reach an
authoritative remote host's agents/spaces/panes over an SSH-bridged JSON API is
landed and exercised end to end:

- Source projection foundation and status markers (`62e6346`, `de19f89`):
  the source rail and one-source sidebar projection model, plus source rail
  status markers.
- Projected read-only view and interactive attach (`541baba`, `8cbf6c3`):
  a selected remote workspace renders a local read-only projection of the
  authoritative remote layout, and projected remote panes can be attached into
  a local interactive view over the remote terminal.
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

Remote hosts remain authoritative for PTYs, panes, hooks, persistence, and
child processes; the local node only aggregates, caches, and routes/proxies.
`bb7d717` polished the projected remote space UI; the still-open source
projection UX parity items (B.4a/B.5b/B.5c/B.5d) are tracked below where not
already satisfied by that polish.

## Completed history (remote roadmap)

Landed resilience/control units on `origin/roadmap/herdr-remote-roadmap`,
newest first:

- **Capability/protocol negotiation cleanup** — `a885999` (`refactor:
  centralize remote capability gates`): centralizes advertised federation ->
  cached remote-source capability mapping, adds cache-side route-method checks,
  and deduplicates `remote_capability_unavailable`; behavior preserved.
- **Stale projection reconciliation / master repair** — `aa9fcaa` (`fix: refresh
  stale projected pane cache`, also `origin/master` tip) plus reconciliation
  `90fa69f` (`chore: reconcile remote roadmap branch`) that lands the master
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
copy affordance. The remaining Meta-Herdr R6.2 projected-UX gap audit and any
B.4a/B.5b/B.5c/B.5d UX work not already satisfied by those commits is **not**
marked complete and is explicitly deferred out of this auto-continue queue (see
"Deferred / out of auto-continue queue" below); it is not silently dropped.

Existing validation coverage: the controlled one-hop Docker federation smoke
was exercised; the Jafar real-host (localhost-SSH) smoke was exercised at
runtime/manually where supported, but no landed Jafar smoke test/script is
claimed here without a separate committed artifact citing it. A
validation-only local/fake multi-host sleep/offline/wake soak is recorded
below; no real-host multi-host sleep/offline/wake soak commit exists yet (see
"Next queue").

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

## Next queue

The queue below is the narrow, sourceable remaining next-unit queue. A unit is
not "started" until a fresh Orchestrator declares it from an approved packet;
the ordering below is the recommended sequence. Each unit inherits the
"Constraints" in the auto-continue provenance guidance below unless a future
approved unit narrows or widens them on the record.

1. **Real configured-host sleep/offline/wake soak and/or committed soak
   harness** — the next step beyond the completed local/fake Docker/localhost-SSH
   multi-host soak recorded above. It may use only configured/approved real
   hosts or an explicitly committed reusable harness/artifact approved in this
   unit; non-destructive diagnostics/control flows only (read-only
   status/check, projected attach, headless submit); **no** live
   install/update, **no** destructive remote actions, **no** PID kill, **no**
   server/host teardown. This is the first remaining unit.
2. **Remote management/ops polish — read-only command/diagnostic surface
   first** — e.g. `remote list` and `remote status`/`remote check` diagnostics
   polish. Read-only in this unit: **no** mutating config and **no** remote
   lifecycle changes here.
3. **Remote management/ops polish — mutating config/bridge lifecycle commands**
   — e.g. `remote add`/`connect`/`reconnect`/`disconnect`/`remove` if approved
   in the unit. Preserve remote authority, local-config/bridge lifecycle
   boundaries, and normal confirmation gates; **no** broad host/process/server
   management.
4. **Setup/update orchestration** — design/implement/test in isolation only.
   Live install/update of Ahmed's real hosts requires a separate explicit
   approval unless the exact action is recorded in a future approved unit; by
   default this unit does not install/update any real host.

## Deferred / out of auto-continue queue

- **Meta-Herdr R6.2 projected-UX gap audit and remaining UX work** — deferred
  out of this auto-continue queue. `bb7d717` and `898e6d4` shipped projection
  UX improvements; the remaining audit and B.4a/B.5b/B.5c/B.5d work is not
  complete and is not part of the queue above. It must be re-sourced by an
  explicit Ahmed prompt or a future approved unit before it re-enters
  auto-continue. Do not treat it as silently deleted.

## Auto-continue provenance guidance

This section exists so a future lane auto-continue boundary can prove
next-unit provenance instead of trusting continuity prose. It does not by
itself authorize anything; the global auto-continue policy and preconditions
still govern.

- **Next-unit source / provenance.** The accepted next-unit source is this
  committed `ROADMAP.md` ("Next queue"). Ahmed explicitly approved (chat,
  2026-07-14) lining up the remaining queue so auto-continue can run through
  completion within the recorded constraints. Because this `ROADMAP.md` update
  is authored in the current unit, the **immediate first succession** is
  authorized by Ahmed's explicit approval recorded here; **after this unit
  lands**, the committed `ROADMAP.md` on `origin/roadmap/herdr-remote-roadmap`
  is the durable base-branch next-unit source for later successions. The first
  remaining unit is **Real configured-host sleep/offline/wake soak and/or
  committed soak harness** (item 1). A next unit that exists only in the
  closing unit's own diff does
  not count as provenance.
- **Roadmap ref token.** Roadmap ref token for the roadmap-push regime
  (regime 2): `herdr-remote-roadmap`. The Orchestrator must validate it
  (`git check-ref-format refs/heads/roadmap/herdr-remote-roadmap`) before any
  use. Under that regime pushes go **only** to
  `refs/heads/roadmap/herdr-remote-roadmap` fast-forward/create-only with
  **no force**; the successor branches off `origin/roadmap/herdr-remote-roadmap`.
- **Constraints (inherited by every queued unit unless re-scoped on the
  record).**
  - No `master`/shared/trunk mutation via the roadmap regime; no force-push.
  - No release/publish.
  - No broad destructive remote ops, no PID kill, no server stop, no host
    management beyond the specific unit scope.
  - Multi-host soak is bounded, configured/approved-host only, and
    non-destructive.
  - Remote management mutating commands preserve remote authority and
    local-config/bridge lifecycle boundaries, and keep normal confirmation
    gates.
  - Setup/update orchestration may be implemented/tested in isolation; live
    install/update of Ahmed's real hosts requires a separate explicit approval
    unless the exact action is recorded in a future approved unit.
- **Bounded scope.** Auto-continue stays bounded to the queue above plus the
  recorded constraints. It never authorizes `master`/trunk/shared pushes,
  force-push, release/publish, or any protected/product decision; those remain
  Ahmed-owned or require their own active authorization. Deferred items (R6.2
  UX) are out of scope until re-sourced.
