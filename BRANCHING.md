# Development and release branches

Status: local development branch/live-dev runtime activated and Omarchy package retired; remote gate remains
Date: 2026-08-20

## Decision

Herdr uses two long-lived branches with different authority:

- `dev` is the development integration branch. Normal task branches start from
  `dev`, pull requests target `dev`, reviewed work lands on `dev`, development
  CI runs on `dev`, and preview builds select only commits reachable from
  `origin/dev`.
- `master` is the production/release branch. Normal feature and fix commits do
  not land directly on it. Reviewed release branches promote an approved `dev`
  state to `master`; stable tags are accepted only when their commit is
  reachable from `origin/master`.

The shared checkout at `/home/amf/Projects/herdr` is the `dev` integration
checkout after activation. Stable production builds are made from an exact
`master` commit, stable tag, or a release worktree -- never from whatever happens
to be built under the shared `dev` checkout.

## Release-channel metadata exception

`master` may receive generated release-channel metadata commits that change only
`website/preview.json`, `website/latest.json`, or the paired reset of
`docs/next/product-announcement.json`. The existing preview and stable release
workflows need those files on the production website branch. This exception does
not permit source, documentation, dependency, workflow, or normal development
changes on `master`.

Preview binaries still come from `dev`. The preview workflow validates that an
explicit or selected preview commit is reachable from `origin/dev`, then writes
only the generated preview manifest to `master`. Because that metadata commit can
advance `master` independently, a release branch must contain both current
`origin/dev` and current `origin/master` before release preparation or
publication. The release recipes enforce both ancestry checks.

## Development flow

1. Start task branches and task worktrees from current `dev`.
2. Validate and review the task branch under the active workflow.
3. Land reviewed task commits on `dev` only through Ahmed's explicit shared-ref
   approval or an effective global authorization. Do not push task work to
   `master`.
4. Build development work through the debug binary in the shared `dev`
   checkout. Run mutating feature/runtime tests only with task-scoped temporary
   config/state and a separate socket/session; the live-dev server is Ahmed's
   real local workflow, not disposable test state.

Retained historical worktrees and unlanded refs are not rebased, retargeted,
landed, or deleted merely because the branch model changed.

## Steam Deck live-dev profile

The Steam Deck's active local server may run the exact debug binary from the
shared `dev` checkout while `master` remains the stable production/release
source. The live-dev command deliberately uses the main live socket and config
so local and federated agents remain one control surface; the debug build keeps
its persistence/log roots under `herdr-dev`. The exact stable `master` release
binary remains installed as the rollback path.

A live-dev cutover is fail-closed:

1. Capture normalized identities for every agent, pane, workspace, and tab,
   plus the current Orchestrator process/session.
2. Prove the stable-to-dev and dev-to-stable live-handoff paths in isolation.
3. Live-handoff to the exact debug binary; never stop the server first and
   never start an empty parallel dev server.
4. Wait boundedly for configured federated hosts to reconnect.
5. Require exact normalized pre/post identity equality and one live copy of the
   current Orchestrator session. Any mismatch rolls back live to the stable
   release binary.
6. Verify an actual Ghostty dev client runs the debug binary and captures
   screenshot proof before treating the launcher path as active.

This profile does not authorize arbitrary live-state testing. Mutating runtime
tests still use isolated task-scoped roots/sockets/sessions unless Ahmed
explicitly approves a live behavior test.

## Stable release flow

1. Ensure the reviewed release scope on `dev` is final.
2. Create `release/<version>` from current `dev` in a dedicated worktree.
3. Bring current `origin/master` release-channel metadata into the release
   branch and verify both `origin/dev` and `origin/master` are ancestors.
4. Run `just release-prepare <version>` on that exact release branch.
5. Review the release commit and run the required validation.
6. Only after Ahmed's exact publication approval, run
   `just release-publish <version>`. It rechecks the branch name, clean state,
   both remote ancestries, docs, version, and tag absence before pushing the
   release branch to `master` and pushing the stable tag.
7. After release automation finishes its generated metadata commit, advance
   `dev` from the released `master` state through a separately authorized,
   verified shared-ref update before accepting more development.

The combined `just release` shortcut is intentionally disabled so release
preparation cannot flow directly into publication without an inspection and
approval boundary.

## Activation gates

This policy diff does not by itself create or push `dev`, change the GitHub
default branch or branch protection, move a shared checkout, publish a release,
change a launcher, install a binary, stop a server, remove a package, or use
`sudo`.

Activation is ordered and fail-closed. Local gates 1, 2, 4, 5, and 6 are
complete with verified proof; remote authority remains separate:

1. validate, stage, approve, and commit this policy/CI/release transition on its
   task branch;
2. after exact approval, create/advance local `dev` to the reviewed commit and
   switch the shared checkout to it without changing any other worktree or ref;
3. after separate exact approval, create `origin/dev` with a fully qualified,
   no-force push and verify it; GitHub default-branch/protection changes remain
   separate external actions;
4. prove fork debug/stable binaries and both live-handoff directions before
   touching the default server;
5. install the exact stable fork rollback binary and repair stable/dev launch
   paths through the approved local-install phase;
6. replace the default server only after exact all-agent equality/reconnect
   proof; remove the inactive Omarchy package only after Ahmed's separate
   package/`sudo` gate. Both actions are complete locally; package removal did
   not stop the dev server or any resumed agent.
