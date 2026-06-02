//! Pure cache state for federated remote agent aggregation.
//!
//! Runtime supervisors, SSH bridges, sockets, UI rendering, and command routing
//! intentionally live elsewhere. This module is rebuildable soft state for
//! remote `AgentInfo` snapshots/events keyed by authoritative host/session.

use std::collections::BTreeMap;

use crate::api::schema::AgentInfo;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteConnectionStatus {
    Connected,
    Disconnected,
}

impl RemoteConnectionStatus {
    pub(crate) fn is_connected(self) -> bool {
        matches!(self, Self::Connected)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteAgentEntry {
    pub(crate) host: RemoteHostKey,
    pub(crate) agent: AgentInfo,
    pub(crate) status: RemoteConnectionStatus,
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
}

impl Default for RemoteHostCache {
    fn default() -> Self {
        Self {
            status: RemoteConnectionStatus::Disconnected,
            agents: BTreeMap::new(),
        }
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

    pub(crate) fn mark_disconnected(&mut self, host: &RemoteHostKey) {
        self.hosts.entry(host.clone()).or_default().status = RemoteConnectionStatus::Disconnected;
    }

    pub(crate) fn apply_agent_update(&mut self, host: RemoteHostKey, agent: AgentInfo) -> bool {
        let host_cache = self.hosts.entry(host).or_insert_with(|| RemoteHostCache {
            status: RemoteConnectionStatus::Connected,
            agents: BTreeMap::new(),
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
            .flat_map(|(host, host_cache)| {
                host_cache
                    .agents
                    .values()
                    .map(move |agent| RemoteAgentEntry {
                        host: host.clone(),
                        agent: agent.clone(),
                        status: host_cache.status,
                    })
            })
            .collect()
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

    use crate::api::schema::{AgentInfo, AgentStatus};

    use super::*;

    fn agent(terminal_id: &str, label: &str, revision: u64) -> AgentInfo {
        AgentInfo {
            terminal_id: terminal_id.to_string(),
            name: Some(label.to_string()),
            agent: Some(label.to_string()),
            title: None,
            display_agent: Some(label.to_string()),
            agent_status: AgentStatus::Working,
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
    fn remote_source_disconnect_marks_entries_stale_but_keeps_them() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);

        cache.mark_disconnected(&host);

        let entries = cache.list_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, RemoteConnectionStatus::Disconnected);
        assert!(entries[0].stale());
    }

    #[test]
    fn remote_source_reconnect_snapshot_clears_stale_and_updates_entries() {
        let mut cache = RemoteSourceCache::default();
        let host = RemoteHostKey::new("jafar", "default");
        cache.replace_connected_snapshot(host.clone(), vec![agent("term-1", "codex", 1)]);
        cache.mark_disconnected(&host);

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

        assert!(cache.remove_host(&remove));
        assert!(!cache.remove_host(&remove));

        let entries = cache.list_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].host, keep);
        assert_eq!(entries[0].agent.terminal_id, "term-1");
    }
}
