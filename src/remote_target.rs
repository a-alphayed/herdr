//! Pure parsing and read-only cache resolution for future host-qualified remote target routing.
//!
//! This module classifies target strings and resolves them against a read-only
//! `RemoteSourceCache` snapshot. It does not open bridges, perform IO, or execute
//! command routing.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::remote_source::{
    RemoteAgentEntry, RemoteAgentKey, RemoteHostKey, RemoteProjectionEntry, RemoteProjectionStatus,
    RemoteSourceCache,
};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteTargetSelector {
    Agent(String),
    Pane(String),
    Terminal(String),
    Workspace(String),
    Tab(String),
}

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
        ("tab:", RemoteTargetSelector::Tab),
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

/// Default SSH `ConnectTimeout` (seconds) for a configured remote host, used
/// when a host config omits `connect_timeout_secs`. Matches the timeout that
/// was previously hardcoded for noninteractive SSH invocations.
pub(crate) const DEFAULT_CONNECT_TIMEOUT_SECS: u32 = 10;

/// Conservative upper bound for a configured host's SSH connect timeout.
pub(crate) const MAX_CONNECT_TIMEOUT_SECS: u32 = 300;

fn default_connect_timeout_secs() -> u32 {
    DEFAULT_CONNECT_TIMEOUT_SECS
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct RemoteHostConfig {
    pub(crate) name: String,
    pub(crate) target: String,
    #[serde(default = "default_remote_session_name")]
    pub(crate) session: String,
    #[serde(default = "default_auto_connect")]
    pub(crate) auto_connect: bool,
    /// SSH `ConnectTimeout` in whole seconds for connection attempts to this
    /// host. Applies to both interactive and noninteractive configured-host
    /// SSH invocations. Default: 10 seconds.
    #[serde(default = "default_connect_timeout_secs")]
    pub(crate) connect_timeout_secs: u32,
}

impl RemoteHostConfig {
    #[cfg(any(unix, test))]
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
            connect_timeout_secs: DEFAULT_CONNECT_TIMEOUT_SECS,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_connect_timeout_secs(mut self, connect_timeout_secs: u32) -> Self {
        self.connect_timeout_secs = connect_timeout_secs;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteHostConfigError {
    InvalidAlias(RemoteAliasError),
    EmptySshTarget,
    SshTargetStartsWithDash,
    EmptySession,
    DuplicateAlias(String),
    ConnectTimeoutZero,
    ConnectTimeoutTooLarge { value: u32, max: u32 },
}

impl std::fmt::Display for RemoteHostConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAlias(err) => write!(f, "{err}"),
            Self::EmptySshTarget => write!(f, "remote SSH target cannot be empty"),
            Self::SshTargetStartsWithDash => write!(f, "remote SSH target cannot start with '-'"),
            Self::EmptySession => write!(f, "remote session cannot be empty"),
            Self::DuplicateAlias(alias) => write!(f, "duplicate remote alias: {alias}"),
            Self::ConnectTimeoutZero => {
                write!(f, "remote connect_timeout_secs cannot be 0")
            }
            Self::ConnectTimeoutTooLarge { value, max } => write!(
                f,
                "remote connect_timeout_secs {value} exceeds maximum of {max}"
            ),
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
        if config.target.starts_with('-') {
            return Err(RemoteHostConfigError::SshTargetStartsWithDash);
        }
        if config.session.is_empty() {
            return Err(RemoteHostConfigError::EmptySession);
        }
        if config.connect_timeout_secs == 0 {
            return Err(RemoteHostConfigError::ConnectTimeoutZero);
        }
        if config.connect_timeout_secs > MAX_CONNECT_TIMEOUT_SECS {
            return Err(RemoteHostConfigError::ConnectTimeoutTooLarge {
                value: config.connect_timeout_secs,
                max: MAX_CONNECT_TIMEOUT_SECS,
            });
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

    pub(crate) fn list(&self) -> Vec<&RemoteHostConfig> {
        self.hosts.values().collect()
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteAgentResolution {
    pub(crate) host: RemoteHostConfig,
    pub(crate) key: RemoteAgentKey,
    pub(crate) entry: RemoteAgentEntry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteAgentCandidate {
    pub(crate) key: RemoteAgentKey,
    pub(crate) label: String,
    pub(crate) pane_id: String,
    pub(crate) terminal_id: String,
    pub(crate) stale: bool,
}

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

pub(crate) fn resolve_remote_agent_target(
    cache: &RemoteSourceCache,
    host: &RemoteHostConfig,
    selector: &RemoteTargetSelector,
) -> Result<RemoteAgentResolution, RemoteAgentResolveError> {
    if matches!(
        selector,
        RemoteTargetSelector::Workspace(_) | RemoteTargetSelector::Tab(_)
    ) {
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
        RemoteTargetSelector::Workspace(_) | RemoteTargetSelector::Tab(_) => false,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteWorkspaceResolution {
    pub(crate) host: RemoteHostConfig,
    pub(crate) workspace: crate::api::schema::WorkspaceInfo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteWorkspaceResolveError {
    NotFound { target: RemoteTargetSelector },
    MetadataUnavailable { target: RemoteTargetSelector },
    UnsupportedSelector { target: RemoteTargetSelector },
}

impl std::fmt::Display for RemoteWorkspaceResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { target } => {
                write!(f, "remote workspace target not found: {target:?}")
            }
            Self::MetadataUnavailable { target } => write!(
                f,
                "remote workspace metadata is unavailable; wait for a live workspace snapshot before mutating target {target:?}"
            ),
            Self::UnsupportedSelector { target } => write!(
                f,
                "remote workspace target must be a workspace selector: {target:?}"
            ),
        }
    }
}

impl std::error::Error for RemoteWorkspaceResolveError {}

pub(crate) fn resolve_remote_workspace_target(
    cache: &RemoteSourceCache,
    host: &RemoteHostConfig,
    selector: &RemoteTargetSelector,
) -> Result<RemoteWorkspaceResolution, RemoteWorkspaceResolveError> {
    let RemoteTargetSelector::Workspace(workspace_id) = selector else {
        return Err(RemoteWorkspaceResolveError::UnsupportedSelector {
            target: selector.clone(),
        });
    };

    let host_key = RemoteHostKey::new(host.name.clone(), host.session.clone());
    let Some(workspaces) = cache.workspace_entries_for_host(&host_key) else {
        return Err(RemoteWorkspaceResolveError::MetadataUnavailable {
            target: selector.clone(),
        });
    };
    let Some(entry) = workspaces
        .into_iter()
        .find(|entry| entry.workspace.workspace_id == *workspace_id)
    else {
        return Err(RemoteWorkspaceResolveError::NotFound {
            target: selector.clone(),
        });
    };

    Ok(RemoteWorkspaceResolution {
        host: host.clone(),
        workspace: entry.workspace,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteTabResolution {
    pub(crate) host: RemoteHostConfig,
    pub(crate) workspace_id: String,
    pub(crate) tab: crate::api::schema::TabInfo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteTabResolveError {
    NotFound {
        target: RemoteTargetSelector,
    },
    MetadataUnavailable {
        target: RemoteTargetSelector,
    },
    MetadataStale {
        target: RemoteTargetSelector,
        workspace_id: Option<String>,
        status: Option<RemoteProjectionStatus>,
    },
    UnsupportedSelector {
        target: RemoteTargetSelector,
    },
}

impl std::fmt::Display for RemoteTabResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { target } => write!(
                f,
                "remote tab target not found in live tab metadata: {target:?}"
            ),
            Self::MetadataUnavailable { target } => write!(
                f,
                "remote tab metadata is unavailable; wait for a live tab snapshot before mutating target {target:?}"
            ),
            Self::MetadataStale {
                target,
                workspace_id,
                status,
            } => {
                let status = status
                    .map(remote_projection_status_label)
                    .unwrap_or("not available");
                match workspace_id {
                    Some(workspace_id) => write!(
                        f,
                        "remote tab metadata for workspace {workspace_id} is {status}; wait for live tab metadata before mutating target {target:?}"
                    ),
                    None => write!(
                        f,
                        "remote tab metadata is {status}; wait for live tab metadata before mutating target {target:?}"
                    ),
                }
            }
            Self::UnsupportedSelector { target } => {
                write!(f, "remote tab target must be a tab selector: {target:?}")
            }
        }
    }
}

impl std::error::Error for RemoteTabResolveError {}

pub(crate) fn resolve_remote_tab_target(
    cache: &RemoteSourceCache,
    host: &RemoteHostConfig,
    selector: &RemoteTargetSelector,
) -> Result<RemoteTabResolution, RemoteTabResolveError> {
    let RemoteTargetSelector::Tab(tab_id) = selector else {
        return Err(RemoteTabResolveError::UnsupportedSelector {
            target: selector.clone(),
        });
    };

    let host_key = RemoteHostKey::new(host.name.clone(), host.session.clone());
    let snapshots = cache.tab_snapshots_for_host(&host_key);
    if snapshots.is_empty() {
        return Err(RemoteTabResolveError::MetadataUnavailable {
            target: selector.clone(),
        });
    }

    let mut saw_live_snapshot = false;
    for snapshot in snapshots {
        if let Some(tab) = snapshot.tabs.iter().find(|tab| tab.tab_id == *tab_id) {
            if snapshot.status != RemoteProjectionStatus::Available {
                return Err(RemoteTabResolveError::MetadataStale {
                    target: selector.clone(),
                    workspace_id: Some(snapshot.workspace_id.clone()),
                    status: Some(snapshot.status),
                });
            }
            return Ok(RemoteTabResolution {
                host: host.clone(),
                workspace_id: snapshot.workspace_id.clone(),
                tab: tab.clone(),
            });
        }
        if snapshot.status == RemoteProjectionStatus::Available {
            saw_live_snapshot = true;
        }
    }

    if !saw_live_snapshot {
        return Err(RemoteTabResolveError::MetadataStale {
            target: selector.clone(),
            workspace_id: None,
            status: None,
        });
    }

    Err(RemoteTabResolveError::NotFound {
        target: selector.clone(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemotePaneResolution {
    pub(crate) host: RemoteHostConfig,
    pub(crate) workspace_id: String,
    pub(crate) pane_id: String,
    pub(crate) terminal_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemotePaneResolveError {
    NotFound {
        target: RemoteTargetSelector,
        terminal_ids_unavailable: bool,
    },
    ProjectionStale {
        target: RemoteTargetSelector,
        workspace_id: Option<String>,
        status: Option<RemoteProjectionStatus>,
    },
    NoProjection {
        target: RemoteTargetSelector,
    },
    HostNotConnected {
        host: String,
        status: String,
    },
    UnsupportedSelector {
        target: RemoteTargetSelector,
    },
}

impl std::fmt::Display for RemotePaneResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound {
                target,
                terminal_ids_unavailable,
            } => {
                if *terminal_ids_unavailable {
                    write!(
                        f,
                        "remote pane target not found because the cached active-tab projection does not include terminal ids; update the remote Herdr binary: {target:?}"
                    )
                } else {
                    write!(
                        f,
                        "remote pane target not found in live active-tab projections: {target:?}"
                    )
                }
            }
            Self::ProjectionStale {
                target,
                workspace_id,
                status,
            } => {
                let status = status
                    .map(remote_projection_status_label)
                    .unwrap_or("not available");
                match workspace_id {
                    Some(workspace_id) => write!(
                        f,
                        "remote projection for workspace {workspace_id} is {status}; wait for a live projection before mutating target {target:?}"
                    ),
                    None => write!(
                        f,
                        "remote projections are {status}; wait for a live projection before mutating target {target:?}"
                    ),
                }
            }
            Self::NoProjection { target } => write!(
                f,
                "remote pane target has no cached projection to resolve against: {target:?}"
            ),
            Self::HostNotConnected { host, status } => write!(
                f,
                "remote host {host} is {status}; wait for it to reconnect before mutating a remote pane"
            ),
            Self::UnsupportedSelector { target } => write!(
                f,
                "remote pane target must be a pane, terminal, or workspace selector: {target:?}"
            ),
        }
    }
}

impl std::error::Error for RemotePaneResolveError {}

pub(crate) fn resolve_remote_pane_target(
    cache: &RemoteSourceCache,
    host: &RemoteHostConfig,
    selector: &RemoteTargetSelector,
) -> Result<RemotePaneResolution, RemotePaneResolveError> {
    if matches!(
        selector,
        RemoteTargetSelector::Agent(_) | RemoteTargetSelector::Tab(_)
    ) {
        return Err(RemotePaneResolveError::UnsupportedSelector {
            target: selector.clone(),
        });
    }

    let host_key = RemoteHostKey::new(host.name.clone(), host.session.clone());
    let projections = cache.projections_for_host(&host_key);
    if projections.is_empty() {
        return Err(RemotePaneResolveError::NoProjection {
            target: selector.clone(),
        });
    }

    match selector {
        RemoteTargetSelector::Workspace(workspace_id) => {
            resolve_remote_workspace_pane(host, selector, &projections, workspace_id)
        }
        RemoteTargetSelector::Pane(pane_id) => {
            resolve_remote_layout_pane(host, selector, &projections, |layout| {
                find_layout_pane_by_pane_id(&layout.root, pane_id)
            })
        }
        RemoteTargetSelector::Terminal(terminal_id) => {
            let any_terminal_id_projected = std::cell::Cell::new(false);
            resolve_remote_layout_pane_with_terminal_tracking(
                host,
                selector,
                &projections,
                |layout| {
                    if layout_has_terminal_id(&layout.root) {
                        any_terminal_id_projected.set(true);
                    }
                    find_layout_pane_by_terminal_id(&layout.root, terminal_id)
                },
                || !any_terminal_id_projected.get(),
            )
        }
        RemoteTargetSelector::Agent(_) | RemoteTargetSelector::Tab(_) => {
            Err(RemotePaneResolveError::UnsupportedSelector {
                target: selector.clone(),
            })
        }
    }
}

fn resolve_remote_workspace_pane(
    host: &RemoteHostConfig,
    selector: &RemoteTargetSelector,
    projections: &[&RemoteProjectionEntry],
    workspace_id: &str,
) -> Result<RemotePaneResolution, RemotePaneResolveError> {
    let Some(projection) = projections
        .iter()
        .copied()
        .find(|projection| projection.workspace_id == workspace_id)
    else {
        return Err(RemotePaneResolveError::NoProjection {
            target: selector.clone(),
        });
    };
    let layout = live_projection_layout(projection, selector)?;
    let Some(pane) = find_layout_pane_by_pane_id(&layout.root, &layout.focused_pane_id) else {
        return Err(RemotePaneResolveError::NotFound {
            target: selector.clone(),
            terminal_ids_unavailable: false,
        });
    };
    pane_resolution_from_layout_pane(host, layout, pane, selector)
}

fn resolve_remote_layout_pane<'a, F>(
    host: &RemoteHostConfig,
    selector: &RemoteTargetSelector,
    projections: &[&'a RemoteProjectionEntry],
    find: F,
) -> Result<RemotePaneResolution, RemotePaneResolveError>
where
    F: FnMut(
        &'a crate::api::schema::LayoutDescription,
    ) -> Option<&'a crate::api::schema::LayoutPane>,
{
    resolve_remote_layout_pane_with_terminal_tracking(host, selector, projections, find, || false)
}

fn resolve_remote_layout_pane_with_terminal_tracking<'a, F, U>(
    host: &RemoteHostConfig,
    selector: &RemoteTargetSelector,
    projections: &[&'a RemoteProjectionEntry],
    mut find: F,
    terminal_ids_unavailable: U,
) -> Result<RemotePaneResolution, RemotePaneResolveError>
where
    F: FnMut(
        &'a crate::api::schema::LayoutDescription,
    ) -> Option<&'a crate::api::schema::LayoutPane>,
    U: FnOnce() -> bool,
{
    let mut saw_live_projection = false;
    for projection in projections {
        let Some(layout) = projection.layout.as_ref() else {
            continue;
        };
        if let Some(pane) = find(layout) {
            if projection.status != RemoteProjectionStatus::Available {
                return Err(RemotePaneResolveError::ProjectionStale {
                    target: selector.clone(),
                    workspace_id: Some(projection.workspace_id.clone()),
                    status: Some(projection.status),
                });
            }
            return pane_resolution_from_layout_pane(host, layout, pane, selector);
        }
        if projection.status == RemoteProjectionStatus::Available {
            saw_live_projection = true;
        }
    }

    if !saw_live_projection {
        return Err(RemotePaneResolveError::ProjectionStale {
            target: selector.clone(),
            workspace_id: None,
            status: None,
        });
    }

    Err(RemotePaneResolveError::NotFound {
        target: selector.clone(),
        terminal_ids_unavailable: terminal_ids_unavailable(),
    })
}

fn live_projection_layout<'a>(
    projection: &'a RemoteProjectionEntry,
    selector: &RemoteTargetSelector,
) -> Result<&'a crate::api::schema::LayoutDescription, RemotePaneResolveError> {
    let Some(layout) = projection.layout.as_ref() else {
        return Err(RemotePaneResolveError::ProjectionStale {
            target: selector.clone(),
            workspace_id: Some(projection.workspace_id.clone()),
            status: Some(projection.status),
        });
    };
    if projection.status != RemoteProjectionStatus::Available {
        return Err(RemotePaneResolveError::ProjectionStale {
            target: selector.clone(),
            workspace_id: Some(projection.workspace_id.clone()),
            status: Some(projection.status),
        });
    }
    Ok(layout)
}

fn pane_resolution_from_layout_pane(
    host: &RemoteHostConfig,
    layout: &crate::api::schema::LayoutDescription,
    pane: &crate::api::schema::LayoutPane,
    selector: &RemoteTargetSelector,
) -> Result<RemotePaneResolution, RemotePaneResolveError> {
    let Some(pane_id) = pane.pane_id.clone() else {
        return Err(RemotePaneResolveError::NotFound {
            target: selector.clone(),
            terminal_ids_unavailable: false,
        });
    };
    let Some(terminal_id) = pane.terminal_id.clone() else {
        return Err(RemotePaneResolveError::NotFound {
            target: selector.clone(),
            terminal_ids_unavailable: true,
        });
    };
    Ok(RemotePaneResolution {
        host: host.clone(),
        workspace_id: layout.workspace_id.clone(),
        pane_id,
        terminal_id,
    })
}

fn find_layout_pane_by_pane_id<'a>(
    node: &'a crate::api::schema::LayoutNode,
    pane_id: &str,
) -> Option<&'a crate::api::schema::LayoutPane> {
    match node {
        crate::api::schema::LayoutNode::Pane { pane } => {
            (pane.pane_id.as_deref() == Some(pane_id)).then_some(pane)
        }
        crate::api::schema::LayoutNode::Split { first, second, .. } => {
            find_layout_pane_by_pane_id(first, pane_id)
                .or_else(|| find_layout_pane_by_pane_id(second, pane_id))
        }
    }
}

fn find_layout_pane_by_terminal_id<'a>(
    node: &'a crate::api::schema::LayoutNode,
    terminal_id: &str,
) -> Option<&'a crate::api::schema::LayoutPane> {
    match node {
        crate::api::schema::LayoutNode::Pane { pane } => {
            (pane.terminal_id.as_deref() == Some(terminal_id)).then_some(pane)
        }
        crate::api::schema::LayoutNode::Split { first, second, .. } => {
            find_layout_pane_by_terminal_id(first, terminal_id)
                .or_else(|| find_layout_pane_by_terminal_id(second, terminal_id))
        }
    }
}

fn layout_has_terminal_id(node: &crate::api::schema::LayoutNode) -> bool {
    match node {
        crate::api::schema::LayoutNode::Pane { pane } => pane.terminal_id.is_some(),
        crate::api::schema::LayoutNode::Split { first, second, .. } => {
            layout_has_terminal_id(first) || layout_has_terminal_id(second)
        }
    }
}

fn remote_projection_status_label(status: RemoteProjectionStatus) -> &'static str {
    match status {
        RemoteProjectionStatus::Available => "available",
        RemoteProjectionStatus::Unavailable => "unavailable",
        RemoteProjectionStatus::StaleLastKnown => "stale",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::api::schema::{
        AgentInfo, AgentStatus, LayoutDescription, LayoutNode, LayoutPane, SplitDirection, TabInfo,
        WorkspaceInfo,
    };
    use crate::remote_source::{
        RemoteAgentKey, RemoteConnectionStatus, RemoteHostKey, RemoteProjectionSnapshot,
        RemoteProjectionStatus, RemoteSourceCache,
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
            screen_detection_skipped: false,
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

    fn workspace(workspace_id: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            workspace_id: workspace_id.to_string(),
            number: 1,
            label: workspace_id.to_string(),
            focused: false,
            pane_count: 1,
            tab_count: 1,
            active_tab_id: format!("{workspace_id}:tab-active"),
            agent_status: AgentStatus::Unknown,
            worktree: None,
        }
    }

    fn tab(workspace_id: &str, tab_id: &str, focused: bool) -> TabInfo {
        TabInfo {
            tab_id: tab_id.to_string(),
            workspace_id: workspace_id.to_string(),
            number: 1,
            label: tab_id.to_string(),
            focused,
            pane_count: 1,
            agent_status: AgentStatus::Unknown,
        }
    }

    fn projected_layout(workspace_id: &str) -> LayoutDescription {
        LayoutDescription {
            workspace_id: workspace_id.to_string(),
            tab_id: format!("{workspace_id}:tab-active"),
            zoomed: false,
            focused_pane_id: format!("{workspace_id}:pane-focused"),
            root: LayoutNode::Split {
                direction: SplitDirection::Right,
                ratio: 0.5,
                first: Box::new(LayoutNode::Pane {
                    pane: LayoutPane {
                        pane_id: Some(format!("{workspace_id}:pane-focused")),
                        label: Some("focused".to_string()),
                        terminal_id: Some(format!("{workspace_id}:term-focused")),
                        ..Default::default()
                    },
                }),
                second: Box::new(LayoutNode::Pane {
                    pane: LayoutPane {
                        pane_id: Some(format!("{workspace_id}:pane-side")),
                        label: Some("side".to_string()),
                        terminal_id: Some(format!("{workspace_id}:term-side")),
                        ..Default::default()
                    },
                }),
            },
        }
    }

    fn projection_snapshot(
        workspace_id: &str,
        status: RemoteProjectionStatus,
        layout: Option<LayoutDescription>,
    ) -> RemoteProjectionSnapshot {
        RemoteProjectionSnapshot {
            workspace_id: workspace_id.to_string(),
            tab_id: Some(format!("{workspace_id}:tab-active")),
            tab_label: Some("active".to_string()),
            status,
            layout,
        }
    }

    fn cache_with_projection(
        host: &RemoteHostKey,
        snapshot: RemoteProjectionSnapshot,
    ) -> RemoteSourceCache {
        let mut cache = RemoteSourceCache::default();
        cache.apply_projection_snapshot(host, vec![snapshot]);
        cache
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
        for target in ["pane:1-1", "terminal:term_abc", "workspace:w1", "tab:t1"] {
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
        assert_eq!(
            parse_target_route("jafar/tab:t1").unwrap(),
            TargetRoute::Remote {
                host: "jafar".to_string(),
                target: RemoteTargetSelector::Tab("t1".to_string()),
            }
        );
    }

    #[test]
    fn remote_target_rejects_empty_typed_handle_payloads() {
        for target in [
            "jafar/pane:",
            "jafar/terminal:",
            "jafar/workspace:",
            "jafar/tab:",
        ] {
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
    fn remote_target_registry_rejects_option_like_ssh_target() {
        let err = RemoteHostRegistry::from_configs(vec![RemoteHostConfig::new(
            "jafar",
            "-oProxyCommand=sh",
            "default",
            true,
        )])
        .unwrap_err();

        assert_eq!(err, RemoteHostConfigError::SshTargetStartsWithDash);
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
    fn remote_target_config_new_defaults_connect_timeout_to_ten_seconds() {
        let config = RemoteHostConfig::new("jafar", "jafar", "default", true);
        assert_eq!(config.connect_timeout_secs, DEFAULT_CONNECT_TIMEOUT_SECS);
        assert_eq!(config.connect_timeout_secs, 10);
    }

    #[test]
    fn remote_target_config_accepts_custom_connect_timeout() {
        let config =
            RemoteHostConfig::new("jafar", "jafar", "default", true).with_connect_timeout_secs(30);
        assert_eq!(config.connect_timeout_secs, 30);
    }

    #[test]
    fn remote_target_registry_rejects_zero_connect_timeout() {
        let err = RemoteHostRegistry::from_configs(vec![RemoteHostConfig::new(
            "jafar", "jafar", "default", true,
        )
        .with_connect_timeout_secs(0)])
        .unwrap_err();

        assert_eq!(err, RemoteHostConfigError::ConnectTimeoutZero);
    }

    #[test]
    fn remote_target_registry_rejects_excessive_connect_timeout() {
        let err = RemoteHostRegistry::from_configs(vec![RemoteHostConfig::new(
            "jafar", "jafar", "default", true,
        )
        .with_connect_timeout_secs(MAX_CONNECT_TIMEOUT_SECS + 1)])
        .unwrap_err();

        assert_eq!(
            err,
            RemoteHostConfigError::ConnectTimeoutTooLarge {
                value: MAX_CONNECT_TIMEOUT_SECS + 1,
                max: MAX_CONNECT_TIMEOUT_SECS,
            }
        );
    }

    #[test]
    fn remote_target_registry_accepts_custom_connect_timeout_and_preserves_auto_connect() {
        let registry = RemoteHostRegistry::from_configs(vec![RemoteHostConfig::new(
            "jafar", "jafar", "default", true,
        )
        .with_connect_timeout_secs(45)])
        .unwrap();

        let host = registry.get("jafar").unwrap();
        assert_eq!(host.connect_timeout_secs, 45);
        assert!(host.auto_connect);
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
        cache.mark_status(
            &host_key,
            crate::remote_source::RemoteConnectionStatus::Disconnected,
        );

        let resolved = resolve_remote_agent_target(
            &cache,
            &host,
            &RemoteTargetSelector::Agent("codex".to_string()),
        )
        .unwrap();

        assert!(resolved.entry.stale());
        assert_eq!(resolved.entry.status, RemoteConnectionStatus::Disconnected);
    }

    #[test]
    fn remote_workspace_target_resolves_from_workspace_snapshot() {
        let host = host_config("jafar", "default");
        let host_key = RemoteHostKey::new("jafar", "default");
        let mut cache = RemoteSourceCache::default();
        cache.replace_workspace_snapshot(host_key, vec![workspace("remote-ws")]);

        let resolved = resolve_remote_workspace_target(
            &cache,
            &host,
            &RemoteTargetSelector::Workspace("remote-ws".to_string()),
        )
        .unwrap();

        assert_eq!(resolved.host, host);
        assert_eq!(resolved.workspace.workspace_id, "remote-ws");
    }

    #[test]
    fn remote_workspace_target_rejects_missing_or_wrong_selector() {
        let host = host_config("jafar", "default");
        let cache = RemoteSourceCache::default();

        assert_eq!(
            resolve_remote_workspace_target(
                &cache,
                &host,
                &RemoteTargetSelector::Workspace("missing".to_string())
            )
            .unwrap_err(),
            RemoteWorkspaceResolveError::MetadataUnavailable {
                target: RemoteTargetSelector::Workspace("missing".to_string())
            }
        );
        assert_eq!(
            resolve_remote_workspace_target(
                &cache,
                &host,
                &RemoteTargetSelector::Tab("tab-1".to_string())
            )
            .unwrap_err(),
            RemoteWorkspaceResolveError::UnsupportedSelector {
                target: RemoteTargetSelector::Tab("tab-1".to_string())
            }
        );
    }

    #[test]
    fn remote_tab_target_resolves_from_live_tab_snapshot() {
        let host = host_config("jafar", "default");
        let host_key = RemoteHostKey::new("jafar", "default");
        let mut cache = RemoteSourceCache::default();
        cache.replace_tab_snapshot(
            &host_key,
            "remote-ws",
            vec![
                tab("remote-ws", "tab-1", false),
                tab("remote-ws", "tab-2", true),
            ],
        );

        let resolved = resolve_remote_tab_target(
            &cache,
            &host,
            &RemoteTargetSelector::Tab("tab-2".to_string()),
        )
        .unwrap();

        assert_eq!(resolved.host, host);
        assert_eq!(resolved.workspace_id, "remote-ws");
        assert_eq!(resolved.tab.tab_id, "tab-2");
    }

    #[test]
    fn remote_tab_target_rejects_stale_missing_and_wrong_selector() {
        let host = host_config("jafar", "default");
        let host_key = RemoteHostKey::new("jafar", "default");
        let mut cache = RemoteSourceCache::default();
        cache.replace_tab_snapshot(
            &host_key,
            "remote-ws",
            vec![tab("remote-ws", "tab-1", true)],
        );
        cache.mark_status(&host_key, RemoteConnectionStatus::Disconnected);

        assert_eq!(
            resolve_remote_tab_target(
                &cache,
                &host,
                &RemoteTargetSelector::Tab("tab-1".to_string())
            )
            .unwrap_err(),
            RemoteTabResolveError::MetadataStale {
                target: RemoteTargetSelector::Tab("tab-1".to_string()),
                workspace_id: Some("remote-ws".to_string()),
                status: Some(RemoteProjectionStatus::StaleLastKnown),
            }
        );

        let empty_cache = RemoteSourceCache::default();
        assert_eq!(
            resolve_remote_tab_target(
                &empty_cache,
                &host,
                &RemoteTargetSelector::Tab("missing".to_string())
            )
            .unwrap_err(),
            RemoteTabResolveError::MetadataUnavailable {
                target: RemoteTargetSelector::Tab("missing".to_string())
            }
        );
        assert_eq!(
            resolve_remote_tab_target(
                &empty_cache,
                &host,
                &RemoteTargetSelector::Workspace("remote-ws".to_string())
            )
            .unwrap_err(),
            RemoteTabResolveError::UnsupportedSelector {
                target: RemoteTargetSelector::Workspace("remote-ws".to_string())
            }
        );
    }

    #[test]
    fn remote_pane_target_resolves_terminal_selector_from_projection() {
        let host = host_config("jafar", "default");
        let host_key = RemoteHostKey::new("jafar", "default");
        let cache = cache_with_projection(
            &host_key,
            projection_snapshot(
                "remote-ws",
                RemoteProjectionStatus::Available,
                Some(projected_layout("remote-ws")),
            ),
        );

        let resolved = resolve_remote_pane_target(
            &cache,
            &host,
            &RemoteTargetSelector::Terminal("remote-ws:term-side".to_string()),
        )
        .unwrap();

        assert_eq!(resolved.host, host);
        assert_eq!(resolved.workspace_id, "remote-ws");
        assert_eq!(resolved.pane_id, "remote-ws:pane-side");
        assert_eq!(resolved.terminal_id, "remote-ws:term-side");
    }

    #[test]
    fn remote_pane_target_resolves_pane_selector_from_projection() {
        let host = host_config("jafar", "default");
        let host_key = RemoteHostKey::new("jafar", "default");
        let cache = cache_with_projection(
            &host_key,
            projection_snapshot(
                "remote-ws",
                RemoteProjectionStatus::Available,
                Some(projected_layout("remote-ws")),
            ),
        );

        let resolved = resolve_remote_pane_target(
            &cache,
            &host,
            &RemoteTargetSelector::Pane("remote-ws:pane-side".to_string()),
        )
        .unwrap();

        assert_eq!(resolved.workspace_id, "remote-ws");
        assert_eq!(resolved.pane_id, "remote-ws:pane-side");
        assert_eq!(resolved.terminal_id, "remote-ws:term-side");
    }

    #[test]
    fn remote_pane_target_resolves_workspace_selector_to_focused_pane() {
        let host = host_config("jafar", "default");
        let host_key = RemoteHostKey::new("jafar", "default");
        let cache = cache_with_projection(
            &host_key,
            projection_snapshot(
                "remote-ws",
                RemoteProjectionStatus::Available,
                Some(projected_layout("remote-ws")),
            ),
        );

        let resolved = resolve_remote_pane_target(
            &cache,
            &host,
            &RemoteTargetSelector::Workspace("remote-ws".to_string()),
        )
        .unwrap();

        assert_eq!(resolved.workspace_id, "remote-ws");
        assert_eq!(resolved.pane_id, "remote-ws:pane-focused");
        assert_eq!(resolved.terminal_id, "remote-ws:term-focused");
    }

    #[test]
    fn remote_pane_target_returns_not_found_for_missing_live_projection_target() {
        let host = host_config("jafar", "default");
        let host_key = RemoteHostKey::new("jafar", "default");
        let cache = cache_with_projection(
            &host_key,
            projection_snapshot(
                "remote-ws",
                RemoteProjectionStatus::Available,
                Some(projected_layout("remote-ws")),
            ),
        );

        assert_eq!(
            resolve_remote_pane_target(
                &cache,
                &host,
                &RemoteTargetSelector::Pane("remote-ws:pane-missing".to_string())
            )
            .unwrap_err(),
            RemotePaneResolveError::NotFound {
                target: RemoteTargetSelector::Pane("remote-ws:pane-missing".to_string()),
                terminal_ids_unavailable: false,
            }
        );
    }

    #[test]
    fn remote_pane_target_rejects_stale_projection_for_mutation() {
        let host = host_config("jafar", "default");
        let host_key = RemoteHostKey::new("jafar", "default");
        let mut cache = RemoteSourceCache::default();
        cache.apply_projection_snapshot(
            &host_key,
            vec![projection_snapshot(
                "remote-ws",
                RemoteProjectionStatus::Available,
                Some(projected_layout("remote-ws")),
            )],
        );
        cache.apply_projection_snapshot(
            &host_key,
            vec![projection_snapshot(
                "remote-ws",
                RemoteProjectionStatus::Unavailable,
                None,
            )],
        );

        assert_eq!(
            resolve_remote_pane_target(
                &cache,
                &host,
                &RemoteTargetSelector::Pane("remote-ws:pane-focused".to_string())
            )
            .unwrap_err(),
            RemotePaneResolveError::ProjectionStale {
                target: RemoteTargetSelector::Pane("remote-ws:pane-focused".to_string()),
                workspace_id: Some("remote-ws".to_string()),
                status: Some(RemoteProjectionStatus::StaleLastKnown),
            }
        );
    }

    #[test]
    fn remote_pane_target_returns_no_projection_for_uncached_host_or_workspace() {
        let host = host_config("jafar", "default");
        let empty_cache = RemoteSourceCache::default();

        assert_eq!(
            resolve_remote_pane_target(
                &empty_cache,
                &host,
                &RemoteTargetSelector::Terminal("term-1".to_string())
            )
            .unwrap_err(),
            RemotePaneResolveError::NoProjection {
                target: RemoteTargetSelector::Terminal("term-1".to_string()),
            }
        );

        let host_key = RemoteHostKey::new("jafar", "default");
        let cache = cache_with_projection(
            &host_key,
            projection_snapshot(
                "remote-ws",
                RemoteProjectionStatus::Available,
                Some(projected_layout("remote-ws")),
            ),
        );

        assert_eq!(
            resolve_remote_pane_target(
                &cache,
                &host,
                &RemoteTargetSelector::Workspace("missing-ws".to_string())
            )
            .unwrap_err(),
            RemotePaneResolveError::NoProjection {
                target: RemoteTargetSelector::Workspace("missing-ws".to_string()),
            }
        );
    }

    #[test]
    fn remote_pane_target_rejects_agent_selector() {
        let host = host_config("jafar", "default");
        let host_key = RemoteHostKey::new("jafar", "default");
        let cache = cache_with_projection(
            &host_key,
            projection_snapshot(
                "remote-ws",
                RemoteProjectionStatus::Available,
                Some(projected_layout("remote-ws")),
            ),
        );

        assert_eq!(
            resolve_remote_pane_target(
                &cache,
                &host,
                &RemoteTargetSelector::Agent("codex".to_string())
            )
            .unwrap_err(),
            RemotePaneResolveError::UnsupportedSelector {
                target: RemoteTargetSelector::Agent("codex".to_string()),
            }
        );
    }

    #[test]
    fn remote_pane_target_distinguishes_older_projection_without_terminal_ids() {
        let host = host_config("jafar", "default");
        let host_key = RemoteHostKey::new("jafar", "default");
        let mut layout = projected_layout("remote-ws");
        if let LayoutNode::Split { first, second, .. } = &mut layout.root {
            if let LayoutNode::Pane { pane } = first.as_mut() {
                pane.terminal_id = None;
            }
            if let LayoutNode::Pane { pane } = second.as_mut() {
                pane.terminal_id = None;
            }
        }
        let cache = cache_with_projection(
            &host_key,
            projection_snapshot("remote-ws", RemoteProjectionStatus::Available, Some(layout)),
        );

        let err = resolve_remote_pane_target(
            &cache,
            &host,
            &RemoteTargetSelector::Terminal("remote-ws:term-focused".to_string()),
        )
        .unwrap_err();

        assert_eq!(
            err,
            RemotePaneResolveError::NotFound {
                target: RemoteTargetSelector::Terminal("remote-ws:term-focused".to_string()),
                terminal_ids_unavailable: true,
            }
        );
        assert!(err.to_string().contains("does not include terminal ids"));
    }

    #[test]
    fn remote_pane_target_background_tab_target_is_not_found() {
        let host = host_config("jafar", "default");
        let host_key = RemoteHostKey::new("jafar", "default");
        let cache = cache_with_projection(
            &host_key,
            projection_snapshot(
                "remote-ws",
                RemoteProjectionStatus::Available,
                Some(projected_layout("remote-ws")),
            ),
        );

        assert_eq!(
            resolve_remote_pane_target(
                &cache,
                &host,
                &RemoteTargetSelector::Pane("remote-ws:pane-background-tab".to_string())
            )
            .unwrap_err(),
            RemotePaneResolveError::NotFound {
                target: RemoteTargetSelector::Pane("remote-ws:pane-background-tab".to_string()),
                terminal_ids_unavailable: false,
            }
        );
    }
}
