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

- `origin/master` tip: `5507f671cf395822526f6ced9de6aae3a5f3ab06`
  (`feat: add expanded desktop hosts section`).
- Active roadmap ref:
  `origin/roadmap/herdr-follow-on-roadmap@355894faeea7f88e8ac9889232611e0542eb3d7c`
  (`355894f`, `feat: add remote bridge lifecycle commands`). This regime-2 ref
  was created create-only from the Ahmed-landed `origin/master@5507f67` above
  and already carries that one verified roadmap-pushed commit (the runtime
  bridge lifecycle commands unit — see Completed history). The token is
  active and existing, **not** create-only for any future push: the next
  roadmap-push under this regime must fast-forward
  `refs/heads/roadmap/herdr-follow-on-roadmap` from `355894f`, still no
  force, and `master`/trunk/shared remain structurally unreachable by this
  regime. The unrelated prior `herdr-remote-roadmap` roadmap is complete and
  separately landed to
  `origin/master@5217edc4c55008155fac101b41a9c04544284770`; that earlier ref
  was deleted on both the remote and the local checkout after landing.
- Lane workflow default: on; lane mode default: non-interactive. No standing
  project trunk/shared auto-land opt-in. Runtime `lane auto-continue on/off`
  state is tracked in `.local/agent-lanes.md` per global/project policy and is
  re-checked at every boundary; this ROADMAP-only unit does not itself toggle
  it or push anything. See `AGENTS.md` "Local lane closeout policy" and the
  global lane policy.

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
`bb7d717` polished the projected remote space UI; the remaining
source-projection UX gaps were audited by the landed Meta-Herdr R6.2
projected-UX gap audit (`bd37a21`, `docs: audit projected host ux gaps`), whose
artifact `docs/next/r6-2-projected-ux-gap-audit.md` is the historical
authoritative source for the now-landed bounded Hosts-section implementation
(`5507f67`, `feat: add expanded desktop hosts section` — see Completed
history), anchored to the committed `### Next presentation layer —
Source/machine projection` section and the B.5d affordance, not the legacy
B.4a/B.5b/B.5c labels. Ahmed's 2026-07-20 review of that landed full-width
Hosts section reversed its G1 structural direction: the audit's full-width,
Spaces-analogous section is **superseded** as the target shape for the new
sole pending unit below (see Next queue), while the audit's preserved
substrate invariants — read-model-only `SidebarSource`, exact B.5d copy-only
semantics, remote capability gates, and collapsed/mobile fallback — remain
applicable and must not regress.

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
`docs: audit projected host ux gaps`); its artifact
`docs/next/r6-2-projected-ux-gap-audit.md` is the historical authoritative
source for the bounded Hosts-section implementation it recommended, which
also landed — see the bullet immediately below. No UX work beyond
`bb7d717`/`898e6d4`/the landed Hosts-section unit is marked shipped; the only
remaining UX work is the new host-rail correction described in Next queue
below.

- **Meta-Herdr R6.2 bounded Hosts-section implementation (landed, structure
  now superseded)** — `5507f671cf395822526f6ced9de6aae3a5f3ab06` (`feat: add
  expanded desktop hosts section`) landed directly to `origin/master`,
  implementing the bounded, audit-sourced Hosts-section work recorded in
  `docs/next/r6-2-projected-ux-gap-audit.md` (`bd37a21`): **G1** replaced the
  compact 10-column source rail with a full-width `Hosts` section embedded in
  the Spaces/Agents sidebar, analogous to `Spaces` (a `" hosts"` header
  mirroring `" spaces"`/`" agents"`, always shown on expanded desktop
  including with zero configured remote hosts and at ordinary narrow widths,
  absent only for collapsed sidebar/mobile); **G2** added host-list scroll
  state; **G3** routed wheel events over the Hosts section to scroll/move
  host selection like Spaces; **G4** added in-section keyboard/navigation
  parity for host selection. It preserved `SidebarSource` as a
  read-model-only selection, exact B.5d copy-only right-click semantics,
  remote capability gates, and collapsed/mobile fallback, and was validated
  with `just check` plus isolated Ghostty Herdr Dev screenshot proof.
  **Ahmed's 2026-07-20 review of this landed full-width section reversed its
  G1 structural direction**: embedding a full-width `Hosts` section inside
  the Spaces/Agents sidebar is no longer the target shape. The audit
  (`bd37a21`) remains the historical authority for how this landed unit was
  built; its full-width G1 structure is **superseded** for the new sole
  pending unit below, while the preserved substrate invariants named above
  (read-model-only `SidebarSource`, exact B.5d semantics, capability gates,
  collapsed/mobile fallback) remain applicable to that new unit too — see
  Next queue.
- **Runtime bridge lifecycle commands (landed)** —
  `355894faeea7f88e8ac9889232611e0542eb3d7c` (`feat: add remote bridge
  lifecycle commands`) implemented the already-designed `herdr remote connect
  <HOST>`, `herdr remote reconnect <HOST>`, and `herdr remote disconnect
  <HOST>` surface: `connect` ensures the configured host's local
  aggregation/API bridge is connected now without unnecessarily replacing a
  healthy bridge; `reconnect` explicitly discards local bridge/supervisor
  state for that host and establishes a fresh bridge; `disconnect` stops only
  local aggregation/bridges for that host and exposes the disconnected state.
  Remote authority was preserved: none of these stops the remote Herdr
  server, kills processes, closes remote panes/workspaces, deletes remote
  state, or performs setup/update; `remote setup` remains the separate
  provisioning/setup-update path. This reviewed commit **created**
  `refs/heads/roadmap/herdr-follow-on-roadmap` create-only/no-force from the
  Ahmed-landed `origin/master@5507f67` above (the Hosts-section unit) — see
  "Current state" and "Auto-continue provenance guidance" for the active
  roadmap ref this created and how future pushes fast-forward it.

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

Ahmed explicitly re-sourced four units (chat, 2026-07-15): the
degraded-monoculture review clearance, the Meta-Herdr R6.2 projected-UX gap
audit, the bounded Hosts-section implementation, and runtime bridge lifecycle
commands. All four are now complete and landed:

- Degraded-monoculture review clearance: commit
  `905eb8aa2900d194da148627c790eb6462169020` (`chore: clear pending degraded
  review markers`) re-reviewed and cleared the two remaining
  `Review-Status: degraded-monoculture` commits
  (`e5bc1242ec5b82d2010fab7906ef82c07a9c9581` and
  `22f3256a80f8d98236bd59afca4314dec7b446ce`) on top of the prior `0bbbad8`
  clearance that already cleared `8b1611a`, `79bd4a2`, and `bb7d717`. No
  degraded markers remain pending.
- Meta-Herdr R6.2 projected-UX gap audit: complete and landed at
  `origin/master@bd37a2151d999b90d4679d8dc84f57faf9de688b` (`bd37a21`, `docs:
  audit projected host ux gaps`).
- Meta-Herdr R6.2 bounded Hosts-section implementation: landed at
  `origin/master@5507f671cf395822526f6ced9de6aae3a5f3ab06` (`5507f67`, `feat:
  add expanded desktop hosts section`) — see Completed history. Its
  full-width G1 structure is now superseded (below), not pending.
- Runtime bridge lifecycle commands: landed at
  `origin/roadmap/herdr-follow-on-roadmap@355894faeea7f88e8ac9889232611e0542eb3d7c`
  (`355894f`, `feat: add remote bridge lifecycle commands`) — see Completed
  history.

No unit from that 2026-07-15 re-sourcing remains pending. The committed
queue below has exactly **one** sole pending unit, sourced separately from
**Ahmed's 2026-07-20 chat correction/reversal** of the just-landed
full-width Hosts-section direction — not from the 2026-07-15 re-sourcing and
not from the dated audit's G1 recommendation. A unit is not "started" until
a fresh Orchestrator declares it from an approved packet; this queued unit
inherits the "Constraints" in the auto-continue provenance guidance below
unless a future approved unit narrows or widens them on the record.

1. **Host rail — restore a dedicated left host-selection rail (Ahmed's
   2026-07-20 correction)**. Ahmed reviewed the landed full-width Hosts
   section (`5507f67`) and reversed its structural direction: a full-width
   `Hosts` section embedded inside the Spaces/Agents sidebar is **not** the
   target shape. `docs/next/r6-2-projected-ux-gap-audit.md` (`bd37a21`)
   remains the historical authority for how the now-superseded full-width
   section came to be built; it is **not** the source for this unit's
   structure. Provenance for this unit is Ahmed's explicit 2026-07-20
   correction only. Required expanded-desktop layout:
   - a narrow, separate left host rail — distinct from the Spaces/Agents
     sidebar, not full-width;
   - an explicit `hosts` header analogous to the `spaces` and `agents`
     section headers — the rail must **not** be header-less, and the
     first/bare `local` row must not itself read as the header;
   - `local` is a selectable row below that header, followed by configured/
     cached remote rows such as `brain`, each with selection and
     connection-status indicators;
   - a persistent vertical divider separates the host rail from the adjacent
     sidebar;
   - the adjacent sidebar contains `spaces`/`agents` for only the selected
     host;
   - switching host rows scopes/replaces the adjacent sidebar contents;
     local and remote spaces/agents must never be mixed or attributed to the
     wrong host;
   - remove the current full-width `Hosts` section from inside the
     Spaces/Agents sidebar (the `5507f67` structure).
   Preserve unless Ahmed separately expands scope: `SidebarSource` as a
   read-model-only selection; remote-host runtime authority (remote
   projection stays read-only; no local focus/PTY/hook/dirty/workspace-
   ownership mutation from selection); projection/direct-attach capability
   gates (e.g. remote `new` footer gated on connected + `workspace_create`);
   the exact B.5d copy-only context menu (`Copy remote diagnostics command` /
   `Copy full remote command`, right-click never switches source,
   clipboard-only, stale/mismatch guard unchanged); safe stale/disconnected
   status presentation; and existing collapsed/mobile specialized
   (local-only) behavior. If implementation surfaces a genuine product
   decision beyond this recorded contract, stop and surface it; do not
   invent scope.
   Validation (future implementation unit, not this one): focused pure
   layout/input/state tests; `just check`; isolated source-built Ghostty
   Herdr Dev screenshot proof (never Ahmed's main workflow server) covering
   the `hosts` header, host selection/scoping (local and at least one
   configured remote host, e.g. `brain`), connection-status states, the
   persistent vertical divider, and no real-host mutation.
   **This ROADMAP entry is documentation/recording only.** It does not
   authorize or start UI implementation; a fresh Orchestrator must still
   declare the unit from an approved packet before any writable
   implementation begins (see "Auto-continue provenance guidance" below).

## Auto-continue provenance guidance

This section exists so a future lane auto-continue boundary can prove
next-unit provenance instead of trusting continuity prose. It does not by
itself authorize anything; the global auto-continue policy and preconditions
still govern.

- **Next-unit source / provenance.** Ahmed's four units re-sourced
  2026-07-15 (degraded-monoculture review clearance, the Meta-Herdr R6.2
  projected-UX gap audit, the bounded Hosts-section implementation, and
  runtime bridge lifecycle commands) are now all complete and landed — see
  Next queue and Completed history for each landed commit. The sole pending
  unit's source is **not** that 2026-07-15 re-sourcing and **not** the dated
  audit's G1 recommendation: it is Ahmed's separate, explicit **2026-07-20**
  chat correction/reversal of the landed full-width Hosts-section direction,
  recorded in this committed `ROADMAP.md` "Next queue" above. A next unit
  that exists only in the closing unit's own diff does not count as
  provenance; a missing shared/roadmap landing, a missing/invalid runtime
  token, or an unclear next unit means auto-continue holds for Ahmed rather
  than inventing one.
- **This ROADMAP unit does not authorize implementation or mutate refs.**
  Recording the new pending unit here is documentation only: it does not turn
  on `lane auto-continue`, does not itself push anything, and does not start
  UI implementation. Once reviewed, this commit — like the runtime bridge
  lifecycle commands unit before it — is landed by fast-forwarding the
  already-existing `refs/heads/roadmap/herdr-follow-on-roadmap` ref forward
  from `355894f`, not by a create-only push, since the ref already exists.
- **Roadmap ref token and succession base.** Active committed token:
  `herdr-follow-on-roadmap` (validates with `git check-ref-format
  refs/heads/roadmap/herdr-follow-on-roadmap`). Under the roadmap-push regime
  (regime 2) pushes go **only** to
  `refs/heads/roadmap/herdr-follow-on-roadmap`, fast-forward with **no
  force**; `master`/trunk/shared are structurally unreachable by that regime.
  The first roadmap-push unit was runtime bridge lifecycle commands: its
  reviewed commit (`355894f`) **created**
  `refs/heads/roadmap/herdr-follow-on-roadmap` create-only/no-force, based on
  the separately Ahmed-landed `origin/master@5507f67` (which itself carries
  the bounded Hosts-section implementation, landed directly to `master`, not
  via this roadmap regime). The ref now exists and already has one landed
  commit, so **every subsequent queued unit, including this ROADMAP unit and
  the new host-rail correction, fast-forwards
  `refs/heads/roadmap/herdr-follow-on-roadmap` from `355894f`** — none of
  them is create-only. If a queued unit's reviewed commit is not landed this
  way, or runtime lane state does not record a valid active token at the
  boundary, auto-continue holds.
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
    gates. The landed runtime bridge lifecycle commands act on local
    aggregation / bridges only; they never stop the remote server, kill
    processes, close remote panes/workspaces, delete remote state, or
    perform setup/update.
  - The sole pending host-rail unit requires focused pure layout/input/state
    tests, `just check`, and isolated Ghostty Herdr Dev end-to-end
    validation with screenshot proof (never Ahmed's main workflow server);
    it is bounded to the exact contract recorded in Next queue above
    (narrow rail, `hosts` header, selectable local/remote rows with status,
    persistent vertical divider, selected-host-scoped adjacent sidebar, no
    cross-host mixing, and the preserved invariants listed there), and holds
    if scope is vague / none / undefined / protected / expands beyond that
    recorded contract.
  - No live real-host install/update or destructive remote op on Ahmed's real
    hosts unless separately explicitly approved.
- **Bounded scope.** Auto-continue stays bounded to the one remaining queued
  unit (the host-rail correction) plus the recorded constraints. It never
  authorizes `master`/trunk/shared pushes, force-push, release/publish, or
  any protected/product decision; those remain Ahmed-owned or require their
  own active authorization.
