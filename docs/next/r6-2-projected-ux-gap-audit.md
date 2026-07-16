# Meta-Herdr R6.2 — projected-UX gap audit

Audit artifact (documentation only; no UX implementation in this unit).

- Date: 2026-07-15 (revised: structure resolved by Ahmed; Hosts-section
  visibility on expanded desktop clarified as target — it always shows
  `local`, including with 0 remotes and at ordinary narrow widths, since the
  full-width section removes the extra 10-column rail — and the rail-era
  no-remote/narrow suppression is recorded as current behavior, not target).
- Base: `origin/master@e72124a8422066d2e987ac53c55bf7fd1c5c1a7f` (task branch
  `audit/r6-2-projected-ux-gap`).
- Work unit: `ROADMAP.md` Next queue unit 2 — Meta-Herdr R6.2 projected-UX gap
  audit.

## Scope

Compare the current source/machine-projection sidebar UX at the base SHA
against the committed design, the exact B.5d semantics, already-landed commits
`bb7d717` and `898e6d4`, and Ahmed's resolved Hosts-section direction. Produce
an evidence-backed gap list, already-satisfied list, one bounded Hosts-section
implementation work unit (with ordered internal slices), validation needs, and
protected decisions. **No UX implementation in this unit.**

## Authoritative inputs (normative)

1. `ROADMAP.md` Next queue unit 2 (Meta-Herdr R6.2 projected-UX gap audit) and
   the "Projection UX / Meta-Herdr R6.2 status" note that accounts for
   `bb7d717` and `898e6d4`.
2. `docs/next/remote-agent-control-design.md`:
   - `### Next presentation layer — Source/machine projection` and its ordered
     recommended slices (vanilla local projection; source rail / projected
     source state; selected remote source through the same panel shape; defer
     keyboard cycling / persistence fallback / all-machines overview).
   - `#### B.5d — Projected source/space diagnostic and full-remote
     command-copy affordance` and its exact copy-only/stale-guard semantics.
   - "Core Invariants" — source/machine projection is a UI/read-model
     selection; selecting a source must not change local workspace ownership,
     focused PTY ownership, remote PTY lifecycle, or hook authority.
3. Ahmed's resolved UX direction. Ahmed required local and remote hosts to be
   ergonomically indistinguishable: the sidebar should expose a **`Hosts`
   heading analogous to `Spaces`**, listing `local`, `brain`, and future hosts
   with the same spacing, selection, and interaction treatment as spaces. On
   the concrete interpretation — **replace the compact 10-column rail with a
   proper, full-width Hosts section analogous to Spaces** — Ahmed confirmed
   `ok get to work`. The structural choice is therefore settled
   unconditionally on this point: hosts become a full-width Spaces-like
   section, not a restyled rail.

`B.4a`, `B.5b`, and `B.5c` are legacy ROADMAP labels, not normative specs.
**Global** keyboard source cycling (a dedicated host-switch keybind),
projection-persistence fallback, and an all-machines/unified overview are
explicitly deferred by the design's own slice 4; none has concrete acceptance
text. They are recorded below as deferred, never as gaps. In-section
keyboard/navigation parity for a Spaces-like Hosts section is **not** the same
thing as global cycling and is part of Ahmed's approved parity target — see
G4.

## Non-goals

- Implement any UX change. This unit is audit-only.
- Reimplement `bb7d717` (projected remote space UI polish) or `898e6d4`
  (projected remote command copy affordance).
- Invent global source cycling, projection persistence, or all-machines
  behavior; these stay deferred.
- Define acceptance for legacy labels or deferred items without committed
  design text.
- Touch Rust, config, `ROADMAP.md`, release/website docs, or any tracked file
  other than this artifact.

## Current implementation map

Projection is a read-model switch layered on the vanilla sidebar. Key symbols
and files (cited by stable symbol + path; line numbers are approximate and not
load-bearing):

- **Source model.** `enum SidebarSource { Local, Remote(RemoteHostKey) }`
  (`src/app/state.rs`). State field `sidebar_source`; default `Local`
  (`src/app/mod.rs`).
- **Selection is a read-model switch.** `AppState::select_sidebar_source`
  (`src/app/state.rs::select_sidebar_source`) sets `sidebar_source`, clears
  `selected_remote_space`/`selected_remote_agent`, and resets both panel
  scrolls. It does **not** touch `active`, focused pane, dirty state, or the
  terminal runtime — proven by
  `clicking_source_rail_switches_projection_without_touching_local_focus_or_dirty_state`
  (`src/app/input/sidebar.rs`). The only user-driven caller is mouse left-click
  (`src/app/input/mouse.rs`); the only other caller is the
  `AppEvent::RemoteSourceRemoved` reducer that resets to `Local`
  (`src/app/actions.rs`). There is no keyboard/Navigate path that selects a
  host.
- **Effective source with safe fallback.**
  `AppState::effective_sidebar_source` returns `Local` unless layout is
  `Desktop`, sidebar is expanded, and `source_rail_rect` is non-default. So a
  collapsed sidebar, mobile layout, or too-narrow screen always projects
  `local`.
- **Source rail geometry.** `const SOURCE_RAIL_WIDTH: u16 = 10`
  (`src/ui/sidebar.rs`). The rail is a fixed 10-column strip on the left edge
  of the sidebar, separated from the spaces/agents panel by a `│`. Layout in
  `compute_desktop_view` (`src/ui.rs`) computes `source_rail_rect` and
  `sidebar_panel_rect`.
- **Rail visibility (current behavior, not target).** `source_rail_should_show`
  shows the rail only when the sidebar is expanded, at least one remote host
  status exists, and `screen.width > SOURCE_RAIL_WIDTH + sidebar_min_width`.
  With no configured remote hosts there is no rail and `local` is vanilla; on
  narrow expanded desktop the rail (and thus source selection) is suppressed
  entirely. This no-remote/extra-width suppression is the rail-era behavior the
  full-width Hosts-section work changes, **not** a behavior to preserve: the
  Hosts section must stay visible on expanded desktop even with 0 remotes (it
  still contains `local`) and, being full-width, needs no extra 10-column rail
  width, so ordinary expanded-desktop narrow widths must not hide it merely
  because they cannot fit `SOURCE_RAIL_WIDTH + sidebar_min_width`. Collapsed
  sidebar and mobile keep their existing local-only/specialized fallback.
- **Rail entries/ordering.** `source_rail_entries` pushes `Local` (`label
  "local"`, no status) first, then remote hosts sorted by
  `source_rail_host_status_order` (host name, then default session first, then
  session string). `remote_host_label` renders `host` for the default session
  and `host/session` otherwise — disambiguating multiple sessions of one host.
- **Rail rendering.** `render_source_rail` (`src/ui/sidebar.rs`) draws one
  1-row-tall entry per source, top-aligned from `area.y`. **No header row is
  drawn.** Entries beyond the rail height are dropped — the loop `break`s when
  `y >= area.y + area.height`, and there is no host-list scroll field in
  `AppState` (only `workspace_scroll`, `agent_panel_scroll`, `tab_scroll`).
  Style: selected → `text`/`surface0`/BOLD with full-width background;
  stale (not connected) → `overlay0`/DIM; else `subtext0`. A right-edge status
  marker `● ○ ↑ ×` (`source_rail_status_marker`) encodes
  Connected/Disconnected/NeedsUpdate/Unreachable. Labels are truncated to fit
  `content_width - 1`.
- **Spaces section.** `render_workspace_list` draws a `" spaces"` header
  (lowercase, leading space, `overlay0`/BOLD) unconditionally at `area.y`, then
  workspace cards (potentially multi-row: label row + branch row), grouping /
  collapse arrows, drag-reorder indicator, scrollbar, and a fixed footer. The
  footer is source-specific:
  - `SidebarSource::Local` → `render_local_actions_row` draws `new` + `menu`.
  - `SidebarSource::Remote(host)` → `render_remote_footer_new` draws `new`
    only when the host is connected and advertises `workspace_create`.
- **Agents panel parity.** `render_agent_detail` draws the same `" agents"`
  header and `─` rule for both sources. `projected_agent_panel_entries_with_runtimes`
  selects local vs remote (`remote_agent_panel_entries_for_host`) entries. The
  sort toggle (`grouped`/`priority`) is hidden for remote projection (see
  Intentional differences).
- **Wheel routing.** Over the rail, `ScrollUp`/`ScrollDown` `return None`
  (consumed/no-op) (`src/app/input/mouse.rs`). Over the Spaces panel, the same
  events call `scroll_workspace_list` or `move_selected_workspace_by_visible_delta`
  (scroll when a scrollbar is needed, otherwise move selection).
- **Remote banner.** `render_remote_source_banner` (`src/ui.rs`) draws a dim
  one-line hint in the terminal area when a remote source is selected but no
  space is projected yet, explicitly noting the local pane stays active.
- **B.5d copy affordance.** Right-click on a remote source rail row or a remote
  space row opens a context menu (`ContextMenuKind::RemoteSource` /
  `RemoteSpace`) with exactly two items: `Copy remote diagnostics command` and
  `Copy full remote command` (`state.rs` `ContextMenuKind::items`). Dispatch in
  `src/app/input/modal.rs`:
  - `copy_remote_diagnostics_command` →
    `remote_target::remote_diagnostics_command(&config.name)` =
    `herdr remote status <quoted-alias> && herdr remote check <quoted-alias>`
    (`src/remote_target.rs`).
  - `copy_remote_full_command` →
    `remote_target::remote_full_command(&config.target, &config.session)` =
    `herdr --remote <quoted-target> --session <quoted-session>` (uses SSH
    `target` + configured `session`, never the alias).
  - Both are clipboard-only via `copy_command_to_clipboard` →
    `state.request_clipboard_write`; they never run, SSH, probe, or mutate.
  - Both share the stale/mismatch guard `resolve_remote_command_config`, which
    requires the configured host to exist and `config.session ==
    projected_session`; otherwise a `NeedsAttention`
    "remote command unavailable" toast is shown and nothing is copied.
  - Right-click does **not** switch the projected source — proven by
    `right_click_remote_source_rail_row_opens_remote_source_menu_without_switching_source`
    and `right_click_local_source_rail_row_stays_consumed_without_remote_menu`
    (`src/app/input/sidebar.rs`). Blank/local rail rows stay consumed/no-op
    (`src/app/input/mouse.rs`).

## Already-satisfied matrix (preserved, not over-claimed)

Interaction parity is **partially** satisfied: the read-model selection and the
exact B.5d right-click safety below hold; the evidence-backed interaction gaps
(G2–G4) are recorded in the next section. Do not treat this matrix as "full
interaction parity."

| Requirement | Evidence | Notes |
| --- | --- | --- |
| Vanilla local projection: `local` uses normal `Spaces` + `Agents` panel layout, footer, navigation. | `projected_agent_panel_entries_with_runtimes` / `local_workspace_list_entries_inner` select `Local`; `render_workspace_list` + `render_agent_detail` render the vanilla chrome; rail is absent with no remote hosts (`source_rail_should_show`). | `bb7d717` landed the projected-space polish; `local` path is unchanged vanilla. |
| Primary click selects source via a read-model switch only (no local focus/PTY/hook/dirty mutation). | `select_sidebar_source`; `clicking_source_rail_switches_projection_without_touching_local_focus_or_dirty_state`. | Matches design slice 2. Core invariant preserved. |
| Selected remote source renders through the same panel shape using cached remote workspaces/agents. | `remote_workspace_list_entries_for_host`, `remote_agent_panel_entries_for_host`, `render_remote_space_row`, `remote_host_panel_entry`; same `spaces`/`agents` headers and section geometry. | Capability-gated `new` footer (`host_supports_workspace_create`). Read-only projection by core invariant. |
| Host ordering and multi-session disambiguation. | `source_rail_entries`, `source_rail_host_status_order`, `source_rail_session_rank`, `remote_host_label` (`host` vs `host/session`). | `local` always first; default session sorts first per host. |
| Connection status affordance on host rows. | `source_rail_status_marker` (`●○↑×`), stale DIM styling, `stale_label` suffix on remote space rows. | Four states: Connected/Disconnected/NeedsUpdate/Unreachable. |
| B.5d: two exact copy-only menu items on remote source rail rows and remote space rows. | `ContextMenuKind::items` (`RemoteSource`/`RemoteSpace` → the two labels); `right_click_remote_source_rail_row_opens_remote_source_menu_without_switching_source`; modal.rs copy tests assert both items present and ordered. | `898e6d4`. |
| B.5d: right-click does not switch source; left-click is the read-model switch. | `src/app/input/mouse.rs` right-click path opens menu without `select_sidebar_source`; left-click path calls `select_sidebar_source`. Two sidebar tests prove both. | Matches B.5d "does not change source selection". Right-click-does-not-switch-source is an intentional exact-semantic exception, preserved under the Hosts-section work. |
| B.5d: diagnostics command uses alias; full command uses SSH target + configured session. | `remote_diagnostics_command(&config.name)`; `remote_full_command(&config.target, &config.session)` (`src/remote_target.rs`). Modal tests assert `herdr remote status jafar && herdr remote check jafar` and `herdr --remote user@10.0.0.5 --session default`. | Both alias and target/session are shell-quoted (`shell_quote`). |
| B.5d: stale/mismatch guard prevents misleading copy and shows attention state. | `resolve_remote_command_config` requires host present and `config.session == projected_session`; `show_remote_command_unavailable_toast` (`NeedsAttention`); modal tests cover missing-config and session-mismatch. | Shared guard across both actions. |
| B.5d: clipboard-only, no run/SSH/probe/mutate. | `copy_command_to_clipboard` → `request_clipboard_write`; no command spawn in either action. | Matches "neither action runs a command…". |
| Local/blank rail rows keep consume/no-op right-click. | `right_click_local_source_rail_row_stays_consumed_without_remote_menu`; the right-click branch of `blank_source_rail_right_click_and_wheel_are_consumed`. | Right-click consume stays under the Hosts section. (The wheel part of that test becomes G3 and is updated in Slice (b).) |
| Collapsed-sidebar / mobile safely fall back to `local` (no source section). | `effective_sidebar_source` (Local unless Desktop + expanded + rail rect); `compute_mobile_view` does not allocate a source section. | Preserved target: collapsed/mobile keep local-only/specialized behavior; no new mobile/collapsed design is authorized. The expanded-desktop no-remote and narrow-width rail suppression (`source_rail_should_show`: requires ≥1 remote status and `screen.width > SOURCE_RAIL_WIDTH + sidebar_min_width`) is current rail-era behavior, not a target — it is the gap recorded in G1. |
| Remote-source-with-no-space banner; local pane stays active. | `render_remote_source_banner`; `remote_source_banner_renders_when_source_is_remote_and_no_space_projected`, `remote_source_banner_hides_when_space_is_projected`. | Read-only main area while a space is projected (`compute_desktop_view` suppresses local hit targets/splits). |

## Concrete remaining gaps

| Gap | Severity / impact | Evidence | Requirement |
| --- | --- | --- | --- |
| **G1. Source lives in a compact 10-column rail, not a full-width Hosts section, and the section is hidden at 0 remotes and on narrow expanded desktop.** No `Hosts` heading; rows are single-line rail rows (fixed width 10, right-edge status marker), not the full-width space-like cards Ahmed directed. Additionally the rail-era visibility rule (`source_rail_should_show`) suppresses the section when there are no remote statuses (so no `Hosts`/`local` row renders at all) and when `screen.width <= SOURCE_RAIL_WIDTH + sidebar_min_width` (so source selection is hidden on narrow expanded desktop). (Direction is resolved; this is the work to do, not an open question.) | High (UX parity). The primary visible deviation, plus a `Hosts`/`local` row that is missing entirely at 0 remotes and on narrow expanded desktop. | `render_source_rail` (no header; 1-row, width-10 entries, right-edge marker); `source_rail_should_show` (requires ≥1 remote status AND `screen.width > SOURCE_RAIL_WIDTH + sidebar_min_width`); `render_workspace_list` (`" spaces"`), `render_agent_detail` (`" agents"`); no `"hosts"`/`"Hosts"` UI string in `src/`; `SOURCE_RAIL_WIDTH = 10`; `compute_desktop_view` rail/panel split. | Ahmed's resolved direction: replace the rail with a full-width Hosts section that always shows on expanded desktop — `Hosts` + `local` plus any configured hosts, including with 0 remotes — with space-like spacing/selection/interaction. The full-width section removes the extra 10-column rail, so ordinary expanded-desktop narrow widths must not hide `Hosts` merely because they cannot fit `SOURCE_RAIL_WIDTH + sidebar_min_width`. Collapsed sidebar and mobile keep their existing local-only behavior (out of scope for this gap). |
| **G2. Host list has no scroll state; rows are clipped.** Entries beyond the visible height are dropped, not scrolled. | Medium (parity). Hosts become unreachable once they exceed the rail height. | `render_source_rail` loop `break`s when `y >= area.y + area.height` (top-aligned only); `AppState` has `workspace_scroll`, `agent_panel_scroll`, `tab_scroll` but no host/source-rail scroll field. | Space-like scroll treatment for the Hosts section. |
| **G3. Wheel over the rail is consumed/no-op; Spaces scrolls or moves selection.** | Medium (parity). Hosts do not respond to wheel the way Spaces do. | `src/app/input/mouse.rs`: `ScrollUp`/`ScrollDown` `return None` when `on_source_rail`; the Spaces branch calls `scroll_workspace_list` (when a scrollbar is needed) or `move_selected_workspace_by_visible_delta`. Current test `blank_source_rail_right_click_and_wheel_are_consumed` asserts wheel-consume today. | Space-like wheel behavior (scroll / move host selection) over the Hosts section. |
| **G4. Hosts are absent from keyboard Navigate-mode selection.** Only mouse left-click selects a host; there is no in-section keyboard/navigation parity. | Medium (parity). Keyboard users cannot move host selection the way they move space/agent selection. | `select_sidebar_source` is reached only via mouse left-click (`src/app/input/mouse.rs`) and the `AppEvent::RemoteSourceRemoved` reset-to-`Local` reducer (`src/app/actions.rs`); no keyboard/Navigate caller. `NavigateAction` (`src/app/input/navigate.rs`) has workspace/tab/agent/pane variants but no source/host variant. | Ordinary in-section keyboard/navigation parity for the Hosts section is part of Ahmed's approved parity target (space-like interaction treatment). This is distinct from deferred **global** source cycling (a dedicated host-switch keybind), which is NOT a gap — see Deferred. |

Note on intentional exceptions: B.5d right-click-does-not-switch-source is an
intentional exact-semantic exception (`898e6d4`) and is preserved, not a gap.
Local/blank-row right-click consume/no-op likewise stays.

## Recommended implementation work unit — Hosts section (ordered internal slices)

One bounded Hosts-section implementation work unit, with the structure settled
by Ahmed (full-width Hosts section replacing the rail). Slices are ordered so a
later SubAgent can implement each without making a product decision. Each slice
is user-visible and therefore requires Ghostty Herdr Dev end-to-end validation
and screenshot proof (see validation matrix). Do **not** invent global source
cycling, projection persistence, or all-machines behavior.

### Slice (a) — Full-width Hosts heading/rows replacing the rail (addresses G1)

- Outcome: replace the compact 10-column rail with a full-width `" hosts"`
  section header (mirroring `" spaces"` / `" agents"`: `overlay0`/BOLD, leading
  space) above host rows that use the space-like row geometry, so hosts share
  Spaces' spacing, selection, and interaction footprint.
- Likely files: `src/ui.rs` (`compute_desktop_view` allocation; remove the
  rail/panel split in favor of a Hosts section rect), `src/ui/sidebar.rs`
  (render path + body-rect accounting mirroring
  `WORKSPACE_SECTION_HEADER_ROWS`/`AGENT_PANEL_HEADER_ROWS`; replace
  `render_source_rail`), `src/app/state.rs` (`source_rail_rect` semantics when
  the rail is removed), `src/app/input/{sidebar,mouse}.rs` (hit testing via
  `source_rail_target_at`/`source_rail_row_areas`).
- Acceptance: expanded desktop always shows the `Hosts` heading with a `local`
  row, plus any configured remote host rows; 0 remotes still shows `Hosts` +
  `local` (the section is not absent merely because there are no remote
  statuses). The full-width section removes the extra 10-column rail, so
  ordinary expanded-desktop narrow widths must not hide `Hosts` merely because
  they cannot fit `SOURCE_RAIL_WIDTH + sidebar_min_width` — the section stays
  usable on narrow expanded desktop. The header/section is absent only for
  collapsed sidebar and mobile layout, whose local-only/specialized behavior is
  explicitly unchanged and out of scope (no new mobile/collapsed design is
  authorized). First-row hit target aligned after the header offset; new/
  updated render tests.
- Preserve: `effective_sidebar_source` safe fallback to `Local`; no local
  focus/PTY/hook/dirty mutation; collapsed/mobile local-only behavior.

### Slice (b) — Shared selection/hit-testing/scroll/navigation parity (addresses G2/G3/G4), preserving read-model state and exact B.5d

- Outcome: add host-list scroll state (analogous to `workspace_scroll`) so hosts
  scroll rather than clip; route wheel over the Hosts section to scroll/move
  host selection like Spaces (replacing the current `on_source_rail` →
  `return None` consume); add in-section keyboard selection/navigation parity
  within the Hosts section (mirroring Spaces Navigate behavior).
- Must preserve: `SidebarSource` stays a **read-model-only** selection
  (read-model switch via `select_sidebar_source`; no local ownership mutation),
  and exact B.5d right-click-does-not-switch-source semantics (right-click still
  opens the copy-only menu / local-blank consume).
- Likely files: `src/app/state.rs` (host-list scroll field + selection helpers),
  `src/app/input/{sidebar,mouse,navigate}.rs` (wheel routing, keyboard parity,
  hit testing), `src/ui/sidebar.rs` (scroll-aware render).
- Acceptance: hosts scroll rather than clip; wheel scrolls/moves host selection
  like Spaces; keyboard moves host selection within the section; existing parity
  and B.5d tests stay green; `select_sidebar_source` stays read-model-only;
  B.5d right-click tests unchanged. Update the wheel assertion of
  `blank_source_rail_right_click_and_wheel_are_consumed` (right-click consume
  stays).
- Stop/decision: if space-like geometry needs multi-row host cards with
  metadata remote hosts lack data for (e.g., a branch row), stop — that is new
  product behavior, not parity; surface it as a protected decision.

### Slice (c) — Responsive/fallback regression tests + Ghostty Dev screenshot proof

- Outcome: regression coverage and runtime proof across layouts and host sets.
- Coverage: 0 remotes → `Hosts` + `local` still shown; 1/N remotes shown with
  `local` first; narrow expanded desktop remains usable without the extra
  10-column rail width (`Hosts` not hidden merely because `screen.width` cannot
  fit `SOURCE_RAIL_WIDTH + sidebar_min_width`); collapsed sidebar and mobile
  retain their local-only/specialized behavior (out of scope, no new design);
  multiple sessions per host; `effective_sidebar_source` fallback unchanged for
  collapsed/mobile; B.5d regression green; hit-target alignment after the
  header offset.
- Runtime: source-built dev binary via `ghostty-herdr-dev`; isolated `herdr-dev`
  config/state or temp `XDG_*`; screenshot proof for local/brain/future-host
  transitions, Connected/Disconnected/NeedsUpdate/Unreachable status rendering,
  B.5d right-click on both row kinds, banner/empty/no-space states. Never touch
  Ahmed's main workflow server.

## Validation matrix

| Layer | What to check | How |
| --- | --- | --- |
| Unit — render | `Hosts` header + `local` row + space-like host rows on expanded desktop, including with 0 remotes and at ordinary narrow widths; section absent only for collapsed sidebar / mobile; host-row style per status; no section leakage into collapsed/mobile local-only view; scroll state renders/clamps correctly. | `#[cfg(test)]` render tests in `src/ui/sidebar.rs` / `src/ui.rs` mirroring `remote_source_banner_*` and the existing rail tests. |
| Unit — interaction | Left-click still switches read model only; right-click still opens B.5d menu without switching; local/blank consume; wheel scrolls/moves host selection; hit areas after header offset; in-section keyboard moves host selection. | Extend `src/app/input/sidebar.rs` / `mouse.rs` / `navigate.rs` tests (`clicking_source_rail_switches_projection...`, `right_click_*`, host scroll/keyboard parity). |
| Unit — B.5d regression | Both menu items present and ordered on remote source row and remote space row; diagnostics/full command strings; stale/mismatch guard toast; clipboard-only; right-click does not switch source. | Existing modal.rs tests (`src/app/input/modal.rs`) — keep green; add coverage if Slice changes touch the host-row render/hit path. |
| Integration — layout | Section allocation: expanded desktop shows the Hosts section at 0/1/N hosts and at ordinary narrow widths (not hidden for no-remote or narrow); collapsed sidebar and mobile suppress the section and fall back to `local`; multiple sessions per host; `effective_sidebar_source` fallback intact for collapsed/mobile. | `compute_view_allocates_source_rail_and_panel_rects_for_remote_sources`; retarget `compute_view_suppresses_source_rail_when_width_cannot_fit_minimum_panel` to assert the section stays visible on narrow expanded desktop and is suppressed only for collapsed/mobile. |
| Runtime (later unit) — Ghostty Herdr Dev | local, `brain`, another/future host visible with Hosts heading; selection transitions; status rendering; wheel/keyboard selection; B.5d right-click on both row kinds; banner/empty/no-space states. | Source-built dev binary via `ghostty-herdr-dev`; isolated `herdr-dev` config/state or temp `XDG_*`; screenshot proof through the Omarchy/Ghostty path. Never touch Ahmed's main workflow server. |

This audit unit claims **no** runtime validation. Runtime/screenshot proof is
prescribed for the later bounded Hosts-section implementation unit only.

## Protected / Ahmed-needed decisions

- **None open for the Hosts section itself.** The structural choice is settled
  (full-width Hosts section replacing the rail). If implementation surfaces a
  genuine product decision that this audit does not authorize — for example
  host-section ordering/sort beyond the existing
  `source_rail_host_status_order`, host-card metadata fields remote hosts lack
  data for, or any new keybind beyond in-section navigation parity — stop and
  surface it; do not invent product behavior.

## Deferred / unspecified items (not gaps)

- **Global keyboard source cycling** (a dedicated host-switch keybind) — design
  slice 4 defers it; no concrete acceptance text. **Not a gap.** In-section
  keyboard/navigation parity for the Hosts section is a different, in-scope
  target (see G4 / Slice (b)).
- **Projection-persistence fallback** — design slice 4 defers it. Not a gap.
- **All-machines / unified overview** — design slice 4 defers it. Not a gap.
- **Legacy labels `B.4a`, `B.5b`, `B.5c`** — not normative; no behavior
  invented from them.

## Intentional remote/local differences (not gaps, not protected decisions)

These are authority/capability boundaries or unspecified panel differences, not
UX divergence to "fix":

- Remote `new` footer is gated on connected + `workspace_create` capability;
  the global `menu` button is local-config-only (local actions row).
- Remote projection is read-only (no local-owned remote PTY, no remote hook
  relay) per core invariants; left-click on a live projected remote pane
  dispatches remote focus, not a local split.
- Remote agent rows use attach targets; remote space rows show cached status
  and metadata, never local terminal runtime.
- The agents-panel sort toggle (`grouped`/`priority`) is hidden for remote
  projection. No authoritative requirement (Ahmed direction or committed design)
  demands it; it is an unspecified panel difference outside this unit's scope,
  recorded here for completeness — not a gap and not a protected decision.

## Sourceability conclusion

A bounded Hosts-section implementation unit **is sourceable, unconditionally.**
Ahmed has resolved the structure: replace the compact rail with a full-width
Spaces-like Hosts section. The interaction/safety substrate that must be
**preserved** — read-model-only `SidebarSource` selection, exact B.5d
right-click semantics (right-click does not switch source), stale guards,
parity panels, status affordances, and collapsed/mobile local-only fallback
— is already satisfied by `bb7d717` and `898e6d4` and proven by existing
tests. The expanded-desktop Hosts visibility rule — `Hosts` + `local` always
shown, including at 0 remotes and at ordinary narrow widths (no extra rail
width needed) — is part of the new full-width section work (G1 / Slice (a)),
not a preserved fallback; the rail-era no-remote/narrow suppression is current
behavior that the work changes. The remaining work is the three ordered
internal slices above: (a) full-width Hosts
heading/rows replacing the rail; (b) shared selection/hit-testing/scroll/
navigation parity while preserving read-model state and exact B.5d; (c)
responsive/fallback regression tests and Ghostty Dev screenshot proof. None of
the slices invents global source cycling, projection persistence, or
all-machines behavior; no open structural decision blocks the unit.
