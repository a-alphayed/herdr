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

- Local `master` and `origin/master` are clean and equal at
  `b3694186d51a7a26d689d5e49ae5973c3e76c24a` (`feat: copy projected
  selections to local clipboard`). Clean local `dev` is active at approved
  policy commit `7e811279731e68851b8130fa8cd693e911798bc3`; origin still has no
  `dev` branch.
- Ahmed selected the durable branch model: `dev` is development integration
  and preview source; `master` is production/release-only, with a narrow
  generated release-channel metadata exception. The decision, live-dev
  profile, and remaining activation gates are in `BRANCHING.md`.
- The current Ahmed-sourced unit has completed local branch activation,
  stable/dev launcher repair, live dev-server replacement, and Omarchy-package
  retirement. Remote `dev` creation/push, GitHub settings, and the follow-up
  policy amendment commit remain distinct gates.
- Nine pre-existing worktrees were inventoried clean and must be preserved.
  Three local-only/unlanded tips remain intentionally untouched:
  `fix/remote-claude-selection@29d02c9a`,
  `feat/hide-projection-role-labels@c026e674`, and
  `feat/r6-2-hosts-section@e983eb61`.
- The prior `herdr-follow-on-roadmap` is complete. Its remote roadmap ref is
  historical and not an active token. This ROADMAP records no approved
  successor after the current manually sourced transition; do not reuse or
  invent a roadmap token.
- The global lane-workflow default remains on. Ahmed set lane workflow off,
  non-interactive mode, and auto-continue off for this unit; that runtime state
  is recorded in `.local/agent-lanes.md`, and this committed history does not
  set a future session's controls. There is no standing trunk/shared auto-land,
  remote-branch creation, live-install, package, or cleanup authorization.
- Steam Deck now runs the exact source debug binary from local `dev` as the
  live server/client: SHA-256 `e83aa23a...`, version 0.7.1, protocol 15. The
  live-dev profile shares the main socket/config and keeps debug state/logs
  under `herdr-dev`. Immediate pre/post normalized identity hash
  `caee36f3...` matches exactly for 15 agents, 14 panes, 10 workspaces, and 10
  tabs after all five federated agents reconnected; the active Orchestrator
  session is present once. Actual Ghostty dev-client and screenshot proof pass.
- `$HOME/.local/bin/herdr` is the exact stable `master@b3694186` rollback
  binary (`a008d799...`). The explicitly installed Omarchy package
  `herdr 0.8.0.r13-1` was removed through Ahmed's visible sudo gate only after
  the live-dev equality/launcher/rollback proofs passed. `/usr/bin/herdr` is
  absent; the dev server and all agents remained live through removal.

The remote-agent control sequence through `b3694186` remains landed and
exercised end to end. Remote hosts retain authority for PTYs, panes, hooks,
persistence, and child processes; the local node only aggregates, caches,
routes/proxies allowed commands, and renders the selected source's control
surface.

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

The only active work described here is the **current in-progress** Steam Deck
production-`master`/development-`dev` transition, manually sourced from Ahmed's
direct instruction and implemented first on `chore/dev-integration-policy`.
It is not a queued successor and its own diff does not authorize a future unit,
roadmap token, roadmap-push, shared/remote branch mutation, cleanup, or
auto-continue succession.

If Ahmed wants more work after this unit, he must provide a separate accepted
source/queue/token. Until then, the correct boundary state is: no approved next
unit; auto-continue held.

## Auto-continue provenance guidance

This section exists so a future lane auto-continue boundary can prove
next-unit provenance instead of trusting continuity prose. It does not by
itself authorize anything; the global auto-continue policy and preconditions
still govern.

- **Prior roadmap complete.** The earlier `herdr-follow-on-roadmap` queue is
  complete; its retained remote roadmap ref points to historical
  `ca7c4fc93da9dcdd9edeadb18762c3c2c6b876af`, behind current
  `origin/master@b3694186d51a7a26d689d5e49ae5973c3e76c24a`. Its old token is
  not active for the current manually sourced branch-policy transition.
- **No active roadmap-push authority for this unit.** This ROADMAP records no
  valid active roadmap ref token or successor base for
  `chore/dev-integration-policy`. Do not reuse the prior completed token, do
  not invent a token from this task branch's diff, and do not push a reviewed
  result to `refs/heads/roadmap/*` unless Ahmed separately supplies an accepted
  queue/token and every global precondition passes.
- **No approved next unit.** A next unit that exists only in this unit's own
  diff does not count as provenance. With no accepted next-unit source here,
  auto-continue holds at closeout.
- **Standing constraints.** No `dev`, `master`, or other shared/origin ref
  mutation without the active global/project authorization; no force-push; no
  release/publish; no launcher/install/default-server/package/sudo action before
  its exact gate; no broad destructive remote ops, PID kill, real-host setup/
  update, or host management beyond separately approved scope. Remote
  projection work continues to preserve remote authority, capability gates,
  no-takeover, stale/disconnected read-only behavior, no transitive routing,
  no local-owned remote PTY, and no projection frame/stream persistence.
