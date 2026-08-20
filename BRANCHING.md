# Development and release branches

Status: accepted direction from Ahmed; activation remains gated
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
4. Build and runtime-test development work through the debug binary in the
   shared `dev` checkout and the isolated `herdr-dev` config/state.

Retained historical worktrees and unlanded refs are not rebased, retargeted,
landed, or deleted merely because the branch model changed.

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

Activation is ordered and fail-closed:

1. validate, stage, approve, and commit this policy/CI/release transition on its
   task branch;
2. after exact approval, create/advance local `dev` to the reviewed commit and
   switch the shared checkout to it without changing any other worktree or ref;
3. after separate exact approval, create `origin/dev` with a fully qualified,
   no-force push and verify it; GitHub default-branch/protection changes remain
   separate external actions;
4. prove fork debug and stable binaries through isolated config/state and an
   actual old-server-to-fork live-handoff smoke before touching the default
   server;
5. install the exact stable fork binary and repair the stable launcher only
   through the approved local-install phase;
6. replace the default server and remove the explicitly installed Omarchy
   package only after rollback/reconnect proof and Ahmed's package/`sudo` gate.
