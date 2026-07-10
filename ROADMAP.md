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

- Branch: `master` / `origin/master`.
- Current head: `8e68e33 feat: reuse remote supervisor bridge state` (Phase G.9).
- Lane workflow default: on; lane mode default: non-interactive. No standing
  project trunk/shared auto-land opt-in or standing auto-continue; see
  `AGENTS.md` "Local lane closeout policy" and the global lane policy.

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
- Resilience G.1-G.9 (`1cda856`, `fdda7e9`, `30f2788`, `b0640b8`,
  `5cad839`, `5d6fc71`, `c65f318`, `e422cbd`, `8e68e33`): stale/non-connected
  mutation fail-fast, bounded configured-host SSH connect timeout, bounded
  transient backoff + deterministic jitter for source probes, per-host
  connection policy (`auto`/`on_demand`/`manual`), remote-agent bridge
  dispatch off the App/headless loops, bounded per-host bridge dispatch
  concurrency, structured `remote.route.*` tracing of configured routed
  agent actions, and G.9 supervisor-state reuse that keeps routed agent
  bridge dispatch from redoing per-request remote binary prep/probes.

Remote hosts remain authoritative for PTYs, panes, hooks, persistence, and
child processes; the local node only aggregates, caches, and routes/proxies.
`bb7d717` polished the projected remote space UI; the still-open source
projection UX parity items (B.4a/B.5b/B.5c/B.5d) are tracked below where not
already satisfied by that polish.

## Next queue

The queue below is the narrow, sourceable next-unit queue. A unit is not
"started" until a fresh Orchestrator declares it from an approved packet; the
ordering below is the recommended sequence. Phase G.9 (item 1) is complete
and Phase G.10 (item 2) is the conditional next unit.

1. **Phase G.9 — supervisor-state reuse** — reuse `remote_supervisor`
   compatibility/preparation state for routed agent bridge dispatch so routed
   actions do not redo per-request remote binary prep/probes. **Complete** at
   `8e68e33` (`feat: reuse remote supervisor bridge state`).
2. **Phase G.10 — optional connection reuse / bounded bridge pool** — held
   conditionally: do not start unless evidence shows remaining per-request
   SSH/process startup still dominates after G.9 supervisor-state reuse, and a
   bounded-pool design is settled/approved.
3. **Stale projection reconciliation** — reconcile/refresh stale cached
   projections against authoritative remote state.
4. **Capability/protocol negotiation cleanup** — clean up capability/protocol
   negotiation for the projection/control routes.
5. **Scheduler/orchestrator availability policy** — availability policy for
   sleeping/offline hosts in the scheduler/orchestrator.
6. **Safe command queue/retry design** — only if non-idempotent
   uncertain-delivery rules remain preserved (no auto-retry of non-idempotent
   mutating commands).
7. **Multi-host sleep/offline/wake soak testing** — resilience soak across
   multiple configured remote hosts.
8. **Meta-Herdr R6.2 projected-UX gap audit and remaining UX work (single
   R6.2 implementation queue)** — when this item is explicitly started, begin
   with a bounded Meta-Herdr R6.2 projected-UX gap audit that compares the
   current implemented/projected source/machine UX against the authoritative
   design (`docs/next/remote-agent-control-design.md`), then use the audit to
   scope the remaining B.4a/B.5b/B.5c/B.5d UX work not already satisfied by
   later commits (`bb7d717`). This item is the single R6.2 implementation
   queue; there is no separate/parallel Meta-Herdr R6.2 implementation
   stream.
9. **Remote management/ops polish** — `remote list`, mutating remote
   management commands, and automatic setup/update orchestration if approved.

## Auto-continue provenance guidance

This section exists so a future lane auto-continue boundary can prove
next-unit provenance instead of trusting continuity prose. It does not by
itself authorize anything; the global auto-continue policy and preconditions
still govern.

- **Next-unit source.** The accepted next-unit source is this committed
  `ROADMAP.md` ("Next queue"), not agent-written continuity text. Phase G.9
  (item 1) is complete at `8e68e33`; the conditional next unit is **Phase G.10
  — optional connection reuse / bounded bridge pool** (item 2), held until
  evidence shows remaining per-request SSH/process startup still dominates
  after G.9 supervisor-state reuse, and a bounded-pool design is settled/
  approved. A next unit that exists only in the closing unit's own diff does
  not count as provenance.
- **Roadmap ref token.** Recommended roadmap ref token for the roadmap-push
  regime (regime 2): `herdr-remote-roadmap`. The Orchestrator must validate it
  (`git check-ref-format refs/heads/roadmap/herdr-remote-roadmap`) before any
  use; under that regime pushes go only to `refs/heads/roadmap/<name>`
  fast-forward/create-only with no force, and the successor branches off
  `origin/roadmap/<name>`.
- **Bounded scope.** Auto-continue stays bounded to the queue above. It never
  authorizes `master`/trunk/shared pushes, force-push, release/publish, or any
  protected/product decision; those remain Ahmed-owned or require their own
  active authorization.
