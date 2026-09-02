#[cfg(unix)]
mod unix;

#[cfg(unix)]
mod routed;

#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(unix)]
pub(crate) use routed::*;

// ---- routed remote-sequence outcome payloads (cross-platform data) ----
//
// Pure data shared between the unix-only routed-sequence executor
// (`remote::routed`), the `AppEvent` payloads that carry completions back to
// the reducer, and the reducer-side application handlers. Defined here so
// `events.rs` and the reducer compile unchanged on Windows (no transport
// types are referenced).

/// Terminal outcome taxonomy for one routed remote request sequence.
///
/// The response semantics are chosen by write/primary progress, never by
/// generation: a known primary success is never converted to "cancelled", and
/// an outcome that cannot be known is reported as indeterminate (never
/// auto-retried).
// Windows retains this payload taxonomy for the shared event/reducer shape;
// only the Unix routed executor constructs its variants.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoutedTaxonomy {
    /// Authoritative primary result (success or remote error response) plus
    /// refresh-leg data when the sequence had refresh legs.
    Completed,
    /// Request bytes were written but no authoritative primary result was
    /// read. Affected caches are marked stale; callers must refresh before
    /// retrying. Never auto-retried.
    IndeterminateAfterWrite,
    /// The primary mutation returned authoritative success, but a refresh leg
    /// failed or the sequence was interrupted mid-refresh. The success is
    /// preserved in the response; affected caches are marked stale.
    PrimarySuccessPreserved,
}

/// One routed refresh leg's active-tab selection plus `layout.export` result
/// (pure data produced off-loop; the reducer applies it on the loop).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RoutedActiveTabFetch {
    pub(crate) tab_id: Option<String>,
    pub(crate) tab_label: Option<String>,
    /// `None` = layout fetch failed or `layout.export` is not advertised.
    pub(crate) layout: Option<crate::api::schema::LayoutDescription>,
}

/// Refresh-leg results for one workspace (pure data; produced off-loop by the
/// IO worker, applied on-loop by the reducer). Mirrors the tab/projection
/// application shape of `refresh_remote_workspace_tabs_and_projection`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RoutedRefreshData {
    pub(crate) workspace_id: String,
    /// `tab.list` results; `None` = fetch failed or not advertised (the
    /// reducer marks the workspace's cached tabs unavailable/stale).
    pub(crate) tabs: Option<Vec<crate::api::schema::TabInfo>>,
    /// Active-tab selection plus `layout.export` result. `None` = no
    /// selection or fetch failed (the reducer marks the projection stale).
    pub(crate) active_tab: Option<RoutedActiveTabFetch>,
}

/// How the reducer applies a routed sequence's completion.
// Windows retains this payload taxonomy for the shared event/reducer shape;
// only the Unix routed executor constructs its variants.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RoutedApply {
    /// Apply refresh data to the cached tabs/projection only.
    RefreshOnly,
    /// The primary response is a tab result (`TabCreated`/`TabInfo`): upsert
    /// the cached tab, then apply refresh data.
    TabUpsert,
    /// The primary response closed a tab: remove the cached tab and mark its
    /// workspace's tab snapshot unavailable, then apply refresh data.
    TabRemove { tab_id: String },
    /// The primary response is a `workspace.rename` result: upsert the cached
    /// workspace from it.
    WorkspaceUpsert,
    /// The primary response is a remote workspace-create result. Preserve the
    /// request token in the taxonomy for callers that need to correlate the
    /// generic routed sequence; cache reconciliation remains generic.
    #[allow(dead_code)] // Retained for generic routed workspace-create API taxonomy.
    WorkspaceCreate { token: u64 },
}

/// Completion payload for [`crate::events::AppEvent::RemoteRoutedSequenceCompleted`].
/// Carries the taxonomy, the authoritative primary response (when one was
/// obtained), refresh data (when the refresh legs produced it), and the
/// workspace whose caches must be marked stale when refresh data is absent.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RoutedCompletion {
    pub(crate) taxonomy: RoutedTaxonomy,
    /// Raw authoritative primary response JSON (`None` when no authoritative
    /// result was read).
    pub(crate) primary: Option<String>,
    /// Refresh-leg results; `None` when no refresh data is available (mark
    /// stale per `stale_workspace_id`).
    pub(crate) refresh: Option<RoutedRefreshData>,
    /// Workspace whose cached tabs/projection must be marked stale when no
    /// refresh data is present.
    pub(crate) stale_workspace_id: Option<String>,
    /// How the reducer applies the primary/refresh payloads.
    pub(crate) apply: RoutedApply,
    /// Route that served the primary request (telemetry mirror of the
    /// route-selection tracing event).
    pub(crate) route: &'static str,
}

#[cfg(windows)]
pub(crate) const REATTACH_COMMAND_ENV_VAR: &str = "HERDR_REATTACH_COMMAND";
#[cfg(windows)]
pub(crate) const REMOTE_CLIENT_BRIDGE_SUBCOMMAND: &str = "remote-client-bridge";
#[cfg(windows)]
pub(crate) const REMOTE_CLIENT_BRIDGE_NO_START_SUBCOMMAND: &str = "remote-client-bridge-no-start";
#[cfg(windows)]
pub(crate) const REMOTE_API_BRIDGE_SUBCOMMAND: &str = "remote-api-bridge";
#[cfg(windows)]
pub(crate) const REMOTE_FEDERATION_CAPABILITIES_SUBCOMMAND: &str = "remote-federation-capabilities";
#[cfg(windows)]
pub(crate) const REMOTE_API_STATUS_SUBCOMMAND: &str = "remote-api-status";
#[cfg(windows)]
pub(crate) const REMOTE_API_PING_SUBCOMMAND: &str = "remote-api-ping";
#[cfg(windows)]
pub(crate) const REMOTE_API_AGENT_LIST_SUBCOMMAND: &str = "remote-api-agent-list";
#[cfg(windows)]
pub(crate) const REMOTE_KEYBINDINGS_ENV_VAR: &str = "HERDR_REMOTE_KEYBINDINGS";

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteKeybindings {
    Local,
    Server,
}

#[cfg(windows)]
impl RemoteKeybindings {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "local" => Ok(Self::Local),
            "server" => Ok(Self::Server),
            _ => Err("--remote-keybindings must be 'local' or 'server'".to_string()),
        }
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteLaunch {
    pub(crate) target: String,
    pub(crate) keybindings: RemoteKeybindings,
    pub(crate) live_handoff: bool,
}

#[cfg(windows)]
#[derive(Debug, Clone)]
pub(crate) struct RemoteHerdr {
    shell_path: String,
}

#[cfg(windows)]
impl RemoteHerdr {
    pub(crate) fn shell_path(&self) -> &str {
        &self.shell_path
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RemoteApiStatusResponse {
    pub(crate) state: RemoteApiStatusState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) capabilities: Option<crate::api::schema::ServerCapabilities>,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RemoteApiStatusState {
    Running,
    NotRunning,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteFailureClass {
    NeedsUpdate,
    Unreachable,
    Unknown,
}

#[cfg(windows)]
pub(crate) fn extract_remote_args(
    args: &[String],
) -> Result<(Vec<String>, Option<RemoteLaunch>), String> {
    let mut cleaned = Vec::with_capacity(args.len());
    if let Some(program) = args.first() {
        cleaned.push(program.clone());
    }

    let mut remote_target = None;
    let mut keybindings = RemoteKeybindings::Local;
    let mut keybindings_seen = false;
    let mut live_handoff = false;
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            cleaned.extend_from_slice(&args[index..]);
            break;
        }
        if arg == "--handoff" {
            live_handoff = true;
            index += 1;
            continue;
        }
        if arg == "--remote" {
            if remote_target.is_some() {
                return Err("--remote can only be specified once".to_string());
            }
            let Some(value) = args.get(index + 1) else {
                return Err("missing value for --remote".to_string());
            };
            remote_target = Some(validate_remote_target(value)?.to_owned());
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--remote=") {
            if remote_target.is_some() {
                return Err("--remote can only be specified once".to_string());
            }
            remote_target = Some(validate_remote_target(value)?.to_owned());
            index += 1;
            continue;
        }
        if arg == "--remote-keybindings" {
            if keybindings_seen {
                return Err("--remote-keybindings can only be specified once".to_string());
            }
            let Some(value) = args.get(index + 1) else {
                return Err("missing value for --remote-keybindings".to_string());
            };
            keybindings = RemoteKeybindings::parse(value)?;
            keybindings_seen = true;
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--remote-keybindings=") {
            if keybindings_seen {
                return Err("--remote-keybindings can only be specified once".to_string());
            }
            keybindings = RemoteKeybindings::parse(value)?;
            keybindings_seen = true;
            index += 1;
            continue;
        }

        cleaned.push(arg.clone());
        index += 1;
    }

    let remote = remote_target.map(|target| RemoteLaunch {
        target,
        keybindings,
        live_handoff,
    });
    if remote.is_none() && keybindings_seen {
        return Err("--remote-keybindings requires --remote".to_string());
    }
    if remote.is_none() && live_handoff {
        cleaned.push("--handoff".to_string());
    }

    Ok((cleaned, remote))
}

#[cfg(windows)]
fn validate_remote_target(target: &str) -> Result<&str, String> {
    validate_remote_target_for(target, "--remote")
}

#[cfg(windows)]
fn validate_remote_target_for<'a>(target: &'a str, label: &str) -> Result<&'a str, String> {
    if target.is_empty() {
        return Err(format!("missing value for {label}"));
    }
    if target.starts_with('-') {
        return Err(format!("{label} target must not start with '-'"));
    }
    Ok(target)
}

#[cfg(windows)]
fn unsupported_remote_error(feature: &str) -> std::io::Error {
    debug_assert!(!crate::platform::capabilities().remote_attach);
    std::io::Error::other(format!("{feature} is not supported on Windows yet"))
}

#[cfg(windows)]
pub(crate) fn run_remote(_remote: RemoteLaunch) -> std::io::Result<()> {
    Err(unsupported_remote_error("remote mode"))
}

#[cfg(windows)]
pub(crate) fn run_remote_client_bridge() -> std::io::Result<()> {
    Err(unsupported_remote_error("remote client bridge"))
}

#[cfg(windows)]
pub(crate) fn run_remote_client_bridge_no_start() -> std::io::Result<()> {
    Err(unsupported_remote_error("remote client bridge (no-start)"))
}

#[cfg(windows)]
pub(crate) fn run_remote_terminal_attach(
    _host: &crate::remote_target::RemoteHostConfig,
    _terminal_id: String,
    _takeover: bool,
) -> std::io::Result<()> {
    Err(unsupported_remote_error("remote terminal attach"))
}

#[cfg(windows)]
pub(crate) fn setup_remote_host_interactive(
    _host: &crate::remote_target::RemoteHostConfig,
    _live_handoff: bool,
) -> std::io::Result<RemoteHerdr> {
    Err(unsupported_remote_error("remote setup"))
}

#[cfg(windows)]
pub(crate) fn run_remote_api_bridge(_args: &[String]) -> std::io::Result<()> {
    Err(unsupported_remote_error("remote API bridge"))
}

#[cfg(windows)]
pub(crate) fn run_remote_federation_capabilities() -> std::io::Result<()> {
    Err(unsupported_remote_error("remote federation capabilities"))
}

#[cfg(windows)]
pub(crate) fn run_remote_api_status() -> std::io::Result<()> {
    Err(unsupported_remote_error("remote API status"))
}

#[cfg(windows)]
pub(crate) fn run_remote_api_ping(_args: &[String]) -> std::io::Result<()> {
    Err(unsupported_remote_error("remote API ping"))
}

#[cfg(windows)]
pub(crate) fn run_remote_api_agent_list(_args: &[String]) -> std::io::Result<()> {
    Err(unsupported_remote_error("remote API agent list"))
}

#[cfg(windows)]
pub(crate) fn send_remote_api_request_to_host(
    _host: &crate::remote_target::RemoteHostConfig,
    _request: &crate::api::schema::Request,
) -> std::io::Result<String> {
    Err(unsupported_remote_error("remote API request"))
}

#[cfg(windows)]
pub(crate) fn send_remote_api_request_to_host_noninteractive(
    _host: &crate::remote_target::RemoteHostConfig,
    _request: &crate::api::schema::Request,
) -> std::io::Result<String> {
    Err(unsupported_remote_error("remote API request"))
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteApiBridgeState {
    pub(crate) shell_path: String,
    pub(crate) capabilities: crate::api::schema::FederationCapabilities,
}

#[cfg(windows)]
pub(crate) fn send_remote_api_request_to_host_noninteractive_with_state(
    _host: &crate::remote_target::RemoteHostConfig,
    _request: &crate::api::schema::Request,
) -> std::io::Result<(String, RemoteApiBridgeState)> {
    Err(unsupported_remote_error("remote API request"))
}

#[cfg(windows)]
pub(crate) fn send_remote_api_request_with_prepared_state(
    _host: &crate::remote_target::RemoteHostConfig,
    _state: &RemoteApiBridgeState,
    _request: &crate::api::schema::Request,
) -> std::io::Result<String> {
    Err(unsupported_remote_error("remote API request"))
}

/// Windows stub: opaque placeholder so the selected-host bridge owner can
/// hold an `Option<SshStdioBridge>`. Remote host glass is not supported on
/// Windows, so [`start_projection_bridge`] never constructs one.
#[cfg(windows)]
pub(crate) struct SshStdioBridge;

/// Windows stub: selected-host glass streaming is not supported yet.
#[cfg(windows)]
#[cfg_attr(
    windows,
    expect(
        dead_code,
        reason = "kept for API parity with the Unix projection bridge starter; the Unix admission path that calls it is cfg(unix)-only"
    )
)]
pub(crate) fn start_projection_bridge(
    _host: &crate::remote_target::RemoteHostConfig,
    _state: &RemoteApiBridgeState,
    _local_socket: std::path::PathBuf,
    _max_concurrent: usize,
) -> std::io::Result<SshStdioBridge> {
    Err(unsupported_remote_error("selected-host glass streaming"))
}

/// Windows stub: pooled persistent-bridge dispatch is never reached (no remote
/// support), so always fall back to the one-shot path.
#[cfg(windows)]
pub(crate) fn try_pooled_remote_api_request(
    _host: &crate::remote_target::RemoteHostConfig,
    _state: &RemoteApiBridgeState,
    _request: &crate::api::schema::Request,
) -> std::io::Result<Option<String>> {
    Ok(None)
}

/// Windows stub: no persistent-bridge pool exists; mark-only invalidation is a no-op.
#[cfg(windows)]
pub(crate) fn invalidate_remote_bridge_pool_host(_key: &crate::remote_source::RemoteHostKey) {}

/// Windows stub: no persistent-bridge pool exists; the inline lifecycle
/// drain+reap is a no-op (remote support is unix-only, so this is never
/// reached with real work on Windows).
#[cfg(windows)]
pub(crate) fn drain_remote_bridge_pool_host_inline(_key: &crate::remote_source::RemoteHostKey) {}

/// Windows stub: no persistent-bridge pool exists; the disconnect off-loop
/// drain has nothing to reap, but it still reports completion from an off-loop
/// worker (never `blocking_send` directly on the App async runtime, matching
/// the unix contract). No remote mutation occurs on Windows.
#[cfg(windows)]
pub(crate) fn drain_remote_bridge_pool_host_off_loop(
    key: crate::remote_source::RemoteHostKey,
    generation: u64,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
) {
    std::thread::spawn(move || {
        let _ = event_tx.blocking_send(crate::events::AppEvent::RemoteSourcePoolDrainCompleted {
            host: key,
            generation,
        });
    });
}

/// Windows stub: no persistent-bridge pool exists, so shutdown drain is a no-op.
#[cfg(windows)]
pub(crate) fn drain_remote_bridge_pool() {}

#[cfg(windows)]
pub(crate) fn prepare_remote_binary_to_host_noninteractive(
    _host: &crate::remote_target::RemoteHostConfig,
) -> std::io::Result<RemoteHerdr> {
    Err(unsupported_remote_error("remote binary preparation"))
}

#[cfg(windows)]
pub(crate) fn remote_federation_capabilities_for_prepared_host_noninteractive(
    _host: &crate::remote_target::RemoteHostConfig,
    _remote_herdr: &RemoteHerdr,
) -> std::io::Result<crate::api::schema::FederationCapabilities> {
    Err(unsupported_remote_error("remote federation capabilities"))
}

#[cfg(windows)]
pub(crate) fn remote_api_status_to_host_noninteractive(
    _host: &crate::remote_target::RemoteHostConfig,
) -> std::io::Result<RemoteApiStatusResponse> {
    Err(unsupported_remote_error("remote API status"))
}

#[cfg(windows)]
pub(crate) fn remote_api_status_for_prepared_host_noninteractive(
    _host: &crate::remote_target::RemoteHostConfig,
    _remote_herdr: &RemoteHerdr,
) -> std::io::Result<RemoteApiStatusResponse> {
    Err(unsupported_remote_error("remote API status"))
}

#[cfg(windows)]
pub(crate) fn classify_remote_failure(err: &std::io::Error) -> RemoteFailureClass {
    match err.kind() {
        std::io::ErrorKind::InvalidData | std::io::ErrorKind::NotFound => {
            RemoteFailureClass::NeedsUpdate
        }
        std::io::ErrorKind::ConnectionRefused
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::TimedOut
        | std::io::ErrorKind::WouldBlock
        | std::io::ErrorKind::BrokenPipe
        | std::io::ErrorKind::PermissionDenied => RemoteFailureClass::Unreachable,
        _ if looks_like_ssh_transport_error(&err.to_string()) => RemoteFailureClass::Unreachable,
        _ => RemoteFailureClass::Unknown,
    }
}

#[cfg(windows)]
pub(crate) fn looks_like_ssh_transport_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "ssh",
        "permission denied",
        "could not resolve hostname",
        "name or service not known",
        "connection timed out",
        "connection refused",
        "connection reset",
        "no route to host",
        "remote platform detection failed: exit status: 255",
        "exit status: 255",
        "host key verification failed",
        "known_hosts",
        "publickey",
        "batchmode",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}
