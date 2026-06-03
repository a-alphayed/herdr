//! Pure parsing and read-only cache resolution for future host-qualified remote target routing.
//!
//! This module classifies target strings and resolves them against a read-only
//! `RemoteSourceCache` snapshot. It does not open bridges, perform IO, or execute
//! command routing.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::remote_source::{RemoteAgentEntry, RemoteAgentKey, RemoteHostKey, RemoteSourceCache};

#[allow(dead_code)] // Staged validation error for future remote config/target routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteAliasError {
    Empty,
    ContainsSlash,
    ContainsColon,
    StartsWithDash,
}

impl std::fmt::Display for RemoteAliasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "remote alias cannot be empty"),
            Self::ContainsSlash => write!(f, "remote alias cannot contain '/'"),
            Self::ContainsColon => write!(f, "remote alias cannot contain ':'"),
            Self::StartsWithDash => write!(f, "remote alias cannot start with '-'"),
        }
    }
}

impl std::error::Error for RemoteAliasError {}

#[allow(dead_code)] // Staged for future remote config and host-qualified target parsing.
pub(crate) fn validate_remote_alias(alias: &str) -> Result<&str, RemoteAliasError> {
    if alias.is_empty() {
        return Err(RemoteAliasError::Empty);
    }
    if alias.contains('/') {
        return Err(RemoteAliasError::ContainsSlash);
    }
    if alias.contains(':') {
        return Err(RemoteAliasError::ContainsColon);
    }
    if alias.starts_with('-') {
        return Err(RemoteAliasError::StartsWithDash);
    }
    Ok(alias)
}

#[allow(dead_code)] // Staged parse error for future cross-host command routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TargetRouteParseError {
    InvalidAlias(RemoteAliasError),
    EmptyRemoteTarget,
}

impl std::fmt::Display for TargetRouteParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAlias(err) => write!(f, "{err}"),
            Self::EmptyRemoteTarget => write!(f, "remote target cannot be empty"),
        }
    }
}

impl std::error::Error for TargetRouteParseError {}

impl From<RemoteAliasError> for TargetRouteParseError {
    fn from(err: RemoteAliasError) -> Self {
        Self::InvalidAlias(err)
    }
}

#[allow(dead_code)] // Staged parser output for future cross-host command routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TargetRoute {
    Local {
        target: String,
    },
    Remote {
        host: String,
        target: RemoteTargetSelector,
    },
}

#[allow(dead_code)] // Staged selector for future cross-host command routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteTargetSelector {
    Agent(String),
    Pane(String),
    Terminal(String),
    Workspace(String),
}

#[allow(dead_code)] // Staged parser entrypoint for future cross-host command routing.
pub(crate) fn parse_target_route(target: &str) -> Result<TargetRoute, TargetRouteParseError> {
    let Some((host, remote_target)) = target.split_once('/') else {
        return Ok(TargetRoute::Local {
            target: target.to_string(),
        });
    };

    validate_remote_alias(host)?;
    if remote_target.is_empty() {
        return Err(TargetRouteParseError::EmptyRemoteTarget);
    }

    Ok(TargetRoute::Remote {
        host: host.to_string(),
        target: parse_remote_target_selector(remote_target)?,
    })
}

#[allow(dead_code)] // Staged selector parser for future cross-host command routing.
pub(crate) fn parse_remote_target_selector(
    target: &str,
) -> Result<RemoteTargetSelector, TargetRouteParseError> {
    for (prefix, constructor) in [
        (
            "pane:",
            RemoteTargetSelector::Pane as fn(String) -> RemoteTargetSelector,
        ),
        ("terminal:", RemoteTargetSelector::Terminal),
        ("workspace:", RemoteTargetSelector::Workspace),
    ] {
        if let Some(value) = target.strip_prefix(prefix) {
            if value.is_empty() {
                return Err(TargetRouteParseError::EmptyRemoteTarget);
            }
            return Ok(constructor(value.to_string()));
        }
    }

    Ok(RemoteTargetSelector::Agent(target.to_string()))
}

fn default_remote_session_name() -> String {
    crate::session::DEFAULT_SESSION_NAME.to_string()
}

fn default_auto_connect() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct RemoteHostConfig {
    pub(crate) name: String,
    pub(crate) target: String,
    #[serde(default = "default_remote_session_name")]
    pub(crate) session: String,
    #[serde(default = "default_auto_connect")]
    pub(crate) auto_connect: bool,
}

impl RemoteHostConfig {
    pub(crate) fn new(
        name: impl Into<String>,
        target: impl Into<String>,
        session: impl Into<String>,
        auto_connect: bool,
    ) -> Self {
        Self {
            name: name.into(),
            target: target.into(),
            session: session.into(),
            auto_connect,
        }
    }
}

#[allow(dead_code)] // Staged validation error for future remote host registry/config loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteHostConfigError {
    InvalidAlias(RemoteAliasError),
    EmptySshTarget,
    EmptySession,
    DuplicateAlias(String),
}

impl std::fmt::Display for RemoteHostConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAlias(err) => write!(f, "{err}"),
            Self::EmptySshTarget => write!(f, "remote SSH target cannot be empty"),
            Self::EmptySession => write!(f, "remote session cannot be empty"),
            Self::DuplicateAlias(alias) => write!(f, "duplicate remote alias: {alias}"),
        }
    }
}

impl std::error::Error for RemoteHostConfigError {}

impl From<RemoteAliasError> for RemoteHostConfigError {
    fn from(err: RemoteAliasError) -> Self {
        Self::InvalidAlias(err)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RemoteHostRegistry {
    hosts: BTreeMap<String, RemoteHostConfig>,
}

impl RemoteHostRegistry {
    pub(crate) fn from_configs(
        configs: Vec<RemoteHostConfig>,
    ) -> Result<Self, RemoteHostConfigError> {
        let mut registry = Self::default();
        for config in configs {
            registry.insert(config)?;
        }
        Ok(registry)
    }

    pub(crate) fn insert(&mut self, config: RemoteHostConfig) -> Result<(), RemoteHostConfigError> {
        validate_remote_alias(&config.name)?;
        if config.target.is_empty() {
            return Err(RemoteHostConfigError::EmptySshTarget);
        }
        if config.session.is_empty() {
            return Err(RemoteHostConfigError::EmptySession);
        }
        if self.hosts.contains_key(&config.name) {
            return Err(RemoteHostConfigError::DuplicateAlias(config.name));
        }
        self.hosts.insert(config.name.clone(), config);
        Ok(())
    }

    pub(crate) fn get(&self, alias: &str) -> Option<&RemoteHostConfig> {
        self.hosts.get(alias)
    }

    #[allow(dead_code)] // Staged deterministic listing for future remote status/config UI; tests exercise it now.
    pub(crate) fn list(&self) -> Vec<&RemoteHostConfig> {
        self.hosts.values().collect()
    }
}

#[allow(dead_code)] // Staged route plan for future cross-host command routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlannedTargetRoute {
    Local {
        target: String,
    },
    Remote {
        host: RemoteHostConfig,
        target: RemoteTargetSelector,
    },
}

#[allow(dead_code)] // Staged route planning error for future cross-host command routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteRoutePlanError {
    Parse(TargetRouteParseError),
    UnknownHost(String),
}

impl std::fmt::Display for RemoteRoutePlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(err) => write!(f, "{err}"),
            Self::UnknownHost(host) => write!(f, "unknown remote host: {host}"),
        }
    }
}

impl std::error::Error for RemoteRoutePlanError {}

impl From<TargetRouteParseError> for RemoteRoutePlanError {
    fn from(err: TargetRouteParseError) -> Self {
        Self::Parse(err)
    }
}

#[allow(dead_code)] // Staged pure planner for future cross-host command routing.
pub(crate) fn plan_target_route(
    registry: &RemoteHostRegistry,
    target: &str,
) -> Result<PlannedTargetRoute, RemoteRoutePlanError> {
    match parse_target_route(target)? {
        TargetRoute::Local { target } => Ok(PlannedTargetRoute::Local { target }),
        TargetRoute::Remote { host, target } => {
            let config = registry
                .get(&host)
                .ok_or_else(|| RemoteRoutePlanError::UnknownHost(host.clone()))?;
            Ok(PlannedTargetRoute::Remote {
                host: config.clone(),
                target,
            })
        }
    }
}

#[allow(dead_code)] // Staged resolver output for future remote command routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteAgentResolution {
    pub(crate) host: RemoteHostConfig,
    pub(crate) key: RemoteAgentKey,
    pub(crate) entry: RemoteAgentEntry,
}

#[allow(dead_code)] // Staged ambiguity details for future remote command routing errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteAgentCandidate {
    pub(crate) key: RemoteAgentKey,
    pub(crate) label: String,
    pub(crate) pane_id: String,
    pub(crate) terminal_id: String,
    pub(crate) stale: bool,
}

#[allow(dead_code)] // Staged resolver error for future remote command routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteAgentResolveError {
    NotFound {
        target: RemoteTargetSelector,
    },
    Ambiguous {
        target: RemoteTargetSelector,
        candidates: Vec<RemoteAgentCandidate>,
    },
    UnsupportedSelector {
        target: RemoteTargetSelector,
    },
}

impl std::fmt::Display for RemoteAgentResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { target } => write!(f, "remote agent target not found: {target:?}"),
            Self::Ambiguous { target, .. } => {
                write!(f, "remote agent target is ambiguous: {target:?}")
            }
            Self::UnsupportedSelector { target } => {
                write!(
                    f,
                    "remote selector is not a single-agent target: {target:?}"
                )
            }
        }
    }
}

impl std::error::Error for RemoteAgentResolveError {}

#[allow(dead_code)] // Staged pure resolver for future remote command routing.
pub(crate) fn resolve_remote_agent_target(
    cache: &RemoteSourceCache,
    host: &RemoteHostConfig,
    selector: &RemoteTargetSelector,
) -> Result<RemoteAgentResolution, RemoteAgentResolveError> {
    if matches!(selector, RemoteTargetSelector::Workspace(_)) {
        return Err(RemoteAgentResolveError::UnsupportedSelector {
            target: selector.clone(),
        });
    }

    let host_key = RemoteHostKey::new(host.name.clone(), host.session.clone());
    let matches: Vec<_> = cache
        .entries_for_host(&host_key)
        .into_iter()
        .filter(|entry| remote_agent_matches_selector(entry, selector))
        .collect();

    match matches.len() {
        0 => Err(RemoteAgentResolveError::NotFound {
            target: selector.clone(),
        }),
        1 => {
            let mut matches = matches.into_iter();
            let Some(entry) = matches.next() else {
                return Err(RemoteAgentResolveError::NotFound {
                    target: selector.clone(),
                });
            };
            let key = RemoteAgentKey::new(&host_key, entry.agent.terminal_id.clone());
            Ok(RemoteAgentResolution {
                host: host.clone(),
                key,
                entry,
            })
        }
        _ => Err(RemoteAgentResolveError::Ambiguous {
            target: selector.clone(),
            candidates: matches
                .into_iter()
                .map(|entry| remote_agent_candidate(&host_key, &entry))
                .collect(),
        }),
    }
}

fn remote_agent_matches_selector(
    entry: &RemoteAgentEntry,
    selector: &RemoteTargetSelector,
) -> bool {
    match selector {
        RemoteTargetSelector::Terminal(terminal_id) => entry.agent.terminal_id == *terminal_id,
        RemoteTargetSelector::Pane(pane_id) => entry.agent.pane_id == *pane_id,
        RemoteTargetSelector::Agent(label) => {
            remote_agent_identity_labels(&entry.agent).contains(label)
        }
        RemoteTargetSelector::Workspace(_) => false,
    }
}

fn remote_agent_candidate(host: &RemoteHostKey, entry: &RemoteAgentEntry) -> RemoteAgentCandidate {
    RemoteAgentCandidate {
        key: RemoteAgentKey::new(host, entry.agent.terminal_id.clone()),
        label: preferred_remote_agent_label(&entry.agent),
        pane_id: entry.agent.pane_id.clone(),
        terminal_id: entry.agent.terminal_id.clone(),
        stale: entry.stale(),
    }
}

fn remote_agent_identity_labels(agent: &crate::api::schema::AgentInfo) -> BTreeSet<String> {
    let mut labels = BTreeSet::new();
    for label in [
        agent.name.as_deref(),
        agent.display_agent.as_deref(),
        agent.agent.as_deref(),
        agent.title.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        labels.insert(label.to_string());
    }
    labels.insert(agent.terminal_id.clone());
    labels
}

fn preferred_remote_agent_label(agent: &crate::api::schema::AgentInfo) -> String {
    agent
        .name
        .as_deref()
        .or(agent.display_agent.as_deref())
        .or(agent.agent.as_deref())
        .or(agent.title.as_deref())
        .unwrap_or(&agent.terminal_id)
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::api::schema::{AgentInfo, AgentStatus};
    use crate::remote_source::{
        RemoteAgentKey, RemoteConnectionStatus, RemoteHostKey, RemoteSourceCache,
    };

    use super::*;

    fn agent_with_fields(
        terminal_id: &str,
        pane_id: &str,
        name: Option<&str>,
        display_agent: Option<&str>,
        agent: Option<&str>,
        title: Option<&str>,
    ) -> AgentInfo {
        AgentInfo {
            terminal_id: terminal_id.to_string(),
            name: name.map(str::to_string),
            agent: agent.map(str::to_string),
            title: title.map(str::to_string),
            display_agent: display_agent.map(str::to_string),
            agent_status: AgentStatus::Working,
            custom_status: None,
            state_labels: HashMap::new(),
            agent_session: None,
            workspace_id: "remote-ws".to_string(),
            tab_id: "remote-tab".to_string(),
            pane_id: pane_id.to_string(),
            focused: false,
            cwd: None,
            foreground_cwd: None,
            revision: 1,
        }
    }

    fn labeled_agent(terminal_id: &str, pane_id: &str, label: &str) -> AgentInfo {
        agent_with_fields(
            terminal_id,
            pane_id,
            Some(label),
            Some(label),
            Some(label),
            None,
        )
    }

    fn host_config(name: &str, session: &str) -> RemoteHostConfig {
        RemoteHostConfig::new(name, name, session, true)
    }

    #[test]
    fn remote_target_accepts_valid_aliases() {
        for alias in ["jafar", "work-mini", "host_1", "host.example"] {
            assert_eq!(validate_remote_alias(alias), Ok(alias));
        }
    }

    #[test]
    fn remote_target_rejects_invalid_aliases() {
        assert_eq!(validate_remote_alias(""), Err(RemoteAliasError::Empty));
        assert_eq!(
            validate_remote_alias("ja/far"),
            Err(RemoteAliasError::ContainsSlash)
        );
        assert_eq!(
            validate_remote_alias("ja:far"),
            Err(RemoteAliasError::ContainsColon)
        );
        assert_eq!(
            validate_remote_alias("-jafar"),
            Err(RemoteAliasError::StartsWithDash)
        );
    }

    #[test]
    fn remote_target_bare_target_is_local_only() {
        assert_eq!(
            parse_target_route("codex").unwrap(),
            TargetRoute::Local {
                target: "codex".to_string(),
            }
        );
    }

    #[test]
    fn remote_target_bare_typed_handle_lookalikes_are_local_only() {
        for target in ["pane:1-1", "terminal:term_abc", "workspace:w1"] {
            assert_eq!(
                parse_target_route(target).unwrap(),
                TargetRoute::Local {
                    target: target.to_string(),
                }
            );
        }
    }

    #[test]
    fn remote_target_splits_host_on_first_slash_and_preserves_remainder() {
        assert_eq!(
            parse_target_route("jafar/codex/review").unwrap(),
            TargetRoute::Remote {
                host: "jafar".to_string(),
                target: RemoteTargetSelector::Agent("codex/review".to_string()),
            }
        );
    }

    #[test]
    fn remote_target_rejects_empty_host_or_target() {
        assert_eq!(
            parse_target_route("/codex").unwrap_err(),
            TargetRouteParseError::InvalidAlias(RemoteAliasError::Empty)
        );
        assert_eq!(
            parse_target_route("jafar/").unwrap_err(),
            TargetRouteParseError::EmptyRemoteTarget
        );
    }

    #[test]
    fn remote_target_rejects_colon_in_alias_but_preserves_colon_in_target() {
        assert_eq!(
            parse_target_route("ja:far/codex").unwrap_err(),
            TargetRouteParseError::InvalidAlias(RemoteAliasError::ContainsColon)
        );
        assert_eq!(
            parse_target_route("jafar/codex:review").unwrap(),
            TargetRoute::Remote {
                host: "jafar".to_string(),
                target: RemoteTargetSelector::Agent("codex:review".to_string()),
            }
        );
    }

    #[test]
    fn remote_target_parses_typed_handles() {
        assert_eq!(
            parse_target_route("jafar/pane:1-1").unwrap(),
            TargetRoute::Remote {
                host: "jafar".to_string(),
                target: RemoteTargetSelector::Pane("1-1".to_string()),
            }
        );
        assert_eq!(
            parse_target_route("jafar/terminal:term_abc").unwrap(),
            TargetRoute::Remote {
                host: "jafar".to_string(),
                target: RemoteTargetSelector::Terminal("term_abc".to_string()),
            }
        );
        assert_eq!(
            parse_target_route("jafar/workspace:w1").unwrap(),
            TargetRoute::Remote {
                host: "jafar".to_string(),
                target: RemoteTargetSelector::Workspace("w1".to_string()),
            }
        );
    }

    #[test]
    fn remote_target_rejects_empty_typed_handle_payloads() {
        for target in ["jafar/pane:", "jafar/terminal:", "jafar/workspace:"] {
            assert_eq!(
                parse_target_route(target).unwrap_err(),
                TargetRouteParseError::EmptyRemoteTarget
            );
        }
    }

    #[test]
    fn remote_target_unknown_colon_prefix_remains_agent_target() {
        assert_eq!(
            parse_target_route("jafar/foo:bar").unwrap(),
            TargetRoute::Remote {
                host: "jafar".to_string(),
                target: RemoteTargetSelector::Agent("foo:bar".to_string()),
            }
        );
    }

    #[test]
    fn remote_target_agent_labels_may_contain_slash_and_colon_after_first_slash() {
        assert_eq!(
            parse_target_route("jafar/team/codex:review/1").unwrap(),
            TargetRoute::Remote {
                host: "jafar".to_string(),
                target: RemoteTargetSelector::Agent("team/codex:review/1".to_string()),
            }
        );
    }

    #[test]
    fn remote_target_registry_accepts_valid_config_and_looks_up_deterministically() {
        let registry = RemoteHostRegistry::from_configs(vec![
            RemoteHostConfig::new("work", "user@work:2222", "default", false),
            RemoteHostConfig::new("jafar", "jafar", "agents", true),
        ])
        .unwrap();

        assert_eq!(registry.get("jafar").unwrap().target, "jafar");
        assert_eq!(registry.get("jafar").unwrap().session, "agents");
        let names: Vec<_> = registry
            .list()
            .into_iter()
            .map(|config| config.name.as_str())
            .collect();
        assert_eq!(names, vec!["jafar", "work"]);
    }

    #[test]
    fn remote_target_registry_rejects_invalid_alias() {
        let err = RemoteHostRegistry::from_configs(vec![RemoteHostConfig::new(
            "bad/alias",
            "host",
            "default",
            true,
        )])
        .unwrap_err();

        assert_eq!(
            err,
            RemoteHostConfigError::InvalidAlias(RemoteAliasError::ContainsSlash)
        );
    }

    #[test]
    fn remote_target_registry_rejects_empty_target_or_session() {
        assert_eq!(
            RemoteHostRegistry::from_configs(vec![RemoteHostConfig::new(
                "jafar", "", "default", true
            )])
            .unwrap_err(),
            RemoteHostConfigError::EmptySshTarget
        );
        assert_eq!(
            RemoteHostRegistry::from_configs(vec![RemoteHostConfig::new(
                "jafar", "host", "", true
            )])
            .unwrap_err(),
            RemoteHostConfigError::EmptySession
        );
    }

    #[test]
    fn remote_target_registry_rejects_duplicate_aliases() {
        let err = RemoteHostRegistry::from_configs(vec![
            RemoteHostConfig::new("jafar", "host-a", "default", true),
            RemoteHostConfig::new("jafar", "host-b", "default", true),
        ])
        .unwrap_err();

        assert_eq!(
            err,
            RemoteHostConfigError::DuplicateAlias("jafar".to_string())
        );
    }

    #[test]
    fn remote_target_registry_duplicate_insert_keeps_original_entry_intact() {
        let mut registry = RemoteHostRegistry::from_configs(vec![RemoteHostConfig::new(
            "jafar", "host-a", "default", true,
        )])
        .unwrap();

        let err = registry
            .insert(RemoteHostConfig::new("jafar", "host-b", "agents", false))
            .unwrap_err();

        assert_eq!(
            err,
            RemoteHostConfigError::DuplicateAlias("jafar".to_string())
        );
        assert_eq!(registry.list().len(), 1);
        let original = registry.get("jafar").unwrap();
        assert_eq!(original.target, "host-a");
        assert_eq!(original.session, "default");
        assert!(original.auto_connect);
    }

    #[test]
    fn remote_target_registry_allows_colons_in_ssh_target() {
        let registry = RemoteHostRegistry::from_configs(vec![
            RemoteHostConfig::new("work", "user@host", "default", true),
            RemoteHostConfig::new("ports", "host:2222", "default", true),
        ])
        .unwrap();

        assert_eq!(registry.get("work").unwrap().target, "user@host");
        assert_eq!(registry.get("ports").unwrap().target, "host:2222");
    }

    #[test]
    fn remote_target_plan_keeps_bare_typed_lookalike_local() {
        let registry = RemoteHostRegistry::from_configs(vec![RemoteHostConfig::new(
            "jafar", "jafar", "default", true,
        )])
        .unwrap();

        assert_eq!(
            plan_target_route(&registry, "pane:1-1").unwrap(),
            PlannedTargetRoute::Local {
                target: "pane:1-1".to_string(),
            }
        );
    }

    #[test]
    fn remote_target_plan_resolves_configured_host_to_remote_plan() {
        let config = RemoteHostConfig::new("jafar", "user@jafar:2222", "agents", true);
        let registry = RemoteHostRegistry::from_configs(vec![config.clone()]).unwrap();

        assert_eq!(
            plan_target_route(&registry, "jafar/terminal:term_abc").unwrap(),
            PlannedTargetRoute::Remote {
                host: config,
                target: RemoteTargetSelector::Terminal("term_abc".to_string()),
            }
        );
    }

    #[test]
    fn remote_target_plan_returns_unknown_host_for_unconfigured_remote_alias() {
        let registry = RemoteHostRegistry::default();

        assert_eq!(
            plan_target_route(&registry, "jafar/codex").unwrap_err(),
            RemoteRoutePlanError::UnknownHost("jafar".to_string())
        );
    }

    #[test]
    fn remote_target_plan_propagates_parser_errors_distinctly() {
        let registry = RemoteHostRegistry::default();

        assert_eq!(
            plan_target_route(&registry, "bad:alias/codex").unwrap_err(),
            RemoteRoutePlanError::Parse(TargetRouteParseError::InvalidAlias(
                RemoteAliasError::ContainsColon
            ))
        );
        assert_eq!(
            plan_target_route(&registry, "jafar/").unwrap_err(),
            RemoteRoutePlanError::Parse(TargetRouteParseError::EmptyRemoteTarget)
        );
    }

    #[test]
    fn remote_target_resolves_terminal_selector_with_host_session_scope() {
        let host = host_config("jafar", "default");
        let other_session = host_config("jafar", "agents");
        let mut cache = RemoteSourceCache::default();
        cache.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![labeled_agent("term-1", "pane-1", "codex")],
        );
        cache.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "agents"),
            vec![labeled_agent("term-1", "pane-other", "other")],
        );

        let resolved = resolve_remote_agent_target(
            &cache,
            &host,
            &RemoteTargetSelector::Terminal("term-1".to_string()),
        )
        .unwrap();

        assert_eq!(resolved.host, host);
        assert_eq!(
            resolved.key,
            RemoteAgentKey::new(&RemoteHostKey::new("jafar", "default"), "term-1")
        );
        assert_eq!(resolved.entry.agent.pane_id, "pane-1");
        assert!(resolve_remote_agent_target(
            &cache,
            &other_session,
            &RemoteTargetSelector::Terminal("term-missing".to_string())
        )
        .is_err());
    }

    #[test]
    fn remote_target_resolves_pane_selector() {
        let host = host_config("jafar", "default");
        let mut cache = RemoteSourceCache::default();
        cache.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![labeled_agent("term-1", "pane-1", "codex")],
        );

        let resolved = resolve_remote_agent_target(
            &cache,
            &host,
            &RemoteTargetSelector::Pane("pane-1".to_string()),
        )
        .unwrap();

        assert_eq!(resolved.entry.agent.terminal_id, "term-1");
    }

    #[test]
    fn remote_target_resolves_agent_selector_by_identity_fields_and_terminal_fallback() {
        let host = host_config("jafar", "default");
        let mut cache = RemoteSourceCache::default();
        cache.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![
                agent_with_fields("term-name", "pane-name", Some("named"), None, None, None),
                agent_with_fields(
                    "term-display",
                    "pane-display",
                    None,
                    Some("displayed"),
                    None,
                    None,
                ),
                agent_with_fields(
                    "term-agent",
                    "pane-agent",
                    None,
                    None,
                    Some("agent-label"),
                    None,
                ),
                agent_with_fields("term-title", "pane-title", None, None, None, Some("titled")),
                agent_with_fields("term-fallback", "pane-fallback", None, None, None, None),
            ],
        );

        for (selector, terminal_id) in [
            ("named", "term-name"),
            ("displayed", "term-display"),
            ("agent-label", "term-agent"),
            ("titled", "term-title"),
            ("term-fallback", "term-fallback"),
            ("term-name", "term-name"),
        ] {
            let resolved = resolve_remote_agent_target(
                &cache,
                &host,
                &RemoteTargetSelector::Agent(selector.to_string()),
            )
            .unwrap();
            assert_eq!(resolved.entry.agent.terminal_id, terminal_id);
        }
    }

    #[test]
    fn remote_target_resolution_keeps_same_name_isolated_by_host_session() {
        let default_host = host_config("jafar", "default");
        let agents_host = host_config("jafar", "agents");
        let mut cache = RemoteSourceCache::default();
        cache.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![labeled_agent("term-default", "pane-1", "codex")],
        );
        cache.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "agents"),
            vec![labeled_agent("term-agents", "pane-1", "codex")],
        );

        let default_resolved = resolve_remote_agent_target(
            &cache,
            &default_host,
            &RemoteTargetSelector::Agent("codex".to_string()),
        )
        .unwrap();
        let agents_resolved = resolve_remote_agent_target(
            &cache,
            &agents_host,
            &RemoteTargetSelector::Agent("codex".to_string()),
        )
        .unwrap();

        assert_eq!(default_resolved.entry.agent.terminal_id, "term-default");
        assert_eq!(agents_resolved.entry.agent.terminal_id, "term-agents");
    }

    #[test]
    fn remote_target_resolution_returns_not_found_for_unknown_selector() {
        let host = host_config("jafar", "default");
        let cache = RemoteSourceCache::default();

        assert_eq!(
            resolve_remote_agent_target(
                &cache,
                &host,
                &RemoteTargetSelector::Agent("missing".to_string())
            )
            .unwrap_err(),
            RemoteAgentResolveError::NotFound {
                target: RemoteTargetSelector::Agent("missing".to_string())
            }
        );
    }

    #[test]
    fn remote_target_resolution_returns_deterministic_ambiguity_candidates() {
        let host = host_config("jafar", "default");
        let mut cache = RemoteSourceCache::default();
        cache.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![
                labeled_agent("term-b", "pane-b", "codex"),
                labeled_agent("term-a", "pane-a", "codex"),
            ],
        );

        let err = resolve_remote_agent_target(
            &cache,
            &host,
            &RemoteTargetSelector::Agent("codex".to_string()),
        )
        .unwrap_err();

        let RemoteAgentResolveError::Ambiguous { candidates, .. } = err else {
            panic!("expected ambiguity");
        };
        let terminal_ids: Vec<_> = candidates
            .iter()
            .map(|candidate| candidate.terminal_id.as_str())
            .collect();
        assert_eq!(terminal_ids, vec!["term-a", "term-b"]);
        assert_eq!(candidates[0].label, "codex");
        assert_eq!(candidates[0].pane_id, "pane-a");
        assert!(!candidates[0].stale);
    }

    #[test]
    fn remote_target_resolution_rejects_workspace_selector_for_single_agent_resolution() {
        let host = host_config("jafar", "default");
        let cache = RemoteSourceCache::default();

        assert_eq!(
            resolve_remote_agent_target(
                &cache,
                &host,
                &RemoteTargetSelector::Workspace("w1".to_string())
            )
            .unwrap_err(),
            RemoteAgentResolveError::UnsupportedSelector {
                target: RemoteTargetSelector::Workspace("w1".to_string())
            }
        );
    }

    #[test]
    fn remote_target_resolution_resolves_stale_entry_with_status_intact() {
        let host = host_config("jafar", "default");
        let host_key = RemoteHostKey::new("jafar", "default");
        let mut cache = RemoteSourceCache::default();
        cache.replace_connected_snapshot(
            host_key.clone(),
            vec![labeled_agent("term-1", "pane-1", "codex")],
        );
        cache.mark_disconnected(&host_key);

        let resolved = resolve_remote_agent_target(
            &cache,
            &host,
            &RemoteTargetSelector::Agent("codex".to_string()),
        )
        .unwrap();

        assert!(resolved.entry.stale());
        assert_eq!(resolved.entry.status, RemoteConnectionStatus::Disconnected);
    }
}
