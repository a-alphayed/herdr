//! Pure cache state for federated remote agent aggregation.
//!
//! Runtime supervisors, SSH bridges, sockets, UI rendering, and command routing
//! intentionally live elsewhere. This module is rebuildable soft state for
//! remote `AgentInfo` snapshots/events keyed by authoritative host/session.

use std::collections::BTreeMap;

use crate::api::schema::{AgentInfo, WorkspaceInfo};

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

    /// Whether this host is available for automatic scheduler/orchestrator
    /// consideration: the cached remote-source status must be `Connected`.
    ///
    /// Only [`RemoteConnectionStatus::Connected`] qualifies. `Disconnected`,
    /// `NeedsUpdate`, and `Unreachable` all return `false`, so sleeping or
    /// roaming hosts are never included in automatic scheduling decisions
    /// even if they are `connection_policy = "auto"`.
    ///
    /// This predicate is for automatic-eligibility and safe prepared-state reuse
    /// only. It is **not** a replacement for `remote_agent_start_host_precheck`,
    /// which intentionally treats a missing cache entry as OK so explicit
    /// `on_demand` no-cache dispatch can proceed.
    pub(crate) fn available_for_automatic_orchestration(self) -> bool {
        matches!(self, Self::Connected)
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
    pub(crate) workspace_rename: bool,
    pub(crate) tab_list: bool,
    pub(crate) tab_create: bool,
    pub(crate) tab_focus: bool,
    pub(crate) tab_close: bool,
    pub(crate) tab_rename: bool,
    pub(crate) pane_split: bool,
    pub(crate) pane_close: bool,
    pub(crate) pane_rename: bool,
    pub(crate) pane_focus: bool,
    pub(crate) pane_focus_direction: bool,
    pub(crate) layout_export: bool,
    /// Optional additive capability gating in-place terminal session
    /// terminal-session streams (`ObserveTerminal` / `ControlTerminal` over the
    /// existing render bridge). Independent of `terminal_attach`: a remote
    /// advertising `terminal_attach` but not this method still fails closed
    /// for terminal streaming. Never a supervisor-ping prerequisite.
    pub(crate) terminal_session_stream: bool,
}

impl RemoteSourceCapabilities {
    /// Build cached route-relevant capabilities from advertised federation
    /// capabilities.
    ///
    /// Centralizes the advertised -> cached mapping so the cached booleans stay
    /// in lockstep with the [`crate::api::schema::FederationCapabilities`] method
    /// constants. Required supervisor-ping methods (`remote_api_bridge`,
    /// `agent_list_local`) are intentionally not cached here: their absence is a
    /// ping-level failure handled by the supervisor, not a route-level gate. A
    /// remote advertising only the required ping methods still connects, with
    /// every optional cached boolean `false`.
    pub(crate) fn from_federation(federation: &crate::api::schema::FederationCapabilities) -> Self {
        use crate::api::schema::FederationCapabilities as F;
        Self {
            workspace_list_local: federation.supports_method(F::WORKSPACE_LIST_LOCAL),
            workspace_create: federation.supports_method(F::WORKSPACE_CREATE),
            workspace_rename: federation.supports_method(F::WORKSPACE_RENAME),
            tab_list: federation.supports_method(F::TAB_LIST),
            tab_create: federation.supports_method(F::TAB_CREATE),
            tab_focus: federation.supports_method(F::TAB_FOCUS),
            tab_close: federation.supports_method(F::TAB_CLOSE),
            tab_rename: federation.supports_method(F::TAB_RENAME),
            pane_split: federation.supports_method(F::PANE_SPLIT),
            pane_close: federation.supports_method(F::PANE_CLOSE),
            pane_rename: federation.supports_method(F::PANE_RENAME),
            pane_focus: federation.supports_method(F::PANE_FOCUS),
            pane_focus_direction: federation.supports_method(F::PANE_FOCUS_DIRECTION),
            layout_export: federation.supports_method(F::LAYOUT_EXPORT),
            terminal_session_stream: federation.supports_method(F::TERMINAL_SESSION_STREAM),
        }
    }

    /// Cache-side bridge from a federation method constant to the cached boolean
    /// a route-level missing-capability gate checks.
    ///
    /// This is distinct from
    /// [`crate::api::schema::FederationCapabilities::supports_method`], which
    /// tests the *advertised* method set. `supports_route_method` tests the
    /// *cached* booleans (sourced from a successful supervisor ping), so a
    /// disconnected/stale host correctly reports `false` for every route method
    /// even if its last-advertised capabilities listed it. Route gates must read
    /// this off capabilities obtained via
    /// [`RemoteSourceCache::host_capabilities`], never off raw advertised
    /// `FederationCapabilities`, so the disconnect/stale lifecycle stays
    /// authoritative.
    ///
    /// Exhaustive over the cached fields relevant to remote control routes
    /// (workspace create/list_local/rename, tab list/create/focus/close/rename,
    /// pane split/close/rename/focus/focus_direction, layout export). Returns
    /// `false` for required ping methods (`remote_api_bridge`,
    /// `agent_list_local`), the persistent bridge/terminal-attach methods, or any
    /// unrecognized method constant.
    pub(crate) fn supports_route_method(&self, method: &str) -> bool {
        use crate::api::schema::FederationCapabilities as F;
        match method {
            F::WORKSPACE_LIST_LOCAL => self.workspace_list_local,
            F::WORKSPACE_CREATE => self.workspace_create,
            F::WORKSPACE_RENAME => self.workspace_rename,
            F::TAB_LIST => self.tab_list,
            F::TAB_CREATE => self.tab_create,
            F::TAB_FOCUS => self.tab_focus,
            F::TAB_CLOSE => self.tab_close,
            F::TAB_RENAME => self.tab_rename,
            F::PANE_SPLIT => self.pane_split,
            F::PANE_CLOSE => self.pane_close,
            F::PANE_RENAME => self.pane_rename,
            F::PANE_FOCUS => self.pane_focus,
            F::PANE_FOCUS_DIRECTION => self.pane_focus_direction,
            F::LAYOUT_EXPORT => self.layout_export,
            F::TERMINAL_SESSION_STREAM => self.terminal_session_stream,
            _ => false,
        }
    }
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
    /// Supervisor-prepared bridge state (prepared shell path + advertised
    /// federation capabilities) for reuse by routed agent dispatch while the
    /// host stays `Connected`. Rebuildable soft state: dropped when the host
    /// becomes non-connected ([`Self::mark_status`]).
    bridge_state: Option<crate::remote::RemoteApiBridgeState>,
}

impl Default for RemoteHostCache {
    fn default() -> Self {
        Self {
            status: RemoteConnectionStatus::Disconnected,
            agents: BTreeMap::new(),
            workspaces: None,
            capabilities: RemoteSourceCapabilities::default(),
            bridge_state: None,
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

    pub(crate) fn upsert_workspace(&mut self, host: RemoteHostKey, workspace: WorkspaceInfo) {
        let host_cache = self.hosts.entry(host).or_default();
        let workspaces = host_cache.workspaces.get_or_insert_with(BTreeMap::new);
        workspaces.insert(workspace.workspace_id.clone(), workspace);
    }

    pub(crate) fn mark_status(&mut self, host: &RemoteHostKey, status: RemoteConnectionStatus) {
        let host_cache = self.hosts.entry(host.clone()).or_default();
        host_cache.status = status;
        if !status.is_connected() {
            // Prepared bridge state is safety-relevant for mutating dispatch:
            // a stale prepared binary/capabilities must never be reused to skip
            // probes after a disconnect/incompatibility. Drop it while keeping
            // display caches (agents/workspaces) stale as today.
            host_cache.bridge_state = None;
            // Phase G.10: also retire idle persistent bridges for this host so
            // they are not reused after a disconnect. Mark-only and cheap; the
            // actual child reap happens lazily on the next checkout/return, so
            // this never stalls the reducer loop on process cleanup.
            crate::remote::invalidate_remote_bridge_pool_host(host);
        }
    }

    /// Mark `host` `Connected` and store supervisor-prepared bridge state
    /// captured from a successful supervisor ping.
    ///
    /// C3 alignment: a successful ping proves the host is reachable, so the
    /// cached status is flipped to `Connected` (a snapshot may not have arrived
    /// yet, in which case the host is connected with no agents) and the prepared
    /// state is stored so routed agent dispatch can reuse it. This is the
    /// reducer-side handler for [`AppEvent::RemoteSourceBridgeState`].
    /// `Connected` here means the prepared state stays; a later non-connected
    /// [`Self::mark_status`] still invalidates it.
    ///
    /// [`AppEvent::RemoteSourceBridgeState`]: crate::events::AppEvent::RemoteSourceBridgeState
    pub(crate) fn set_connected_bridge_state(
        &mut self,
        host: &RemoteHostKey,
        bridge_state: crate::remote::RemoteApiBridgeState,
    ) {
        let host_cache = self.hosts.entry(host.clone()).or_default();
        host_cache.status = RemoteConnectionStatus::Connected;
        host_cache.bridge_state = Some(bridge_state);
    }

    /// Prepared bridge state for routed agent dispatch, available only while the
    /// host is `Connected` and has cached state. Returns `None` for stale /
    /// non-connected hosts (so dispatch falls back to the full non-interactive
    /// bridge path) and for hosts with no cached prepared state.
    pub(crate) fn connected_bridge_state(
        &self,
        host: &RemoteHostKey,
    ) -> Option<crate::remote::RemoteApiBridgeState> {
        let host_cache = self.hosts.get(host)?;
        if host_cache.status.available_for_automatic_orchestration() {
            host_cache.bridge_state.clone()
        } else {
            None
        }
    }

    pub(crate) fn ensure_host(&mut self, host: RemoteHostKey, status: RemoteConnectionStatus) {
        self.hosts.entry(host).or_insert_with(|| RemoteHostCache {
            status,
            agents: BTreeMap::new(),
            workspaces: None,
            capabilities: RemoteSourceCapabilities::default(),
            bridge_state: None,
        });
    }

    pub(crate) fn apply_agent_update(&mut self, host: RemoteHostKey, agent: AgentInfo) -> bool {
        let host_cache = self.hosts.entry(host).or_insert_with(|| RemoteHostCache {
            status: RemoteConnectionStatus::Connected,
            agents: BTreeMap::new(),
            workspaces: None,
            capabilities: RemoteSourceCapabilities::default(),
            bridge_state: None,
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

    use crate::api::schema::{AgentInfo, AgentStatus, WorkspaceInfo};

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
                tab_create: true,
                tab_focus: true,
                tab_close: true,
                layout_export: true,
                ..Default::default()
            },
        );

        assert_eq!(
            cache.host_capabilities(&capable),
            RemoteSourceCapabilities {
                workspace_list_local: true,
                workspace_create: true,
                tab_list: true,
                tab_create: true,
                tab_focus: true,
                tab_close: true,
                layout_export: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn remote_source_capabilities_from_federation_maps_advertised_methods_to_cached_booleans() {
        use crate::api::schema::FederationCapabilities;

        // Advertising the full current federation method set caches every
        // route-relevant boolean true.
        let full = RemoteSourceCapabilities::from_federation(&FederationCapabilities::current());
        assert!(full.workspace_list_local);
        assert!(full.workspace_create);
        assert!(full.workspace_rename);
        assert!(full.tab_list);
        assert!(full.tab_create);
        assert!(full.tab_focus);
        assert!(full.tab_close);
        assert!(full.tab_rename);
        assert!(full.pane_split);
        assert!(full.pane_close);
        assert!(full.pane_rename);
        assert!(full.pane_focus);
        assert!(full.pane_focus_direction);
        assert!(full.layout_export);
        assert!(full.terminal_session_stream);

        // Advertising none of the route-relevant methods caches all booleans
        // false, matching the default (a remote that advertises only the
        // required ping methods still connects with empty cached capabilities).
        let empty = RemoteSourceCapabilities::from_federation(&FederationCapabilities {
            methods: Vec::new(),
        });
        assert_eq!(empty, RemoteSourceCapabilities::default());

        // Advertising a subset caches exactly that subset.
        let partial = RemoteSourceCapabilities::from_federation(&FederationCapabilities {
            methods: [
                FederationCapabilities::TAB_LIST,
                FederationCapabilities::PANE_SPLIT,
                FederationCapabilities::REMOTE_API_BRIDGE,
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        });
        assert!(partial.tab_list);
        assert!(partial.pane_split);
        assert!(!partial.tab_create);
        assert!(!partial.layout_export);
        assert!(!partial.terminal_session_stream);
        // Required ping methods are not route-relevant cached capabilities, so
        // advertising `remote_api_bridge` alone does not flip any cached boolean.
        assert!(!partial.workspace_list_local);
    }

    #[test]
    fn remote_source_supports_route_method_maps_known_constants_and_returns_false_for_unknown() {
        use crate::api::schema::FederationCapabilities as F;

        // A mixed cached capability set: odd-indexed fields true, even false, so
        // every constant maps to a distinct expected boolean.
        let capabilities = RemoteSourceCapabilities {
            workspace_list_local: true,
            workspace_create: false,
            workspace_rename: true,
            tab_list: false,
            tab_create: true,
            tab_focus: false,
            tab_close: true,
            tab_rename: false,
            pane_split: true,
            pane_close: false,
            pane_rename: true,
            pane_focus: false,
            pane_focus_direction: true,
            layout_export: false,
            terminal_session_stream: true,
        };

        // Known route-method constants map to their cached booleans.
        assert!(capabilities.supports_route_method(F::WORKSPACE_LIST_LOCAL));
        assert!(!capabilities.supports_route_method(F::WORKSPACE_CREATE));
        assert!(capabilities.supports_route_method(F::WORKSPACE_RENAME));
        assert!(!capabilities.supports_route_method(F::TAB_LIST));
        assert!(capabilities.supports_route_method(F::TAB_CREATE));
        assert!(!capabilities.supports_route_method(F::TAB_FOCUS));
        assert!(capabilities.supports_route_method(F::TAB_CLOSE));
        assert!(!capabilities.supports_route_method(F::TAB_RENAME));
        assert!(capabilities.supports_route_method(F::PANE_SPLIT));
        assert!(!capabilities.supports_route_method(F::PANE_CLOSE));
        assert!(capabilities.supports_route_method(F::PANE_RENAME));
        assert!(!capabilities.supports_route_method(F::PANE_FOCUS));
        assert!(capabilities.supports_route_method(F::PANE_FOCUS_DIRECTION));
        assert!(!capabilities.supports_route_method(F::LAYOUT_EXPORT));
        assert!(capabilities.supports_route_method(F::TERMINAL_SESSION_STREAM));
        // Required ping methods, persistent bridge, and terminal attach are not
        // route-relevant cached capabilities, so they always return false even
        // when the host is otherwise fully capable.
        assert!(!capabilities.supports_route_method(F::REMOTE_API_BRIDGE));
        assert!(!capabilities.supports_route_method(F::AGENT_LIST_LOCAL));
        assert!(!capabilities.supports_route_method(F::REMOTE_API_BRIDGE_PERSISTENT));
        assert!(!capabilities.supports_route_method(F::TERMINAL_ATTACH));

        // Unknown / non-cached method constants return false.
        assert!(!capabilities.supports_route_method("not_a_real_method"));
        assert!(!capabilities.supports_route_method(""));

        // A default (empty) capability set reports false for every route method.
        let empty = RemoteSourceCapabilities::default();
        assert!(!empty.supports_route_method(F::PANE_SPLIT));
        assert!(!empty.supports_route_method(F::TAB_CREATE));
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

    #[test]
    fn remote_source_unreachable_status_preserves_agent_and_workspace_state() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);
        cache.replace_workspace_snapshot(host.clone(), vec![workspace("ws-1", "tmp")]);
        cache.mark_status(&host, RemoteConnectionStatus::Unreachable);

        let agents = cache.list_entries();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].status, RemoteConnectionStatus::Unreachable);
        assert_eq!(agents[0].status.stale_label(), Some("unreachable"));
        assert!(agents[0].stale());

        let workspaces = cache
            .workspace_entries_for_host(&host)
            .expect("workspace snapshot kept");
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].status, RemoteConnectionStatus::Unreachable);
        let statuses = cache.list_host_statuses();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].host, host);
        assert_eq!(statuses[0].status, RemoteConnectionStatus::Unreachable);
    }

    #[test]
    fn remote_source_connected_snapshot_reconciles_stale_agent_state() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);
        cache.mark_status(&host, RemoteConnectionStatus::Disconnected);
        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 2)]);
        let agents = cache.list_entries();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].status, RemoteConnectionStatus::Connected);
        assert!(!agents[0].stale());
        assert_eq!(agents[0].agent.revision, 2);
    }

    fn full_bridge_state() -> crate::remote::RemoteApiBridgeState {
        crate::remote::RemoteApiBridgeState {
            shell_path: "\"$HOME/.local/bin/herdr\"".to_string(),
            capabilities: crate::api::schema::FederationCapabilities::current(),
        }
    }

    #[test]
    fn remote_connection_status_available_for_automatic_orchestration_only_when_connected() {
        // Automatic scheduler/orchestrator eligibility requires Connected only.
        assert!(RemoteConnectionStatus::Connected.available_for_automatic_orchestration());
        assert!(!RemoteConnectionStatus::Disconnected.available_for_automatic_orchestration());
        assert!(!RemoteConnectionStatus::NeedsUpdate.available_for_automatic_orchestration());
        assert!(!RemoteConnectionStatus::Unreachable.available_for_automatic_orchestration());
        // is_connected and available_for_automatic_orchestration agree exactly.
        for status in [
            RemoteConnectionStatus::Connected,
            RemoteConnectionStatus::Disconnected,
            RemoteConnectionStatus::NeedsUpdate,
            RemoteConnectionStatus::Unreachable,
        ] {
            assert_eq!(
                status.is_connected(),
                status.available_for_automatic_orchestration(),
                "is_connected and available_for_automatic_orchestration must agree for {status:?}"
            );
        }
    }

    #[test]
    fn remote_source_connected_bridge_state_available_only_when_connected() {
        // C5/test 4 gating: connected_bridge_state returns the cached prepared
        // state only while the host is Connected, and None otherwise (so stale
        // agent.read and non-connected dispatch fall back to the full path).
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        let state = full_bridge_state();

        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);
        cache.set_connected_bridge_state(&host, state.clone());
        assert_eq!(cache.connected_bridge_state(&host).as_ref(), Some(&state));

        // Non-connected marks must hide the prepared state for dispatch.
        for status in [
            RemoteConnectionStatus::Disconnected,
            RemoteConnectionStatus::Unreachable,
            RemoteConnectionStatus::NeedsUpdate,
        ] {
            cache.mark_status(&host, status);
            assert_eq!(
                cache.connected_bridge_state(&host),
                None,
                "prepared state must be hidden for {status:?}"
            );
            // Reconnect to re-test the next status variant from Connected.
            cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);
            cache.set_connected_bridge_state(&host, state.clone());
        }
    }

    #[test]
    fn remote_source_mark_status_invalidates_prepared_state_but_preserves_display() {
        // C5/test 3: mark_status(Disconnected|Unreachable|NeedsUpdate) drops the
        // prepared bridge state (safety-relevant for mutating dispatch) while
        // keeping display caches (agents/workspaces) stale as today.
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);
        cache.replace_workspace_snapshot(host.clone(), vec![workspace("ws-1", "tmp")]);
        cache.set_connected_bridge_state(&host, full_bridge_state());
        assert!(cache.connected_bridge_state(&host).is_some());

        cache.mark_status(&host, RemoteConnectionStatus::Unreachable);

        // Prepared state is gone: mutating dispatch cannot reuse stale prep.
        assert!(cache.connected_bridge_state(&host).is_none());
        // Display caches remain (stale) for read-only views.
        let agents = cache.list_entries();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].status, RemoteConnectionStatus::Unreachable);
        assert!(agents[0].stale());
        let workspaces = cache
            .workspace_entries_for_host(&host)
            .expect("snapshot kept");
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].status, RemoteConnectionStatus::Unreachable);
    }

    #[test]
    fn remote_source_reconnect_keeps_connected_bridge_state_until_marked() {
        // Replacing the connected snapshot does not drop prepared state on its
        // own; only a non-connected mark_status hides it.
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        let state = full_bridge_state();
        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);
        cache.set_connected_bridge_state(&host, state.clone());
        assert_eq!(cache.connected_bridge_state(&host).as_ref(), Some(&state));

        cache.replace_connected_snapshot(host.clone(), vec![agent("term-2", "claude", 1)]);
        // Still connected and still prepared.
        assert_eq!(cache.connected_bridge_state(&host).as_ref(), Some(&state));
    }

    #[test]
    fn remote_source_set_connected_bridge_state_marks_connected_and_stores_state() {
        // C3: the reducer-side handler for `AppEvent::RemoteSourceBridgeState`
        // (a successful supervisor ping) must mark the host `Connected` and
        // store the prepared state together. This is what lets a host seeded
        // `Disconnected` (no snapshot/agents yet) hold prepared state and clear
        // the `agent.start --host` connected precheck. A later non-connected
        // mark still invalidates it.
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        let state = full_bridge_state();

        // Seed the host `Disconnected` (mirrors startup seeding), with no agents.
        cache.ensure_host(host.clone(), RemoteConnectionStatus::Disconnected);
        assert_eq!(
            cache.host_status(&host),
            Some(RemoteConnectionStatus::Disconnected)
        );
        assert!(cache.connected_bridge_state(&host).is_none());

        cache.set_connected_bridge_state(&host, state.clone());
        assert_eq!(
            cache.host_status(&host),
            Some(RemoteConnectionStatus::Connected)
        );
        assert_eq!(cache.connected_bridge_state(&host).as_ref(), Some(&state));
        // No snapshot arrived yet: connected host with prepared state, no agents.
        assert!(cache.list_entries().is_empty());

        // A later non-connected mark still invalidates the prepared state.
        cache.mark_status(&host, RemoteConnectionStatus::Unreachable);
        assert_eq!(
            cache.host_status(&host),
            Some(RemoteConnectionStatus::Unreachable)
        );
        assert!(cache.connected_bridge_state(&host).is_none());
    }
}
