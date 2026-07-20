use serde::{Deserialize, Serialize};

/// Parameters for the local-only `remote.connect`, `remote.reconnect`, and
/// `remote.disconnect` lifecycle methods.
///
/// `host` is the configured remote host alias. The running local server
/// re-resolves it against its own loaded registry so a stale client/server
/// config cannot target an unintended host. These methods control only the
/// running local controller's aggregation/supervisor/bridge state; they are
/// reached only on the running LOCAL server and are never routed
/// remote-of-remote (not advertised as federation capabilities/routed
/// methods). `disconnect` is entirely local and sends no remote request.
/// `connect`/`reconnect` deliberately cause the LOCAL supervisor worker to
/// perform a non-mutating SSH/API health/capability ping; they never
/// provision/install/update/start/stop/mutate the remote Herdr server,
/// processes, panes, workspaces, config, or state, and never open a new remote
/// shell-command shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RemoteLifecycleHostParams {
    pub host: String,
}

/// Which explicit runtime lifecycle action the caller requested. Mirrors the
/// CLI method and the local API method name so the response is self-describing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RemoteLifecycleAction {
    Connect,
    Reconnect,
    Disconnect,
}

impl RemoteLifecycleAction {
    pub(crate) fn method_name(self) -> &'static str {
        match self {
            Self::Connect => "remote.connect",
            Self::Reconnect => "remote.reconnect",
            Self::Disconnect => "remote.disconnect",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Reconnect => "reconnect",
            Self::Disconnect => "disconnect",
        }
    }
}

/// Resulting LOCAL aggregation status for a host after a lifecycle action. This
/// is the cached [`crate::remote_source::RemoteConnectionStatus`] projection
/// into the JSON API shape (the local controller's view), NOT a fresh probe of
/// the remote host. `remote status`/`check` remain the direct remote-health
/// diagnostics; lifecycle results report local aggregation state only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RemoteLifecycleResultStatus {
    /// The host's local aggregation is `Connected` after the action.
    Connected,
    /// The host's local aggregation is `Disconnected` (explicit disconnect, or
    /// a connect/reconnect that could not establish a connection yet but left a
    /// retrying supervisor alive).
    Disconnected,
    /// A connect/reconnect attempt reached the host but its remote Herdr is
    /// unreachable right now; local aggregation is marked stale/unreachable.
    Unreachable,
    /// A connect/reconnect attempt found a missing/incompatible remote Herdr;
    /// local aggregation is marked needs-update and no install/update ran.
    NeedsUpdate,
    /// The host had no live/healthy supervisor and the connect/reconnect
    /// attempt is in flight or failed before a definitive status (the local
    /// retrying supervisor remains alive).
    Unhealthy,
}

/// Typed result of a local-only runtime lifecycle action.
///
/// Reports the host alias, the remote session the local controller aggregates,
/// the requested action, the resulting LOCAL aggregation status, whether local
/// runtime state changed, and a constant reminder that the remote Herdr server
/// remains authoritative/running. `detail` carries actionable guidance
/// (e.g. needs-setup, unreachable SSH) when applicable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RemoteLifecycleResult {
    pub host: String,
    pub session: String,
    pub action: RemoteLifecycleAction,
    pub status: RemoteLifecycleResultStatus,
    pub changed: bool,
    /// Always `true`: lifecycle actions never stop/restart the remote Herdr
    /// server. The remote host remains authoritative and running; only the
    /// local controller's aggregation/supervisor/bridge state is affected.
    pub remote_authoritative: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
