use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::scrollbar::{render_scrollbar, should_show_scrollbar};
use super::status::{agent_icon, state_dot, state_label, state_label_color};
use super::text::{display_width, display_width_u16, truncate_end};
use crate::app::state::{AgentPanelSort, Palette, SidebarSource};
use crate::app::{AppState, Mode};
use crate::detect::AgentState;
use crate::terminal::TerminalRuntimeRegistry;

const WORKSPACE_SECTION_HEADER_ROWS: u16 = 2;
const AGENT_PANEL_HEADER_ROWS: u16 = 3;
const HOST_RAIL_HEADER_ROWS: u16 = 2;
const HOST_ROW_LEADING_GUTTER: u16 = 1;
/// Fixed width of the dedicated host-selection rail beside the Spaces/Agents
/// panel, matching the established pre-existing rail pattern
/// (`SOURCE_RAIL_WIDTH`). The rail is always full sidebar height on expanded
/// desktop; it is never sized to the current host count.
const HOST_RAIL_WIDTH: u16 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentPanelEntryLocation {
    Local {
        ws_idx: usize,
        tab_idx: usize,
        pane_id: crate::layout::PaneId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentPanelEntry {
    pub location: AgentPanelEntryLocation,
    pub primary_label: String,
    pub primary_tab_label: Option<String>,
    pub agent_label: Option<String>,
    pub state: AgentState,
    pub seen: bool,
    pub last_agent_state_change_seq: Option<u64>,
    pub custom_status: Option<String>,
    pub state_labels: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostListEntry {
    pub(crate) source: SidebarSource,
    pub(crate) label: String,
    pub(crate) status: Option<crate::remote_source::RemoteConnectionStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostRowArea {
    pub(crate) source: SidebarSource,
    pub(crate) rect: Rect,
}

impl AgentPanelEntry {
    pub(crate) fn local_target(&self) -> Option<(usize, usize, crate::layout::PaneId)> {
        match self.location {
            AgentPanelEntryLocation::Local {
                ws_idx,
                tab_idx,
                pane_id,
            } => Some((ws_idx, tab_idx, pane_id)),
        }
    }
}

pub(crate) fn host_list_entries(app: &AppState) -> Vec<HostListEntry> {
    // `local` is always first. Configured host keys (every `connection_policy`,
    // including `manual`/`on_demand`) are merged with cached statuses only here,
    // so a display-only configured host never becomes a synthetic
    // `RemoteSourceCache` entry.
    let mut entries = vec![HostListEntry {
        source: SidebarSource::Local,
        label: "local".to_string(),
        status: None,
    }];

    // Union of cached host keys and configured display-only host keys.
    let mut host_keys: std::collections::BTreeSet<crate::remote_source::RemoteHostKey> = app
        .remote_sources
        .list_host_statuses()
        .into_iter()
        .map(|entry| entry.host)
        .collect();
    host_keys.extend(app.configured_remote_hosts.iter().cloned());

    let mut remote_entries: Vec<HostListEntry> = host_keys
        .into_iter()
        .map(|host| {
            let status = app.remote_sources.host_status(&host);
            HostListEntry {
                label: remote_host_label(&host),
                source: SidebarSource::Remote(host),
                status,
            }
        })
        .collect();
    remote_entries.sort_by(host_list_order);
    entries.extend(remote_entries);
    entries
}

fn host_list_order(left: &HostListEntry, right: &HostListEntry) -> std::cmp::Ordering {
    let left_key = match &left.source {
        SidebarSource::Local => return std::cmp::Ordering::Less,
        SidebarSource::Remote(host) => host,
    };
    let right_key = match &right.source {
        SidebarSource::Local => return std::cmp::Ordering::Greater,
        SidebarSource::Remote(host) => host,
    };
    left_key
        .host
        .cmp(&right_key.host)
        .then_with(|| {
            host_session_rank(&left_key.session).cmp(&host_session_rank(&right_key.session))
        })
        .then_with(|| left_key.session.cmp(&right_key.session))
}

fn host_session_rank(session: &str) -> u8 {
    if session == crate::session::DEFAULT_SESSION_NAME {
        0
    } else {
        1
    }
}

fn host_status_marker(
    status: crate::remote_source::RemoteConnectionStatus,
    p: &Palette,
) -> (&'static str, Style) {
    match status {
        crate::remote_source::RemoteConnectionStatus::Connected => {
            ("●", Style::default().fg(p.green))
        }
        crate::remote_source::RemoteConnectionStatus::Disconnected => (
            "○",
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        ),
        crate::remote_source::RemoteConnectionStatus::NeedsUpdate => (
            "↑",
            Style::default().fg(p.yellow).add_modifier(Modifier::BOLD),
        ),
        crate::remote_source::RemoteConnectionStatus::Unreachable => {
            ("×", Style::default().fg(p.red).add_modifier(Modifier::BOLD))
        }
    }
}

/// Fixed width of the dedicated host-selection rail.
pub(crate) fn host_rail_width() -> u16 {
    HOST_RAIL_WIDTH
}

/// Content rect for the host rail: rail width minus the right-edge column
/// reserved for the rail's own internal divider (drawn by `render_host_rail`,
/// separate from the outer sidebar/main-area divider drawn by
/// `render_sidebar`).
pub(crate) fn host_rail_content_rect(area: Rect) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::default();
    }
    Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height)
}

/// Body rect below the ` hosts` header and its breathing row, narrowed by one
/// column when a scrollbar is needed. The rail is full sidebar height, so the
/// body spans the remaining rows below the two-row section header area.
pub(crate) fn host_list_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    let content = host_rail_content_rect(area);
    if content.width == 0 || content.height <= HOST_RAIL_HEADER_ROWS {
        return Rect::default();
    }
    let body_y = content.y.saturating_add(HOST_RAIL_HEADER_ROWS);
    let body_height = (content.y + content.height).saturating_sub(body_y);
    let body_width = content.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(content.x, body_y, body_width, body_height)
}

fn host_list_visible_count(app: &AppState, area: Rect, scroll: usize) -> usize {
    let body = host_list_body_rect(area, false);
    if body.width == 0 || body.height == 0 {
        return 0;
    }
    let total = host_list_entries(app).len();
    let remaining = total.saturating_sub(scroll);
    remaining.min(body.height as usize)
}

pub(crate) fn host_list_scroll_metrics(app: &AppState, area: Rect) -> crate::pane::ScrollMetrics {
    let entries = host_list_entries(app);
    let total_rows = entries.len();
    let scroll = app.host_list_scroll.min(total_rows.saturating_sub(1));
    let viewport_rows = host_list_visible_count(app, area, scroll);
    let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
    let offset_from_bottom = total_rows
        .saturating_sub(scroll)
        .saturating_sub(viewport_rows);
    crate::pane::ScrollMetrics {
        offset_from_bottom,
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn host_list_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = host_list_scroll_metrics(app, area);
    let body = host_list_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(2),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn normalized_host_list_scroll(app: &AppState, requested: usize) -> usize {
    let area = app.view.host_rail_rect;
    if area == Rect::default() {
        return 0;
    }
    let entry_count = host_list_entries(app).len();
    if entry_count == 0 {
        return 0;
    }
    // Clamp to the viewport's true maximum first-visible offset
    // (`entry_count - viewport_capacity`), which is 0 when every entry fits.
    // This prevents a stale nonzero scroll from skipping `local`/leading hosts
    // after the list shrinks until it fits (e.g. hosts disconnecting). Clamping
    // to `entry_count - 1` instead would leave a dangling offset that hides
    // leading rows. `viewport_capacity` is the body height (scrollbar presence
    // only narrows the body width, not its height), and every mutation path
    // normalizes before metrics are read, so scrollbar/drag/wheel math stays
    // consistent.
    let viewport_capacity = host_list_body_rect(area, false).height as usize;
    let max_first_visible = entry_count.saturating_sub(viewport_capacity);
    requested.min(max_first_visible)
}

/// Scroll-aware host row areas, accounting for the header offset and the
/// current `host_list_scroll`. Off-viewport rows are unreachable here.
pub(crate) fn host_list_row_areas(app: &AppState) -> Vec<HostRowArea> {
    let area = app.view.host_rail_rect;
    if area == Rect::default() {
        return Vec::new();
    }
    let metrics = host_list_scroll_metrics(app, area);
    let body = host_list_body_rect(area, should_show_scrollbar(metrics));
    if body.width == 0 || body.height == 0 {
        return Vec::new();
    }
    let entries = host_list_entries(app);
    let mut rows = Vec::new();
    let mut y = body.y;
    let body_bottom = body.y + body.height;
    for entry in entries.into_iter().skip(app.host_list_scroll) {
        if y >= body_bottom {
            break;
        }
        rows.push(HostRowArea {
            source: entry.source,
            rect: Rect::new(body.x, y, body.width, 1),
        });
        y = y.saturating_add(1);
    }
    rows
}

pub(crate) fn host_target_at(app: &AppState, col: u16, row: u16) -> Option<SidebarSource> {
    if app.view.host_rail_rect == Rect::default() {
        return None;
    }
    host_list_row_areas(app).into_iter().find_map(|entry| {
        (col >= entry.rect.x
            && col < entry.rect.x + entry.rect.width
            && row >= entry.rect.y
            && row < entry.rect.y + entry.rect.height)
            .then_some(entry.source)
    })
}

fn sidebar_section_heights(total_h: u16, split_ratio: f32) -> (u16, u16) {
    if total_h == 0 {
        return (0, 0);
    }

    if total_h < 6 {
        let ws_h = total_h.div_ceil(2);
        return (ws_h, total_h.saturating_sub(ws_h));
    }

    let ratio = split_ratio.clamp(0.1, 0.9);
    let ws_h = ((total_h as f32) * ratio).round() as u16;
    let ws_h = ws_h.clamp(3, total_h.saturating_sub(3));
    let detail_h = total_h.saturating_sub(ws_h);
    (ws_h, detail_h)
}

pub(crate) fn expanded_sidebar_sections(area: Rect, split_ratio: f32) -> (Rect, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), Rect::default());
    }

    let (ws_h, detail_h) = sidebar_section_heights(content.height, split_ratio);
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h);
    let detail_area = Rect::new(content.x, content.y + ws_h, content.width, detail_h);
    (ws_area, detail_area)
}

pub(crate) fn sidebar_section_divider_rect(area: Rect, split_ratio: f32) -> Rect {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height < 6 {
        return Rect::default();
    }

    let (ws_h, _) = sidebar_section_heights(content.height, split_ratio);
    Rect::new(content.x, content.y + ws_h, content.width, 1)
}

fn agent_panel_sort_label(sort: AgentPanelSort) -> &'static str {
    match sort {
        AgentPanelSort::Spaces => "grouped",
        AgentPanelSort::Priority => "priority",
    }
}

pub(crate) fn agent_panel_toggle_rect(area: Rect, sort: AgentPanelSort) -> Rect {
    if area.width == 0 || area.height < 2 {
        return Rect::default();
    }

    let label = agent_panel_sort_label(sort);
    let width = display_width_u16(label);
    Rect::new(
        area.x + area.width.saturating_sub(width),
        area.y + 1,
        width,
        1,
    )
}

pub(crate) fn agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, None)
}

pub(crate) fn agent_panel_entries_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, Some(terminal_runtimes))
}

fn agent_panel_entries_with_runtimes(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> Vec<AgentPanelEntry> {
    let empty_runtimes;
    let terminal_runtimes = match terminal_runtimes {
        Some(terminal_runtimes) => terminal_runtimes,
        None => {
            empty_runtimes = TerminalRuntimeRegistry::new();
            &empty_runtimes
        }
    };

    let mut entries = local_agent_panel_entries_with_runtimes(app, terminal_runtimes);
    sort_agent_panel_entries(app, &mut entries);
    entries
}

fn local_agent_panel_entries_with_runtimes(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<AgentPanelEntry> {
    app.workspaces
        .iter()
        .enumerate()
        .flat_map(|(ws_idx, ws)| {
            let multi_tab = ws.tabs.len() > 1;
            let workspace_label = ws.display_name_from(&app.terminals, terminal_runtimes);
            ws.pane_details(&app.terminals)
                .into_iter()
                .map(move |detail| AgentPanelEntry {
                    location: AgentPanelEntryLocation::Local {
                        ws_idx,
                        tab_idx: detail.tab_idx,
                        pane_id: detail.pane_id,
                    },
                    primary_label: workspace_label.clone(),
                    primary_tab_label: multi_tab.then_some(detail.tab_label),
                    agent_label: Some(detail.agent_label),
                    state: detail.state,
                    seen: detail.seen,
                    last_agent_state_change_seq: detail.last_agent_state_change_seq,
                    custom_status: detail.custom_status,
                    state_labels: detail.state_labels,
                })
        })
        .collect()
}

fn sort_agent_panel_entries(app: &AppState, entries: &mut [AgentPanelEntry]) {
    if matches!(app.agent_panel_sort, AgentPanelSort::Priority) {
        entries.sort_by_key(|entry| {
            (
                std::cmp::Reverse(workspace_attention_priority(entry.state, entry.seen)),
                std::cmp::Reverse(entry.last_agent_state_change_seq),
            )
        });
    }
}

fn remote_host_label(host: &crate::remote_source::RemoteHostKey) -> String {
    if host.session == crate::session::DEFAULT_SESSION_NAME {
        host.host.clone()
    } else {
        format!("{}/{}", host.host, host.session)
    }
}

pub(super) fn agent_panel_status_key(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Working, _) => "working",
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Unknown, _) => "unknown",
    }
}

fn truncate_text(text: &str, max_width: usize) -> String {
    let len = text.chars().count();
    if len <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    let prefix: String = text.chars().take(max_width.saturating_sub(1)).collect();
    format!("{prefix}…")
}

pub(crate) fn agent_panel_entry_content_height(entry: &AgentPanelEntry) -> u16 {
    let _ = entry;
    2
}

pub(crate) fn agent_panel_entry_gap_after(entry: &AgentPanelEntry) -> u16 {
    let _ = entry;
    1
}

fn format_agent_panel_primary_label_content(entry: &AgentPanelEntry, max_width: usize) -> String {
    let Some(tab_label) = entry.primary_tab_label.as_deref() else {
        return truncate_end(&entry.primary_label, max_width);
    };

    let separator = " · ";
    let separator_width = display_width(separator);
    if max_width <= separator_width + 2 {
        return truncate_end(
            &format!("{}{}{}", entry.primary_label, separator, tab_label),
            max_width,
        );
    }

    let available = max_width.saturating_sub(separator_width);
    let min_tab = 4.min(available.saturating_sub(1)).max(1);
    let preferred_workspace = ((available * 2) / 3).max(1);
    let mut workspace_budget = preferred_workspace
        .min(available.saturating_sub(min_tab))
        .max(1);
    let mut tab_budget = available.saturating_sub(workspace_budget);

    let workspace_len = display_width(&entry.primary_label);
    let tab_len = display_width(tab_label);

    if workspace_len < workspace_budget {
        let spare = workspace_budget - workspace_len;
        workspace_budget = workspace_len;
        tab_budget = (tab_budget + spare).min(available.saturating_sub(workspace_budget));
    }
    if tab_len < tab_budget {
        let spare = tab_budget - tab_len;
        tab_budget = tab_len;
        workspace_budget = (workspace_budget + spare).min(available.saturating_sub(tab_budget));
    }

    format!(
        "{}{}{}",
        truncate_end(&entry.primary_label, workspace_budget),
        separator,
        truncate_end(tab_label, tab_budget)
    )
}

fn format_agent_panel_primary_label(entry: &AgentPanelEntry, max_width: usize) -> String {
    format_agent_panel_primary_label_content(entry, max_width)
}

fn workspace_row_height(ws: &crate::workspace::Workspace) -> u16 {
    if ws.branch().is_some() {
        2
    } else {
        1
    }
}

fn workspace_attention_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        (AgentState::Idle, false) => 3,
        (AgentState::Working, _) => 2,
        (AgentState::Idle, true) => 1,
        (AgentState::Unknown, _) => 0,
    }
}

fn space_aggregate_state(app: &AppState, key: &str) -> (AgentState, bool) {
    app.workspaces
        .iter()
        .filter(|ws| ws.worktree_space().is_some_and(|space| space.key == key))
        .map(|ws| ws.aggregate_state(&app.terminals))
        .max_by_key(|(state, seen)| workspace_attention_priority(*state, *seen))
        .unwrap_or((AgentState::Unknown, true))
}

pub(crate) fn workspace_parent_group_state(
    app: &AppState,
    ws_idx: usize,
) -> Option<(String, bool)> {
    let space = app.workspaces.get(ws_idx)?.worktree_space()?;
    if space.is_linked_worktree {
        return None;
    }
    let member_count = app
        .workspaces
        .iter()
        .filter(|ws| {
            ws.worktree_space()
                .is_some_and(|member| member.key == space.key)
        })
        .count();
    (member_count >= 2).then(|| {
        (
            space.key.clone(),
            app.collapsed_space_keys.contains(&space.key),
        )
    })
}

pub(crate) fn grouped_child_display_label(
    label: &str,
    branch: Option<&str>,
    has_custom_name: bool,
) -> String {
    if has_custom_name {
        return label.to_string();
    }
    let Some(branch) = branch else {
        return label.to_string();
    };
    branch
        .strip_prefix("worktree/")
        .unwrap_or(branch)
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceListEntry {
    Workspace { ws_idx: usize, indented: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceListRowArea {
    entry: WorkspaceListEntry,
    rect: Rect,
}

pub(crate) fn next_entry_is_indented_workspace(entries: &[WorkspaceListEntry], idx: usize) -> bool {
    matches!(
        entries.get(idx.saturating_add(1)),
        Some(WorkspaceListEntry::Workspace { indented: true, .. })
    )
}

fn workspace_list_entry_content_height(app: &AppState, entry: &WorkspaceListEntry) -> u16 {
    match entry {
        WorkspaceListEntry::Workspace { ws_idx, indented } => {
            let Some(ws) = app.workspaces.get(*ws_idx) else {
                return 0;
            };
            if *indented {
                1
            } else {
                workspace_row_height(ws)
            }
        }
    }
}

fn workspace_list_entry_gap_after(entries: &[WorkspaceListEntry], idx: usize) -> u16 {
    match entries.get(idx) {
        Some(WorkspaceListEntry::Workspace { indented, .. }) => {
            u16::from(!(*indented && next_entry_is_indented_workspace(entries, idx)))
        }
        None => 0,
    }
}

pub(crate) fn normalized_workspace_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    let body = workspace_list_body_rect(ws_area, false);
    if body.height == 0 {
        return requested;
    }

    let entry_count = workspace_list_entries(app).len();
    if entry_count == 0 {
        0
    } else {
        requested.min(entry_count.saturating_sub(1))
    }
}

pub(crate) fn workspace_list_entries(app: &AppState) -> Vec<WorkspaceListEntry> {
    local_workspace_list_entries_inner(app, false)
}

/// Like [`workspace_list_entries`] but always expands worktree groups, ignoring
/// `collapsed_space_keys`. The mobile switcher has no collapse affordance and
/// always shows the full worktree tree.
pub(crate) fn workspace_list_entries_expanded(app: &AppState) -> Vec<WorkspaceListEntry> {
    local_workspace_list_entries_inner(app, true)
}

fn local_workspace_list_entries_inner(
    app: &AppState,
    force_expanded: bool,
) -> Vec<WorkspaceListEntry> {
    let mut members_by_key = std::collections::HashMap::<String, Vec<usize>>::new();
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        if let Some(space) = ws.worktree_space() {
            members_by_key
                .entry(space.key.clone())
                .or_default()
                .push(ws_idx);
        }
    }
    let grouped_keys = members_by_key
        .iter()
        .filter(|(_, members)| {
            members.len() >= 2
                && members.iter().any(|idx| {
                    app.workspaces
                        .get(*idx)
                        .and_then(|ws| ws.worktree_space())
                        .is_some_and(|space| !space.is_linked_worktree)
                })
        })
        .map(|(key, _)| key.clone())
        .collect::<std::collections::HashSet<_>>();

    let visible_group_idx = if matches!(app.mode, Mode::Navigate) {
        Some(app.selected)
    } else {
        app.active
    };
    let active_group = visible_group_idx.and_then(|idx| {
        app.workspaces
            .get(idx)
            .and_then(|ws| ws.worktree_space())
            .map(|space| space.key.clone())
    });

    let mut emitted_groups = std::collections::HashSet::<String>::new();
    let mut entries = Vec::new();
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        let Some(space) = ws
            .worktree_space()
            .filter(|space| grouped_keys.contains(&space.key))
        else {
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
            continue;
        };

        if !emitted_groups.insert(space.key.clone()) {
            continue;
        }

        let Some(members) = members_by_key.get(&space.key) else {
            continue;
        };
        let Some(parent_idx) = members.iter().copied().find(|idx| {
            app.workspaces
                .get(*idx)
                .and_then(|member| member.worktree_space())
                .is_some_and(|member_space| !member_space.is_linked_worktree)
        }) else {
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
            continue;
        };
        let collapsed = !force_expanded && app.collapsed_space_keys.contains(&space.key);
        entries.push(WorkspaceListEntry::Workspace {
            ws_idx: parent_idx,
            indented: false,
        });

        if collapsed {
            if let Some(active_idx) = visible_group_idx
                .filter(|idx| *idx != parent_idx)
                .filter(|_| active_group.as_deref() == Some(space.key.as_str()))
            {
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx: active_idx,
                    indented: true,
                });
            }
        } else {
            for member_idx in members {
                if *member_idx == parent_idx {
                    continue;
                }
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx: *member_idx,
                    indented: true,
                });
            }
        }
    }
    entries
}

pub(crate) fn workspace_list_rect(area: Rect, split_ratio: f32) -> Rect {
    let (ws_area, _) = expanded_sidebar_sections(area, split_ratio);
    ws_area
}

pub(crate) fn workspace_list_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    workspace_list_body_rect_inner(area, has_scrollbar, true)
}

fn workspace_list_body_rect_inner(area: Rect, has_scrollbar: bool, reserve_footer: bool) -> Rect {
    if area.width == 0 || area.height <= WORKSPACE_SECTION_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(WORKSPACE_SECTION_HEADER_ROWS);
    let body_bottom = if reserve_footer {
        area.y + area.height.saturating_sub(1)
    } else {
        area.y + area.height
    };
    let body_height = body_bottom.saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

/// Returns the fixed footer row for an already-computed workspace-list area.
pub(crate) fn workspace_list_footer_rect(area: Rect) -> Rect {
    if area == Rect::default() {
        return Rect::default();
    }

    let y = area.y + area.height.saturating_sub(1);
    Rect::new(area.x, y, area.width, 1)
}

pub(crate) fn workspace_list_local_actions_rect(app: &AppState, area: Rect) -> Rect {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    workspace_list_footer_rect(ws_area)
}

fn workspace_list_visible_count(app: &AppState, area: Rect, scroll: usize) -> usize {
    let body = workspace_list_body_rect(area, false);
    if body.width == 0 || body.height == 0 {
        return 0;
    }

    workspace_list_row_areas_for_body(app, body, scroll).len()
}

pub(crate) fn workspace_list_scroll_metrics(
    app: &AppState,
    area: Rect,
) -> crate::pane::ScrollMetrics {
    let entries = workspace_list_entries(app);
    let total_rows = entries.len();
    let scroll = app.workspace_scroll.min(total_rows.saturating_sub(1));
    let viewport_rows = workspace_list_visible_count(app, area, scroll);
    let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
    let offset_from_bottom = total_rows
        .saturating_sub(scroll)
        .saturating_sub(viewport_rows);

    crate::pane::ScrollMetrics {
        offset_from_bottom,
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn workspace_list_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = workspace_list_scroll_metrics(app, area);
    let body = workspace_list_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn agent_panel_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= AGENT_PANEL_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(AGENT_PANEL_HEADER_ROWS);
    let body_height = (area.y + area.height).saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

fn agent_panel_visible_count(entries: &[AgentPanelEntry], area: Rect, scroll: usize) -> usize {
    let body = agent_panel_body_rect(area, false);
    if body.width == 0 || body.height == 0 {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    for entry in entries.iter().skip(scroll) {
        let content_height = agent_panel_entry_content_height(entry);
        if used_rows.saturating_add(content_height) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(content_height);
        visible += 1;
        let gap = agent_panel_entry_gap_after(entry);
        if gap > 0 && used_rows < body.height {
            used_rows = used_rows.saturating_add(gap);
        }
    }
    visible
}

fn agent_panel_max_scroll(entries: &[AgentPanelEntry], area: Rect) -> usize {
    if entries.is_empty() {
        return 0;
    }

    let body = agent_panel_body_rect(area, false);
    if body.width == 0 || body.height == 0 {
        return entries.len();
    }

    for scroll in 0..entries.len() {
        if agent_panel_visible_count(entries, area, scroll) >= entries.len().saturating_sub(scroll)
        {
            return scroll;
        }
    }

    entries.len().saturating_sub(1)
}

pub(crate) fn agent_panel_scroll_metrics(app: &AppState, area: Rect) -> crate::pane::ScrollMetrics {
    let entries = agent_panel_entries(app);
    let max_offset_from_bottom = agent_panel_max_scroll(&entries, area);
    let scroll = app.agent_panel_scroll.min(max_offset_from_bottom);
    let viewport_rows = agent_panel_visible_count(&entries, area, scroll);
    let offset_from_bottom = max_offset_from_bottom.saturating_sub(scroll);

    crate::pane::ScrollMetrics {
        offset_from_bottom,
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn agent_panel_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = agent_panel_scroll_metrics(app, area);
    let body = agent_panel_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn workspace_list_new_button_rect(row_rect: Rect) -> Rect {
    if row_rect.width == 0 || row_rect.height == 0 {
        return Rect::default();
    }

    let width = 5u16.min(row_rect.width.max(1));
    Rect::new(row_rect.x, row_rect.y, width, 1)
}

pub(crate) fn workspace_list_menu_button_rect(row_rect: Rect, attention_badge: bool) -> Rect {
    if row_rect.width == 0 || row_rect.height == 0 {
        return Rect::default();
    }

    let width = if attention_badge { 8 } else { 6 }.min(row_rect.width.max(1));
    let x = row_rect.x + row_rect.width.saturating_sub(width);
    Rect::new(x, row_rect.y, width, row_rect.height)
}

pub(crate) fn compute_workspace_list_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::WorkspaceCardArea> {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    if ws_area == Rect::default() {
        return Vec::new();
    }

    let mut cards = Vec::new();

    let metrics = workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect(ws_area, should_show_scrollbar(metrics));
    if body.width > 0 && body.height > 0 {
        for row in workspace_list_row_areas_for_body(app, body, app.workspace_scroll) {
            match row.entry {
                WorkspaceListEntry::Workspace { ws_idx, indented } => {
                    cards.push(crate::app::state::WorkspaceCardArea {
                        ws_idx,
                        rect: row.rect,
                        indented,
                    });
                }
            }
        }
    }

    cards
}

pub(crate) fn compute_workspace_card_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::WorkspaceCardArea> {
    compute_workspace_list_areas(app, area)
}

fn workspace_list_row_areas_for_body(
    app: &AppState,
    body: Rect,
    scroll: usize,
) -> Vec<WorkspaceListRowArea> {
    let mut rows = Vec::new();
    if body.width == 0 || body.height == 0 {
        return rows;
    }

    let entries = workspace_list_entries(app);
    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        let row_height = workspace_list_entry_content_height(app, entry);
        if row_height == 0 {
            continue;
        }
        let gap = workspace_list_entry_gap_after(&entries, entry_idx);
        if row_y.saturating_add(row_height).saturating_add(gap) > body_bottom {
            break;
        }
        rows.push(WorkspaceListRowArea {
            entry: entry.clone(),
            rect: Rect::new(body.x, row_y, body.width, row_height),
        });
        row_y = row_y.saturating_add(row_height + gap);
    }

    rows
}

/// Auto-scale sidebar width based on workspace identity + agent summary.
pub(crate) fn collapsed_sidebar_sections(area: Rect) -> (Rect, Option<u16>, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), None, Rect::default());
    }

    if content.height < 7 {
        return (content, None, Rect::default());
    }

    let total_h = content.height as usize;
    let ws_h = total_h.div_ceil(2);
    let detail_h = total_h.saturating_sub(ws_h + 1);
    if ws_h == 0 || detail_h == 0 {
        return (content, None, Rect::default());
    }

    let divider_y = content.y + ws_h as u16;
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h as u16);
    let detail_area = Rect::new(content.x, divider_y + 1, content.width, detail_h as u16);
    (ws_area, Some(divider_y), detail_area)
}

/// Collapsed sidebar: workspace glance on top, compact agent list below.
pub(super) fn render_sidebar_collapsed(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let is_navigating = matches!(app.mode, Mode::Navigate);

    let p = &app.palette;
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };
    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, divider_y, detail_area) = collapsed_sidebar_sections(area);
    if ws_area == Rect::default() {
        render_sidebar_toggle(app, frame, area, true, p);
        return;
    }

    for (visible_idx, ws) in app.workspaces.iter().enumerate() {
        let y = ws_area.y + visible_idx as u16;
        if y >= ws_area.y + ws_area.height {
            break;
        }
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);
        let (icon, icon_style) = state_dot(agg_state, agg_seen, p);
        let is_selected = visible_idx == app.selected && is_navigating;
        let is_active = Some(visible_idx) == app.active;
        let row_style = if is_selected {
            Style::default().bg(p.surface0)
        } else if is_active {
            Style::default().bg(p.surface_dim)
        } else {
            Style::default()
        };
        let num_style = if is_selected {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else if is_active {
            Style::default().fg(p.text).bg(p.surface_dim)
        } else {
            Style::default().fg(p.overlay0)
        };

        if is_selected || is_active {
            let buf = frame.buffer_mut();
            for x in ws_area.x..ws_area.x + ws_area.width {
                buf[(x, y)].set_style(row_style);
            }
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{}", visible_idx + 1), num_style),
                Span::styled(" ", row_style),
                Span::styled(icon, icon_style),
            ])),
            Rect::new(ws_area.x, y, ws_area.width, 1),
        );
    }

    if let Some(divider_y) = divider_y {
        let buf = frame.buffer_mut();
        for x in ws_area.x..ws_area.x + ws_area.width {
            buf[(x, divider_y)].set_symbol("─");
            buf[(x, divider_y)].set_style(Style::default().fg(p.surface_dim));
        }
    }

    let detail_ws_idx = if is_navigating {
        Some(app.selected)
    } else {
        app.active
    };
    let detail_content_area = Rect::new(
        detail_area.x,
        detail_area.y,
        detail_area.width,
        detail_area.height.saturating_sub(1),
    );
    if detail_content_area != Rect::default() {
        if let Some(ws_idx) = detail_ws_idx {
            if let Some(ws) = app.workspaces.get(ws_idx) {
                for (detail_idx, detail) in ws.pane_details(&app.terminals).iter().enumerate() {
                    let y = detail_content_area.y + detail_idx as u16;
                    if y >= detail_content_area.y + detail_content_area.height {
                        break;
                    }
                    let pane_num = ws
                        .public_pane_number(detail.pane_id)
                        .unwrap_or(detail_idx + 1);
                    let pane_style = Style::default().fg(p.overlay0);
                    let (icon, icon_style) =
                        agent_icon(detail.state, detail.seen, app.spinner_tick, p);
                    frame.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled(format!("{pane_num}"), pane_style),
                            Span::styled(" ", pane_style),
                            Span::styled(icon, icon_style),
                        ])),
                        Rect::new(detail_content_area.x, y, detail_content_area.width, 1),
                    );
                }
            }
        }
    }

    render_sidebar_toggle(app, frame, area, true, p);
}

pub(crate) fn workspace_drop_indicator_row(
    cards: &[crate::app::state::WorkspaceCardArea],
    area: Rect,
    insert_idx: usize,
) -> Option<u16> {
    if area.height == 0 {
        return None;
    }
    let list_bottom = area.y + area.height.saturating_sub(1);

    let first = cards.first()?;
    if insert_idx == first.ws_idx {
        return first.rect.y.checked_sub(1).filter(|y| *y < list_bottom);
    }

    if let Some(row) = cards
        .last()
        .filter(|card| insert_idx == card.ws_idx.saturating_add(1))
        .map(|card| card.rect.y.saturating_add(card.rect.height))
        .filter(|y| *y < list_bottom)
    {
        return Some(row);
    }

    if let Some(card) = cards.iter().find(|card| card.ws_idx == insert_idx) {
        return card.rect.y.checked_sub(1).filter(|y| *y < list_bottom);
    }

    None
}

pub(super) fn render_sidebar(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;
    let is_navigating = matches!(app.mode, Mode::Navigate);
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };

    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    render_host_rail(app, frame, app.view.host_rail_rect);

    let panel_area = if app.view.sidebar_panel_rect == Rect::default() {
        area
    } else {
        app.view.sidebar_panel_rect
    };
    let (ws_area, detail_area) = expanded_sidebar_sections(panel_area, app.sidebar_section_split);

    render_workspace_list(app, terminal_runtimes, frame, ws_area, is_navigating);
    render_agent_detail(app, terminal_runtimes, frame, detail_area);
    render_sidebar_toggle(app, frame, area, false, p);
}

fn render_host_rail(app: &AppState, frame: &mut Frame, area: Rect) {
    if area == Rect::default() {
        return;
    }

    let p = &app.palette;

    // The rail's own internal divider separates it from the adjacent
    // Spaces/Agents panel. Unlike the outer sidebar/main-area divider (drawn
    // by `render_sidebar`), it is a static visual element: it never reflects
    // Navigate-mode accent styling and is never draggable.
    let divider_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(divider_x, y)].set_symbol("│");
        buf[(divider_x, y)].set_style(Style::default().fg(p.surface_dim));
    }

    // Header mirrors the ` spaces` / ` agents` language.
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " hosts",
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        )])),
        Rect::new(area.x, area.y, area.width.saturating_sub(1), 1),
    );

    let metrics = host_list_scroll_metrics(app, area);
    let scrollbar_rect = host_list_scrollbar_rect(app, area);
    let rows = host_list_row_areas(app);
    let selected_source = app.effective_sidebar_source();

    for row in &rows {
        let entry = host_list_entries(app)
            .into_iter()
            .find(|entry| entry.source == row.source);
        let Some(entry) = entry else {
            continue;
        };
        render_host_row(app, &entry, row.rect, &selected_source, p, frame);
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }
}

fn render_host_row(
    _app: &AppState,
    entry: &HostListEntry,
    rect: Rect,
    selected_source: &SidebarSource,
    p: &Palette,
    frame: &mut Frame,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }

    let selected = entry.source == *selected_source;
    // A configured display-only host with no cached status is treated locally
    // as stale/disconnected for presentation only; this never becomes cache
    // state.
    let stale = entry.status.is_none() || entry.status.is_some_and(|s| !s.is_connected());
    let style = if selected {
        Style::default()
            .fg(p.text)
            .bg(p.surface0)
            .add_modifier(Modifier::BOLD)
    } else if stale {
        Style::default().fg(p.overlay0).add_modifier(Modifier::DIM)
    } else {
        Style::default().fg(p.subtext0)
    };
    if selected {
        let buf = frame.buffer_mut();
        for x in rect.x..rect.x + rect.width {
            buf[(x, rect.y)].set_style(Style::default().bg(p.surface0));
        }
    }

    // Right-edge status marker for cached remote statuses.
    let marker_rect = entry
        .status
        .and_then(|_| (rect.width > 1).then_some(Rect::new(rect.x + rect.width - 1, rect.y, 1, 1)));
    let label_x = rect.x.saturating_add(HOST_ROW_LEADING_GUTTER);
    let label_width = rect
        .width
        .saturating_sub(HOST_ROW_LEADING_GUTTER)
        .saturating_sub(u16::from(marker_rect.is_some()));
    frame.render_widget(
        Paragraph::new(truncate_text(&entry.label, label_width as usize)).style(style),
        Rect::new(label_x, rect.y, label_width, 1),
    );
    if let (Some(status), Some(marker_rect)) = (entry.status, marker_rect) {
        let (symbol, marker_style) = host_status_marker(status, p);
        let marker_style = if selected {
            marker_style.bg(p.surface0)
        } else {
            marker_style
        };
        frame.render_widget(
            Paragraph::new(Span::styled(symbol, marker_style)),
            marker_rect,
        );
    }
}

fn render_workspace_list(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
    is_navigating: bool,
) {
    let p = &app.palette;
    let dragged_ws_idx = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder { source_ws_idx, .. }) => {
            Some(*source_ws_idx)
        }
        _ => None,
    };
    let insertion_row = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder {
            insert_idx: Some(insert_idx),
            ..
        }) => workspace_drop_indicator_row(&app.view.workspace_card_areas, area, *insert_idx),
        _ => None,
    };

    // Keep workspace cards above the fixed local-actions footer.
    let list_bottom = area.y + area.height.saturating_sub(1);
    if area.height > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                " spaces",
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            )])),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    let metrics = workspace_list_scroll_metrics(app, area);
    let scrollbar_rect = workspace_list_scrollbar_rect(app, area);
    let cards = &app.view.workspace_card_areas;

    for card in cards {
        let i = card.ws_idx;
        let ws = &app.workspaces[i];
        let row_y = card.rect.y;
        let row_height = card.rect.height;
        let selected = i == app.selected && is_navigating;
        let is_active = Some(i) == app.active;
        let is_dragged = dragged_ws_idx == Some(i);
        let highlighted = selected || is_active || is_dragged;
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);

        if highlighted {
            let bg = if selected {
                p.surface0
            } else if is_dragged {
                p.surface1
            } else {
                p.surface_dim
            };
            let buf = frame.buffer_mut();
            for y in row_y..row_y + row_height {
                if y >= list_bottom {
                    break;
                }
                for x in card.rect.x..card.rect.x + card.rect.width {
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
            }
        }

        let name_style = if selected || is_active || is_dragged {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };

        let (icon, icon_style) = state_dot(agg_state, agg_seen, p);
        let label = ws.display_name_from(&app.terminals, terminal_runtimes);
        let mut line1 = Vec::new();
        let mut show_workspace_icon = true;
        if card.indented {
            line1.push(Span::styled("   ", Style::default()));
        } else if let Some((key, collapsed)) = workspace_parent_group_state(app, i) {
            let icon = if collapsed { "▸" } else { "▾" };
            let (state_icon, state_style) = if collapsed {
                let (state, seen) = space_aggregate_state(app, &key);
                state_dot(state, seen, p)
            } else {
                (icon, Style::default().fg(p.accent))
            };
            line1.push(Span::styled(icon, Style::default().fg(p.accent)));
            if collapsed {
                line1.push(Span::styled(" ", Style::default()));
                line1.push(Span::styled(state_icon, state_style));
                show_workspace_icon = false;
            }
            line1.push(Span::styled(" ", Style::default()));
        } else {
            line1.push(Span::styled(" ", Style::default()));
        }
        if show_workspace_icon {
            line1.push(Span::styled(icon, icon_style));
            line1.push(Span::styled(" ", Style::default()));
        }
        if card.indented {
            let display_label = grouped_child_display_label(
                &label,
                ws.branch().as_deref(),
                ws.custom_name.is_some(),
            );
            line1.push(Span::styled(display_label, name_style));
        } else {
            line1.push(Span::styled(label, name_style));
        }

        frame.render_widget(
            Paragraph::new(Line::from(line1)),
            Rect::new(card.rect.x, row_y, card.rect.width, 1),
        );

        if row_height > 1 && row_y + 1 < list_bottom {
            if let Some(branch) = ws.branch() {
                let upstream_label = ws.git_ahead_behind().and_then(|(ahead, behind)| {
                    let mut parts = Vec::new();
                    if ahead > 0 {
                        parts.push((format!("↑{}", ahead), p.green));
                    }
                    if behind > 0 {
                        parts.push((format!("↓{}", behind), p.red));
                    }
                    (!parts.is_empty()).then_some(parts)
                });
                let reserved = upstream_label
                    .as_ref()
                    .map(|parts| {
                        parts.iter().map(|(label, _)| label.len()).sum::<usize>() + parts.len()
                    })
                    .unwrap_or(0);
                let max_branch_len = (card.rect.width as usize).saturating_sub(5 + reserved);
                let branch_display = truncate_end(&branch, max_branch_len);
                let branch_color = if selected || is_active {
                    p.mauve
                } else {
                    p.overlay0
                };
                let branch_indent = if card.indented { "     " } else { "   " };
                let mut spans = vec![
                    Span::styled(branch_indent, Style::default()),
                    Span::styled(branch_display, Style::default().fg(branch_color)),
                ];
                if let Some(parts) = upstream_label {
                    spans.push(Span::styled(" ", Style::default()));
                    for (idx, (label, color)) in parts.into_iter().enumerate() {
                        if idx > 0 {
                            spans.push(Span::styled(" ", Style::default()));
                        }
                        spans.push(Span::styled(label, Style::default().fg(color)));
                    }
                }
                frame.render_widget(
                    Paragraph::new(Line::from(spans)),
                    Rect::new(card.rect.x, row_y + 1, card.rect.width, 1),
                );
            }
        }
    }

    render_local_actions_row(
        frame,
        workspace_list_footer_rect(area),
        p,
        app.mouse_capture,
        app.global_menu_attention_badge_visible(),
    );

    if let Some(y) = insertion_row.filter(|y| *y < list_bottom) {
        let indicator_right = scrollbar_rect
            .map(|rect| rect.x)
            .unwrap_or(area.x + area.width);
        let buf = frame.buffer_mut();
        for x in area.x..indicator_right {
            buf[(x, y)].set_symbol("─");
            buf[(x, y)].set_style(Style::default().fg(p.accent));
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }
}

fn render_agent_detail(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;

    if area.height < 3 {
        return;
    }

    let sep_line = "─".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(&sep_line, Style::default().fg(p.surface_dim))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " agents",
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        )])),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
    let toggle_rect = agent_panel_toggle_rect(area, app.agent_panel_sort);
    if toggle_rect != Rect::default() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                agent_panel_sort_label(app.agent_panel_sort),
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Right),
            toggle_rect,
        );
    }

    let details = agent_panel_entries_from(app, terminal_runtimes);
    let metrics = agent_panel_scroll_metrics(app, area);
    let scrollbar_rect = agent_panel_scrollbar_rect(app, area);
    let body = agent_panel_body_rect(area, should_show_scrollbar(metrics));
    if body == Rect::default() {
        return;
    }

    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    for detail in details.iter().skip(app.agent_panel_scroll) {
        let content_height = agent_panel_entry_content_height(detail);
        if row_y.saturating_add(content_height) > body_bottom {
            break;
        }

        // Check if this entry corresponds to the active local pane.
        let is_active = detail
            .local_target()
            .is_some_and(|(ws_idx, tab_idx, pane_id)| app.is_active_pane(ws_idx, tab_idx, pane_id));
        let is_highlighted = is_active;

        let (icon, icon_style) = agent_icon(detail.state, detail.seen, app.spinner_tick, p);
        let label_color = state_label_color(detail.state, detail.seen, p);
        let label = detail
            .state_labels
            .get(agent_panel_status_key(detail.state, detail.seen))
            .map(String::as_str)
            .unwrap_or_else(|| state_label(detail.state, detail.seen));

        let row_style = if is_highlighted {
            Style::default().bg(p.surface_dim)
        } else {
            Style::default()
        };

        let name_style = if is_highlighted {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD)
        };
        let status_style = if is_highlighted {
            Style::default().fg(label_color)
        } else {
            Style::default().fg(label_color).add_modifier(Modifier::DIM)
        };
        let agent_style = Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);

        let primary_label =
            format_agent_panel_primary_label(detail, body.width.saturating_sub(3) as usize);
        let name_line = Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(icon, icon_style),
            Span::styled(" ", Style::default()),
            Span::styled(primary_label, name_style),
        ]);
        frame.render_widget(
            Paragraph::new(name_line).style(row_style),
            Rect::new(body.x, row_y, body.width, 1),
        );
        row_y += 1;

        let mut status_spans = vec![
            Span::styled("   ", Style::default()),
            Span::styled(label, status_style),
        ];
        if let Some(agent_label) = &detail.agent_label {
            status_spans.push(Span::styled(" · ", agent_style));
            status_spans.push(Span::styled(agent_label, agent_style));
        }
        if let Some(custom_status) = &detail.custom_status {
            status_spans.push(Span::styled(" · ", agent_style));
            status_spans.push(Span::styled(custom_status.clone(), agent_style));
        }
        frame.render_widget(
            Paragraph::new(Line::from(status_spans)).style(row_style),
            Rect::new(body.x, row_y, body.width, 1),
        );
        row_y += 1;

        let gap = agent_panel_entry_gap_after(detail);
        if gap > 0 && row_y < body_bottom {
            row_y = row_y.saturating_add(gap);
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }
}

fn render_local_actions_row(
    frame: &mut Frame,
    rect: Rect,
    p: &Palette,
    mouse_capture: bool,
    attention_badge: bool,
) {
    if !mouse_capture || rect == Rect::default() {
        return;
    }

    let new_rect = workspace_list_new_button_rect(rect);
    frame.render_widget(
        Paragraph::new(Span::styled(" new", Style::default().fg(p.overlay0))),
        new_rect,
    );

    let menu_rect = workspace_list_menu_button_rect(rect, attention_badge);
    let menu_line = if attention_badge {
        Line::from(vec![
            Span::styled(
                "● ",
                Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled("menu", Style::default().fg(p.overlay0)),
        ])
    } else {
        Line::from(vec![Span::styled("menu", Style::default().fg(p.overlay0))])
    };
    frame.render_widget(
        Paragraph::new(menu_line).alignment(Alignment::Right),
        menu_rect,
    );
}

pub(crate) fn collapsed_sidebar_toggle_rect(area: Rect) -> Rect {
    let bottom_y = area.y + area.height.saturating_sub(1);
    let content_w = area.width.saturating_sub(1);
    if content_w == 0 || area.height == 0 {
        return Rect::default();
    }
    let x = area.x + content_w / 2;
    Rect::new(x, bottom_y, 1, 1)
}

pub(crate) fn expanded_sidebar_toggle_rect(area: Rect) -> Rect {
    if area.width <= 1 || area.height == 0 {
        return Rect::default();
    }
    Rect::new(
        area.x + area.width.saturating_sub(2),
        area.y + area.height.saturating_sub(1),
        1,
        1,
    )
}

fn render_sidebar_toggle(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    collapsed: bool,
    p: &Palette,
) {
    let toggle_area = if collapsed {
        collapsed_sidebar_toggle_rect(area)
    } else {
        expanded_sidebar_toggle_rect(area)
    };
    if toggle_area == Rect::default() {
        return;
    }
    let icon = if collapsed { "»" } else { "«" };
    let icon_style = if collapsed && app.global_menu_attention_badge_visible() {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0)
    };
    frame.render_widget(Paragraph::new(Span::styled(icon, icon_style)), toggle_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{detect::Agent, remote_source::RemoteHostKey, workspace::Workspace};
    use ratatui::{backend::TestBackend, Terminal};

    fn workspace_entries(app: &AppState) -> Vec<WorkspaceListEntry> {
        workspace_list_entries(app)
    }

    fn rendered_workspace_rows(app: &AppState, area: Rect) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        terminal
            .draw(|frame| render_workspace_list(app, &terminal_runtimes, frame, area, false))
            .unwrap();

        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    fn rendered_host_rail_buffer(app: &AppState, area: Rect) -> ratatui::buffer::Buffer {
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_host_rail(app, frame, area))
            .unwrap();

        terminal.backend().buffer().clone()
    }

    #[test]
    fn render_sidebar_toggle_draws_expanded_collapse_icon() {
        let app = crate::app::state::AppState::test_new();
        let area = Rect::new(0, 0, 26, 20);
        let mut terminal =
            Terminal::new(TestBackend::new(26, 20)).expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_toggle(&app, frame, area, false, &app.palette))
            .expect("sidebar toggle should render");

        let toggle = expanded_sidebar_toggle_rect(area);
        assert_eq!(
            terminal.backend().buffer()[(toggle.x, toggle.y)].symbol(),
            "«"
        );
    }

    #[test]
    fn expanded_sidebar_toggle_sits_inside_sidebar_content() {
        let area = Rect::new(0, 0, 26, 20);
        let toggle = expanded_sidebar_toggle_rect(area);

        assert_eq!(toggle.x, area.x + area.width - 2);
        assert_eq!(toggle.y, area.y + area.height - 1);
    }

    #[test]
    fn all_workspaces_agent_panel_entries_use_workspace_and_optional_tab_labels() {
        let mut app = crate::app::state::AppState::test_new();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let mut second = Workspace::test_new("two");
        let second_tab = second.test_add_tab(Some("logs"));
        let second_pane = second.tabs[second_tab].root_pane;

        app.workspaces = vec![first, second];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.workspaces[1].tabs[second_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.active = Some(0);
        app.selected = 0;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "one");
        assert!(entries[0].primary_tab_label.is_none());
        assert_eq!(entries[0].agent_label.as_deref(), Some("pi"));
        assert_eq!(entries[1].primary_label, "two");
        assert_eq!(entries[1].primary_tab_label.as_deref(), Some("logs"));
        assert_eq!(entries[1].agent_label.as_deref(), Some("claude"));
    }

    #[test]
    fn priority_agent_panel_sort_uses_attention_then_space_order() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
            Workspace::test_new("four"),
        ];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;

        let set_state = |app: &mut crate::app::state::AppState, ws_idx: usize, state| {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = state;
        };
        set_state(&mut app, 0, AgentState::Working);
        set_state(&mut app, 1, AgentState::Idle);
        set_state(&mut app, 2, AgentState::Working);
        set_state(&mut app, 3, AgentState::Blocked);

        let done_pane = app.workspaces[1].tabs[0].root_pane;
        app.workspaces[1].tabs[0]
            .panes
            .get_mut(&done_pane)
            .unwrap()
            .seen = false;

        let labels: Vec<String> = agent_panel_entries(&app)
            .into_iter()
            .map(|entry| entry.primary_label)
            .collect();

        assert_eq!(labels, ["four", "two", "one", "three"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn all_workspaces_agent_panel_entries_use_live_root_runtime_cwd_for_workspace_label() {
        let unique = format!(
            "herdr-agent-panel-runtime-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let stale_cwd = root.join("issue-264-nix-support");
        let live_cwd = root.join("herdr");
        std::fs::create_dir_all(stale_cwd.join(".git")).unwrap();
        std::fs::create_dir_all(live_cwd.join(".git")).unwrap();

        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("stale-name");
        workspace.custom_name = None;
        workspace.identity_cwd = stale_cwd.clone();
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.cwd = stale_cwd;
        terminal.detected_agent = Some(Agent::Pi);
        app.active = Some(0);
        app.selected = 0;

        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            &crate::pane::PaneLaunchEnv::default(),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let mut runtime_registry = TerminalRuntimeRegistry::new();
        runtime_registry.insert(terminal_id, runtime);
        let entries = agent_panel_entries_from(&app, &runtime_registry);
        let primary_label = entries[0].primary_label.clone();

        for (_, runtime) in runtime_registry.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);

        assert_eq!(primary_label, "herdr");
    }

    #[test]
    fn all_workspaces_agent_panel_entries_prefer_agent_names_for_agent_identity() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("bridge");
        let first_pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .set_agent_name("planner".into());
        app.active = Some(0);
        app.selected = 0;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "bridge");
        assert_eq!(entries[0].agent_label.as_deref(), Some("planner"));
    }

    #[test]
    fn empty_local_workspace_list_renders_footer_actions() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = Vec::new();
        app.active = None;
        app.selected = 0;
        app.mouse_capture = true;

        let area = Rect::new(0, 0, 40, 16);
        let rows = rendered_workspace_rows(&app, area);
        let footer = workspace_list_footer_rect(area);

        assert!(workspace_list_entries(&app).is_empty());
        assert_ne!(footer, Rect::default());
        assert!(rows[footer.y as usize].contains(" new"));
        assert!(rows[footer.y as usize].trim_end().ends_with("menu"));
    }

    #[test]
    fn host_rail_remote_status_markers_render_in_status_column() {
        let mut app = crate::app::state::AppState::test_new();
        for (host, status) in [
            (
                "alpha",
                crate::remote_source::RemoteConnectionStatus::Connected,
            ),
            (
                "bravo",
                crate::remote_source::RemoteConnectionStatus::Disconnected,
            ),
            (
                "charlie",
                crate::remote_source::RemoteConnectionStatus::NeedsUpdate,
            ),
            (
                "delta",
                crate::remote_source::RemoteConnectionStatus::Unreachable,
            ),
        ] {
            app.remote_sources.mark_status(
                &RemoteHostKey::new(host, crate::session::DEFAULT_SESSION_NAME),
                status,
            );
        }
        // Header (row 0) + breathing row (row 1) + local (row 2) + four remote
        // hosts (rows 3..6).
        let width = 10u16;
        let area = Rect::new(0, 0, width, 8);
        app.view.host_rail_rect = area;

        let buffer = rendered_host_rail_buffer(&app, area);
        let marker_x = area.x + area.width - 2;

        assert_eq!(buffer[(marker_x, 3)].symbol(), "●");
        assert_eq!(buffer[(marker_x, 3)].style().fg, Some(app.palette.green));
        assert_eq!(buffer[(marker_x, 4)].symbol(), "○");
        assert_eq!(buffer[(marker_x, 4)].style().fg, Some(app.palette.overlay0));
        assert!(buffer[(marker_x, 4)]
            .style()
            .add_modifier
            .contains(Modifier::DIM));
        assert_eq!(buffer[(marker_x, 5)].symbol(), "↑");
        assert_eq!(buffer[(marker_x, 5)].style().fg, Some(app.palette.yellow));
        assert!(buffer[(marker_x, 5)]
            .style()
            .add_modifier
            .contains(Modifier::BOLD));
        assert_eq!(buffer[(marker_x, 6)].symbol(), "×");
        assert_eq!(buffer[(marker_x, 6)].style().fg, Some(app.palette.red));
        assert!(buffer[(marker_x, 6)]
            .style()
            .add_modifier
            .contains(Modifier::BOLD));
    }

    #[test]
    fn host_rail_selected_remote_marker_keeps_surface_background() {
        let mut app = crate::app::state::AppState::test_new();
        let host = RemoteHostKey::new("charlie", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources.mark_status(
            &host,
            crate::remote_source::RemoteConnectionStatus::NeedsUpdate,
        );
        let width = 10u16;
        let area = Rect::new(0, 0, width, 4);
        app.view.host_rail_rect = area;
        app.select_sidebar_source(SidebarSource::Remote(host));

        let buffer = rendered_host_rail_buffer(&app, area);
        let marker_x = area.x + area.width - 2;
        let marker_style = buffer[(marker_x, 3)].style();

        assert_eq!(buffer[(marker_x, 3)].symbol(), "↑");
        assert_eq!(marker_style.fg, Some(app.palette.yellow));
        assert_eq!(marker_style.bg, Some(app.palette.surface0));
        assert!(marker_style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn host_rail_remote_label_truncates_before_marker() {
        let mut app = crate::app::state::AppState::test_new();
        app.remote_sources.mark_status(
            &RemoteHostKey::new("verylongremotehost", crate::session::DEFAULT_SESSION_NAME),
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        let width = 10u16;
        let area = Rect::new(0, 0, width, 4);
        app.view.host_rail_rect = area;

        let buffer = rendered_host_rail_buffer(&app, area);
        // Header occupies row 0, row 1 is the section breathing row, and local
        // occupies row 2, so the remote host renders on row 3. The right-edge
        // divider column (area.width - 1) is
        // the rail's own internal divider (drawn by `render_host_rail`, not
        // the outer sidebar/main-area divider from `render_sidebar`), so read
        // only the content columns up to and including the status marker.
        let row = (0..(area.width - 1))
            .map(|x| buffer[(x, 3)].symbol())
            .collect::<String>();

        assert_eq!(row, " verylo…●");
    }

    #[test]
    fn host_rail_header_buffer_and_body_rows_follow_sidebar_spacing() {
        let mut app = crate::app::state::AppState::test_new();
        app.remote_sources.mark_status(
            &RemoteHostKey::new("verylongremotehost", crate::session::DEFAULT_SESSION_NAME),
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        let area = Rect::new(0, 0, 10, 4);
        app.view.host_rail_rect = area;

        let buffer = rendered_host_rail_buffer(&app, area);
        let row = |y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        };

        assert_eq!(row(0), " hosts   │");
        assert_eq!(row(1), "         │");
        assert_eq!(row(2), " local   │");
        assert_eq!(row(3), " verylo…●│");
    }

    #[test]
    fn configured_manual_and_on_demand_hosts_visible_without_cache_entries() {
        // G1: configured manual/on_demand hosts are carried by the pure display
        // collection and merged with cached statuses only when building host
        // rows. They must NEVER become synthetic RemoteSourceCache entries, so
        // the on-demand dispatch contract (no-cache precheck passes; cached
        // non-connected fails fast) is preserved.
        let mut app = crate::app::state::AppState::test_new();
        let auto_host = RemoteHostKey::new("alpha", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources.mark_status(
            &auto_host,
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        // Configured display-only hosts (manual / on_demand) carried only by
        // the pure display collection, NOT seeded into the cache.
        let manual = RemoteHostKey::new("brain", crate::session::DEFAULT_SESSION_NAME);
        let on_demand = RemoteHostKey::new("future", "agents");
        app.configured_remote_hosts = [manual.clone(), on_demand.clone()].into_iter().collect();

        let entries = host_list_entries(&app);
        let labels: Vec<_> = entries.iter().map(|entry| entry.label.clone()).collect();
        // local first, then deterministic host/session order across the union
        // of cached + configured-only hosts.
        assert_eq!(labels, vec!["local", "alpha", "brain", "future/agents"]);

        // Critical boundary: the configured-only hosts are NOT in the cache.
        assert!(app.remote_sources.host_status(&manual).is_none());
        assert!(app.remote_sources.host_status(&on_demand).is_none());
        // The cached auto host is unaffected.
        assert!(app.remote_sources.host_status(&auto_host).is_some());

        // A configured-only host renders with no cached status (stale styling is
        // derived locally by the row builder), never as a synthetic entry.
        let manual_entry = entries
            .iter()
            .find(|entry| entry.source == SidebarSource::Remote(manual.clone()))
            .expect("manual host present in entries");
        assert!(manual_entry.status.is_none());
    }

    #[test]
    fn host_list_scroll_clamps_and_renders_scrollbar_when_overflowing() {
        // G2/G3: a long host list is capped to the Hosts viewport, scrolls, and
        // renders a scrollbar; scroll offsets clamp at the bounds, mirroring the
        // existing Spaces workspace-list scroll behavior.
        let mut app = crate::app::state::AppState::test_new();
        for i in 0..30 {
            let host =
                RemoteHostKey::new(format!("host{i:02}"), crate::session::DEFAULT_SESSION_NAME);
            app.remote_sources.mark_status(
                &host,
                crate::remote_source::RemoteConnectionStatus::Connected,
            );
        }
        // Small host rail viewport: header + breathing row + 3 body rows.
        let area = Rect::new(0, 0, 26, 5);
        app.view.host_rail_rect = area;

        let metrics = host_list_scroll_metrics(&app, area);
        // 30 remote hosts + local = 31 entries; viewport = 3 body rows.
        assert_eq!(metrics.viewport_rows, 3);
        assert_eq!(metrics.max_offset_from_bottom, 31 - 3);
        assert!(should_show_scrollbar(metrics));
        assert!(host_list_scrollbar_rect(&app, area).is_some());

        // Scrolling past the last visible offset clamps to the viewport's true
        // maximum first-visible offset (entry_count - viewport_capacity = 28),
        // not entry_count - 1, so the trailing viewport shows the last hosts.
        app.host_list_scroll = normalized_host_list_scroll(&app, 100);
        assert_eq!(app.host_list_scroll, 28);

        // Visible rows track the scroll offset: only the viewport-height rows
        // starting below the header and breathing row are reachable hit
        // targets.
        app.host_list_scroll = 5;
        let rows = host_list_row_areas(&app);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].rect.y, area.y + 2);
    }

    #[test]
    fn host_list_scroll_clamps_to_zero_when_list_shrinks_to_fit() {
        // Reviewer B finding 1: after an overflow-scrolled host list shrinks
        // until all entries fit, a stale nonzero scroll must clamp to 0 (the
        // viewport's true maximum first-visible offset), not entry_count - 1,
        // so `local`/leading hosts are never skipped.
        let mut app = crate::app::state::AppState::test_new();
        for i in 0..30 {
            app.remote_sources.mark_status(
                &RemoteHostKey::new(format!("host{i:02}"), crate::session::DEFAULT_SESSION_NAME),
                crate::remote_source::RemoteConnectionStatus::Connected,
            );
        }
        // Header + breathing row + 2 body rows -> viewport capacity 2; 31
        // entries overflow.
        let area = Rect::new(0, 0, 26, 4);
        app.view.host_rail_rect = area;
        app.host_list_scroll = 5;
        let overflow_metrics = host_list_scroll_metrics(&app, area);
        assert!(overflow_metrics.max_offset_from_bottom > 0);

        // Shrink: only one host survives, so local + 1 = 2 entries fit the
        // 2-row viewport.
        let keeper = RemoteHostKey::new("host00", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources = crate::remote_source::RemoteSourceCache::default();
        app.remote_sources.mark_status(
            &keeper,
            crate::remote_source::RemoteConnectionStatus::Connected,
        );

        // The stale scroll (5) must clamp to 0 now that everything fits.
        app.host_list_scroll = normalized_host_list_scroll(&app, app.host_list_scroll);
        assert_eq!(app.host_list_scroll, 0);

        // Local and the surviving host are both visible (no leading skip).
        let visible: Vec<_> = host_list_row_areas(&app)
            .into_iter()
            .map(|row| row.source)
            .collect();
        assert!(visible.contains(&SidebarSource::Local));
        assert!(visible.contains(&SidebarSource::Remote(keeper)));
        assert_eq!(visible.len(), 2);
    }

    #[test]
    fn all_workspaces_primary_label_truncates_workspace_and_tab() {
        let entry = AgentPanelEntry {
            location: AgentPanelEntryLocation::Local {
                ws_idx: 0,
                tab_idx: 0,
                pane_id: crate::layout::PaneId::from_raw(1),
            },
            primary_label: "agent-browser".into(),
            primary_tab_label: Some("test-escalation".into()),
            agent_label: Some("claude".into()),
            state: AgentState::Idle,
            seen: true,
            last_agent_state_change_seq: None,
            custom_status: None,
            state_labels: std::collections::HashMap::new(),
        };

        let label = format_agent_panel_primary_label(&entry, 18);

        assert_eq!(label, "agent-bro… · test…");
    }

    #[test]
    fn expanded_sidebar_sections_handle_tiny_heights() {
        let (ws_area, detail_area) = expanded_sidebar_sections(Rect::new(0, 0, 20, 5), 0.9);

        assert_eq!(ws_area, Rect::new(0, 0, 19, 3));
        assert_eq!(detail_area, Rect::new(0, 3, 19, 2));
    }

    #[test]
    fn sidebar_section_divider_is_hidden_for_tiny_heights() {
        let divider = sidebar_section_divider_rect(Rect::new(0, 0, 20, 5), 0.5);

        assert_eq!(divider, Rect::default());
    }

    #[test]
    fn grouped_child_label_keeps_custom_workspace_name() {
        assert_eq!(
            grouped_child_display_label("renamed issue", Some("worktree/issue-137"), true),
            "renamed issue"
        );
    }

    #[test]
    fn grouped_child_label_uses_short_branch_for_auto_named_workspace() {
        assert_eq!(
            grouped_child_display_label("herdr-issue", Some("worktree/issue-137"), false),
            "issue-137"
        );
    }

    #[test]
    fn workspace_list_truncates_cjk_branch_without_panic() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("repo");
        ws.cached_git_branch = Some("feature/中文-分支-644".into());
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 1, 15, 2),
            indented: false,
        }];

        let mut terminal = Terminal::new(TestBackend::new(15, 6)).expect("test terminal");
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();

        terminal
            .draw(|frame| {
                render_workspace_list(&app, &runtimes, frame, Rect::new(0, 0, 15, 6), false)
            })
            .expect("workspace list should render");
    }

    fn workspace_with_worktree_space(
        name: &str,
        key: Option<&str>,
        checkout_key: &str,
    ) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        if let Some(key) = key {
            ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                key: key.into(),
                label: "herdr".into(),
                repo_root: std::path::PathBuf::from("/repo/herdr"),
                checkout_path: std::path::PathBuf::from(checkout_key),
                is_linked_worktree: name != "main",
            });
        }
        ws
    }

    fn workspace_with_git_space(name: &str, key: &str) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        ws.cached_git_space = Some(crate::workspace::GitSpaceMetadata {
            key: key.into(),
            checkout_key: format!("/repo/{name}"),
            label: "herdr".into(),
            repo_root: std::path::PathBuf::from(format!("/repo/{name}")),
            is_linked_worktree: false,
        });
        ws
    }

    #[test]
    fn parent_workspace_row_stays_clickable_when_grouped() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        let cards = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 20));
        assert_eq!(cards[0].ws_idx, 0);
        assert!(!cards[0].indented);
        assert_eq!(cards[1].ws_idx, 1);
        assert!(cards[1].indented);
        assert_eq!(cards[1].rect.y, cards[0].rect.y + cards[0].rect.height + 1);
    }

    #[test]
    fn linked_only_worktree_members_do_not_form_parentless_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
        ];

        let entries = workspace_entries(&app);

        assert_eq!(
            entries,
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false
                },
            ]
        );
    }

    #[test]
    fn compact_space_group_scroll_offset_can_start_inside_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("one", Some("repo-key"), "/repo/herdr-one"),
            workspace_with_worktree_space("two", Some("repo-key"), "/repo/herdr-two"),
        ];
        let area = Rect::new(0, 0, 30, 20);
        app.workspace_scroll = normalized_workspace_scroll(&app, area, 2);

        let cards = compute_workspace_list_areas(&app, area);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 2);
    }

    #[test]
    fn workspace_scroll_metrics_count_display_entries_not_raw_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;

        let ws_area = Rect::new(0, 0, 30, 6);
        let metrics = workspace_list_scroll_metrics(&app, ws_area);

        assert_eq!(metrics.viewport_rows, 1);
        assert_eq!(metrics.max_offset_from_bottom, 1);
        assert_eq!(metrics.offset_from_bottom, 1);
    }

    #[test]
    fn workspace_scroll_offset_applies_to_group_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;
        app.workspace_scroll = 1;

        let cards = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 12));
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 2);
    }

    #[test]
    fn workspace_list_entries_group_multiple_workspaces_in_same_git_space() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_group_non_contiguous_explicit_members() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("normal", "other-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_group_normal_git_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_git_space("two", "repo-key"),
        ];

        assert_eq!(
            workspace_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_auto_attach_normal_git_workspace_to_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("scratch", "repo-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_leave_single_git_and_non_git_workspaces_flat() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_worktree_space("notes", None, "/notes"),
        ];

        assert_eq!(
            workspace_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn collapsed_group_hides_inactive_children_but_keeps_active_visible() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.active = Some(1);
        app.mode = Mode::Terminal;
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );

        app.active = None;
        app.mode = Mode::Terminal;
        assert_eq!(
            workspace_entries(&app),
            vec![WorkspaceListEntry::Workspace {
                ws_idx: 0,
                indented: false,
            }]
        );
    }

    // ── render helper ────────────────────────────────────────────────────────────

    #[test]
    fn collapsed_group_keeps_selected_child_visible_in_navigate_mode() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.mode = Mode::Navigate;
        app.selected = 1;
        app.active = Some(1);
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }
}
