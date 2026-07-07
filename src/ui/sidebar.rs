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
const SOURCE_RAIL_WIDTH: u16 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentPanelEntryLocation {
    Local {
        ws_idx: usize,
        tab_idx: usize,
        pane_id: crate::layout::PaneId,
    },
    Remote {
        host: String,
        session: String,
        terminal_id: String,
    },
    // All-source test views construct host headers; normal projections list a selected host directly.
    #[allow(dead_code)]
    RemoteHost { host: String, session: String },
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
pub(crate) struct SourceRailEntry {
    pub(crate) source: SidebarSource,
    pub(crate) label: String,
    pub(crate) status: Option<crate::remote_source::RemoteConnectionStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceRailRowArea {
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
            AgentPanelEntryLocation::Remote { .. } | AgentPanelEntryLocation::RemoteHost { .. } => {
                None
            }
        }
    }

    pub(crate) fn remote_attach_target(&self) -> Option<crate::remote_source::RemoteAttachTarget> {
        match &self.location {
            AgentPanelEntryLocation::Remote {
                host,
                session,
                terminal_id,
            } => Some(crate::remote_source::RemoteAttachTarget {
                host: host.clone(),
                session: session.clone(),
                terminal_id: terminal_id.clone(),
                label: remote_attach_label(host, session, &self.primary_label),
            }),
            AgentPanelEntryLocation::Local { .. } | AgentPanelEntryLocation::RemoteHost { .. } => {
                None
            }
        }
    }
}

pub(crate) fn source_rail_width() -> u16 {
    SOURCE_RAIL_WIDTH
}

pub(crate) fn source_rail_should_show(app: &AppState, screen: Rect) -> bool {
    !app.sidebar_collapsed
        && !app.remote_sources.list_host_statuses().is_empty()
        && screen.width > SOURCE_RAIL_WIDTH + app.sidebar_min_width
}

pub(crate) fn source_rail_entries(app: &AppState) -> Vec<SourceRailEntry> {
    let mut entries = Vec::new();
    if app.remote_sources.list_host_statuses().is_empty() {
        return entries;
    }

    entries.push(SourceRailEntry {
        source: SidebarSource::Local,
        label: "local".to_string(),
        status: None,
    });
    let mut remote_entries = app.remote_sources.list_host_statuses();
    remote_entries.sort_by(source_rail_host_status_order);
    entries.extend(remote_entries.into_iter().map(|entry| SourceRailEntry {
        source: SidebarSource::Remote(entry.host.clone()),
        label: remote_host_label(&entry.host),
        status: Some(entry.status),
    }));
    entries
}

fn source_rail_host_status_order(
    left: &crate::remote_source::RemoteHostStatusEntry,
    right: &crate::remote_source::RemoteHostStatusEntry,
) -> std::cmp::Ordering {
    left.host
        .host
        .cmp(&right.host.host)
        .then_with(|| {
            source_rail_session_rank(&left.host.session)
                .cmp(&source_rail_session_rank(&right.host.session))
        })
        .then_with(|| left.host.session.cmp(&right.host.session))
}

fn source_rail_session_rank(session: &str) -> u8 {
    if session == crate::session::DEFAULT_SESSION_NAME {
        0
    } else {
        1
    }
}

fn source_rail_status_marker(
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

fn source_rail_row_areas(app: &AppState, area: Rect) -> Vec<SourceRailRowArea> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }

    source_rail_entries(app)
        .into_iter()
        .enumerate()
        .filter_map(|(idx, entry)| {
            let y = area.y.saturating_add(idx as u16);
            (y < area.y + area.height).then_some(SourceRailRowArea {
                source: entry.source,
                rect: Rect::new(area.x, y, area.width, 1),
            })
        })
        .collect()
}

pub(crate) fn source_rail_target_at(app: &AppState, col: u16, row: u16) -> Option<SidebarSource> {
    let area = app.view.source_rail_rect;
    source_rail_row_areas(app, area)
        .into_iter()
        .find_map(|entry| {
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
    projected_agent_panel_entries_with_runtimes(app, None)
}

pub(crate) fn agent_panel_entries_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<AgentPanelEntry> {
    projected_agent_panel_entries_with_runtimes(app, Some(terminal_runtimes))
}

#[cfg(test)]
pub(crate) fn all_source_agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    all_source_agent_panel_entries_with_runtimes(app, None)
}

fn projected_agent_panel_entries_with_runtimes(
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

    let mut entries = match app.effective_sidebar_source() {
        SidebarSource::Local => local_agent_panel_entries_with_runtimes(app, terminal_runtimes),
        SidebarSource::Remote(host) => remote_agent_panel_entries_for_host(app, &host),
    };
    sort_agent_panel_entries(app, &mut entries);
    entries
}

#[cfg(test)]
fn all_source_agent_panel_entries_with_runtimes(
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
    entries.extend(remote_agent_panel_entries(app));
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

fn remote_agent_panel_entries_for_host(
    app: &AppState,
    host: &crate::remote_source::RemoteHostKey,
) -> Vec<AgentPanelEntry> {
    app.remote_sources
        .entries_for_host(host)
        .into_iter()
        .map(|entry| remote_agent_panel_entry(app, entry))
        .collect()
}

#[cfg(test)]
fn remote_agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    let mut agents_by_host: std::collections::BTreeMap<
        crate::remote_source::RemoteHostKey,
        Vec<crate::remote_source::RemoteAgentEntry>,
    > = std::collections::BTreeMap::new();
    for entry in app.remote_sources.list_entries() {
        agents_by_host
            .entry(entry.host.clone())
            .or_default()
            .push(entry);
    }

    let mut entries = Vec::new();
    for host_entry in app.remote_sources.list_host_statuses() {
        if let Some(host_agents) = agents_by_host.remove(&host_entry.host) {
            entries.push(remote_host_panel_entry(host_entry));
            entries.extend(
                host_agents
                    .into_iter()
                    .map(|entry| remote_agent_panel_entry(app, entry)),
            );
        } else if host_entry.agent_count == 0 && !host_entry.status.is_connected() {
            entries.push(remote_host_panel_entry(host_entry));
        }
    }

    for (host, host_agents) in agents_by_host {
        let status = host_agents
            .first()
            .map(|entry| entry.status)
            .unwrap_or(crate::remote_source::RemoteConnectionStatus::Disconnected);
        entries.push(remote_host_panel_entry(
            crate::remote_source::RemoteHostStatusEntry {
                host,
                status,
                agent_count: host_agents.len(),
            },
        ));
        entries.extend(
            host_agents
                .into_iter()
                .map(|entry| remote_agent_panel_entry(app, entry)),
        );
    }

    entries
}

fn remote_agent_panel_entry(
    app: &AppState,
    entry: crate::remote_source::RemoteAgentEntry,
) -> AgentPanelEntry {
    let agent_label = remote_agent_label(&entry.agent);
    let (state, seen) = remote_agent_state(entry.agent.agent_status);
    let target = crate::remote_source::RemoteAttachTarget {
        host: entry.host.host.clone(),
        session: entry.host.session.clone(),
        terminal_id: entry.agent.terminal_id.clone(),
        label: remote_attach_label(&entry.host.host, &entry.host.session, &agent_label),
    };
    let attached = !entry.stale() && app.has_remote_attach_pane(&target);
    let custom_status = if entry.stale() {
        entry.status.stale_label().map(str::to_string)
    } else {
        remote_agent_custom_status(entry.agent.custom_status.clone(), attached)
    };

    AgentPanelEntry {
        location: AgentPanelEntryLocation::Remote {
            host: entry.host.host,
            session: entry.host.session,
            terminal_id: entry.agent.terminal_id.clone(),
        },
        primary_label: agent_label,
        primary_tab_label: None,
        agent_label: None,
        state,
        seen,
        last_agent_state_change_seq: Some(entry.agent.revision),
        custom_status,
        state_labels: entry.agent.state_labels,
    }
}

fn remote_agent_custom_status(custom_status: Option<String>, attached: bool) -> Option<String> {
    if !attached {
        return custom_status;
    }

    match custom_status {
        Some(status) if !status.trim().is_empty() => Some(format!("{status} · attached")),
        _ => Some("attached".to_string()),
    }
}

#[cfg(test)]
fn remote_host_panel_entry(entry: crate::remote_source::RemoteHostStatusEntry) -> AgentPanelEntry {
    let primary_label = remote_host_header_label(&entry.host);
    let mut state_labels = std::collections::HashMap::new();
    state_labels.insert("unknown".to_string(), "remote".to_string());

    AgentPanelEntry {
        location: AgentPanelEntryLocation::RemoteHost {
            host: entry.host.host,
            session: entry.host.session,
        },
        primary_label,
        primary_tab_label: None,
        agent_label: None,
        state: AgentState::Unknown,
        seen: true,
        last_agent_state_change_seq: None,
        custom_status: entry.status.stale_label().map(str::to_string),
        state_labels,
    }
}

#[cfg(test)]
fn remote_host_header_label(host: &crate::remote_source::RemoteHostKey) -> String {
    format!("{} agents", remote_host_label(host))
}

fn remote_host_label(host: &crate::remote_source::RemoteHostKey) -> String {
    if host.session == crate::session::DEFAULT_SESSION_NAME {
        host.host.clone()
    } else {
        format!("{}/{}", host.host, host.session)
    }
}

fn remote_attach_label(host: &str, session: &str, agent_label: &str) -> String {
    if session == crate::session::DEFAULT_SESSION_NAME {
        format!("{host}/{agent_label}")
    } else {
        format!("{host}/{session}/{agent_label}")
    }
}

fn remote_space_label(
    agents: &[crate::remote_source::RemoteAgentEntry],
    workspace_id: &str,
) -> String {
    agents
        .iter()
        .find_map(|entry| {
            remote_path_basename(entry.agent.cwd.as_deref())
                .or_else(|| remote_path_basename(entry.agent.foreground_cwd.as_deref()))
        })
        .unwrap_or_else(|| workspace_id.to_string())
}

fn remote_path_basename(path: Option<&str>) -> Option<String> {
    let trimmed = path?.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return None;
    }
    let basename = trimmed.rsplit(['/', '\\']).next()?;
    (!basename.is_empty()).then(|| basename.to_string())
}

fn remote_agent_label(agent: &crate::api::schema::AgentInfo) -> String {
    agent
        .name
        .as_deref()
        .or(agent.display_agent.as_deref())
        .or(agent.agent.as_deref())
        .or(agent.title.as_deref())
        .unwrap_or(&agent.terminal_id)
        .to_string()
}

fn remote_agent_state(status: crate::api::schema::AgentStatus) -> (AgentState, bool) {
    match status {
        crate::api::schema::AgentStatus::Working => (AgentState::Working, true),
        crate::api::schema::AgentStatus::Blocked => (AgentState::Blocked, true),
        crate::api::schema::AgentStatus::Idle => (AgentState::Idle, true),
        crate::api::schema::AgentStatus::Done => (AgentState::Idle, false),
        crate::api::schema::AgentStatus::Unknown => (AgentState::Unknown, true),
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
    match &entry.location {
        AgentPanelEntryLocation::RemoteHost { .. } => 2,
        AgentPanelEntryLocation::Local { .. } | AgentPanelEntryLocation::Remote { .. } => 2,
    }
}

pub(crate) fn agent_panel_entry_gap_after(entry: &AgentPanelEntry) -> u16 {
    match &entry.location {
        AgentPanelEntryLocation::RemoteHost { .. } => 1,
        AgentPanelEntryLocation::Local { .. } | AgentPanelEntryLocation::Remote { .. } => 1,
    }
}

fn agent_panel_primary_label_indent(entry: &AgentPanelEntry, max_width: usize) -> &'static str {
    let indent = match &entry.location {
        AgentPanelEntryLocation::Remote { .. } => "  ",
        AgentPanelEntryLocation::Local { .. } | AgentPanelEntryLocation::RemoteHost { .. } => "",
    };
    if max_width > indent.chars().count() {
        indent
    } else {
        ""
    }
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
    let indent = agent_panel_primary_label_indent(entry, max_width);
    let label = format_agent_panel_primary_label_content(
        entry,
        max_width.saturating_sub(indent.chars().count()),
    );
    format!("{indent}{label}")
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

/// UI-only metadata extracted from `WorkspaceInfo` for rendering a remote space row.
/// Avoids storing the full (large) `WorkspaceInfo` inside the enum variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemoteSpaceMetadata {
    focused: bool,
    agent_status: crate::api::schema::AgentStatus,
    pane_count: usize,
    tab_count: usize,
}

impl RemoteSpaceMetadata {
    fn from_workspace_info(info: &crate::api::schema::WorkspaceInfo) -> Self {
        Self {
            focused: info.focused,
            agent_status: info.agent_status,
            pane_count: info.pane_count,
            tab_count: info.tab_count,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceListEntry {
    Workspace {
        ws_idx: usize,
        indented: bool,
    },
    RemoteSpace {
        key: crate::remote_source::RemoteSpaceKey,
        label: String,
        status: crate::remote_source::RemoteConnectionStatus,
        /// UI metadata when available (metadata-backed spaces).
        /// `None` for agent-derived fallback spaces.
        metadata: Option<RemoteSpaceMetadata>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceListRemoteRowArea {
    pub(crate) target: WorkspaceListRemoteTarget,
    pub(crate) rect: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceListRowArea {
    entry: WorkspaceListEntry,
    rect: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceListRemoteTarget {
    Space {
        key: crate::remote_source::RemoteSpaceKey,
    },
    New {
        host: crate::remote_source::RemoteHostKey,
    },
}

pub(crate) fn next_entry_is_indented_workspace(entries: &[WorkspaceListEntry], idx: usize) -> bool {
    matches!(
        entries.get(idx.saturating_add(1)),
        Some(WorkspaceListEntry::Workspace { indented: true, .. })
    )
}

fn next_entry_is_remote_space(entries: &[WorkspaceListEntry], idx: usize) -> bool {
    matches!(
        entries.get(idx.saturating_add(1)),
        Some(WorkspaceListEntry::RemoteSpace { .. })
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
        WorkspaceListEntry::RemoteSpace { .. } => 1,
    }
}

fn workspace_list_entry_gap_after(entries: &[WorkspaceListEntry], idx: usize) -> u16 {
    match entries.get(idx) {
        Some(WorkspaceListEntry::Workspace { indented, .. }) => {
            u16::from(!(*indented && next_entry_is_indented_workspace(entries, idx)))
        }
        Some(WorkspaceListEntry::RemoteSpace { .. }) => {
            u16::from(!next_entry_is_remote_space(entries, idx))
        }
        None => 0,
    }
}

pub(crate) fn normalized_workspace_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    let body = workspace_list_body_rect_for_source(app, ws_area, false);
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
    match app.effective_sidebar_source() {
        SidebarSource::Local => local_workspace_list_entries_inner(app, false),
        SidebarSource::Remote(host) => remote_workspace_list_entries_for_host(app, &host),
    }
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

fn remote_workspace_list_entries_for_host(
    app: &AppState,
    host: &crate::remote_source::RemoteHostKey,
) -> Vec<WorkspaceListEntry> {
    let mut agents_by_host: std::collections::BTreeMap<
        crate::remote_source::RemoteHostKey,
        std::collections::BTreeMap<String, Vec<crate::remote_source::RemoteAgentEntry>>,
    > = std::collections::BTreeMap::new();
    for entry in app.remote_sources.list_entries() {
        agents_by_host
            .entry(entry.host.clone())
            .or_default()
            .entry(entry.agent.workspace_id.clone())
            .or_default()
            .push(entry);
    }

    let spaces = agents_by_host.get(host);

    let mut entries = Vec::new();
    match app.remote_sources.workspace_entries_for_host(host) {
        Some(workspaces) if workspaces.is_empty() => {}
        Some(workspaces) => {
            let mut metadata_ids = std::collections::BTreeSet::new();
            for entry in workspaces {
                let ws = entry.workspace;
                metadata_ids.insert(ws.workspace_id.clone());
                entries.push(WorkspaceListEntry::RemoteSpace {
                    key: crate::remote_source::RemoteSpaceKey {
                        host: host.host.clone(),
                        session: host.session.clone(),
                        workspace_id: ws.workspace_id.clone(),
                    },
                    label: remote_workspace_metadata_label(&ws),
                    status: entry.status,
                    metadata: Some(RemoteSpaceMetadata::from_workspace_info(&ws)),
                });
            }

            if let Some(spaces) = spaces {
                for (workspace_id, agents) in spaces {
                    if metadata_ids.contains(workspace_id) {
                        continue;
                    }
                    entries.push(remote_agent_derived_space_entry(host, workspace_id, agents));
                }
            }
        }
        None => {
            if let Some(spaces) = spaces {
                for (workspace_id, agents) in spaces {
                    entries.push(remote_agent_derived_space_entry(host, workspace_id, agents));
                }
            }
        }
    }

    entries
}

fn remote_agent_derived_space_entry(
    host: &crate::remote_source::RemoteHostKey,
    workspace_id: &str,
    agents: &[crate::remote_source::RemoteAgentEntry],
) -> WorkspaceListEntry {
    let status = agents
        .first()
        .map(|entry| entry.status)
        .unwrap_or(crate::remote_source::RemoteConnectionStatus::Disconnected);
    WorkspaceListEntry::RemoteSpace {
        key: crate::remote_source::RemoteSpaceKey {
            host: host.host.clone(),
            session: host.session.clone(),
            workspace_id: workspace_id.to_string(),
        },
        label: remote_space_label(agents, workspace_id),
        status,
        metadata: None,
    }
}

fn remote_workspace_metadata_label(workspace: &crate::api::schema::WorkspaceInfo) -> String {
    let label = workspace.label.trim();
    if label.is_empty() {
        workspace.workspace_id.clone()
    } else {
        label.to_string()
    }
}

pub(crate) fn workspace_list_rect(area: Rect, split_ratio: f32) -> Rect {
    let (ws_area, _) = expanded_sidebar_sections(area, split_ratio);
    ws_area
}

pub(crate) fn workspace_list_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    workspace_list_body_rect_inner(area, has_scrollbar, true)
}

fn workspace_list_body_rect_for_source(_app: &AppState, area: Rect, has_scrollbar: bool) -> Rect {
    // Footer is always reserved for both local and remote sources so that layout
    // does not flicker on connect/disconnect/capability changes.
    workspace_list_body_rect(area, has_scrollbar)
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
    if !matches!(app.effective_sidebar_source(), SidebarSource::Local) {
        return Rect::default();
    }

    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    workspace_list_footer_rect(ws_area)
}

fn workspace_list_visible_count(app: &AppState, area: Rect, scroll: usize) -> usize {
    let body = workspace_list_body_rect_for_source(app, area, false);
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
    let body = workspace_list_body_rect_for_source(app, area, true);
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
) -> (
    Vec<crate::app::state::WorkspaceCardArea>,
    Vec<WorkspaceListRemoteRowArea>,
) {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    if ws_area == Rect::default() {
        return (Vec::new(), Vec::new());
    }

    let mut cards = Vec::new();
    let mut remote_rows = Vec::new();

    let metrics = workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect_for_source(app, ws_area, should_show_scrollbar(metrics));
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
                WorkspaceListEntry::RemoteSpace { key, .. } => {
                    remote_rows.push(WorkspaceListRemoteRowArea {
                        target: WorkspaceListRemoteTarget::Space { key },
                        rect: row.rect,
                    });
                }
            }
        }
    }

    // Fixed footer: emit a New hit target for capable/connected remote sources.
    if let SidebarSource::Remote(host) = app.effective_sidebar_source() {
        if app
            .remote_sources
            .host_status(&host)
            .is_some_and(|s| s.is_connected())
            && app.remote_sources.host_supports_workspace_create(&host)
        {
            let footer_rect = workspace_list_footer_rect(ws_area);
            let new_rect = workspace_list_new_button_rect(footer_rect);
            if new_rect != Rect::default() {
                remote_rows.push(WorkspaceListRemoteRowArea {
                    target: WorkspaceListRemoteTarget::New { host },
                    rect: new_rect,
                });
            }
        }
    }

    (cards, remote_rows)
}

pub(crate) fn compute_workspace_card_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::WorkspaceCardArea> {
    compute_workspace_list_areas(app, area).0
}

pub(crate) fn workspace_list_remote_target_at(
    app: &AppState,
    area: Rect,
    col: u16,
    row: u16,
) -> Option<WorkspaceListRemoteTarget> {
    let (_, remote_rows) = compute_workspace_list_areas(app, area);
    remote_rows.into_iter().find_map(|area| {
        (col >= area.rect.x
            && col < area.rect.x + area.rect.width
            && row >= area.rect.y
            && row < area.rect.y + area.rect.height)
            .then_some(area.target)
    })
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

    render_source_rail(app, frame, app.view.source_rail_rect);

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

fn render_source_rail(app: &AppState, frame: &mut Frame, area: Rect) {
    if area == Rect::default() {
        return;
    }

    let p = &app.palette;
    let selected_source = app.effective_sidebar_source();
    let separator_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(separator_x, y)].set_symbol("│");
        buf[(separator_x, y)].set_style(Style::default().fg(p.surface_dim));
    }

    let entries = source_rail_entries(app);
    for (idx, entry) in entries.iter().enumerate() {
        let y = area.y.saturating_add(idx as u16);
        if y >= area.y + area.height {
            break;
        }
        let row = SourceRailRowArea {
            source: entry.source.clone(),
            rect: Rect::new(area.x, y, area.width, 1),
        };
        let selected = entry.source == selected_source;
        let stale = entry.status.is_some_and(|status| !status.is_connected());
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
            for x in row.rect.x..row.rect.x + row.rect.width.saturating_sub(1) {
                buf[(x, row.rect.y)].set_style(Style::default().bg(p.surface0));
            }
        }
        let content_width = row.rect.width.saturating_sub(1);
        let marker_rect = entry.status.and_then(|_| {
            (content_width > 0).then_some(Rect::new(
                row.rect.x + content_width.saturating_sub(1),
                row.rect.y,
                1,
                1,
            ))
        });
        let label_width = content_width.saturating_sub(u16::from(marker_rect.is_some()));
        frame.render_widget(
            Paragraph::new(truncate_text(&entry.label, label_width as usize)).style(style),
            Rect::new(row.rect.x, row.rect.y, label_width, 1),
        );
        if let (Some(status), Some(marker_rect)) = (entry.status, marker_rect) {
            let (symbol, marker_style) = source_rail_status_marker(status, p);
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

    // Both local and remote sources reserve a fixed footer row; the body rows
    // (card and remote-space rendering) must stay above this boundary.
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
    let body = workspace_list_body_rect_for_source(app, area, should_show_scrollbar(metrics));
    let row_areas = workspace_list_row_areas_for_body(app, body, app.workspace_scroll);
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

    for row in row_areas {
        match row.entry {
            WorkspaceListEntry::Workspace { .. } => {}
            WorkspaceListEntry::RemoteSpace {
                key,
                label,
                status,
                metadata,
            } => {
                render_remote_space_row(app, &key, &label, status, metadata, frame, row.rect, p);
            }
        }
    }

    if matches!(app.effective_sidebar_source(), SidebarSource::Local) {
        render_local_actions_row(
            frame,
            workspace_list_footer_rect(area),
            p,
            app.mouse_capture,
            app.global_menu_attention_badge_visible(),
        );
    } else if let SidebarSource::Remote(ref host) = app.effective_sidebar_source() {
        // Render the remote `new` button in the fixed footer when the host is
        // connected and advertises workspace creation. Footer row is always
        // reserved regardless of capability to avoid layout flicker.
        let footer_rect = workspace_list_footer_rect(area);
        if footer_rect != Rect::default()
            && app
                .remote_sources
                .host_status(host)
                .is_some_and(|s| s.is_connected())
            && app.remote_sources.host_supports_workspace_create(host)
        {
            render_remote_footer_new(frame, footer_rect, p);
        }
    }

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
    let remote_projection = matches!(app.effective_sidebar_source(), SidebarSource::Remote(_));
    let toggle_rect = agent_panel_toggle_rect(area, app.agent_panel_sort);
    if !remote_projection && toggle_rect != Rect::default() {
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

        if matches!(&detail.location, AgentPanelEntryLocation::RemoteHost { .. }) {
            render_remote_host_header(
                detail,
                frame,
                Rect::new(body.x, row_y, body.width, content_height),
                p,
            );
            row_y = row_y.saturating_add(content_height);
            let gap = agent_panel_entry_gap_after(detail);
            if gap > 0 && row_y < body_bottom {
                row_y = row_y.saturating_add(gap);
            }
            continue;
        }

        // Check if this entry corresponds to the active local pane or a selected
        // remote row. Remote rows do not have local panes, so they need a
        // separate selection highlight.
        let is_active = detail
            .local_target()
            .is_some_and(|(ws_idx, tab_idx, pane_id)| app.is_active_pane(ws_idx, tab_idx, pane_id));
        let is_selected_remote = detail.remote_attach_target().is_some_and(|target| {
            app.selected_remote_agent
                .as_ref()
                .is_some_and(|selected| selected == &target.key())
        });
        let is_highlighted = is_active || is_selected_remote;

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

fn render_remote_host_header(detail: &AgentPanelEntry, frame: &mut Frame, rect: Rect, p: &Palette) {
    render_remote_section_header(
        &detail.primary_label,
        detail.custom_status.as_deref(),
        frame,
        rect,
        p,
    );
}

fn render_remote_section_header(
    primary_label: &str,
    status: Option<&str>,
    frame: &mut Frame,
    rect: Rect,
    p: &Palette,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }

    let sep_line = "─".repeat(rect.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(&sep_line, Style::default().fg(p.surface_dim))),
        Rect::new(rect.x, rect.y, rect.width, 1),
    );

    if rect.height < 2 {
        return;
    }

    let header = if let Some(status) = status {
        format!("{primary_label} · {status}")
    } else {
        primary_label.to_string()
    };
    let label = truncate_text(&header, rect.width.saturating_sub(1) as usize);
    let line = Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::styled(
            label,
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        ),
    ]);

    frame.render_widget(
        Paragraph::new(line),
        Rect::new(rect.x, rect.y + 1, rect.width, 1),
    );
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

fn render_remote_footer_new(frame: &mut Frame, rect: Rect, p: &Palette) {
    let button_rect = workspace_list_new_button_rect(rect);
    if button_rect == Rect::default() {
        return;
    }

    frame.render_widget(
        Paragraph::new(Span::styled(" new", Style::default().fg(p.overlay0))),
        button_rect,
    );
}

fn remote_space_metadata_suffix(meta: RemoteSpaceMetadata) -> Option<String> {
    // Only surface non-trivial counts that give useful context.
    if meta.tab_count <= 1 && meta.pane_count <= 1 {
        return None;
    }
    let mut parts = Vec::new();
    if meta.tab_count > 1 {
        parts.push(format!("{} tabs", meta.tab_count));
    }
    if meta.pane_count > 1 {
        parts.push(format!("{} panes", meta.pane_count));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

fn render_remote_space_row(
    app: &AppState,
    key: &crate::remote_source::RemoteSpaceKey,
    label: &str,
    status: crate::remote_source::RemoteConnectionStatus,
    metadata: Option<RemoteSpaceMetadata>,
    frame: &mut Frame,
    rect: Rect,
    p: &Palette,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }

    let selected = app.selected_remote_space.as_ref().is_some_and(|s| s == key);
    // Focused active-style only for connected rows — cached `focused==true` on a
    // stale/disconnected host must not look live.
    let focused = !selected && status.is_connected() && metadata.is_some_and(|m| m.focused);

    // Full-row background: selected (surface0) wins over focused (surface_dim).
    // Selection background is kept even for disconnected rows as a projection indicator.
    if selected || focused {
        let bg = if selected { p.surface0 } else { p.surface_dim };
        let buf = frame.buffer_mut();
        for x in rect.x..rect.x + rect.width {
            buf[(x, rect.y)].set_style(Style::default().bg(bg));
        }
    }

    // State dot: live agent_status only when connected; neutral dim dot for stale rows.
    let (dot_str, dot_style) = if status.is_connected() {
        if let Some(meta) = metadata {
            let (state, seen) = remote_agent_state(meta.agent_status);
            state_dot(state, seen, p)
        } else {
            ("·", Style::default().fg(p.overlay0))
        }
    } else {
        (
            "·",
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        )
    };

    // Label style: stale/disconnected wins over selected/focused — cached metadata
    // must not render as live even when selected.
    let label_style = if !status.is_connected() {
        Style::default().fg(p.overlay0).add_modifier(Modifier::DIM)
    } else if selected || focused {
        Style::default().fg(p.text).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.subtext0)
    };

    // Gutter: " " + dot + " " = 3 chars. Remaining width for label+suffix.
    let gutter_width = 3usize;
    let available = (rect.width as usize).saturating_sub(gutter_width);

    // Build the text: stale label takes priority over metadata suffix.
    let row_text = if let Some(stale) = status.stale_label() {
        let full = format!("{label} · {stale}");
        truncate_text(&full, available)
    } else if let Some(meta) = metadata {
        if let Some(suffix) = remote_space_metadata_suffix(meta) {
            let separator = " · ";
            let label_w = display_width(label);
            let sep_w = display_width(separator);
            let suffix_w = display_width(&suffix);
            if label_w + sep_w + suffix_w <= available {
                format!("{label}{separator}{suffix}")
            } else {
                truncate_text(label, available)
            }
        } else {
            truncate_text(label, available)
        }
    } else {
        truncate_text(label, available)
    };

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(dot_str, dot_style),
            Span::styled(" ", Style::default()),
            Span::styled(row_text, label_style),
        ])),
        Rect::new(rect.x, rect.y, rect.width, 1),
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
    use std::collections::HashMap;

    use super::*;
    use crate::{
        api::schema::{AgentInfo, AgentStatus, WorkspaceInfo},
        detect::Agent,
        remote_source::{RemoteHostKey, RemoteSourceCapabilities},
        workspace::Workspace,
    };
    use ratatui::{backend::TestBackend, Terminal};

    fn remote_agent(
        terminal_id: &str,
        label: &str,
        status: AgentStatus,
        revision: u64,
    ) -> AgentInfo {
        AgentInfo {
            terminal_id: terminal_id.to_string(),
            name: None,
            agent: Some(label.to_string()),
            title: None,
            display_agent: Some(label.to_string()),
            agent_status: status,
            screen_detection_skipped: false,
            custom_status: None,
            state_labels: HashMap::new(),
            agent_session: None,
            workspace_id: "remote-ws".to_string(),
            tab_id: "remote-tab".to_string(),
            pane_id: "remote-pane".to_string(),
            focused: false,
            cwd: None,
            foreground_cwd: None,
            revision,
        }
    }

    fn remote_workspace(workspace_id: &str, label: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            workspace_id: workspace_id.to_string(),
            number: 1,
            label: label.to_string(),
            focused: false,
            pane_count: 0,
            tab_count: 1,
            active_tab_id: "remote-tab".to_string(),
            agent_status: AgentStatus::Unknown,
            worktree: None,
        }
    }

    fn enable_remote_workspace_create(app: &mut crate::app::state::AppState, host: &RemoteHostKey) {
        app.remote_sources.set_capabilities(
            host,
            RemoteSourceCapabilities {
                workspace_list_local: true,
                workspace_create: true,
                tab_list: true,
                layout_export: true,
                ..Default::default()
            },
        );
    }

    fn workspace_entries(app: &AppState) -> Vec<WorkspaceListEntry> {
        workspace_list_entries(app)
    }

    fn select_remote_projection(app: &mut AppState, host: &RemoteHostKey) {
        app.view.layout = crate::app::state::ViewLayout::Desktop;
        app.view.source_rail_rect = Rect::new(0, 0, source_rail_width(), 20);
        app.view.sidebar_panel_rect = Rect::new(source_rail_width(), 0, app.sidebar_width, 20);
        app.select_sidebar_source(SidebarSource::Remote(host.clone()));
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

    fn rendered_source_rail_buffer(app: &AppState, area: Rect) -> ratatui::buffer::Buffer {
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_source_rail(app, frame, area))
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

        let entries = all_source_agent_panel_entries(&app);
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

        let entries = all_source_agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "bridge");
        assert_eq!(entries[0].agent_label.as_deref(), Some("planner"));
    }

    #[test]
    fn all_workspaces_agent_panel_appends_remote_entries_after_local_entries() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("local");
        let local_pane = workspace.tabs[0].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&local_pane]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Pi);
        app.remote_sources.replace_connected_snapshot(
            RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME),
            vec![remote_agent(
                "remote-term",
                "smoke-agent",
                AgentStatus::Working,
                1,
            )],
        );

        let entries = all_source_agent_panel_entries(&app);

        assert_eq!(entries.len(), 3);
        assert!(matches!(
            &entries[0].location,
            AgentPanelEntryLocation::Local { .. }
        ));
        assert_eq!(entries[0].primary_label, "local");
        assert_eq!(entries[1].primary_label, "jafar agents");
        assert_eq!(
            entries[1].location,
            AgentPanelEntryLocation::RemoteHost {
                host: "jafar".to_string(),
                session: crate::session::DEFAULT_SESSION_NAME.to_string(),
            }
        );
        assert!(entries[1].remote_attach_target().is_none());
        assert_eq!(agent_panel_entry_content_height(&entries[1]), 2);
        assert_eq!(agent_panel_entry_gap_after(&entries[1]), 1);
        assert_eq!(entries[2].primary_label, "smoke-agent");
        assert_eq!(
            entries[2].location,
            AgentPanelEntryLocation::Remote {
                host: "jafar".to_string(),
                session: crate::session::DEFAULT_SESSION_NAME.to_string(),
                terminal_id: "remote-term".to_string(),
            }
        );
        assert_eq!(
            entries[2].remote_attach_target().unwrap().label,
            "jafar/smoke-agent"
        );
        assert_eq!(entries[2].state, AgentState::Working);
        assert!(entries[2].seen);
    }

    #[test]
    fn remote_agent_panel_host_header_renders_divider_and_label_on_separate_rows() {
        let entry = AgentPanelEntry {
            location: AgentPanelEntryLocation::RemoteHost {
                host: "jafar".to_string(),
                session: crate::session::DEFAULT_SESSION_NAME.to_string(),
            },
            primary_label: "jafar agents".to_string(),
            primary_tab_label: None,
            agent_label: None,
            state: AgentState::Unknown,
            seen: true,
            last_agent_state_change_seq: None,
            custom_status: Some("unreachable".to_string()),
            state_labels: std::collections::HashMap::new(),
        };
        let backend = ratatui::backend::TestBackend::new(40, 2);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let palette = Palette::catppuccin();

        terminal
            .draw(|frame| {
                render_remote_host_header(&entry, frame, Rect::new(0, 0, 40, 2), &palette)
            })
            .unwrap();

        let divider_row = (0..40)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>();
        let label_row = (0..40)
            .map(|x| terminal.backend().buffer()[(x, 1)].symbol())
            .collect::<String>();

        assert!(divider_row.contains("─"));
        assert!(!divider_row.contains("jafar"));
        assert!(label_row.starts_with(" jafar agents · unreachable"));
    }

    #[test]
    fn remote_agent_panel_header_identifies_local_agents_section() {
        let app = crate::app::state::AppState::test_new();
        let backend = ratatui::backend::TestBackend::new(40, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let terminal_runtimes = TerminalRuntimeRegistry::new();

        terminal
            .draw(|frame| {
                render_agent_detail(&app, &terminal_runtimes, frame, Rect::new(0, 0, 40, 8))
            })
            .unwrap();

        let header_row = (0..40)
            .map(|x| terminal.backend().buffer()[(x, 1)].symbol())
            .collect::<String>();

        assert!(header_row.contains(" agents"));
    }

    #[test]
    fn local_workspace_projection_excludes_remote_rows() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("local")];
        app.active = Some(0);
        app.selected = 0;
        app.mouse_capture = true;
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources
            .replace_workspace_snapshot(host, vec![remote_workspace("remote-ws", "blank shell")]);

        let area = Rect::new(0, 0, 40, 16);
        let rows = rendered_workspace_rows(&app, area);
        let body = workspace_list_body_rect(area, false);
        let row_areas = workspace_list_row_areas_for_body(&app, body, app.workspace_scroll);
        let header_row = &rows[0];
        let local_row = row_areas
            .iter()
            .find(|row| matches!(row.entry, WorkspaceListEntry::Workspace { .. }))
            .expect("local workspace row")
            .rect
            .y;
        let footer = workspace_list_footer_rect(area);

        assert!(header_row.contains(" spaces"));
        assert!(!header_row.contains("new"));
        assert!(!header_row.contains("menu"));
        assert_eq!(workspace_list_entries(&app).len(), 1);
        assert!(matches!(
            workspace_list_entries(&app).first(),
            Some(WorkspaceListEntry::Workspace { ws_idx: 0, .. })
        ));
        assert!(!row_areas.iter().any(|row| row.rect.y == footer.y));
        for y in body.y..footer.y {
            assert!(!rows[y as usize].contains(" new"));
            assert!(!rows[y as usize].contains("menu"));
        }
        assert!(rows[footer.y as usize].contains(" new"));
        assert!(rows[footer.y as usize].trim_end().ends_with("menu"));
        assert!(local_row < footer.y);
        assert!(!row_areas
            .iter()
            .any(|row| matches!(row.entry, WorkspaceListEntry::RemoteSpace { .. })));
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
    fn remote_space_workspace_list_shows_remote_spaces_without_local_workspaces() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = Vec::new();
        app.active = None;
        app.selected = 0;
        let mut agent = remote_agent("remote-term", "fed-detach-smoke", AgentStatus::Working, 1);
        agent.cwd = Some("/home/amf/tmp".to_string());
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources
            .replace_connected_snapshot(host.clone(), vec![agent]);
        select_remote_projection(&mut app, &host);

        let entries = workspace_entries(&app);

        assert_eq!(entries.len(), 1);
        assert!(matches!(
            &entries[0],
            WorkspaceListEntry::RemoteSpace {
                key,
                label,
                status: crate::remote_source::RemoteConnectionStatus::Connected,
                ..
            } if key.host == "jafar"
                && key.session == crate::session::DEFAULT_SESSION_NAME
                && key.workspace_id == "remote-ws"
                && label == "tmp"
        ));

        let (cards, remote_rows) = compute_workspace_list_areas(&app, Rect::new(0, 0, 40, 32));
        assert!(cards.is_empty());
        assert_eq!(remote_rows.len(), 1);
        assert!(matches!(
            &remote_rows[0].target,
            WorkspaceListRemoteTarget::Space { key }
                if key.host == "jafar"
                    && key.session == crate::session::DEFAULT_SESSION_NAME
                    && key.workspace_id == "remote-ws"
        ));

        let backend = ratatui::backend::TestBackend::new(40, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &terminal_runtimes,
                    frame,
                    Rect::new(0, 0, 40, 12),
                    false,
                )
            })
            .unwrap();
        let rows = (0..12)
            .map(|y| {
                (0..40)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(!rows.iter().any(|row| row.contains("jafar spaces")));
        assert!(rows.iter().any(|row| row.contains("tmp")));
    }

    #[test]
    fn remote_space_workspace_list_shows_workspace_metadata_without_remote_agents() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = Vec::new();
        app.active = None;
        app.selected = 0;
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources.replace_workspace_snapshot(
            host.clone(),
            vec![remote_workspace("remote-ws", "blank shell")],
        );
        select_remote_projection(&mut app, &host);

        let entries = workspace_entries(&app);

        assert!(matches!(
            &entries[..],
            [WorkspaceListEntry::RemoteSpace {
                key,
                label,
                status: crate::remote_source::RemoteConnectionStatus::Connected,
                ..
            }] if key.workspace_id == "remote-ws"
                && label == "blank shell"
        ));
        let (cards, remote_rows) = compute_workspace_list_areas(&app, Rect::new(0, 0, 40, 32));
        assert!(cards.is_empty());
        assert_eq!(remote_rows.len(), 1);
        assert!(matches!(
            &remote_rows[0].target,
            WorkspaceListRemoteTarget::Space { key }
                if key.host == "jafar" && key.workspace_id == "remote-ws"
        ));
    }

    #[test]
    fn remote_workspace_new_action_renders_for_connected_capable_host_with_zero_spaces() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = Vec::new();
        app.active = None;
        app.selected = 0;
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources
            .replace_connected_snapshot(host.clone(), Vec::new());
        app.remote_sources
            .replace_workspace_snapshot(host.clone(), Vec::new());
        enable_remote_workspace_create(&mut app, &host);
        select_remote_projection(&mut app, &host);

        let entries = workspace_entries(&app);
        assert_eq!(entries.len(), 0);

        // sidebar_panel_rect equivalent — compute_workspace_list_areas derives ws_area from this
        let area = Rect::new(0, 0, 40, 12);
        let ws_area = workspace_list_rect(area, app.sidebar_section_split);
        let (cards, remote_rows) = compute_workspace_list_areas(&app, area);
        assert!(cards.is_empty());
        assert_eq!(remote_rows.len(), 1);
        let new_row = remote_rows
            .iter()
            .find(|row| matches!(row.target, WorkspaceListRemoteTarget::New { .. }))
            .expect("remote new action in footer");
        assert_eq!(
            workspace_list_remote_target_at(&app, area, new_row.rect.x, new_row.rect.y),
            Some(WorkspaceListRemoteTarget::New {
                host: RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME),
            })
        );

        // render_workspace_list receives the workspace list area (ws_area), not the full
        // sidebar panel area; use ws_area for the terminal buffer and render call so the
        // hit-rect y-coordinate matches the rendered row.
        let backend = ratatui::backend::TestBackend::new(ws_area.width, ws_area.height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        terminal
            .draw(|frame| render_workspace_list(&app, &terminal_runtimes, frame, ws_area, false))
            .unwrap();
        let rows = (0..ws_area.height)
            .map(|y| {
                (0..ws_area.width)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(!rows.iter().any(|row| row.contains("jafar spaces")));
        assert_eq!(rows[new_row.rect.y as usize].trim(), "new");
    }

    #[test]
    fn remote_workspace_new_action_renders_after_remote_spaces_not_header() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = Vec::new();
        app.active = None;
        app.selected = 0;
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources.replace_workspace_snapshot(
            host.clone(),
            vec![remote_workspace("remote-ws", "blank shell")],
        );
        enable_remote_workspace_create(&mut app, &host);
        select_remote_projection(&mut app, &host);

        let entries = workspace_entries(&app);
        assert!(matches!(
            &entries[..],
            [WorkspaceListEntry::RemoteSpace { .. }]
        ));

        let (_, remote_rows) = compute_workspace_list_areas(&app, Rect::new(0, 0, 40, 32));
        let space_row = remote_rows
            .iter()
            .find(|row| matches!(row.target, WorkspaceListRemoteTarget::Space { .. }))
            .expect("remote space row");
        let new_row = remote_rows
            .iter()
            .find(|row| matches!(row.target, WorkspaceListRemoteTarget::New { .. }))
            .expect("remote new action");
        // Space row is in the body; new is in the fixed footer below.
        assert!(space_row.rect.y < new_row.rect.y);

        let rows = rendered_workspace_rows(&app, Rect::new(0, 0, 40, 18));
        assert!(!rows.iter().any(|row| row.contains("jafar spaces")));
        let rendered_space_row = rows
            .iter()
            .position(|row| row.contains("blank shell"))
            .expect("rendered remote space row");
        let rendered_new_row = rows
            .iter()
            .position(|row| row.trim() == "new")
            .expect("rendered remote new row");
        assert!(rendered_space_row < rendered_new_row);
    }

    #[test]
    fn remote_workspace_list_reserves_fixed_footer_row() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = Vec::new();
        app.active = None;
        app.selected = 0;
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources.replace_workspace_snapshot(
            host.clone(),
            vec![remote_workspace("remote-ws", "blank shell")],
        );
        enable_remote_workspace_create(&mut app, &host);
        select_remote_projection(&mut app, &host);

        // `area` is the workspace-list area — the same rect passed to
        // `render_workspace_list` / `rendered_workspace_rows`.
        let area = Rect::new(0, 0, 40, WORKSPACE_SECTION_HEADER_ROWS + 3);
        let bottom_y = area.y + area.height.saturating_sub(1);
        let body = workspace_list_body_rect_for_source(&app, area, false);
        // Footer is always reserved for remote sources; body must not overlap the last row.
        assert!(body.y + body.height <= bottom_y);

        // Verify footer geometry using the same helpers render uses (workspace-list
        // area, not an enclosing panel area). compute_workspace_list_areas takes an
        // enclosing sidebar panel rect and derives ws_area internally, so it must
        // not be passed the already-derived workspace-list area here.
        let footer = workspace_list_footer_rect(area);
        assert_eq!(footer.y, bottom_y);
        let new_rect = workspace_list_new_button_rect(footer);
        assert_ne!(new_rect, Rect::default());

        let rows = rendered_workspace_rows(&app, area);
        assert_eq!(rows[bottom_y as usize].trim(), "new");
        assert!(!rows[bottom_y as usize].contains("menu"));
    }

    #[test]
    fn remote_workspace_new_action_hides_for_incapable_or_disconnected_hosts() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = Vec::new();
        app.active = None;
        let incapable = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        let disconnected = RemoteHostKey::new("work", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources
            .replace_connected_snapshot(incapable.clone(), Vec::new());
        app.remote_sources
            .replace_workspace_snapshot(incapable.clone(), Vec::new());
        app.remote_sources
            .replace_workspace_snapshot(disconnected.clone(), Vec::new());
        enable_remote_workspace_create(&mut app, &disconnected);
        app.remote_sources.mark_status(
            &disconnected,
            crate::remote_source::RemoteConnectionStatus::Unreachable,
        );
        select_remote_projection(&mut app, &incapable);

        let entries = workspace_entries(&app);

        assert!(entries.is_empty());
        select_remote_projection(&mut app, &disconnected);
        assert!(workspace_entries(&app).is_empty());
    }

    #[test]
    fn remote_space_workspace_list_prefers_metadata_label_and_merges_missing_agent_spaces() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = Vec::new();
        app.active = None;
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        let mut metadata_agent = remote_agent("term-a", "codex", AgentStatus::Working, 1);
        metadata_agent.cwd = Some("/home/amf/agent-cwd".to_string());
        let mut fallback_agent = remote_agent("term-b", "claude", AgentStatus::Working, 1);
        fallback_agent.workspace_id = "agent-only-ws".to_string();
        fallback_agent.cwd = Some("/home/amf/fallback".to_string());
        app.remote_sources
            .replace_connected_snapshot(host.clone(), vec![fallback_agent, metadata_agent]);
        app.remote_sources.replace_workspace_snapshot(
            host.clone(),
            vec![remote_workspace("remote-ws", "metadata label")],
        );
        select_remote_projection(&mut app, &host);

        let labels = workspace_list_entries(&app)
            .into_iter()
            .filter_map(|entry| match entry {
                WorkspaceListEntry::RemoteSpace { label, .. } => Some(label),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(labels, vec!["metadata label", "fallback"]);
    }

    #[test]
    fn remote_space_workspace_list_authoritative_empty_metadata_suppresses_agent_fallback() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = Vec::new();
        app.active = None;
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources.replace_connected_snapshot(
            host.clone(),
            vec![remote_agent(
                "remote-term",
                "fed-detach-smoke",
                AgentStatus::Working,
                1,
            )],
        );
        app.remote_sources
            .replace_workspace_snapshot(host.clone(), Vec::new());
        select_remote_projection(&mut app, &host);

        let entries = workspace_entries(&app);

        assert!(entries.is_empty());
    }

    #[test]
    fn remote_space_workspace_list_keeps_metadata_rows_stale_after_disconnect() {
        let mut app = crate::app::state::AppState::test_new();
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources.replace_workspace_snapshot(
            host.clone(),
            vec![remote_workspace("remote-ws", "blank shell")],
        );
        app.remote_sources.mark_status(
            &host,
            crate::remote_source::RemoteConnectionStatus::Unreachable,
        );
        select_remote_projection(&mut app, &host);

        let entries = workspace_list_entries(&app);

        assert!(matches!(
            entries.iter().find(|entry| matches!(entry, WorkspaceListEntry::RemoteSpace { .. })),
            Some(WorkspaceListEntry::RemoteSpace {
                label,
                status: crate::remote_source::RemoteConnectionStatus::Unreachable,
                ..
            }) if label == "blank shell"
        ));
    }

    #[test]
    fn workspace_list_remote_space_hit_testing_respects_scroll_with_local_rows() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.workspace_scroll = 3;
        let mut agent = remote_agent("remote-term", "fed-detach-smoke", AgentStatus::Working, 1);
        agent.cwd = Some("/home/amf/tmp".to_string());
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources
            .replace_connected_snapshot(host.clone(), vec![agent]);
        select_remote_projection(&mut app, &host);

        let area = Rect::new(0, 0, 40, 18);
        let (_, remote_rows) = compute_workspace_list_areas(&app, area);
        let space_row = remote_rows
            .iter()
            .find(|row| matches!(row.target, WorkspaceListRemoteTarget::Space { .. }))
            .expect("visible remote space row");

        assert_eq!(
            workspace_list_remote_target_at(&app, area, space_row.rect.x + 1, space_row.rect.y),
            Some(WorkspaceListRemoteTarget::Space {
                key: crate::remote_source::RemoteSpaceKey {
                    host: "jafar".to_string(),
                    session: crate::session::DEFAULT_SESSION_NAME.to_string(),
                    workspace_id: "remote-ws".to_string(),
                }
            })
        );
    }

    #[test]
    fn remote_space_workspace_list_moves_space_labels_out_of_agent_panel() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = Vec::new();
        app.active = None;
        app.selected = 0;
        let mut agent = remote_agent("remote-term", "fed-detach-smoke", AgentStatus::Working, 1);
        agent.cwd = Some("/home/amf/tmp".to_string());
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources
            .replace_connected_snapshot(host.clone(), vec![agent]);
        select_remote_projection(&mut app, &host);

        let workspace_labels = workspace_list_entries(&app)
            .into_iter()
            .filter_map(|entry| match entry {
                WorkspaceListEntry::RemoteSpace { label, .. } => Some(label),
                WorkspaceListEntry::Workspace { .. } => None,
            })
            .collect::<Vec<_>>();
        let agent_labels = agent_panel_entries(&app)
            .into_iter()
            .map(|entry| entry.primary_label)
            .collect::<Vec<_>>();

        assert_eq!(workspace_labels, vec!["tmp"]);
        assert_eq!(agent_labels, vec!["fed-detach-smoke"]);
        assert!(!agent_labels.iter().any(|label| label == "tmp"));
    }

    #[test]
    fn remote_agent_panel_lists_remote_agents_directly_under_one_host() {
        let mut app = crate::app::state::AppState::test_new();
        let mut first = remote_agent("term-a", "codex", AgentStatus::Working, 1);
        first.workspace_id = "workspace-a".to_string();
        let mut second = remote_agent("term-b", "claude", AgentStatus::Idle, 1);
        second.workspace_id = "workspace-b".to_string();
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources
            .replace_connected_snapshot(host.clone(), vec![second, first]);
        select_remote_projection(&mut app, &host);

        let entries = agent_panel_entries(&app);
        let labels: Vec<_> = entries
            .iter()
            .map(|entry| entry.primary_label.as_str())
            .collect();

        assert_eq!(labels, vec!["codex", "claude"]);
        assert!(entries
            .iter()
            .all(|entry| !matches!(&entry.location, AgentPanelEntryLocation::Local { .. })));
        assert!(matches!(
            &entries[0].location,
            AgentPanelEntryLocation::Remote { .. }
        ));
        assert!(matches!(
            &entries[1].location,
            AgentPanelEntryLocation::Remote { .. }
        ));
    }

    #[test]
    fn remote_space_label_prefers_agent_cwd_basename_then_foreground_cwd_then_workspace_id() {
        let mut app = crate::app::state::AppState::test_new();
        let mut cwd_agent = remote_agent("term-cwd", "codex", AgentStatus::Working, 1);
        cwd_agent.workspace_id = "a-cwd".to_string();
        cwd_agent.cwd = Some("/home/amf/project".to_string());
        let mut foreground_agent = remote_agent("term-fg", "claude", AgentStatus::Working, 1);
        foreground_agent.workspace_id = "b-foreground".to_string();
        foreground_agent.foreground_cwd = Some("/tmp/logs/".to_string());
        let mut fallback_agent = remote_agent("term-fallback", "pi", AgentStatus::Working, 1);
        fallback_agent.workspace_id = "c-workspace-id".to_string();
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources.replace_connected_snapshot(
            host.clone(),
            vec![fallback_agent, foreground_agent, cwd_agent],
        );
        select_remote_projection(&mut app, &host);

        let entries = workspace_list_entries(&app);
        let space_labels: Vec<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                WorkspaceListEntry::RemoteSpace { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();

        assert_eq!(space_labels, vec!["project", "logs", "c-workspace-id"]);
    }

    #[test]
    fn all_workspaces_agent_panel_remote_entries_include_non_default_session_and_stale_status() {
        let mut app = crate::app::state::AppState::test_new();
        let host = RemoteHostKey::new("jafar", "agents");
        app.remote_sources.replace_connected_snapshot(
            host.clone(),
            vec![remote_agent("remote-term", "claude", AgentStatus::Done, 1)],
        );
        app.remote_sources.mark_status(
            &host,
            crate::remote_source::RemoteConnectionStatus::NeedsUpdate,
        );

        let entries = all_source_agent_panel_entries(&app);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].primary_label, "jafar/agents agents");
        assert_eq!(entries[0].custom_status.as_deref(), Some("needs update"));
        assert_eq!(
            entries[0].location,
            AgentPanelEntryLocation::RemoteHost {
                host: "jafar".to_string(),
                session: "agents".to_string(),
            }
        );
        assert_eq!(entries[1].primary_label, "claude");
        assert_eq!(entries[1].state, AgentState::Idle);
        assert!(!entries[1].seen);
        assert_eq!(entries[1].custom_status.as_deref(), Some("needs update"));
        assert_eq!(
            entries[1].location,
            AgentPanelEntryLocation::Remote {
                host: "jafar".to_string(),
                session: "agents".to_string(),
                terminal_id: "remote-term".to_string(),
            }
        );
        assert_eq!(
            entries[1].remote_attach_target().unwrap().label,
            "jafar/agents/claude"
        );
    }

    #[test]
    fn remote_agent_panel_entry_shows_attached_status_for_matching_local_attach_pane() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("local");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.terminals.get_mut(&terminal_id).unwrap().remote_attach =
            Some(crate::remote_source::RemoteAttachTarget {
                host: "jafar".into(),
                session: crate::session::DEFAULT_SESSION_NAME.into(),
                terminal_id: "remote-term".into(),
                label: "jafar/smoke-agent".into(),
            });
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources.replace_connected_snapshot(
            host.clone(),
            vec![remote_agent(
                "remote-term",
                "smoke-agent",
                AgentStatus::Working,
                1,
            )],
        );
        select_remote_projection(&mut app, &host);

        let entries = agent_panel_entries(&app);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].primary_label, "smoke-agent");
        assert_eq!(entries[0].custom_status.as_deref(), Some("attached"));
    }

    #[test]
    fn remote_agent_panel_entry_keeps_custom_status_with_attached_marker() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("local");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.terminals.get_mut(&terminal_id).unwrap().remote_attach =
            Some(crate::remote_source::RemoteAttachTarget {
                host: "jafar".into(),
                session: crate::session::DEFAULT_SESSION_NAME.into(),
                terminal_id: "remote-term".into(),
                label: "jafar/smoke-agent".into(),
            });
        let mut agent = remote_agent("remote-term", "smoke-agent", AgentStatus::Working, 1);
        agent.custom_status = Some("busy".into());
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources
            .replace_connected_snapshot(host.clone(), vec![agent]);
        select_remote_projection(&mut app, &host);

        let entries = agent_panel_entries(&app);

        assert_eq!(entries[0].custom_status.as_deref(), Some("busy · attached"));
    }

    #[test]
    fn all_workspaces_agent_panel_shows_remote_host_statuses_without_agents() {
        let mut app = crate::app::state::AppState::test_new();
        app.remote_sources.mark_status(
            &RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME),
            crate::remote_source::RemoteConnectionStatus::Unreachable,
        );
        app.remote_sources.mark_status(
            &RemoteHostKey::new("lab", crate::session::DEFAULT_SESSION_NAME),
            crate::remote_source::RemoteConnectionStatus::Disconnected,
        );
        app.remote_sources.mark_status(
            &RemoteHostKey::new("work", "agents"),
            crate::remote_source::RemoteConnectionStatus::NeedsUpdate,
        );

        let entries = all_source_agent_panel_entries(&app);

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].primary_label, "jafar agents");
        assert_eq!(entries[0].custom_status.as_deref(), Some("unreachable"));
        assert_eq!(
            entries[0].location,
            AgentPanelEntryLocation::RemoteHost {
                host: "jafar".to_string(),
                session: crate::session::DEFAULT_SESSION_NAME.to_string(),
            }
        );
        assert_eq!(entries[1].primary_label, "lab agents");
        assert_eq!(entries[1].custom_status.as_deref(), Some("disconnected"));
        assert_eq!(entries[2].primary_label, "work/agents agents");
        assert_eq!(entries[2].custom_status.as_deref(), Some("needs update"));
    }

    #[test]
    fn remote_space_and_agent_rows_preserve_stale_status_for_cached_agents() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("local");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.terminals.get_mut(&terminal_id).unwrap().remote_attach =
            Some(crate::remote_source::RemoteAttachTarget {
                host: "jafar".into(),
                session: "agents".into(),
                terminal_id: "remote-term".into(),
                label: "jafar/agents/claude".into(),
            });
        let host = RemoteHostKey::new("jafar", "agents");
        app.remote_sources.replace_connected_snapshot(
            host.clone(),
            vec![remote_agent("remote-term", "claude", AgentStatus::Done, 1)],
        );
        app.remote_sources.mark_status(
            &host,
            crate::remote_source::RemoteConnectionStatus::Unreachable,
        );
        select_remote_projection(&mut app, &host);

        let entries = agent_panel_entries(&app);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].primary_label, "claude");
        assert_eq!(entries[0].custom_status.as_deref(), Some("unreachable"));
        assert!(matches!(
            &entries[0].location,
            AgentPanelEntryLocation::Remote { .. }
        ));

        let workspace_entries = workspace_list_entries(&app);
        assert!(workspace_entries.iter().any(|entry| matches!(
            entry,
            WorkspaceListEntry::RemoteSpace {
                key,
                status: crate::remote_source::RemoteConnectionStatus::Unreachable,
                ..
            } if key.host == "jafar" && key.session == "agents" && key.workspace_id == "remote-ws"
        )));
    }

    #[test]
    fn all_workspaces_agent_panel_hides_connected_remote_host_without_agents() {
        let mut app = crate::app::state::AppState::test_new();
        app.remote_sources.replace_connected_snapshot(
            RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME),
            Vec::new(),
        );

        assert!(all_source_agent_panel_entries(&app).is_empty());
    }

    #[test]
    fn remote_space_and_agent_entries_have_expected_targets() {
        let mut app = crate::app::state::AppState::test_new();
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources.replace_connected_snapshot(
            host.clone(),
            vec![remote_agent(
                "remote-term",
                "smoke-agent",
                AgentStatus::Working,
                1,
            )],
        );
        select_remote_projection(&mut app, &host);

        let entries = agent_panel_entries(&app);
        let workspace_entries = workspace_list_entries(&app);

        assert_eq!(entries.len(), 1);
        assert!(matches!(
            workspace_entries.iter().find(|entry| matches!(
                entry,
                WorkspaceListEntry::RemoteSpace { .. }
            )),
            Some(WorkspaceListEntry::RemoteSpace {
                key,
                label,
                status: crate::remote_source::RemoteConnectionStatus::Connected,
                ..
            }) if key.host == "jafar"
                && key.session == crate::session::DEFAULT_SESSION_NAME
                && key.workspace_id == "remote-ws"
                && label == "remote-ws"
        ));
        assert!(entries[0].local_target().is_none());
        assert_eq!(entries[0].custom_status, None);
        assert_eq!(
            entries[0].remote_attach_target().unwrap().key(),
            crate::remote_source::RemoteAgentKey {
                host: "jafar".to_string(),
                session: crate::session::DEFAULT_SESSION_NAME.to_string(),
                terminal_id: "remote-term".to_string(),
            }
        );
        assert!(!app.agent_panel_entry_has_remote_attach_pane(&entries[0]));
    }

    #[test]
    fn remote_agent_panel_entry_detects_matching_local_attach_pane() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("local");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.terminals.get_mut(&terminal_id).unwrap().remote_attach =
            Some(crate::remote_source::RemoteAttachTarget {
                host: "jafar".into(),
                session: crate::session::DEFAULT_SESSION_NAME.into(),
                terminal_id: "remote-term".into(),
                label: "jafar/smoke-agent".into(),
            });
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources.replace_connected_snapshot(
            host.clone(),
            vec![remote_agent(
                "remote-term",
                "smoke-agent",
                AgentStatus::Working,
                1,
            )],
        );
        select_remote_projection(&mut app, &host);

        let entries = agent_panel_entries(&app);

        assert_eq!(entries.len(), 1);
        assert!(app.agent_panel_entry_has_remote_attach_pane(&entries[0]));
    }

    #[test]
    fn source_rail_remote_status_markers_render_in_status_column() {
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
        let area = Rect::new(0, 0, source_rail_width(), 8);
        app.view.source_rail_rect = area;

        let buffer = rendered_source_rail_buffer(&app, area);
        let marker_x = area.x + area.width - 2;

        assert_eq!(buffer[(marker_x, 1)].symbol(), "●");
        assert_eq!(buffer[(marker_x, 1)].style().fg, Some(app.palette.green));
        assert_eq!(buffer[(marker_x, 2)].symbol(), "○");
        assert_eq!(buffer[(marker_x, 2)].style().fg, Some(app.palette.overlay0));
        assert!(buffer[(marker_x, 2)]
            .style()
            .add_modifier
            .contains(Modifier::DIM));
        assert_eq!(buffer[(marker_x, 3)].symbol(), "↑");
        assert_eq!(buffer[(marker_x, 3)].style().fg, Some(app.palette.yellow));
        assert!(buffer[(marker_x, 3)]
            .style()
            .add_modifier
            .contains(Modifier::BOLD));
        assert_eq!(buffer[(marker_x, 4)].symbol(), "×");
        assert_eq!(buffer[(marker_x, 4)].style().fg, Some(app.palette.red));
        assert!(buffer[(marker_x, 4)]
            .style()
            .add_modifier
            .contains(Modifier::BOLD));
    }

    #[test]
    fn source_rail_selected_remote_marker_keeps_surface_background() {
        let mut app = crate::app::state::AppState::test_new();
        let host = RemoteHostKey::new("charlie", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources.mark_status(
            &host,
            crate::remote_source::RemoteConnectionStatus::NeedsUpdate,
        );
        let area = Rect::new(0, 0, source_rail_width(), 4);
        app.view.source_rail_rect = area;
        app.view.sidebar_panel_rect = Rect::new(source_rail_width(), 0, app.sidebar_width, 4);
        app.select_sidebar_source(SidebarSource::Remote(host));

        let buffer = rendered_source_rail_buffer(&app, area);
        let marker_x = area.x + area.width - 2;
        let marker_style = buffer[(marker_x, 1)].style();

        assert_eq!(buffer[(marker_x, 1)].symbol(), "↑");
        assert_eq!(marker_style.fg, Some(app.palette.yellow));
        assert_eq!(marker_style.bg, Some(app.palette.surface0));
        assert!(marker_style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn source_rail_remote_label_truncates_before_marker() {
        let mut app = crate::app::state::AppState::test_new();
        app.remote_sources.mark_status(
            &RemoteHostKey::new("verylongremotehost", crate::session::DEFAULT_SESSION_NAME),
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        let area = Rect::new(0, 0, source_rail_width(), 4);
        app.view.source_rail_rect = area;

        let buffer = rendered_source_rail_buffer(&app, area);
        let row = (0..area.width)
            .map(|x| buffer[(x, 1)].symbol())
            .collect::<String>();

        assert_eq!(row, "verylon…●│");
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

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 20));

        assert!(headers.is_empty());
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

        let (cards, headers) = compute_workspace_list_areas(&app, area);

        assert!(headers.is_empty());
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

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 12));

        assert!(headers.is_empty());
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

    fn render_space_row_buffer(
        app: &AppState,
        key: &crate::remote_source::RemoteSpaceKey,
        label: &str,
        status: crate::remote_source::RemoteConnectionStatus,
        metadata: Option<RemoteSpaceMetadata>,
    ) -> ratatui::buffer::Buffer {
        let width = 40u16;
        let area = Rect::new(0, 0, width, 1);
        let backend = ratatui::backend::TestBackend::new(width, 1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let p = app.palette.clone();
        terminal
            .draw(|frame| {
                render_remote_space_row(app, key, label, status, metadata, frame, area, &p)
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn test_space_key() -> crate::remote_source::RemoteSpaceKey {
        crate::remote_source::RemoteSpaceKey {
            host: "jafar".to_string(),
            session: crate::session::DEFAULT_SESSION_NAME.to_string(),
            workspace_id: "remote-ws".to_string(),
        }
    }

    // ── remote_space_metadata_suffix unit tests ───────────────────────────────

    #[test]
    fn remote_space_metadata_suffix_none_for_one_tab_one_pane() {
        let meta = RemoteSpaceMetadata {
            focused: false,
            agent_status: AgentStatus::Unknown,
            pane_count: 1,
            tab_count: 1,
        };
        assert_eq!(remote_space_metadata_suffix(meta), None);
    }

    #[test]
    fn remote_space_metadata_suffix_none_for_zero_counts() {
        let meta = RemoteSpaceMetadata {
            focused: false,
            agent_status: AgentStatus::Unknown,
            pane_count: 0,
            tab_count: 0,
        };
        assert_eq!(remote_space_metadata_suffix(meta), None);
    }

    #[test]
    fn remote_space_metadata_suffix_two_tabs_only() {
        let meta = RemoteSpaceMetadata {
            focused: false,
            agent_status: AgentStatus::Unknown,
            pane_count: 1,
            tab_count: 2,
        };
        assert_eq!(
            remote_space_metadata_suffix(meta),
            Some("2 tabs".to_string())
        );
    }

    #[test]
    fn remote_space_metadata_suffix_two_panes_only() {
        let meta = RemoteSpaceMetadata {
            focused: false,
            agent_status: AgentStatus::Unknown,
            pane_count: 2,
            tab_count: 1,
        };
        assert_eq!(
            remote_space_metadata_suffix(meta),
            Some("2 panes".to_string())
        );
    }

    #[test]
    fn remote_space_metadata_suffix_tabs_and_panes() {
        let meta = RemoteSpaceMetadata {
            focused: false,
            agent_status: AgentStatus::Unknown,
            pane_count: 3,
            tab_count: 2,
        };
        assert_eq!(
            remote_space_metadata_suffix(meta),
            Some("2 tabs · 3 panes".to_string())
        );
    }

    // ── render_remote_space_row behavior tests ────────────────────────────────

    #[test]
    fn connected_metadata_row_renders_live_dot_and_suffix_for_nontrivial_counts() {
        let app = AppState::test_new();
        let key = test_space_key();
        let meta = RemoteSpaceMetadata {
            focused: false,
            agent_status: AgentStatus::Working,
            pane_count: 1,
            tab_count: 2,
        };
        let buf = render_space_row_buffer(
            &app,
            &key,
            "project",
            crate::remote_source::RemoteConnectionStatus::Connected,
            Some(meta),
        );
        let row: String = (0..40).map(|x| buf[(x, 0)].symbol()).collect();

        // Dot should be live (Working → "●"), not the neutral "·".
        assert_eq!(buf[(1, 0)].symbol(), "●");
        assert_eq!(buf[(1, 0)].style().fg, Some(app.palette.yellow));
        // Suffix "2 tabs" should appear in the rendered text.
        assert!(row.contains("2 tabs"), "expected '2 tabs' in row: {row:?}");
    }

    #[test]
    fn connected_focused_row_gets_active_background_selected_wins() {
        let app = AppState::test_new();
        let key = test_space_key();
        let meta = RemoteSpaceMetadata {
            focused: true,
            agent_status: AgentStatus::Idle,
            pane_count: 1,
            tab_count: 1,
        };

        // Focused but not selected → surface_dim background.
        let buf = render_space_row_buffer(
            &app,
            &key,
            "project",
            crate::remote_source::RemoteConnectionStatus::Connected,
            Some(meta),
        );
        assert_eq!(
            buf[(5, 0)].style().bg,
            Some(app.palette.surface_dim),
            "connected focused row should get surface_dim background"
        );

        // Selected → surface0 background wins over focused.
        let mut app2 = AppState::test_new();
        app2.selected_remote_space = Some(key.clone());
        let buf2 = render_space_row_buffer(
            &app2,
            &key,
            "project",
            crate::remote_source::RemoteConnectionStatus::Connected,
            Some(meta),
        );
        assert_eq!(
            buf2[(5, 0)].style().bg,
            Some(app2.palette.surface0),
            "selected row should get surface0 background, overriding focused"
        );
    }

    #[test]
    fn disconnected_cached_focused_row_renders_dim_not_live() {
        let app = AppState::test_new();
        let key = test_space_key();
        // Cached metadata claims focused=true and Working agent_status, but host is unreachable.
        let meta = RemoteSpaceMetadata {
            focused: true,
            agent_status: AgentStatus::Working,
            pane_count: 1,
            tab_count: 1,
        };
        let buf = render_space_row_buffer(
            &app,
            &key,
            "project",
            crate::remote_source::RemoteConnectionStatus::Unreachable,
            Some(meta),
        );

        // Background must not be focused/active (no surface_dim set by render).
        assert_ne!(
            buf[(5, 0)].style().bg,
            Some(app.palette.surface_dim),
            "disconnected cached-focused row must not get active background"
        );

        // Dot must be neutral "·" with DIM, not the live working "●".
        assert_eq!(
            buf[(1, 0)].symbol(),
            "·",
            "disconnected row must show neutral dot"
        );
        assert!(
            buf[(1, 0)].style().add_modifier.contains(Modifier::DIM),
            "disconnected dot must be dim"
        );

        // Label text must be dim (stale text wins).
        // For Unreachable, row text contains "unreachable".
        let row: String = (0..40).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(
            row.contains("unreachable"),
            "stale text should appear in row: {row:?}"
        );
        // Label style fg is overlay0 + DIM (not text + BOLD).
        let label_x = 3u16;
        assert_eq!(buf[(label_x, 0)].style().fg, Some(app.palette.overlay0));
        assert!(buf[(label_x, 0)]
            .style()
            .add_modifier
            .contains(Modifier::DIM));
    }

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
