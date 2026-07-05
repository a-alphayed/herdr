//! Pure cache state for federated remote agent aggregation.
//!
//! Runtime supervisors, SSH bridges, sockets, UI rendering, and command routing
//! intentionally live elsewhere. This module is rebuildable soft state for
//! remote `AgentInfo` snapshots/events keyed by authoritative host/session.

use std::collections::BTreeMap;

use crate::api::schema::{AgentInfo, LayoutDescription, WorkspaceInfo};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RemoteHostKey {
    pub(crate) host: String,
    pub(crate) session: String,
}

impl RemoteHostKey {
    pub(crate) fn new(host: impl Into<String>, session: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            session: session.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RemoteAgentKey {
    pub(crate) host: String,
    pub(crate) session: String,
    pub(crate) terminal_id: String,
}

impl RemoteAgentKey {
    #[allow(dead_code)] // Staged for supervisor/remove events before runtime sender integration exists.
    pub(crate) fn new(host: &RemoteHostKey, terminal_id: impl Into<String>) -> Self {
        Self {
            host: host.host.clone(),
            session: host.session.clone(),
            terminal_id: terminal_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RemoteSpaceKey {
    pub(crate) host: String,
    pub(crate) session: String,
    pub(crate) workspace_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteAttachTarget {
    pub(crate) host: String,
    pub(crate) session: String,
    pub(crate) terminal_id: String,
    pub(crate) label: String,
}

impl RemoteAttachTarget {
    pub(crate) fn key(&self) -> RemoteAgentKey {
        RemoteAgentKey {
            host: self.host.clone(),
            session: self.session.clone(),
            terminal_id: self.terminal_id.clone(),
        }
    }

    pub(crate) fn same_remote_terminal(&self, other: &Self) -> bool {
        self.host == other.host
            && self.session == other.session
            && self.terminal_id == other.terminal_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteConnectionStatus {
    Connected,
    Disconnected,
    NeedsUpdate,
    Unreachable,
}

impl RemoteConnectionStatus {
    pub(crate) fn is_connected(self) -> bool {
        matches!(self, Self::Connected)
    }

    pub(crate) fn stale_label(self) -> Option<&'static str> {
        match self {
            Self::Connected => None,
            Self::Disconnected => Some("disconnected"),
            Self::NeedsUpdate => Some("needs update"),
            Self::Unreachable => Some("unreachable"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteAgentEntry {
    pub(crate) host: RemoteHostKey,
    pub(crate) agent: AgentInfo,
    pub(crate) status: RemoteConnectionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteWorkspaceEntry {
    pub(crate) host: RemoteHostKey,
    pub(crate) workspace: WorkspaceInfo,
    pub(crate) status: RemoteConnectionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteHostStatusEntry {
    pub(crate) host: RemoteHostKey,
    pub(crate) status: RemoteConnectionStatus,
    pub(crate) agent_count: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RemoteSourceCapabilities {
    pub(crate) workspace_list_local: bool,
    pub(crate) workspace_create: bool,
    pub(crate) tab_list: bool,
    pub(crate) layout_export: bool,
}

/// Projection availability for a single remote workspace's active-tab layout.
///
/// Projections are rebuildable soft state, like the rest of this cache: a fetch
/// failure never disconnects the host or drops agents/workspaces, and a
/// disconnect preserves the last-known layout but marks it stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteProjectionStatus {
    /// A fresh `layout.export` succeeded for the workspace's active tab.
    Available,
    /// The most recent projection fetch failed (or there is no active tab) and
    /// no prior layout is cached.
    Unavailable,
    /// A prior layout is cached but the most recent fetch failed or the host
    /// went non-connected. The cached layout is kept for read-only display.
    StaleLastKnown,
}

/// One remote workspace's projected layout, cached for read-only display.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RemoteProjectionEntry {
    pub(crate) workspace_id: String,
    pub(crate) tab_id: Option<String>,
    pub(crate) tab_label: Option<String>,
    pub(crate) status: RemoteProjectionStatus,
    pub(crate) layout: Option<LayoutDescription>,
}

/// A projection snapshot flowing from a supervisor into the cache reducer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RemoteProjectionSnapshot {
    pub(crate) workspace_id: String,
    pub(crate) tab_id: Option<String>,
    pub(crate) tab_label: Option<String>,
    pub(crate) status: RemoteProjectionStatus,
    pub(crate) layout: Option<LayoutDescription>,
}

impl RemoteAgentEntry {
    pub(crate) fn stale(&self) -> bool {
        !self.status.is_connected()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RemoteSourceCache {
    hosts: BTreeMap<RemoteHostKey, RemoteHostCache>,
}

#[derive(Debug, Clone)]
struct RemoteHostCache {
    status: RemoteConnectionStatus,
    agents: BTreeMap<String, AgentInfo>,
    workspaces: Option<BTreeMap<String, WorkspaceInfo>>,
    capabilities: RemoteSourceCapabilities,
    projections: BTreeMap<String, RemoteProjectionEntry>,
}

impl Default for RemoteHostCache {
    fn default() -> Self {
        Self {
            status: RemoteConnectionStatus::Disconnected,
            agents: BTreeMap::new(),
            workspaces: None,
            capabilities: RemoteSourceCapabilities::default(),
            projections: BTreeMap::new(),
        }
    }
}

impl RemoteHostCache {
    fn entries(&self, host: &RemoteHostKey) -> Vec<RemoteAgentEntry> {
        self.agents
            .values()
            .map(|agent| RemoteAgentEntry {
                host: host.clone(),
                agent: agent.clone(),
                status: self.status,
            })
            .collect()
    }

    fn workspace_entries(&self, host: &RemoteHostKey) -> Option<Vec<RemoteWorkspaceEntry>> {
        Some(
            self.workspaces
                .as_ref()?
                .values()
                .map(|workspace| RemoteWorkspaceEntry {
                    host: host.clone(),
                    workspace: workspace.clone(),
                    status: self.status,
                })
                .collect(),
        )
    }
}

impl RemoteSourceCache {
    pub(crate) fn replace_connected_snapshot(
        &mut self,
        host: RemoteHostKey,
        agents: Vec<AgentInfo>,
    ) {
        let host_cache = self.hosts.entry(host).or_default();
        host_cache.status = RemoteConnectionStatus::Connected;
        host_cache.agents = agents
            .into_iter()
            .map(|agent| (agent.terminal_id.clone(), agent))
            .collect();
    }

    pub(crate) fn replace_workspace_snapshot(
        &mut self,
        host: RemoteHostKey,
        workspaces: Vec<WorkspaceInfo>,
    ) {
        let host_cache = self.hosts.entry(host).or_default();
        host_cache.status = RemoteConnectionStatus::Connected;
        host_cache.workspaces = Some(
            workspaces
                .into_iter()
                .map(|workspace| (workspace.workspace_id.clone(), workspace))
                .collect(),
        );
    }

    pub(crate) fn clear_workspace_snapshot(&mut self, host: &RemoteHostKey) {
        if let Some(host_cache) = self.hosts.get_mut(host) {
            host_cache.workspaces = None;
        }
    }

    pub(crate) fn set_capabilities(
        &mut self,
        host: &RemoteHostKey,
        capabilities: RemoteSourceCapabilities,
    ) {
        self.hosts.entry(host.clone()).or_default().capabilities = capabilities;
    }

    pub(crate) fn host_capabilities(&self, host: &RemoteHostKey) -> RemoteSourceCapabilities {
        self.hosts
            .get(host)
            .map(|host_cache| host_cache.capabilities)
            .unwrap_or_default()
    }

    pub(crate) fn host_status(&self, host: &RemoteHostKey) -> Option<RemoteConnectionStatus> {
        self.hosts.get(host).map(|host_cache| host_cache.status)
    }

    pub(crate) fn host_supports_workspace_create(&self, host: &RemoteHostKey) -> bool {
        self.host_capabilities(host).workspace_create
    }

    pub(crate) fn upsert_workspace(&mut self, host: RemoteHostKey, workspace: WorkspaceInfo) {
        let host_cache = self.hosts.entry(host).or_default();
        let workspaces = host_cache.workspaces.get_or_insert_with(BTreeMap::new);
        workspaces.insert(workspace.workspace_id.clone(), workspace);
    }

    pub(crate) fn mark_status(&mut self, host: &RemoteHostKey, status: RemoteConnectionStatus) {
        let host_cache = self.hosts.entry(host.clone()).or_default();
        host_cache.status = status;
        // A non-connected host keeps its cached projections for read-only display
        // but they are no longer fresh, so available projections become stale.
        if !status.is_connected() {
            for projection in host_cache.projections.values_mut() {
                if projection.status == RemoteProjectionStatus::Available {
                    projection.status = RemoteProjectionStatus::StaleLastKnown;
                }
            }
        }
    }

    /// Replace the cached projections for a host with a fresh supervisor snapshot.
    ///
    /// Per-workspace projection fetch failures never disconnect the host or drop
    /// agents/workspaces: they only turn the affected workspace's projection
    /// unavailable, or stale-last-known when a prior layout is still cached.
    pub(crate) fn apply_projection_snapshot(
        &mut self,
        host: &RemoteHostKey,
        projections: Vec<RemoteProjectionSnapshot>,
    ) {
        let host_cache = self.hosts.entry(host.clone()).or_default();
        let mut next: BTreeMap<String, RemoteProjectionEntry> = BTreeMap::new();
        for snapshot in projections {
            let entry = match snapshot.status {
                RemoteProjectionStatus::Available => RemoteProjectionEntry {
                    workspace_id: snapshot.workspace_id.clone(),
                    tab_id: snapshot.tab_id.clone(),
                    tab_label: snapshot.tab_label.clone(),
                    status: RemoteProjectionStatus::Available,
                    layout: snapshot.layout.clone(),
                },
                RemoteProjectionStatus::Unavailable | RemoteProjectionStatus::StaleLastKnown => {
                    // A fetch failure keeps the last-known layout when present so the
                    // read-only view can still show it (marked stale); otherwise the
                    // projection is simply unavailable for this workspace.
                    let (status, layout) = host_cache
                        .projections
                        .get(&snapshot.workspace_id)
                        .and_then(|existing| existing.layout.clone())
                        .map(|layout| (RemoteProjectionStatus::StaleLastKnown, Some(layout)))
                        .unwrap_or((RemoteProjectionStatus::Unavailable, None));
                    RemoteProjectionEntry {
                        workspace_id: snapshot.workspace_id.clone(),
                        tab_id: snapshot.tab_id.clone(),
                        tab_label: snapshot.tab_label.clone(),
                        status,
                        layout,
                    }
                }
            };
            next.insert(snapshot.workspace_id, entry);
        }
        host_cache.projections = next;
    }

    /// Look up the cached projection for a selected remote space.
    pub(crate) fn projection_for_space(
        &self,
        key: &RemoteSpaceKey,
    ) -> Option<RemoteProjectionEntry> {
        let host = RemoteHostKey::new(key.host.clone(), key.session.clone());
        self.hosts
            .get(&host)
            .and_then(|host_cache| host_cache.projections.get(&key.workspace_id))
            .cloned()
    }

    pub(crate) fn projections_for_host(&self, host: &RemoteHostKey) -> Vec<&RemoteProjectionEntry> {
        self.hosts
            .get(host)
            .map(|host_cache| host_cache.projections.values().collect())
            .unwrap_or_default()
    }

    pub(crate) fn ensure_host(&mut self, host: RemoteHostKey, status: RemoteConnectionStatus) {
        self.hosts.entry(host).or_insert_with(|| RemoteHostCache {
            status,
            agents: BTreeMap::new(),
            workspaces: None,
            capabilities: RemoteSourceCapabilities::default(),
            projections: BTreeMap::new(),
        });
    }

    pub(crate) fn apply_agent_update(&mut self, host: RemoteHostKey, agent: AgentInfo) -> bool {
        let host_cache = self.hosts.entry(host).or_insert_with(|| RemoteHostCache {
            status: RemoteConnectionStatus::Connected,
            agents: BTreeMap::new(),
            workspaces: None,
            capabilities: RemoteSourceCapabilities::default(),
            projections: BTreeMap::new(),
        });
        host_cache.status = RemoteConnectionStatus::Connected;

        match host_cache.agents.get(&agent.terminal_id) {
            Some(existing) if existing.revision >= agent.revision => false,
            _ => {
                host_cache.agents.insert(agent.terminal_id.clone(), agent);
                true
            }
        }
    }

    pub(crate) fn remove_host(&mut self, host: &RemoteHostKey) -> bool {
        self.hosts.remove(host).is_some()
    }

    pub(crate) fn remove_agent(&mut self, key: &RemoteAgentKey) -> bool {
        let host = RemoteHostKey::new(key.host.clone(), key.session.clone());
        let Some(host_cache) = self.hosts.get_mut(&host) else {
            return false;
        };
        host_cache.agents.remove(&key.terminal_id).is_some()
    }

    pub(crate) fn list_entries(&self) -> Vec<RemoteAgentEntry> {
        self.hosts
            .iter()
            .flat_map(|(host, host_cache)| host_cache.entries(host))
            .collect()
    }

    pub(crate) fn list_host_statuses(&self) -> Vec<RemoteHostStatusEntry> {
        self.hosts
            .iter()
            .map(|(host, host_cache)| RemoteHostStatusEntry {
                host: host.clone(),
                status: host_cache.status,
                agent_count: host_cache.agents.len(),
            })
            .collect()
    }

    pub(crate) fn entries_for_host(&self, host: &RemoteHostKey) -> Vec<RemoteAgentEntry> {
        self.hosts
            .get(host)
            .map(|host_cache| host_cache.entries(host))
            .unwrap_or_default()
    }

    pub(crate) fn workspace_entries_for_host(
        &self,
        host: &RemoteHostKey,
    ) -> Option<Vec<RemoteWorkspaceEntry>> {
        self.hosts
            .get(host)
            .and_then(|host_cache| host_cache.workspace_entries(host))
    }

    #[allow(dead_code)] // Staged for target routing/cache lookups in later slices.
    pub(crate) fn agent(&self, key: &RemoteAgentKey) -> Option<RemoteAgentEntry> {
        let host = RemoteHostKey::new(key.host.clone(), key.session.clone());
        let host_cache = self.hosts.get(&host)?;
        let agent = host_cache.agents.get(&key.terminal_id)?;
        Some(RemoteAgentEntry {
            host,
            agent: agent.clone(),
            status: host_cache.status,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::api::schema::{
        AgentInfo, AgentStatus, LayoutDescription, LayoutNode, LayoutPane, WorkspaceInfo,
    };

    use super::*;

    fn agent(terminal_id: &str, label: &str, revision: u64) -> AgentInfo {
        AgentInfo {
            terminal_id: terminal_id.to_string(),
            name: Some(label.to_string()),
            agent: Some(label.to_string()),
            title: None,
            display_agent: Some(label.to_string()),
            agent_status: AgentStatus::Working,
            screen_detection_skipped: false,
            custom_status: None,
            state_labels: HashMap::new(),
            agent_session: None,
            workspace_id: "w1".to_string(),
            tab_id: "t1".to_string(),
            pane_id: format!("pane-{terminal_id}"),
            focused: false,
            cwd: None,
            foreground_cwd: None,
            revision,
        }
    }

    fn workspace(workspace_id: &str, label: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            workspace_id: workspace_id.to_string(),
            number: 1,
            label: label.to_string(),
            focused: false,
            pane_count: 0,
            tab_count: 1,
            active_tab_id: "t1".to_string(),
            agent_status: AgentStatus::Unknown,
            worktree: None,
        }
    }

    #[test]
    fn remote_source_snapshot_inserts_agents_with_host_session_metadata() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");

        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);

        let entries = cache.list_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].host, host);
        assert_eq!(entries[0].agent.terminal_id, "term-1");
        assert_eq!(entries[0].agent.display_agent.as_deref(), Some("codex"));
        assert_eq!(entries[0].status, RemoteConnectionStatus::Connected);
        assert!(!entries[0].stale());
    }

    #[test]
    fn remote_source_connected_snapshot_removes_missing_agents_for_host() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        cache.replace_connected_snapshot(
            host.clone(),
            vec![agent("term-1", "codex", 1), agent("term-2", "claude", 1)],
        );

        cache.replace_connected_snapshot(host, vec![agent("term-2", "claude", 2)]);

        let terminal_ids: Vec<_> = cache
            .list_entries()
            .into_iter()
            .map(|entry| entry.agent.terminal_id)
            .collect();
        assert_eq!(terminal_ids, vec!["term-2"]);
    }

    #[test]
    fn remote_source_workspace_snapshot_stores_authoritative_metadata() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");

        cache.replace_workspace_snapshot(
            host.clone(),
            vec![workspace("ws-b", "blank shell"), workspace("ws-a", "tmp")],
        );

        let entries = cache.workspace_entries_for_host(&host).expect("snapshot");
        let labels: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.workspace.workspace_id.as_str(),
                    entry.workspace.label.as_str(),
                    entry.status,
                )
            })
            .collect();
        assert_eq!(
            labels,
            vec![
                ("ws-a", "tmp", RemoteConnectionStatus::Connected),
                ("ws-b", "blank shell", RemoteConnectionStatus::Connected),
            ]
        );
    }

    #[test]
    fn remote_source_capabilities_are_host_scoped() {
        let mut cache = RemoteSourceCache::default();
        let capable = RemoteHostKey::new("jafar", "default");
        let other = RemoteHostKey::new("work", "default");

        assert_eq!(
            cache.host_capabilities(&capable),
            RemoteSourceCapabilities::default()
        );
        cache.set_capabilities(
            &capable,
            RemoteSourceCapabilities {
                workspace_list_local: true,
                workspace_create: true,
                tab_list: true,
                layout_export: true,
            },
        );

        assert!(cache.host_supports_workspace_create(&capable));
        assert_eq!(
            cache.host_capabilities(&capable),
            RemoteSourceCapabilities {
                workspace_list_local: true,
                workspace_create: true,
                tab_list: true,
                layout_export: true,
            }
        );
        assert!(!cache.host_supports_workspace_create(&other));
    }

    #[test]
    fn remote_source_upsert_workspace_updates_metadata_snapshot() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");

        cache.replace_workspace_snapshot(host.clone(), vec![workspace("ws-a", "old")]);
        cache.upsert_workspace(host.clone(), workspace("ws-b", "blank shell"));
        cache.upsert_workspace(host.clone(), workspace("ws-a", "tmp"));

        let labels: Vec<_> = cache
            .workspace_entries_for_host(&host)
            .expect("snapshot")
            .into_iter()
            .map(|entry| (entry.workspace.workspace_id, entry.workspace.label))
            .collect();
        assert_eq!(
            labels,
            vec![
                ("ws-a".to_string(), "tmp".to_string()),
                ("ws-b".to_string(), "blank shell".to_string())
            ]
        );
    }

    #[test]
    fn remote_source_workspace_snapshot_distinguishes_empty_from_missing() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");

        assert!(cache.workspace_entries_for_host(&host).is_none());
        cache.replace_workspace_snapshot(host.clone(), Vec::new());

        assert_eq!(
            cache.workspace_entries_for_host(&host),
            Some(Vec::<RemoteWorkspaceEntry>::new())
        );
    }

    #[test]
    fn remote_source_agent_snapshot_preserves_workspace_snapshot() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        cache.replace_workspace_snapshot(host.clone(), vec![workspace("ws-a", "tmp")]);

        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);

        assert_eq!(cache.list_entries().len(), 1);
        let workspaces = cache.workspace_entries_for_host(&host).expect("snapshot");
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].workspace.label, "tmp");
    }

    #[test]
    fn remote_source_workspace_snapshot_preserves_agents() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);

        cache.replace_workspace_snapshot(host, vec![workspace("ws-a", "tmp")]);

        let entries = cache.list_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agent.terminal_id, "term-1");
    }

    #[test]
    fn remote_source_clear_workspace_snapshot_restores_missing_snapshot_state() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);
        cache.replace_workspace_snapshot(host.clone(), vec![workspace("ws-a", "tmp")]);

        cache.clear_workspace_snapshot(&host);

        assert!(cache.workspace_entries_for_host(&host).is_none());
        let entries = cache.list_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agent.terminal_id, "term-1");
    }

    #[test]
    fn remote_source_ensure_host_adds_empty_status_without_clobbering_existing() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");

        cache.ensure_host(host.clone(), RemoteConnectionStatus::Disconnected);
        assert_eq!(cache.list_entries().len(), 0);
        let statuses = cache.list_host_statuses();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].host, host);
        assert_eq!(statuses[0].status, RemoteConnectionStatus::Disconnected);
        assert_eq!(statuses[0].agent_count, 0);

        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);
        cache.ensure_host(host, RemoteConnectionStatus::Disconnected);
        let statuses = cache.list_host_statuses();
        assert_eq!(statuses[0].status, RemoteConnectionStatus::Connected);
        assert_eq!(statuses[0].agent_count, 1);
    }

    #[test]
    fn remote_source_disconnect_marks_entries_stale_but_keeps_them() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);
        cache.replace_workspace_snapshot(host.clone(), vec![workspace("ws-a", "tmp")]);

        cache.mark_status(&host, RemoteConnectionStatus::Disconnected);

        let entries = cache.list_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, RemoteConnectionStatus::Disconnected);
        assert_eq!(entries[0].status.stale_label(), Some("disconnected"));
        assert!(entries[0].stale());
        let workspace_entries = cache.workspace_entries_for_host(&host).expect("snapshot");
        assert_eq!(
            workspace_entries[0].status,
            RemoteConnectionStatus::Disconnected
        );
    }

    #[test]
    fn remote_source_specific_failure_status_marks_entries_stale_but_keeps_them() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);

        cache.mark_status(&host, RemoteConnectionStatus::NeedsUpdate);
        let entries = cache.list_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, RemoteConnectionStatus::NeedsUpdate);
        assert_eq!(entries[0].status.stale_label(), Some("needs update"));
        assert!(entries[0].stale());

        cache.mark_status(&host, RemoteConnectionStatus::Unreachable);
        let entries = cache.list_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, RemoteConnectionStatus::Unreachable);
        assert_eq!(entries[0].status.stale_label(), Some("unreachable"));
        assert!(entries[0].stale());
    }

    #[test]
    fn remote_source_status_without_agents_keeps_host_visible() {
        let mut cache = RemoteSourceCache::default();
        let jafar = RemoteHostKey::new("jafar", "default");
        let work = RemoteHostKey::new("work", "agents");

        cache.mark_status(&jafar, RemoteConnectionStatus::Unreachable);
        cache.mark_status(&work, RemoteConnectionStatus::NeedsUpdate);

        assert!(cache.list_entries().is_empty());
        let hosts = cache.list_host_statuses();
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].host, jafar);
        assert_eq!(hosts[0].status, RemoteConnectionStatus::Unreachable);
        assert_eq!(hosts[0].agent_count, 0);
        assert_eq!(hosts[1].host, work);
        assert_eq!(hosts[1].status, RemoteConnectionStatus::NeedsUpdate);
        assert_eq!(hosts[1].agent_count, 0);
    }

    #[test]
    fn remote_source_reconnect_snapshot_clears_stale_and_updates_entries() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);
        cache.mark_status(&host, RemoteConnectionStatus::Disconnected);

        cache.replace_connected_snapshot(host, vec![agent("term-1", "codex-new", 3)]);

        let entries = cache.list_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, RemoteConnectionStatus::Connected);
        assert!(!entries[0].stale());
        assert_eq!(entries[0].agent.revision, 3);
        assert_eq!(entries[0].agent.display_agent.as_deref(), Some("codex-new"));
    }

    #[test]
    fn remote_source_update_ignores_same_or_older_revision_and_applies_newer() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 10)]);

        assert!(!cache.apply_agent_update(host.clone(), agent("term-1", "old", 9)));
        assert!(!cache.apply_agent_update(host.clone(), agent("term-1", "same", 10)));
        let entry = cache
            .agent(&RemoteAgentKey::new(&host, "term-1"))
            .expect("cached agent");
        assert_eq!(entry.agent.display_agent.as_deref(), Some("codex"));
        assert_eq!(entry.agent.revision, 10);

        assert!(cache.apply_agent_update(host.clone(), agent("term-1", "new", 11)));
        let entry = cache
            .agent(&RemoteAgentKey::new(&host, "term-1"))
            .expect("cached agent");
        assert_eq!(entry.agent.display_agent.as_deref(), Some("new"));
        assert_eq!(entry.agent.revision, 11);
    }

    #[test]
    fn remote_source_same_terminal_id_on_different_host_sessions_does_not_collide() {
        let mut cache = RemoteSourceCache::default();
        let jafar_default = RemoteHostKey::new("jafar", "default");
        let jafar_agents = RemoteHostKey::new("jafar", "agents");
        let work_default = RemoteHostKey::new("work", "default");

        cache.replace_connected_snapshot(jafar_default.clone(), vec![agent("term-1", "codex", 1)]);
        cache.replace_connected_snapshot(jafar_agents.clone(), vec![agent("term-1", "claude", 1)]);
        cache.replace_connected_snapshot(work_default.clone(), vec![agent("term-1", "pi", 1)]);

        assert_eq!(
            cache
                .agent(&RemoteAgentKey::new(&jafar_default, "term-1"))
                .expect("jafar default")
                .agent
                .display_agent
                .as_deref(),
            Some("codex")
        );
        assert_eq!(
            cache
                .agent(&RemoteAgentKey::new(&jafar_agents, "term-1"))
                .expect("jafar agents")
                .agent
                .display_agent
                .as_deref(),
            Some("claude")
        );
        assert_eq!(
            cache
                .agent(&RemoteAgentKey::new(&work_default, "term-1"))
                .expect("work default")
                .agent
                .display_agent
                .as_deref(),
            Some("pi")
        );
    }

    #[test]
    fn remote_source_lists_entries_in_host_session_terminal_order() {
        let mut cache = RemoteSourceCache::default();
        cache.replace_connected_snapshot(
            RemoteHostKey::new("work", "default"),
            vec![agent("term-2", "pi", 1), agent("term-1", "claude", 1)],
        );
        cache.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "agents"),
            vec![agent("term-3", "codex", 1)],
        );
        cache.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![agent("term-1", "gemini", 1)],
        );

        let order: Vec<_> = cache
            .list_entries()
            .into_iter()
            .map(|entry| (entry.host.host, entry.host.session, entry.agent.terminal_id))
            .collect();

        assert_eq!(
            order,
            vec![
                (
                    "jafar".to_string(),
                    "agents".to_string(),
                    "term-3".to_string()
                ),
                (
                    "jafar".to_string(),
                    "default".to_string(),
                    "term-1".to_string()
                ),
                (
                    "work".to_string(),
                    "default".to_string(),
                    "term-1".to_string()
                ),
                (
                    "work".to_string(),
                    "default".to_string(),
                    "term-2".to_string()
                ),
            ]
        );
    }

    #[test]
    fn remote_source_remove_agent_deletes_only_that_host_session_terminal() {
        let mut cache = RemoteSourceCache::default();
        let keep = RemoteHostKey::new("jafar", "default");
        let remove = RemoteHostKey::new("jafar", "agents");
        cache.replace_connected_snapshot(keep.clone(), vec![agent("term-1", "codex", 1)]);
        cache.replace_connected_snapshot(remove.clone(), vec![agent("term-1", "claude", 1)]);

        assert!(cache.remove_agent(&RemoteAgentKey::new(&remove, "term-1")));
        assert!(!cache.remove_agent(&RemoteAgentKey::new(&remove, "term-1")));

        let entries = cache.list_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].host, keep);
        assert_eq!(entries[0].agent.terminal_id, "term-1");
    }

    #[test]
    fn remote_source_remove_host_deletes_only_that_host_session() {
        let mut cache = RemoteSourceCache::default();
        let keep = RemoteHostKey::new("jafar", "default");
        let remove = RemoteHostKey::new("jafar", "agents");
        cache.replace_connected_snapshot(keep.clone(), vec![agent("term-1", "codex", 1)]);
        cache.replace_connected_snapshot(remove.clone(), vec![agent("term-2", "claude", 1)]);
        cache.replace_workspace_snapshot(keep.clone(), vec![workspace("ws-keep", "keep")]);
        cache.replace_workspace_snapshot(remove.clone(), vec![workspace("ws-remove", "remove")]);

        assert!(cache.remove_host(&remove));
        assert!(!cache.remove_host(&remove));

        let entries = cache.list_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].host, keep);
        assert_eq!(entries[0].agent.terminal_id, "term-1");
        let workspace_entries = cache
            .workspace_entries_for_host(&keep)
            .expect("keep workspace snapshot");
        assert_eq!(workspace_entries.len(), 1);
        assert_eq!(workspace_entries[0].workspace.workspace_id, "ws-keep");
        assert!(cache.workspace_entries_for_host(&remove).is_none());
    }

    fn layout_for(tab_id: &str) -> LayoutDescription {
        LayoutDescription {
            workspace_id: "w1".to_string(),
            tab_id: tab_id.to_string(),
            zoomed: false,
            focused_pane_id: format!("{tab_id}-1"),
            root: LayoutNode::Pane {
                pane: LayoutPane {
                    label: Some("shell".to_string()),
                    ..Default::default()
                },
            },
        }
    }

    #[test]
    fn remote_source_apply_projection_snapshot_caches_available_layout() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");

        cache.apply_projection_snapshot(
            &host,
            vec![RemoteProjectionSnapshot {
                workspace_id: "ws-1".to_string(),
                tab_id: Some("w1:1".to_string()),
                tab_label: Some("dev".to_string()),
                status: RemoteProjectionStatus::Available,
                layout: Some(layout_for("w1:1")),
            }],
        );

        let entry = cache
            .projection_for_space(&RemoteSpaceKey {
                host: "jafar".to_string(),
                session: "default".to_string(),
                workspace_id: "ws-1".to_string(),
            })
            .expect("projection cached");
        assert_eq!(entry.status, RemoteProjectionStatus::Available);
        assert_eq!(entry.workspace_id, "ws-1");
        assert_eq!(entry.tab_label.as_deref(), Some("dev"));
        assert_eq!(entry.layout.as_ref().unwrap().tab_id, "w1:1");
    }

    #[test]
    fn remote_source_projections_for_host_returns_all_cached_projections_for_host_session() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        let other = RemoteHostKey::new("jafar", "agents");

        cache.apply_projection_snapshot(
            &host,
            vec![
                RemoteProjectionSnapshot {
                    workspace_id: "ws-b".to_string(),
                    tab_id: Some("w2:1".to_string()),
                    tab_label: None,
                    status: RemoteProjectionStatus::Available,
                    layout: Some(layout_for("w2:1")),
                },
                RemoteProjectionSnapshot {
                    workspace_id: "ws-a".to_string(),
                    tab_id: Some("w1:1".to_string()),
                    tab_label: None,
                    status: RemoteProjectionStatus::Available,
                    layout: Some(layout_for("w1:1")),
                },
            ],
        );
        cache.apply_projection_snapshot(
            &other,
            vec![RemoteProjectionSnapshot {
                workspace_id: "ws-other".to_string(),
                tab_id: Some("other:1".to_string()),
                tab_label: None,
                status: RemoteProjectionStatus::Available,
                layout: Some(layout_for("other:1")),
            }],
        );

        let workspace_ids: Vec<_> = cache
            .projections_for_host(&host)
            .into_iter()
            .map(|entry| entry.workspace_id.as_str())
            .collect();

        assert_eq!(workspace_ids, vec!["ws-a", "ws-b"]);
    }

    #[test]
    fn remote_source_apply_projection_snapshot_keeps_last_known_layout_as_stale_on_failure() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");

        // First snapshot delivers an available projection for ws-1 and a fresh
        // unavailable one for ws-2 (no prior layout).
        cache.apply_projection_snapshot(
            &host,
            vec![
                RemoteProjectionSnapshot {
                    workspace_id: "ws-1".to_string(),
                    tab_id: Some("w1:1".to_string()),
                    tab_label: None,
                    status: RemoteProjectionStatus::Available,
                    layout: Some(layout_for("w1:1")),
                },
                RemoteProjectionSnapshot {
                    workspace_id: "ws-2".to_string(),
                    tab_id: Some("w2:1".to_string()),
                    tab_label: None,
                    status: RemoteProjectionStatus::Unavailable,
                    layout: None,
                },
            ],
        );

        // Second snapshot reports both as unavailable (fetch failures). ws-1 must
        // keep its last-known layout but become stale; ws-2 has no prior layout
        // and stays unavailable.
        cache.apply_projection_snapshot(
            &host,
            vec![
                RemoteProjectionSnapshot {
                    workspace_id: "ws-1".to_string(),
                    tab_id: Some("w1:1".to_string()),
                    tab_label: None,
                    status: RemoteProjectionStatus::Unavailable,
                    layout: None,
                },
                RemoteProjectionSnapshot {
                    workspace_id: "ws-2".to_string(),
                    tab_id: Some("w2:1".to_string()),
                    tab_label: None,
                    status: RemoteProjectionStatus::Unavailable,
                    layout: None,
                },
            ],
        );

        let ws1 = cache
            .projection_for_space(&RemoteSpaceKey {
                host: "jafar".to_string(),
                session: "default".to_string(),
                workspace_id: "ws-1".to_string(),
            })
            .expect("ws-1 projection");
        assert_eq!(ws1.status, RemoteProjectionStatus::StaleLastKnown);
        assert_eq!(ws1.layout.as_ref().unwrap().tab_id, "w1:1");

        let ws2 = cache
            .projection_for_space(&RemoteSpaceKey {
                host: "jafar".to_string(),
                session: "default".to_string(),
                workspace_id: "ws-2".to_string(),
            })
            .expect("ws-2 projection");
        assert_eq!(ws2.status, RemoteProjectionStatus::Unavailable);
        assert!(ws2.layout.is_none());
    }

    #[test]
    fn remote_source_disconnect_marks_available_projection_stale_and_preserves_layout() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        cache.apply_projection_snapshot(
            &host,
            vec![RemoteProjectionSnapshot {
                workspace_id: "ws-1".to_string(),
                tab_id: Some("w1:1".to_string()),
                tab_label: None,
                status: RemoteProjectionStatus::Available,
                layout: Some(layout_for("w1:1")),
            }],
        );

        // Disconnect must preserve agents/workspaces/projections; it only marks
        // the available projection stale.
        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);
        cache.mark_status(&host, RemoteConnectionStatus::Disconnected);

        let entry = cache
            .projection_for_space(&RemoteSpaceKey {
                host: "jafar".to_string(),
                session: "default".to_string(),
                workspace_id: "ws-1".to_string(),
            })
            .expect("projection kept");
        assert_eq!(entry.status, RemoteProjectionStatus::StaleLastKnown);
        assert_eq!(entry.layout.as_ref().unwrap().tab_id, "w1:1");
        assert_eq!(cache.list_entries().len(), 1);
    }
}
