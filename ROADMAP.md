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

- `origin/master` tip: `36aa263167d117aa9d9cced2369754cdf87b8fe8`
  (`chore: rename home-mini test fixtures`).
- No active roadmap ref/token at this documentation gate. The prior
  `herdr-remote-roadmap` roadmap is complete and landed to
  `origin/master@5217edc4c55008155fac101b41a9c04544284770`; the
  `refs/heads/roadmap/herdr-remote-roadmap` ref was deleted on both the remote
  and the local checkout after landing. No roadmap-push regime is active.
- Lane workflow default: on; lane mode default: non-interactive. No standing
  project trunk/shared auto-land opt-in. No active auto-continue roadmap regime
  is recorded in runtime lane state at this documentation gate. The proposed
  `herdr-follow-on-roadmap` token recorded for the new queue below is **not**
  activated by this ROADMAP-only unit. See `AGENTS.md` "Local lane closeout
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
`bb7d717` polished the projected remote space UI; remaining source-projection
UX gaps are audited and implemented as the Meta-Herdr R6.2 projected-UX gap
audit and bounded UX implementation units of the Next queue below (anchored to
the committed `### Next presentation layer — Source/machine projection`
section and the B.5d affordance, not the legacy B.4a/B.5b/B.5c labels).

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
  `reconnect`/`disconnect`) were not shipped in this slice (they are now
  queued as the runtime bridge lifecycle commands unit, the first of the
  remaining Next queue), distinct from `remote setup`. No protocol
  change. No commit SHA is invented here; the closeout commit SHA is recorded
  in the closeout receipt when the work unit lands.

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
  and interactive provisioning are explicitly **not** shipped in this slice
  (the lifecycle commands are now queued as the runtime bridge lifecycle
  commands unit, the first of the remaining Next queue). No protocol
  change. No commit SHA is invented here; the closeout commit SHA is recorded
  in the closeout receipt when the work unit lands.

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
copy affordance. The remaining Meta-Herdr R6.2 projected-UX gap audit and any
UX work not already satisfied by those commits is **not** marked complete; it
is queued as the Meta-Herdr R6.2 projected-UX gap audit and bounded UX
implementation units of the Next queue below (re-sourced 2026-07-15), not
silently dropped.

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

Ahmed explicitly re-sourced four units (chat, 2026-07-15). The first of those,
the degraded-monoculture review clearance, is already completed and landed:
commit `905eb8aa2900d194da148627c790eb6462169020` (`chore: clear pending
degraded review markers`) re-reviewed and cleared the two remaining
`Review-Status: degraded-monoculture` commits
(`e5bc1242ec5b82d2010fab7906ef82c07a9c9581` and
`22f3256a80f8d98236bd59afca4314dec7b446ce`) on top of the prior `0bbbad8`
clearance that already cleared `8b1611a`, `79bd4a2`, and `bb7d717`. No
degraded markers remain pending, so the remaining committed queue below is the
three pending units in policy-required order, beginning with runtime bridge
lifecycle. A unit is not "started" until a fresh Orchestrator declares it
from an approved packet; any queued unit inherits the "Constraints" in the
auto-continue provenance guidance below unless a future approved unit narrows
or widens them on the record.

1. **Runtime bridge lifecycle commands** — implement the already-designed
   `herdr remote connect <HOST>`, `herdr remote reconnect <HOST>`, and
   `herdr remote disconnect <HOST>` surface:
   - `connect`: ensure the configured host's local aggregation/API bridge is
     connected now; do not replace a healthy bridge unnecessarily.
   - `reconnect`: explicitly discard local bridge/supervisor state for that
     host and establish a fresh bridge.
   - `disconnect`: stop only local aggregation/bridges for that host and expose
     the disconnected state.
   Remote authority is preserved: none of these stops the remote Herdr server,
   kills processes, closes remote panes/workspaces, deletes remote state, or
   performs setup/update. `remote setup` remains the separate provisioning /
   setup/update path. Implement/test in isolation; live lifecycle action on
   Ahmed's real hosts requires separate explicit approval.

2. **Meta-Herdr R6.2 projected-UX gap audit** — audit current source/machine
   projection UX against concrete committed design content, not undefined
   legacy labels:
   - `docs/next/remote-agent-control-design.md` section
     `### Next presentation layer — Source/machine projection` and its ordered
     presentation-layer requirements (vanilla local projection, source rail /
     projected source state, remote source rendered through the same panel
     shape);
   - explicit `#### B.5d — Projected source/space diagnostic and full-remote
     command-copy affordance`, including its two copy-only context-menu
     semantics (`Copy remote diagnostics command`, `Copy full remote command`)
     and its stale/mismatch guard;
   - the document's source-projection invariants, interaction states,
     validation requirements, and separately deferred/undefined items such as
     keyboard source cycling.
   `B.4a`, `B.5b`, and `B.5c` are legacy ROADMAP labels, not normative specs;
   keyboard source cycling is deferred but not concretely specified. If no
   concrete design text maps a legacy label or deferred item to a behavior,
   record it as an Ahmed-needed clarification rather than inventing criteria.
   Account for already-landed improvements `bb7d717` (`feat: polish projected
   remote space UI`) and `898e6d4` (`feat: add projected remote command copy
   affordance`), so shipped behavior is not reimplemented; audit B.5d only for
   evidence-backed remaining gaps against its exact committed semantics. This
   unit is audit-only: produce a concrete gap list, already-satisfied list,
   bounded recommended implementation slices, validation needs, and protected
   decisions. No UX implementation in this unit.

3. **Meta-Herdr R6.2 bounded UX implementation** — implement only concrete
   gaps from the Meta-Herdr R6.2 projected-UX gap audit unit (unit 2) that map
   directly to the committed source/machine-projection design and require no
   new product/architecture decision. Before plan review, verify the audit
   produced a clear bounded scope; if the audit is vague, finds no remaining
   gap, relies only on an undefined legacy label, or raises protected
   UX/architecture choices, hold for Ahmed instead of inventing scope.
   User-visible changes require Ghostty Herdr Dev end-to-end validation and
   screenshot proof.

## Auto-continue provenance guidance

This section exists so a future lane auto-continue boundary can prove
next-unit provenance instead of trusting continuity prose. It does not by
itself authorize anything; the global auto-continue policy and preconditions
still govern.

- **Next-unit source / provenance.** The accepted next-unit source is the
  committed `ROADMAP.md` "Next queue" above. Ahmed explicitly re-sourced four
  units (chat, 2026-07-15); the first of those, the degraded-monoculture review
  clearance, is already completed and landed by
  `905eb8aa2900d194da148627c790eb6462169020`, so the remaining committed queue
  is the three pending units ordered per global policy and beginning with
  runtime bridge lifecycle. A next unit that exists only in the closing unit's
  own diff does not count as provenance; a missing shared landing, a
  missing/invalid runtime token, or an unclear next unit means auto-continue
  holds for Ahmed rather than inventing one.
- **This ROADMAP unit does not activate runtime state.** Recording the
  `herdr-follow-on-roadmap` token here is documentation only. This unit does
  not turn on `lane auto-continue`, create or push
  `refs/heads/roadmap/herdr-follow-on-roadmap`, or land to `master`. The
  reviewed queue commit must first be landed to shared history (`origin/master`)
  by a separate Ahmed approval before any queued unit may run under the roadmap
  regime.
- **Roadmap ref token and succession base.** Proposed committed token for the
  new queue: `herdr-follow-on-roadmap` (validates with
  `git check-ref-format refs/heads/roadmap/herdr-follow-on-roadmap`). Under the
  roadmap-push regime (regime 2) pushes go **only** to
  `refs/heads/roadmap/herdr-follow-on-roadmap` fast-forward/create-only with
  **no force**; `master`/trunk/shared are structurally unreachable by that
  regime. The first roadmap-push unit (runtime bridge lifecycle commands)
  bases on the Ahmed-landed `origin/master` commit that contains this reviewed
  queue, and its reviewed commit creates
  `refs/heads/roadmap/herdr-follow-on-roadmap` create-only / no-force.
  Subsequent queued units branch from
  `origin/roadmap/herdr-follow-on-roadmap` after its verified roadmap-push. If
  the queue commit is not separately landed to shared history, or runtime lane
  state does not record a valid active token at the boundary, auto-continue
  holds.
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
    gates. Runtime bridge lifecycle commands (unit 1) act on local aggregation
    / bridges only; they never stop the remote server, kill processes, close
    remote panes/workspaces, delete remote state, or perform setup/update.
  - R6.2 bounded UX implementation (unit 3) requires Ghostty Herdr Dev
    end-to-end validation and screenshot proof; it is bounded to concrete
    audit gaps mapped to committed design and holds if scope is vague / none /
    undefined / protected.
  - No live real-host install/update or destructive remote op on Ahmed's real
    hosts unless separately explicitly approved.
- **Bounded scope.** Auto-continue stays bounded to the remaining three
  queued units plus the recorded constraints. It never authorizes
  `master`/trunk/shared pushes, force-push, release/publish, or any
  protected/product decision; those remain Ahmed-owned or require their own
  active authorization.
