//! Pure parsing and read-only cache resolution for future host-qualified remote target routing.
//!
//! This module classifies target strings and resolves them against a read-only
//! `RemoteSourceCache` snapshot. It does not open bridges, perform IO, or execute
//! command routing.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::remote_source::{RemoteAgentEntry, RemoteAgentKey, RemoteHostKey, RemoteSourceCache};

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

/// Per-host connection policy controlling whether the local aggregator probes a
/// configured remote host automatically and whether explicit on-demand mutating
/// commands may reach it without a prior live connection.
///
/// This enum is the single stored source of truth on [`RemoteHostConfig`]; the
/// legacy `auto_connect` TOML boolean is accepted only as a backward-compatible
/// alias (see [`resolve_connection_policy`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RemoteConnectionPolicy {
    /// The host is probed/connected automatically at startup and on config
    /// reload by the remote source supervisor, seeded as a disconnected remote
    /// source, and treated as a configured auto source event sender. Explicit
    /// mutating commands still fail fast when its cached status is non-connected.
    /// Equivalent to legacy `auto_connect = true`.
    #[default]
    Auto,
    /// The host is not probed automatically, but an explicit on-demand mutating
    /// command (e.g. `agent.start --host`) may attempt a live bridge dispatch
    /// when there is no cached non-connected status; a cached
    /// disconnected/unreachable/needs-update status still fails fast before
    /// forwarding. Equivalent to legacy `auto_connect = false`.
    OnDemand,
    /// The host is never reached implicitly. It is not probed automatically, and
    /// an explicit mutating `agent.start --host` fails locally before dispatch
    /// with a distinct policy error. Use this for sleeping/roaming remotes that
    /// must not be woken or auto-probed just because they are configured.
    Manual,
}

impl RemoteConnectionPolicy {
    /// Whether this host is eligible for automatic scheduler/orchestrator
    /// consideration: started/probed automatically by the remote source
    /// supervisor at startup and on config reload, seeded as a disconnected
    /// remote source, and treated as a configured auto source event sender.
    /// Only [`RemoteConnectionPolicy::Auto`] qualifies; `OnDemand` and
    /// `Manual` hosts are excluded so sleeping/roaming remotes are never
    /// probed or woken by background scheduling.
    pub(crate) fn starts_automatically(self) -> bool {
        matches!(self, RemoteConnectionPolicy::Auto)
    }

    /// Whether this policy refuses explicit mutating dispatch
    /// (e.g. `agent.start --host`). Only [`RemoteConnectionPolicy::Manual`]
    /// qualifies; `Auto` and `OnDemand` allow explicit start. The positive
    /// counterpart is [`Self::allows_explicit_start`].
    pub(crate) fn is_manual(self) -> bool {
        matches!(self, RemoteConnectionPolicy::Manual)
    }

    /// Whether an explicit mutating start command (e.g. `agent.start --host`)
    /// may proceed for this policy. `Auto` and `OnDemand` return `true`;
    /// `Manual` returns `false`. This is the positive counterpart to
    /// [`Self::is_manual`] and is the intended guard for dispatch sites so the
    /// intent — "is explicit start allowed?" — reads forward rather than as a
    /// negated `is_manual` check.
    pub(crate) fn allows_explicit_start(self) -> bool {
        !self.is_manual()
    }

    pub(crate) fn as_toml_str(self) -> &'static str {
        match self {
            RemoteConnectionPolicy::Auto => "auto",
            RemoteConnectionPolicy::OnDemand => "on_demand",
            RemoteConnectionPolicy::Manual => "manual",
        }
    }

    /// Whether this policy is consistent with a legacy `auto_connect` boolean.
    /// `Auto` is consistent with `true`; `OnDemand` and `Manual` with `false`.
    fn is_consistent_with_legacy(self, auto_connect: bool) -> bool {
        match self {
            RemoteConnectionPolicy::Auto => auto_connect,
            RemoteConnectionPolicy::OnDemand | RemoteConnectionPolicy::Manual => !auto_connect,
        }
    }
}

/// Error raised when an explicit `connection_policy` and a legacy
/// `auto_connect` boolean are both present but inconsistent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteConnectionPolicyConflict {
    policy: RemoteConnectionPolicy,
    auto_connect: bool,
}

impl std::fmt::Display for RemoteConnectionPolicyConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.policy {
            RemoteConnectionPolicy::Auto => write!(
                f,
                "remote connection_policy = \"auto\" requires auto_connect = true, but auto_connect = {} was set",
                self.auto_connect
            ),
            policy => write!(
                f,
                "remote connection_policy = \"{}\" requires auto_connect = false, but auto_connect = {} was set",
                policy.as_toml_str(),
                self.auto_connect
            ),
        }
    }
}

impl std::error::Error for RemoteConnectionPolicyConflict {}

/// Resolve the canonical connection policy from the explicit
/// `connection_policy` TOML field and the legacy `auto_connect` boolean,
/// rejecting inconsistent combinations with a clear error.
///
/// Presence of each input is detected independently by the caller (the custom
/// [`Deserialize`] impl on [`RemoteHostConfig`]), so a missing `auto_connect`
/// never conflicts with the default [`RemoteConnectionPolicy::Auto`] policy and
/// `auto_connect = false` alone resolves to [`RemoteConnectionPolicy::OnDemand`].
fn resolve_connection_policy(
    explicit: Option<RemoteConnectionPolicy>,
    legacy: Option<bool>,
) -> Result<RemoteConnectionPolicy, RemoteConnectionPolicyConflict> {
    match (explicit, legacy) {
        (Some(policy), Some(auto_connect)) => {
            if policy.is_consistent_with_legacy(auto_connect) {
                Ok(policy)
            } else {
                Err(RemoteConnectionPolicyConflict {
                    policy,
                    auto_connect,
                })
            }
        }
        (Some(policy), None) => Ok(policy),
        (None, Some(true)) => Ok(RemoteConnectionPolicy::Auto),
        (None, Some(false)) => Ok(RemoteConnectionPolicy::OnDemand),
        (None, None) => Ok(RemoteConnectionPolicy::Auto),
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteHostConfig {
    pub(crate) name: String,
    pub(crate) target: String,
    pub(crate) session: String,
    /// Per-host connection policy. Single stored source of truth; defaults to
    /// [`RemoteConnectionPolicy::Auto`] when neither `connection_policy` nor
    /// the legacy `auto_connect` field is set. See [`RemoteConnectionPolicy`]
    /// and [`resolve_connection_policy`].
    pub(crate) connection_policy: RemoteConnectionPolicy,
    /// SSH `ConnectTimeout` in whole seconds for connection attempts to this
    /// host. Applies to both interactive and noninteractive configured-host
    /// SSH invocations. Default: 10 seconds.
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
            // The constructor keeps the legacy `auto_connect` boolean shape so
            // existing call sites keep compiling; `true` maps to `Auto` and
            // `false` to `OnDemand`. A `Manual` host is built via TOML or via
            // [`RemoteHostConfig::with_connection_policy`].
            connection_policy: if auto_connect {
                RemoteConnectionPolicy::Auto
            } else {
                RemoteConnectionPolicy::OnDemand
            },
            connect_timeout_secs: DEFAULT_CONNECT_TIMEOUT_SECS,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_connect_timeout_secs(mut self, connect_timeout_secs: u32) -> Self {
        self.connect_timeout_secs = connect_timeout_secs;
        self
    }

    /// Build a configured remote host from explicit federation fields.
    ///
    /// Used by the config-only `remote add` mutation path, which resolves
    /// `connection_policy` and `connect_timeout_secs` directly from CLI options
    /// rather than the legacy `auto_connect` boolean. Unlike [`Self::new`],
    /// this is available on all targets because `remote add` only edits local
    /// config and never opens an SSH bridge. The caller still validates the
    /// resulting host through [`RemoteHostRegistry::from_configs`] before
    /// writing config.
    pub(crate) fn from_explicit_fields(
        name: impl Into<String>,
        target: impl Into<String>,
        session: impl Into<String>,
        connection_policy: RemoteConnectionPolicy,
        connect_timeout_secs: u32,
    ) -> Self {
        Self {
            name: name.into(),
            target: target.into(),
            session: session.into(),
            connection_policy,
            connect_timeout_secs,
        }
    }

    /// Override the connection policy. Test-only helper for building hosts
    /// whose policy is not reachable from the legacy bool constructor (e.g.
    /// [`RemoteConnectionPolicy::Manual`]).
    #[cfg(test)]
    pub(crate) fn with_connection_policy(mut self, policy: RemoteConnectionPolicy) -> Self {
        self.connection_policy = policy;
        self
    }
}

impl<'de> Deserialize<'de> for RemoteHostConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Helper struct so serde can detect presence of `auto_connect` and
        // `connection_policy` independently via `Option<...>` defaults. A naive
        // defaulted `bool` would make `auto_connect = false` indistinguishable
        // from an omitted field and could spuriously conflict with the default
        // `Auto` policy.
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        struct HostConfigToml {
            name: String,
            target: String,
            #[serde(default = "default_remote_session_name")]
            session: String,
            #[serde(default)]
            auto_connect: Option<bool>,
            #[serde(default)]
            connection_policy: Option<RemoteConnectionPolicy>,
            #[serde(default = "default_connect_timeout_secs")]
            connect_timeout_secs: u32,
        }

        let raw = HostConfigToml::deserialize(deserializer)?;
        let connection_policy = resolve_connection_policy(raw.connection_policy, raw.auto_connect)
            .map_err(serde::de::Error::custom)?;
        Ok(RemoteHostConfig {
            name: raw.name,
            target: raw.target,
            session: raw.session,
            connection_policy,
            connect_timeout_secs: raw.connect_timeout_secs,
        })
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

/// Shell-quote a single POSIX shell argument using single-quote escaping.
///
/// Only non-empty values whose characters are all ASCII alphanumeric or one
/// of `@ % _ + = : , . / -` are left unquoted; every other value (empty
/// strings, whitespace, and shell metacharacters such as `;`, `&`, `|`, `$`,
/// backticks, quotes, and backslashes) is wrapped in single quotes, with
/// embedded single quotes escaped via the standard ` '\'' ` sequence. This
/// mirrors the stricter private quoting helper already used by the remote SSH
/// attach/install path (`src/remote/unix.rs`) so the copied command is safe
/// to paste into a POSIX shell. It is a focused, dependency-free helper
/// rather than a new external crate.
pub(crate) fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Build the safe, non-mutating diagnostics command for a configured-host
/// alias. Shape: `herdr remote status <quoted-alias> && herdr remote check
/// <quoted-alias>`. Both `remote status` and `remote check` are read-only and
/// never spawn or mutate remote state from this command string; running it is
/// the user's explicit choice after copying it.
pub(crate) fn remote_diagnostics_command(alias: &str) -> String {
    let quoted = shell_quote(alias);
    format!("herdr remote status {quoted} && herdr remote check {quoted}")
}

/// Build the explicit full remote Herdr client command from a host config's
/// configured SSH target and session. Shape: `herdr --remote <quoted-target>
/// --session <quoted-session>`. Uses the raw SSH `target` and configured
/// `session`, never the alias, matching the `herdr --remote` CLI contract.
pub(crate) fn remote_full_command(target: &str, session: &str) -> String {
    format!(
        "herdr --remote {} --session {}",
        shell_quote(target),
        shell_quote(session)
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::api::schema::{AgentInfo, AgentStatus, WorkspaceInfo};
    use crate::remote_source::{
        RemoteAgentKey, RemoteConnectionStatus, RemoteHostKey, RemoteSourceCache,
    };

    use super::*;

    #[test]
    fn shell_quote_leaves_safe_tokens_unquoted() {
        assert_eq!(shell_quote("jafar"), "jafar");
        assert_eq!(shell_quote("user@host"), "user@host");
        assert_eq!(shell_quote("10.0.0.5"), "10.0.0.5");
    }

    #[test]
    fn shell_quote_wraps_empty_and_unsafe_values() {
        assert_eq!(shell_quote(""), "''");
        // Whitespace forces single-quote wrapping.
        assert_eq!(shell_quote("a b"), "'a b'");
        // Single quotes are escaped via the standard '\'\'' sequence: one
        // backslash before the embedded quote in the rendered command text.
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        // Double quotes, backslashes, and shell metacharacters also force wrapping.
        assert_eq!(shell_quote("a$b"), "'a$b'");
        assert_eq!(shell_quote("a|b"), "'a|b'");
        // Command separators/backgrounding characters must also be quoted --
        // these are not in the safe-unquoted allow-list.
        assert_eq!(shell_quote("ja;far"), "'ja;far'");
        assert_eq!(shell_quote("a&b"), "'a&b'");
    }

    #[test]
    fn shell_quote_leaves_allow_listed_punctuation_unquoted() {
        // Every char in the safe-unquoted set together in one token.
        assert_eq!(shell_quote("user@host:2222"), "user@host:2222");
        assert_eq!(shell_quote("a_b+c=d,e.f/g-h%i"), "a_b+c=d,e.f/g-h%i");
    }

    #[test]
    fn remote_diagnostics_command_quotes_alias_in_both_positions() {
        let cmd = remote_diagnostics_command("jafar");
        assert_eq!(cmd, "herdr remote status jafar && herdr remote check jafar");

        // An unsafe alias is quoted in both positions.
        let cmd = remote_diagnostics_command("ja far");
        assert_eq!(
            cmd,
            "herdr remote status 'ja far' && herdr remote check 'ja far'"
        );

        // A shell-metacharacter alias (`;`) is quoted in both positions too.
        let cmd = remote_diagnostics_command("ja;far");
        assert_eq!(
            cmd,
            "herdr remote status 'ja;far' && herdr remote check 'ja;far'"
        );
    }

    #[test]
    fn remote_full_command_uses_target_and_session_not_alias() {
        // target differs from a typical alias; the command must use the target.
        let cmd = remote_full_command("user@10.0.0.5", "default");
        assert_eq!(cmd, "herdr --remote user@10.0.0.5 --session default");
    }

    #[test]
    fn remote_full_command_quotes_target_and_session_safely() {
        // A session name with a space is quoted; a target with a shell
        // metacharacter is quoted.
        let cmd = remote_full_command("host'a", "my session");
        assert_eq!(cmd, "herdr --remote 'host'\\''a' --session 'my session'");
    }

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
        assert!(original.connection_policy.starts_automatically());
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
        assert!(host.connection_policy.starts_automatically());
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

    #[derive(Deserialize)]
    struct HostsDoc {
        hosts: Vec<RemoteHostConfig>,
    }

    fn parse_hosts(body: &str) -> Result<Vec<RemoteHostConfig>, toml::de::Error> {
        toml::from_str::<HostsDoc>(&format!("[[hosts]]\n{body}")).map(|doc| doc.hosts)
    }

    #[test]
    fn connection_policy_defaults_to_auto_when_neither_field_is_set() {
        let hosts = parse_hosts(
            r#"
name = "jafar"
target = "jafar"
session = "default"
"#,
        )
        .expect("parses");
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].connection_policy, RemoteConnectionPolicy::Auto);
    }

    #[test]
    fn connection_policy_parses_explicit_auto_on_demand_manual() {
        for (value, expected) in [
            ("auto", RemoteConnectionPolicy::Auto),
            ("on_demand", RemoteConnectionPolicy::OnDemand),
            ("manual", RemoteConnectionPolicy::Manual),
        ] {
            let hosts = parse_hosts(&format!(
                r#"
name = "jafar"
target = "jafar"
session = "default"
connection_policy = "{value}"
"#
            ))
            .unwrap_or_else(|err| panic!("parsing {value} failed: {err}"));
            assert_eq!(hosts[0].connection_policy, expected, "value {value}");
        }
    }

    #[test]
    fn legacy_auto_connect_false_resolves_to_on_demand() {
        let hosts = parse_hosts(
            r#"
name = "jafar"
target = "jafar"
session = "default"
auto_connect = false
"#,
        )
        .expect("parses");
        assert_eq!(hosts[0].connection_policy, RemoteConnectionPolicy::OnDemand);
    }

    #[test]
    fn legacy_auto_connect_true_resolves_to_auto() {
        let hosts = parse_hosts(
            r#"
name = "jafar"
target = "jafar"
session = "default"
auto_connect = true
"#,
        )
        .expect("parses");
        assert_eq!(hosts[0].connection_policy, RemoteConnectionPolicy::Auto);
    }

    #[test]
    fn resolve_connection_policy_accepts_consistent_legacy_boolean() {
        // auto + auto_connect = true; on_demand / manual + auto_connect = false.
        assert_eq!(
            resolve_connection_policy(Some(RemoteConnectionPolicy::Auto), Some(true)),
            Ok(RemoteConnectionPolicy::Auto)
        );
        assert_eq!(
            resolve_connection_policy(Some(RemoteConnectionPolicy::OnDemand), Some(false)),
            Ok(RemoteConnectionPolicy::OnDemand)
        );
        assert_eq!(
            resolve_connection_policy(Some(RemoteConnectionPolicy::Manual), Some(false)),
            Ok(RemoteConnectionPolicy::Manual)
        );
        // Explicit policy with no legacy boolean always wins.
        assert_eq!(
            resolve_connection_policy(Some(RemoteConnectionPolicy::Manual), None),
            Ok(RemoteConnectionPolicy::Manual)
        );
    }

    #[test]
    fn resolve_connection_policy_rejects_conflicting_legacy_boolean() {
        assert_eq!(
            resolve_connection_policy(Some(RemoteConnectionPolicy::Auto), Some(false)),
            Err(RemoteConnectionPolicyConflict {
                policy: RemoteConnectionPolicy::Auto,
                auto_connect: false
            })
        );
        assert_eq!(
            resolve_connection_policy(Some(RemoteConnectionPolicy::OnDemand), Some(true)),
            Err(RemoteConnectionPolicyConflict {
                policy: RemoteConnectionPolicy::OnDemand,
                auto_connect: true
            })
        );
        assert_eq!(
            resolve_connection_policy(Some(RemoteConnectionPolicy::Manual), Some(true)),
            Err(RemoteConnectionPolicyConflict {
                policy: RemoteConnectionPolicy::Manual,
                auto_connect: true
            })
        );
        // Error message names both fields so the conflict is actionable.
        let message = resolve_connection_policy(Some(RemoteConnectionPolicy::OnDemand), Some(true))
            .unwrap_err()
            .to_string();
        assert!(message.contains("connection_policy = \"on_demand\""));
        assert!(message.contains("auto_connect = true"));
    }

    #[test]
    fn connection_policy_toml_rejects_conflicting_legacy_boolean() {
        let err = parse_hosts(
            r#"
name = "jafar"
target = "jafar"
session = "default"
connection_policy = "on_demand"
auto_connect = true
"#,
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("connection_policy"),
            "expected conflict message, got: {message}"
        );
        assert!(message.contains("auto_connect"));
    }

    #[test]
    fn connection_policy_starts_automatically_only_for_auto() {
        assert!(RemoteConnectionPolicy::Auto.starts_automatically());
        assert!(!RemoteConnectionPolicy::OnDemand.starts_automatically());
        assert!(!RemoteConnectionPolicy::Manual.starts_automatically());
        assert!(!RemoteConnectionPolicy::Auto.is_manual());
        assert!(!RemoteConnectionPolicy::OnDemand.is_manual());
        assert!(RemoteConnectionPolicy::Manual.is_manual());
    }

    #[test]
    fn remote_connection_policy_allows_explicit_start_for_auto_and_on_demand_not_manual() {
        // Automatic orchestration eligibility: Auto only.
        assert!(RemoteConnectionPolicy::Auto.starts_automatically());
        assert!(!RemoteConnectionPolicy::OnDemand.starts_automatically());
        assert!(!RemoteConnectionPolicy::Manual.starts_automatically());
        // Explicit mutating start dispatch: Auto and OnDemand allowed, Manual rejected.
        assert!(RemoteConnectionPolicy::Auto.allows_explicit_start());
        assert!(RemoteConnectionPolicy::OnDemand.allows_explicit_start());
        assert!(!RemoteConnectionPolicy::Manual.allows_explicit_start());
        // allows_explicit_start and is_manual are strict inverses.
        assert!(!RemoteConnectionPolicy::Auto.is_manual());
        assert!(!RemoteConnectionPolicy::OnDemand.is_manual());
        assert!(RemoteConnectionPolicy::Manual.is_manual());
    }
}
