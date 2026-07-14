//! Poll-based remote source supervisors.
//!
//! Runtime-only glue: supervisors query remote API snapshots and send AppEvents.
//! They do not mutate AppState, subscribe to output streams, route commands, or render UI.

use std::io;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::debug;

use crate::api::client::{parse_response_value, ApiClientError};
use crate::api::schema::{
    AgentInfo, EmptyParams, LayoutDescription, LayoutExportParams, Method, PingParams, Request,
    ResponseResult, TabInfo, TabListParams, WorkspaceInfo,
};
use crate::events::AppEvent;
use crate::remote_source::{
    RemoteConnectionStatus, RemoteHostKey, RemoteProjectionSnapshot, RemoteProjectionStatus,
    RemoteSourceCapabilities, RemoteTabSnapshot,
};
use crate::remote_target::{RemoteHostConfig, RemoteHostRegistry};

const REMOTE_SOURCE_PING_INTERVAL: Duration = Duration::from_secs(30);
const REMOTE_SOURCE_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(60);
const REMOTE_SOURCE_RETRY_INTERVAL: Duration = Duration::from_secs(15);
const REMOTE_SOURCE_INCOMPATIBLE_RETRY_INTERVAL: Duration = Duration::from_secs(5 * 60);
const REMOTE_SOURCE_STOP_POLL_INTERVAL: Duration = Duration::from_millis(250);

// Transient (Unreachable/Unknown) failures use bounded exponential backoff on
// top of the short retry interval. Both the base and the cap are expressed in
// seconds because the backoff is computed by bit-shifting a `u64` second count.
/// Base (and first-attempt) transient retry interval. Consecutive transient
/// failures double this value up to [`REMOTE_SOURCE_TRANSIENT_BACKOFF_CAP_SECS`].
const REMOTE_SOURCE_TRANSIENT_BACKOFF_BASE_SECS: u64 = REMOTE_SOURCE_RETRY_INTERVAL.as_secs();
/// Ceiling for transient exponential backoff (~5 minutes). This is intentionally
/// a distinct constant from [`REMOTE_SOURCE_INCOMPATIBLE_RETRY_INTERVAL`]: both
/// happen to be ~5 minutes, but the former is the transient backoff ceiling
/// while the latter is the fixed long interval for non-transient `NeedsUpdate`
/// failures and must not be conflated with it.
const REMOTE_SOURCE_TRANSIENT_BACKOFF_CAP_SECS: u64 = 5 * 60;
/// Largest bit-shift applied to the base before the cap clamps the result.
/// `2^5 = 32`, so step 5 already yields `15 * 32 = 480s` which exceeds the 300s
/// cap; any larger consecutive-failure index therefore collapses to the cap.
/// Clamping the shift also keeps `1u64 << step` from overflowing.
const TRANSIENT_BACKOFF_MAX_SHIFT: u32 = 5;
/// Maximum jitter window layered on top of the pure base backoff for transient
/// failures. Below the cap this bounds an additive window with the base as its
/// lower bound; at the cap (where the base equals the cap) it bounds a
/// subtractive de-synchronization window instead, so hosts spread across
/// `[cap - window, cap]` rather than pinning to exactly the cap.
const REMOTE_SOURCE_TRANSIENT_JITTER_WINDOW_SECS: u64 = 30;

pub(crate) struct RemoteSourceSupervisorHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RemoteSourceSupervisorHandle {
    fn start(host: RemoteHostConfig, event_tx: mpsc::Sender<AppEvent>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread =
            thread::spawn(move || remote_source_supervisor_loop(host, event_tx, thread_stop));
        Self {
            stop,
            thread: Some(thread),
        }
    }

    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Do not join here: an SSH request may be blocked in the OS. Dropping the
        // JoinHandle detaches the worker after publishing the stop flag.
        self.thread.take();
    }
}

impl Drop for RemoteSourceSupervisorHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn start_remote_source_supervisors(
    registry: &RemoteHostRegistry,
    event_tx: mpsc::Sender<AppEvent>,
) -> Vec<RemoteSourceSupervisorHandle> {
    auto_connect_hosts(registry)
        .into_iter()
        .map(|host| RemoteSourceSupervisorHandle::start(host, event_tx.clone()))
        .collect()
}

pub(crate) fn stop_remote_source_supervisors(handles: &mut Vec<RemoteSourceSupervisorHandle>) {
    for handle in handles.iter_mut() {
        handle.stop();
    }
    handles.clear();
}

pub(crate) fn auto_connect_hosts(registry: &RemoteHostRegistry) -> Vec<RemoteHostConfig> {
    registry
        .list()
        .into_iter()
        .filter(|host| host.connection_policy.starts_automatically())
        .cloned()
        .collect()
}

fn remote_source_supervisor_loop(
    host: RemoteHostConfig,
    event_tx: mpsc::Sender<AppEvent>,
    stop: Arc<AtomicBool>,
) {
    remote_source_supervisor_loop_with(
        host,
        event_tx,
        stop,
        send_remote_api_request_with_send_result,
    );
}

/// Result of one supervisor bridge round-trip: the JSON response plus any
/// prepared bridge state captured on a successful round-trip (prepared remote
/// Herdr shell path + advertised federation capabilities).
///
/// The real supervisor sender ([`send_remote_api_request_with_send_result`])
/// calls the remote helper that returns both the response and prepared state,
/// so a successful ping can publish that state for routed agent dispatch reuse.
/// Test send closures return response-only results ([`Self::response_only`] /
/// `From<String>`) with `bridge_state: None`.
#[derive(Debug, Clone)]
pub(crate) struct RemoteSourceSendResult {
    pub(crate) response: String,
    pub(crate) bridge_state: Option<crate::remote::RemoteApiBridgeState>,
}

impl RemoteSourceSendResult {
    /// Response-only send result with no prepared bridge state. Used by test
    /// send closures that do not exercise the prepared-state capture path.
    pub(crate) fn response_only(response: String) -> Self {
        Self {
            response,
            bridge_state: None,
        }
    }
}

impl From<String> for RemoteSourceSendResult {
    fn from(response: String) -> Self {
        Self::response_only(response)
    }
}

/// Production supervisor sender: runs the real non-interactive remote API
/// request and captures the prepared bridge state on success so a connected
/// supervisor ping can publish it for routed agent dispatch reuse.
fn send_remote_api_request_with_send_result(
    host: &RemoteHostConfig,
    request: &Request,
) -> io::Result<RemoteSourceSendResult> {
    let (response, bridge_state) =
        crate::remote::send_remote_api_request_to_host_noninteractive_with_state(host, request)?;
    Ok(RemoteSourceSendResult {
        response,
        bridge_state: Some(bridge_state),
    })
}

fn remote_source_supervisor_loop_with<F>(
    host: RemoteHostConfig,
    event_tx: mpsc::Sender<AppEvent>,
    stop: Arc<AtomicBool>,
    send: F,
) where
    F: Fn(&RemoteHostConfig, &Request) -> io::Result<RemoteSourceSendResult>,
{
    let host_key = RemoteHostKey::new(host.name.clone(), host.session.clone());
    let mut next_ping = Instant::now();
    let mut next_snapshot = Instant::now();
    let mut capabilities = RemoteSourceCapabilities::default();
    let mut transient_backoff = TransientBackoff::default();

    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= next_ping {
            match send_ping(&host, &send) {
                Ok((next_capabilities, bridge_state)) => {
                    capabilities = next_capabilities;
                    // Any successful round-trip proves the host is reachable
                    // again, so clear any accumulated transient backoff.
                    transient_backoff.reset();
                    next_ping = now + REMOTE_SOURCE_PING_INTERVAL;
                    // C3: capture prepared bridge state on the ping path so a
                    // connected supervisor cache is paired with prepared state
                    // promptly. Published through the AppEvent/reducer path only;
                    // the supervisor thread never mutates AppState/RemoteSourceCache
                    // directly. A later non-connected ping/snapshot clears it.
                    if let Some(bridge_state) = bridge_state {
                        if !stop.load(Ordering::Relaxed) {
                            let _ = event_tx.blocking_send(AppEvent::RemoteSourceBridgeState {
                                host: host_key.clone(),
                                bridge_state,
                            });
                        }
                    }
                }
                Err(err) => {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    debug!(host = %host.name, session = %host.session, err = %err, "remote source ping failed");
                    let status = remote_source_failure_status(&err);
                    let _ = event_tx.blocking_send(AppEvent::RemoteSourceDisconnected {
                        host: host_key.clone(),
                        status,
                    });
                    let retry_interval = retry_interval_for_failure(
                        &err,
                        &mut transient_backoff,
                        &host_key.host,
                        &host_key.session,
                    );
                    next_ping = now + retry_interval;
                    // Defer the next snapshot probe to at least the chosen retry
                    // interval so an offline host does not immediately run deeper
                    // snapshot/projection probes in the same iteration, and so a
                    // single transient failure does not double-escalate the
                    // shared backoff through both ping and snapshot this tick.
                    next_snapshot = next_snapshot.max(now + retry_interval);
                }
            }
        }

        let now = Instant::now();
        if now >= next_snapshot {
            match send_remote_source_snapshot(&host, &send, capabilities) {
                Ok((agents, workspaces, projections, tabs)) => {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let _ = event_tx.blocking_send(AppEvent::RemoteSourceSnapshot {
                        host: host_key.clone(),
                        agents,
                        workspaces,
                        capabilities,
                        projections,
                        tabs,
                    });
                    // A successful snapshot also proves reachability.
                    transient_backoff.reset();
                    next_snapshot = now + REMOTE_SOURCE_SNAPSHOT_INTERVAL;
                }
                Err(err) => {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    debug!(host = %host.name, session = %host.session, err = %err, "remote source snapshot failed");
                    let status = remote_source_failure_status(&err);
                    let _ = event_tx.blocking_send(AppEvent::RemoteSourceDisconnected {
                        host: host_key.clone(),
                        status,
                    });
                    let retry_interval = retry_interval_for_failure(
                        &err,
                        &mut transient_backoff,
                        &host_key.host,
                        &host_key.session,
                    );
                    next_snapshot = now + retry_interval;
                }
            }
        }

        sleep_until_next_due(next_ping.min(next_snapshot), &stop);
    }
}

fn sleep_until_next_due(next_due: Instant, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= next_due {
            break;
        }
        thread::sleep((next_due - now).min(REMOTE_SOURCE_STOP_POLL_INTERVAL));
    }
}

/// Bounded exponential backoff for transient remote-supervisor failures.
///
/// Only [`RemoteFailureClass::Unreachable`] and [`RemoteFailureClass::Unknown`]
/// failures consume this state; [`RemoteFailureClass::NeedsUpdate`] failures
/// (missing/incompatible binary, invalid data) are not transient, keep the fixed
/// [`REMOTE_SOURCE_INCOMPATIBLE_RETRY_INTERVAL`], and must neither consume nor
/// reset this counter.
///
/// Deterministic jitter is layered on top of the pure base sequence by
/// [`TransientBackoff::record_failure`] via [`transient_retry_interval`]: it is
/// keyed on `(host, session, failure_index)` through an in-tree FNV-1a hash so a
/// given host/session retries on a stable, de-synchronized schedule that is
/// reproducible across processes and toolchains, without randomness, wall-clock
/// time, or global state. The pure base sequence itself stays available and
/// testable via [`transient_backoff_interval`].
///
/// [`RemoteFailureClass::Unreachable`]: crate::remote::RemoteFailureClass::Unreachable
/// [`RemoteFailureClass::Unknown`]: crate::remote::RemoteFailureClass::Unknown
/// [`RemoteFailureClass::NeedsUpdate`]: crate::remote::RemoteFailureClass::NeedsUpdate
#[derive(Debug, Default)]
struct TransientBackoff {
    consecutive_failures: u32,
}

impl TransientBackoff {
    /// Returns the jittered retry interval for the current transient failure,
    /// then advances the consecutive-failure count so the next failure waits
    /// longer. The jitter is deterministic for `(host, session, failure_index)`
    /// so a given host/session retries on a stable, de-synchronized schedule
    /// instead of pinning to the pure base sequence.
    fn record_failure(&mut self, host: &str, session: &str) -> Duration {
        let interval = transient_retry_interval(self.consecutive_failures, host, session);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        interval
    }

    /// Clears accumulated backoff after any successful ping or snapshot.
    fn reset(&mut self) {
        self.consecutive_failures = 0;
    }
}

/// Pure (jitter-free) base retry interval for the Nth consecutive transient
/// failure (0-indexed).
///
/// Sequence: 15s, 30s, 60s, 120s, 240s, then capped at
/// [`REMOTE_SOURCE_TRANSIENT_BACKOFF_CAP_SECS`] (~5 minutes). Below the cap this
/// is the lower bound of the actual jittered retry interval (see
/// [`transient_retry_interval`]); at the cap the jitter is subtractive, so the
/// base is the upper bound there.
fn transient_backoff_interval(failure_index: u32) -> Duration {
    let step = failure_index.min(TRANSIENT_BACKOFF_MAX_SHIFT);
    let secs = REMOTE_SOURCE_TRANSIENT_BACKOFF_BASE_SECS
        .saturating_mul(1u64 << step)
        .min(REMOTE_SOURCE_TRANSIENT_BACKOFF_CAP_SECS);
    Duration::from_secs(secs)
}

/// Deterministic transient retry interval: the pure base backoff with jitter
/// layered on top. Only transient ([`Unreachable`] / [`Unknown`]) failures use
/// this; see [`TransientBackoff`].
///
/// Below the cap, the base sequence is the lower bound and a small additive
/// window `0..=min(JITTER_WINDOW, max(1, base/4))` is layered on top, then
/// clamped to the cap, so a host's first transient retry can grow past the
/// fixed 15s base but never shrinks below it. At the cap a subtractive window
/// keeps hosts from synchronizing forever on exactly 300s: the result lies in
/// `[cap - JITTER_WINDOW, cap]`.
///
/// The offset is deterministic for `(host, session, failure_index)` via
/// [`transient_jitter_seed`] (an in-tree FNV-1a hash), so a given host/session
/// retries on a stable, de-synchronized schedule that is reproducible across
/// processes and toolchains, without randomness, wall-clock time, or global
/// state.
///
/// [`Unreachable`]: crate::remote::RemoteFailureClass::Unreachable
/// [`Unknown`]: crate::remote::RemoteFailureClass::Unknown
fn transient_retry_interval(failure_index: u32, host: &str, session: &str) -> Duration {
    let base = transient_backoff_interval(failure_index);
    let base_secs = base.as_secs();
    let seed = transient_jitter_seed(host, session, failure_index);
    let interval_secs = if base_secs < REMOTE_SOURCE_TRANSIENT_BACKOFF_CAP_SECS {
        // Additive window: base is the lower bound; jitter is bounded by a
        // quarter of the base, clamped to the max window with a 1s floor.
        let window = REMOTE_SOURCE_TRANSIENT_JITTER_WINDOW_SECS
            .min(base_secs / 4)
            .max(1);
        let jitter = seed % (window + 1);
        base_secs
            .saturating_add(jitter)
            .min(REMOTE_SOURCE_TRANSIENT_BACKOFF_CAP_SECS)
    } else {
        // Subtractive window at the cap: de-synchronize without exceeding the
        // cap or dropping below `cap - JITTER_WINDOW`.
        let offset = seed % (REMOTE_SOURCE_TRANSIENT_JITTER_WINDOW_SECS + 1);
        REMOTE_SOURCE_TRANSIENT_BACKOFF_CAP_SECS.saturating_sub(offset)
    };
    Duration::from_secs(interval_secs)
}

/// FNV-1a 64-bit offset basis.
const FNV1A_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
/// FNV-1a 64-bit prime.
const FNV1A_PRIME: u64 = 0x100000001b3;

/// Small in-tree FNV-1a 64-bit hasher.
///
/// Stable across processes and toolchains, and deliberately independent of
/// `std::collections::hash_map::DefaultHasher`, whose algorithm is unspecified
/// and may change between Rust releases. This is used only to make transient
/// retry jitter deterministic per host/session/failure-index; it is not a
/// cryptographic primitive.
struct Fnv1aHasher(u64);

impl Default for Fnv1aHasher {
    fn default() -> Self {
        Self(FNV1A_OFFSET_BASIS)
    }
}

impl Fnv1aHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(FNV1A_PRIME);
        }
    }

    fn finish(self) -> u64 {
        self.0
    }
}

/// Stable, unambiguous FNV-1a seed over `(host, session, failure_index)`.
///
/// Each field is length-prefixed (and the index written as fixed little-endian
/// bytes) so two distinct triples cannot fold into the same byte stream, e.g.
/// host `"ab"` + session `"c"` never collides with host `"a"` + session `"bc"`,
/// and the fixed-width encoding keeps the seed identical across architectures.
fn transient_jitter_seed(host: &str, session: &str, failure_index: u32) -> u64 {
    let mut hasher = Fnv1aHasher::default();
    hasher.write(&(host.len() as u64).to_le_bytes());
    hasher.write(host.as_bytes());
    hasher.write(&(session.len() as u64).to_le_bytes());
    hasher.write(session.as_bytes());
    hasher.write(&failure_index.to_le_bytes());
    hasher.finish()
}

/// Retry interval for a remote-supervisor failure.
///
/// Transient (`Unreachable`/`Unknown`) failures escalate the shared transient
/// backoff; `NeedsUpdate` failures keep the fixed long interval and do not touch
/// the backoff state, so an incompatible-binary host does not burn through the
/// transient escalation ladder (and a transient flap right after a `NeedsUpdate`
/// still starts from the base).
fn retry_interval_for_failure(
    err: &io::Error,
    backoff: &mut TransientBackoff,
    host: &str,
    session: &str,
) -> Duration {
    match crate::remote::classify_remote_failure(err) {
        crate::remote::RemoteFailureClass::NeedsUpdate => REMOTE_SOURCE_INCOMPATIBLE_RETRY_INTERVAL,
        crate::remote::RemoteFailureClass::Unreachable
        | crate::remote::RemoteFailureClass::Unknown => backoff.record_failure(host, session),
    }
}

fn remote_source_failure_status(err: &io::Error) -> RemoteConnectionStatus {
    match crate::remote::classify_remote_failure(err) {
        crate::remote::RemoteFailureClass::NeedsUpdate => RemoteConnectionStatus::NeedsUpdate,
        crate::remote::RemoteFailureClass::Unreachable => RemoteConnectionStatus::Unreachable,
        crate::remote::RemoteFailureClass::Unknown => RemoteConnectionStatus::Disconnected,
    }
}

fn send_ping<F>(
    host: &RemoteHostConfig,
    send: &F,
) -> io::Result<(
    RemoteSourceCapabilities,
    Option<crate::remote::RemoteApiBridgeState>,
)>
where
    F: Fn(&RemoteHostConfig, &Request) -> io::Result<RemoteSourceSendResult>,
{
    let result = send(host, &ping_request())?;
    let capabilities = parse_ping_response(&result.response)?;
    Ok((capabilities, result.bridge_state))
}

/// Core snapshot + projected layouts produced by [`send_remote_source_snapshot`].
type RemoteSourceSnapshotParts = (
    Vec<AgentInfo>,
    Option<Vec<WorkspaceInfo>>,
    Vec<RemoteProjectionSnapshot>,
    Vec<RemoteTabSnapshot>,
);

fn send_remote_source_snapshot<F>(
    host: &RemoteHostConfig,
    send: &F,
    capabilities: RemoteSourceCapabilities,
) -> io::Result<RemoteSourceSnapshotParts>
where
    F: Fn(&RemoteHostConfig, &Request) -> io::Result<RemoteSourceSendResult>,
{
    let agents = send_agent_list(host, send)?;
    let workspaces = if capabilities.workspace_list_local {
        Some(send_workspace_list(host, send)?)
    } else {
        None
    };
    // Bounded eager projection fetch: at most one layout.export per remote
    // workspace active tab, and only when both tab_list and layout_export are
    // advertised. Per-workspace failures become unavailable/stale projection
    // metadata, never an io::Err that would drive RemoteSourceDisconnected.
    let (projections, tabs) = build_projections(host, send, capabilities, workspaces.as_deref());
    Ok((agents, workspaces, projections, tabs))
}

fn build_projections<F>(
    host: &RemoteHostConfig,
    send: &F,
    capabilities: RemoteSourceCapabilities,
    workspaces: Option<&[WorkspaceInfo]>,
) -> (Vec<RemoteProjectionSnapshot>, Vec<RemoteTabSnapshot>)
where
    F: Fn(&RemoteHostConfig, &Request) -> io::Result<RemoteSourceSendResult>,
{
    let Some(workspaces) = workspaces else {
        return (Vec::new(), Vec::new());
    };
    let mut projections = Vec::new();
    let mut tab_snapshots = Vec::new();
    for workspace in workspaces {
        let tab_list = if capabilities.tab_list {
            match send_tab_list(host, send, &workspace.workspace_id) {
                Ok(tabs) => {
                    tab_snapshots.push(RemoteTabSnapshot {
                        workspace_id: workspace.workspace_id.clone(),
                        status: RemoteProjectionStatus::Available,
                        tabs: tabs.clone(),
                    });
                    Some(tabs)
                }
                Err(err) => {
                    debug!(
                        host = %host.name,
                        session = %host.session,
                        workspace = %workspace.workspace_id,
                        err = %err,
                        "remote tab list fetch failed"
                    );
                    tab_snapshots.push(RemoteTabSnapshot {
                        workspace_id: workspace.workspace_id.clone(),
                        status: RemoteProjectionStatus::Unavailable,
                        tabs: Vec::new(),
                    });
                    None
                }
            }
        } else {
            None
        };

        if !capabilities.layout_export {
            continue;
        }

        let active_tab_id = active_tab_id_for_workspace(workspace, tab_list.as_deref());
        let Some(active_tab_id) = active_tab_id else {
            projections.push(RemoteProjectionSnapshot {
                workspace_id: workspace.workspace_id.clone(),
                tab_id: None,
                tab_label: None,
                status: RemoteProjectionStatus::Unavailable,
                layout: None,
            });
            continue;
        };
        let tab_label = tab_list
            .as_deref()
            .and_then(|tabs| tabs.iter().find(|tab| tab.tab_id == active_tab_id))
            .map(|tab| tab.label.clone());
        match send_layout_export(host, send, &active_tab_id) {
            Ok(layout) => projections.push(RemoteProjectionSnapshot {
                workspace_id: workspace.workspace_id.clone(),
                tab_id: Some(layout.tab_id.clone()),
                tab_label,
                status: RemoteProjectionStatus::Available,
                layout: Some(layout),
            }),
            Err(err) => {
                debug!(
                    host = %host.name,
                    session = %host.session,
                    workspace = %workspace.workspace_id,
                    err = %err,
                    "remote projection fetch failed"
                );
                projections.push(RemoteProjectionSnapshot {
                    workspace_id: workspace.workspace_id.clone(),
                    tab_id: Some(active_tab_id),
                    tab_label,
                    status: RemoteProjectionStatus::Unavailable,
                    layout: None,
                });
            }
        }
    }
    (projections, tab_snapshots)
}

fn active_tab_id_for_workspace(
    workspace: &WorkspaceInfo,
    tabs: Option<&[TabInfo]>,
) -> Option<String> {
    let workspace_active = workspace.active_tab_id.trim();
    if !workspace_active.is_empty() {
        return Some(workspace_active.to_string());
    }
    tabs.and_then(|tabs| tabs.iter().find(|tab| tab.focused).or_else(|| tabs.first()))
        .map(|tab| tab.tab_id.clone())
}

fn send_tab_list<F>(
    host: &RemoteHostConfig,
    send: &F,
    workspace_id: &str,
) -> io::Result<Vec<TabInfo>>
where
    F: Fn(&RemoteHostConfig, &Request) -> io::Result<RemoteSourceSendResult>,
{
    let result = send(host, &tab_list_request(workspace_id))?;
    parse_tab_list_response(&result.response)
}

fn send_layout_export<F>(
    host: &RemoteHostConfig,
    send: &F,
    tab_id: &str,
) -> io::Result<LayoutDescription>
where
    F: Fn(&RemoteHostConfig, &Request) -> io::Result<RemoteSourceSendResult>,
{
    let result = send(host, &layout_export_request(tab_id))?;
    parse_layout_export_response(&result.response)
}

fn parse_layout_export_response(response: &str) -> io::Result<LayoutDescription> {
    match parse_success_response(response)? {
        ResponseResult::LayoutExport { layout } => Ok(layout),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected layout.export response, got {other:?}"),
        )),
    }
}

fn send_agent_list<F>(host: &RemoteHostConfig, send: &F) -> io::Result<Vec<AgentInfo>>
where
    F: Fn(&RemoteHostConfig, &Request) -> io::Result<RemoteSourceSendResult>,
{
    let result = send(host, &agent_list_request())?;
    parse_agent_list_response(&result.response)
}

fn send_workspace_list<F>(host: &RemoteHostConfig, send: &F) -> io::Result<Vec<WorkspaceInfo>>
where
    F: Fn(&RemoteHostConfig, &Request) -> io::Result<RemoteSourceSendResult>,
{
    let result = send(host, &workspace_list_request())?;
    parse_workspace_list_response(&result.response)
}

pub(crate) fn ping_request() -> Request {
    Request {
        id: "remote-source.ping".to_string(),
        method: Method::Ping(PingParams::default()),
    }
}

pub(crate) fn agent_list_request() -> Request {
    Request {
        id: "remote-source.agent-list".to_string(),
        method: Method::AgentListLocal(EmptyParams::default()),
    }
}

pub(crate) fn workspace_list_request() -> Request {
    Request {
        id: "remote-source.workspace-list".to_string(),
        method: Method::WorkspaceListLocal(EmptyParams::default()),
    }
}

pub(crate) fn tab_list_request(workspace_id: &str) -> Request {
    Request {
        id: "remote-source.tab-list".to_string(),
        method: Method::TabList(TabListParams {
            workspace_id: Some(workspace_id.to_string()),
        }),
    }
}

pub(crate) fn layout_export_request(tab_id: &str) -> Request {
    Request {
        id: "remote-source.layout-export".to_string(),
        method: Method::LayoutExport(LayoutExportParams {
            tab_id: Some(tab_id.to_string()),
            pane_id: None,
        }),
    }
}

pub(crate) fn parse_ping_response(response: &str) -> io::Result<RemoteSourceCapabilities> {
    match parse_success_response(response)? {
        ResponseResult::Pong { capabilities, .. } => {
            let Some(federation) = capabilities.and_then(|capabilities| capabilities.federation)
            else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "remote API ping did not advertise federation support",
                ));
            };
            for method in [
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::AGENT_LIST_LOCAL,
            ] {
                if !federation.supports_method(method) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("remote API ping did not advertise federation method {method}"),
                    ));
                }
            }
            Ok(RemoteSourceCapabilities::from_federation(&federation))
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected pong response, got {other:?}"),
        )),
    }
}

pub(crate) fn parse_workspace_list_response(response: &str) -> io::Result<Vec<WorkspaceInfo>> {
    match parse_success_response(response)? {
        ResponseResult::WorkspaceList { workspaces } => Ok(workspaces),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected workspace.list_local response, got {other:?}"),
        )),
    }
}

pub(crate) fn parse_agent_list_response(response: &str) -> io::Result<Vec<AgentInfo>> {
    match parse_success_response(response)? {
        ResponseResult::AgentList { agents } => Ok(agents),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected agent.list response, got {other:?}"),
        )),
    }
}

pub(crate) fn parse_tab_list_response(response: &str) -> io::Result<Vec<TabInfo>> {
    match parse_success_response(response)? {
        ResponseResult::TabList { tabs } => Ok(tabs),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected tab.list response, got {other:?}"),
        )),
    }
}

fn parse_success_response(response: &str) -> io::Result<ResponseResult> {
    let value: serde_json::Value = serde_json::from_str(response).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid remote API response JSON: {err}"),
        )
    })?;
    parse_response_value(value)
        .map(|response| response.result)
        .map_err(remote_api_client_error)
}

fn remote_api_client_error(err: ApiClientError) -> io::Error {
    match err {
        ApiClientError::ErrorResponse(response) => io::Error::other(format!(
            "remote API error {}: {}",
            response.error.code, response.error.message
        )),
        other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::AtomicUsize;

    use crate::api::schema::{
        AgentStatus, ErrorBody, ErrorResponse, LayoutNode, LayoutPane, SuccessResponse,
    };

    use super::*;

    fn agent(terminal_id: &str) -> AgentInfo {
        AgentInfo {
            terminal_id: terminal_id.to_string(),
            name: Some("codex".to_string()),
            agent: Some("codex".to_string()),
            title: None,
            display_agent: Some("Codex".to_string()),
            agent_status: AgentStatus::Working,
            screen_detection_skipped: false,
            custom_status: None,
            state_labels: HashMap::new(),
            agent_session: None,
            workspace_id: "ws-1".to_string(),
            tab_id: "tab-1".to_string(),
            pane_id: "pane-1".to_string(),
            focused: false,
            cwd: None,
            foreground_cwd: None,
            revision: 1,
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
            active_tab_id: "tab-1".to_string(),
            agent_status: AgentStatus::Unknown,
            worktree: None,
        }
    }

    fn pong_response() -> String {
        serde_json::to_string(&SuccessResponse {
            id: "remote-source.ping".to_string(),
            result: ResponseResult::Pong {
                version: env!("CARGO_PKG_VERSION").to_string(),
                protocol: crate::protocol::PROTOCOL_VERSION,
                capabilities: Some(crate::api::schema::ServerCapabilities::current()),
            },
        })
        .unwrap()
    }

    fn old_pong_response_without_federation() -> String {
        serde_json::to_string(&SuccessResponse {
            id: "remote-source.ping".to_string(),
            result: ResponseResult::Pong {
                version: env!("CARGO_PKG_VERSION").to_string(),
                protocol: crate::protocol::PROTOCOL_VERSION,
                capabilities: Some(crate::api::schema::ServerCapabilities {
                    live_handoff: true,
                    detached_server_daemon: false,
                    federation: None,
                }),
            },
        })
        .unwrap()
    }

    fn pong_response_without_workspace_list_local() -> String {
        serde_json::to_string(&SuccessResponse {
            id: "remote-source.ping".to_string(),
            result: ResponseResult::Pong {
                version: env!("CARGO_PKG_VERSION").to_string(),
                protocol: crate::protocol::PROTOCOL_VERSION,
                capabilities: Some(crate::api::schema::ServerCapabilities {
                    live_handoff: true,
                    detached_server_daemon: false,
                    federation: Some(crate::api::schema::FederationCapabilities {
                        methods: vec![
                            crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE
                                .to_string(),
                            crate::api::schema::FederationCapabilities::AGENT_LIST_LOCAL
                                .to_string(),
                        ],
                    }),
                }),
            },
        })
        .unwrap()
    }

    fn agent_list_response(agents: Vec<AgentInfo>) -> String {
        serde_json::to_string(&SuccessResponse {
            id: "remote-source.agent-list".to_string(),
            result: ResponseResult::AgentList { agents },
        })
        .unwrap()
    }

    fn workspace_list_response(workspaces: Vec<WorkspaceInfo>) -> String {
        serde_json::to_string(&SuccessResponse {
            id: "remote-source.workspace-list".to_string(),
            result: ResponseResult::WorkspaceList { workspaces },
        })
        .unwrap()
    }

    fn tab(tab_id: &str, focused: bool) -> TabInfo {
        TabInfo {
            tab_id: tab_id.to_string(),
            workspace_id: "ws-1".to_string(),
            number: if focused { 1 } else { 2 },
            label: if focused { "active" } else { "other" }.to_string(),
            focused,
            pane_count: 1,
            agent_status: AgentStatus::Unknown,
        }
    }

    fn tab_list_response(tabs: Vec<TabInfo>) -> String {
        serde_json::to_string(&SuccessResponse {
            id: "remote-source.tab-list".to_string(),
            result: ResponseResult::TabList { tabs },
        })
        .unwrap()
    }

    fn layout_export_response(workspace_id: &str, tab_id: &str) -> String {
        serde_json::to_string(&SuccessResponse {
            id: "remote-source.layout-export".to_string(),
            result: ResponseResult::LayoutExport {
                layout: LayoutDescription {
                    workspace_id: workspace_id.to_string(),
                    tab_id: tab_id.to_string(),
                    zoomed: false,
                    focused_pane_id: format!("{tab_id}-1"),
                    root: LayoutNode::Pane {
                        pane: LayoutPane {
                            label: Some("shell".to_string()),
                            ..Default::default()
                        },
                    },
                },
            },
        })
        .unwrap()
    }

    #[test]
    fn remote_supervisor_returns_only_auto_policy_hosts() {
        // Only `Auto` hosts are started/probed automatically; `OnDemand` and
        // `Manual` hosts (sleeping/roaming remotes) are excluded.
        use crate::remote_target::RemoteConnectionPolicy;
        let registry = RemoteHostRegistry::from_configs(vec![
            RemoteHostConfig::new("jafar", "jafar", "default", true),
            RemoteHostConfig::new("ondemand", "ondemand", "default", false),
            RemoteHostConfig::new("manual", "manual", "default", true)
                .with_connection_policy(RemoteConnectionPolicy::Manual),
        ])
        .unwrap();

        let hosts = auto_connect_hosts(&registry);

        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].name, "jafar");
    }

    #[test]
    fn remote_supervisor_builds_poll_requests_without_subscriptions() {
        assert!(matches!(ping_request().method, Method::Ping(_)));
        assert!(matches!(
            agent_list_request().method,
            Method::AgentListLocal(_)
        ));
        assert!(matches!(
            workspace_list_request().method,
            Method::WorkspaceListLocal(_)
        ));
        assert!(matches!(
            tab_list_request("ws-1").method,
            Method::TabList(_)
        ));
    }

    #[test]
    fn remote_supervisor_ping_backoff_keeps_transient_failures_short() {
        let err = io::Error::new(io::ErrorKind::TimedOut, "ssh timed out");
        let mut backoff = TransientBackoff::default();

        // The first transient failure still starts at the short retry interval
        // with a small additive jitter window (base 15, window 3 -> [15s, 18s]);
        // only consecutive transient failures escalate from here.
        let interval = retry_interval_for_failure(&err, &mut backoff, "jafar", "default");
        assert!(interval >= REMOTE_SOURCE_RETRY_INTERVAL);
        assert!(interval <= REMOTE_SOURCE_RETRY_INTERVAL + Duration::from_secs(3));
        assert_eq!(interval, transient_retry_interval(0, "jafar", "default"));
    }

    #[test]
    fn remote_supervisor_ping_backoff_uses_long_interval_for_invalid_data() {
        let err = io::Error::new(
            io::ErrorKind::InvalidData,
            "remote API ping did not advertise federation support",
        );
        let mut backoff = TransientBackoff::default();

        assert_eq!(
            retry_interval_for_failure(&err, &mut backoff, "jafar", "default"),
            REMOTE_SOURCE_INCOMPATIBLE_RETRY_INTERVAL
        );
        assert!(REMOTE_SOURCE_INCOMPATIBLE_RETRY_INTERVAL > REMOTE_SOURCE_RETRY_INTERVAL);
        // NeedsUpdate must not consume the transient backoff counter.
        assert_eq!(backoff.consecutive_failures, 0);
    }

    #[test]
    fn remote_supervisor_ping_backoff_uses_long_interval_for_missing_binary() {
        let err = io::Error::new(
            io::ErrorKind::NotFound,
            "compatible herdr binary was not found",
        );
        let mut backoff = TransientBackoff::default();

        assert_eq!(
            retry_interval_for_failure(&err, &mut backoff, "jafar", "default"),
            REMOTE_SOURCE_INCOMPATIBLE_RETRY_INTERVAL
        );
        // NeedsUpdate must not consume the transient backoff counter.
        assert_eq!(backoff.consecutive_failures, 0);
    }

    #[test]
    fn remote_supervisor_transient_backoff_doubles_then_caps() {
        // Pure (jitter-free) base sequence: 15s, 30s, 60s, 120s, 240s, capped.
        // Jitter is layered on separately via transient_retry_interval, so the
        // pure base sequence stays an exact, testable value independent of jitter.
        assert_eq!(transient_backoff_interval(0), Duration::from_secs(15));
        assert_eq!(transient_backoff_interval(1), Duration::from_secs(30));
        assert_eq!(transient_backoff_interval(2), Duration::from_secs(60));
        assert_eq!(transient_backoff_interval(3), Duration::from_secs(120));
        assert_eq!(transient_backoff_interval(4), Duration::from_secs(240));
        // 15 * 2^5 = 480s exceeds the 300s cap; every further index holds at cap.
        assert_eq!(
            transient_backoff_interval(5),
            Duration::from_secs(REMOTE_SOURCE_TRANSIENT_BACKOFF_CAP_SECS)
        );
        assert_eq!(
            transient_backoff_interval(u32::MAX),
            Duration::from_secs(REMOTE_SOURCE_TRANSIENT_BACKOFF_CAP_SECS)
        );
        assert_eq!(REMOTE_SOURCE_TRANSIENT_BACKOFF_CAP_SECS, 5 * 60);
    }

    #[test]
    fn remote_supervisor_transient_backoff_resets_after_success() {
        let mut backoff = TransientBackoff::default();
        let host = "jafar";
        let session = "default";

        // The jittered interval for each failure index is deterministic for this
        // host/session: record_failure mirrors transient_retry_interval.
        let first = backoff.record_failure(host, session);
        assert_eq!(first, transient_retry_interval(0, host, session));
        let second = backoff.record_failure(host, session);
        backoff.record_failure(host, session);
        let fourth = backoff.record_failure(host, session);
        assert_eq!(fourth, transient_retry_interval(3, host, session));
        // Base 120 -> additive window min(30, 30) = 30, so the result is [120s, 150s].
        assert!(fourth >= Duration::from_secs(120));
        assert!(fourth <= Duration::from_secs(150));

        // Any successful ping or snapshot clears the ladder: the next transient
        // failure restarts the jitter sequence at the first interval.
        backoff.reset();
        assert_eq!(backoff.record_failure(host, session), first);
        assert_eq!(backoff.record_failure(host, session), second);
    }

    #[test]
    fn remote_supervisor_needs_update_failure_neither_escalates_nor_resets_backoff() {
        let mut backoff = TransientBackoff::default();
        let host = "jafar";
        let session = "default";
        let transient = io::Error::new(io::ErrorKind::TimedOut, "ssh timed out");
        let needs_update = io::Error::new(
            io::ErrorKind::NotFound,
            "compatible herdr binary was not found",
        );

        // First transient failure (index 0): jittered base 15, window 3 -> [15s, 18s].
        let first_transient = retry_interval_for_failure(&transient, &mut backoff, host, session);
        assert!(first_transient >= REMOTE_SOURCE_RETRY_INTERVAL);
        assert!(first_transient <= REMOTE_SOURCE_RETRY_INTERVAL + Duration::from_secs(3));
        assert_eq!(first_transient, transient_retry_interval(0, host, session));

        // An intervening NeedsUpdate failure keeps the fixed long interval...
        assert_eq!(
            retry_interval_for_failure(&needs_update, &mut backoff, host, session),
            REMOTE_SOURCE_INCOMPATIBLE_RETRY_INTERVAL
        );

        // ...and must NOT consume or reset the transient counter: the next
        // transient failure is the SECOND attempt (base 30, window 7), not first.
        let second_transient = retry_interval_for_failure(&transient, &mut backoff, host, session);
        assert!(second_transient >= Duration::from_secs(30));
        assert!(second_transient <= Duration::from_secs(37));
        assert_eq!(second_transient, transient_retry_interval(1, host, session));
    }

    #[test]
    fn remote_supervisor_transient_jitter_is_deterministic_per_host_session_index() {
        // Same (host, session, index) always returns the same interval, via both
        // the free helper and the stateful record_failure path.
        assert_eq!(
            transient_retry_interval(0, "jafar", "default"),
            transient_retry_interval(0, "jafar", "default")
        );
        assert_eq!(
            transient_retry_interval(3, "jafar", "default"),
            transient_retry_interval(3, "jafar", "default")
        );
        let mut a = TransientBackoff::default();
        let mut b = TransientBackoff::default();
        for index in 0..6 {
            assert_eq!(
                a.record_failure("jafar", "default"),
                b.record_failure("jafar", "default"),
                "index {index} must be stable"
            );
        }
    }

    #[test]
    fn remote_supervisor_transient_jitter_varies_and_stays_bounded_below_cap() {
        // Below the cap every interval is within [base, base + window] and never
        // exceeds the cap, while distinct hosts/sessions de-synchronize.
        for index in 0u32..5 {
            let base = transient_backoff_interval(index);
            let window = (base.as_secs() / 4).clamp(1, REMOTE_SOURCE_TRANSIENT_JITTER_WINDOW_SECS);
            let values = [
                transient_retry_interval(index, "jafar", "default"),
                transient_retry_interval(index, "home-mini", "dev"),
                transient_retry_interval(index, "steamdeck", "work"),
            ];
            for value in values {
                assert!(
                    value >= base,
                    "index {index}: {value:?} below base {base:?}"
                );
                assert!(
                    value <= base + Duration::from_secs(window),
                    "index {index}: {value:?} above base+window"
                );
                assert!(value.as_secs() <= REMOTE_SOURCE_TRANSIENT_BACKOFF_CAP_SECS);
            }
        }
        // De-synchronization: at least one host differs from another for some
        // below-cap index, so they do not all retry on the same wall-clock tick.
        let de_synchronized = (0u32..5).any(|index| {
            transient_retry_interval(index, "jafar", "default")
                != transient_retry_interval(index, "home-mini", "default")
        });
        assert!(
            de_synchronized,
            "expected hosts to de-synchronize below cap"
        );
    }

    #[test]
    fn remote_supervisor_transient_jitter_at_cap_de_synchronizes_within_window() {
        // At/above the cap the interval is a subtractive window: [cap - window, cap].
        let cap = Duration::from_secs(REMOTE_SOURCE_TRANSIENT_BACKOFF_CAP_SECS);
        let floor = Duration::from_secs(
            REMOTE_SOURCE_TRANSIENT_BACKOFF_CAP_SECS - REMOTE_SOURCE_TRANSIENT_JITTER_WINDOW_SECS,
        );
        let mut any_below_cap = false;
        for index in 5u32..12 {
            for (host, session) in [
                ("jafar", "default"),
                ("home-mini", "dev"),
                ("steamdeck", "work"),
            ] {
                let value = transient_retry_interval(index, host, session);
                assert!(value <= cap, "index {index}: {value:?} above cap");
                assert!(value >= floor, "index {index}: {value:?} below floor");
                if value < cap {
                    any_below_cap = true;
                }
            }
        }
        // Hosts must not pin forever to exactly the cap: at least one de-synchronizes.
        assert!(
            any_below_cap,
            "expected cap hosts to de-synchronize below the cap"
        );
    }

    #[test]
    fn remote_supervisor_failure_status_marks_invalid_data_as_needs_update() {
        let err = io::Error::new(
            io::ErrorKind::InvalidData,
            "remote API ping did not advertise federation support",
        );

        assert_eq!(
            remote_source_failure_status(&err),
            RemoteConnectionStatus::NeedsUpdate
        );
    }

    #[test]
    fn remote_supervisor_failure_status_marks_not_found_as_needs_update() {
        let err = io::Error::new(
            io::ErrorKind::NotFound,
            "compatible herdr binary was not found",
        );

        assert_eq!(
            remote_source_failure_status(&err),
            RemoteConnectionStatus::NeedsUpdate
        );
    }

    #[test]
    fn remote_supervisor_failure_status_marks_transport_as_unreachable() {
        let err = io::Error::new(io::ErrorKind::TimedOut, "ssh timed out");

        assert_eq!(
            remote_source_failure_status(&err),
            RemoteConnectionStatus::Unreachable
        );
    }

    #[test]
    fn remote_supervisor_failure_status_keeps_unknown_as_disconnected() {
        let err = io::Error::other("unexpected remote source failure");

        assert_eq!(
            remote_source_failure_status(&err),
            RemoteConnectionStatus::Disconnected
        );
    }

    #[test]
    fn remote_supervisor_loop_sends_snapshot_on_success() {
        let (tx, mut rx) = mpsc::channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let calls = Arc::new(AtomicUsize::new(0));
        let thread_calls = Arc::clone(&calls);
        let host = RemoteHostConfig::new("jafar", "jafar", "default", true);

        let handle = thread::spawn(move || {
            remote_source_supervisor_loop_with(host, tx, thread_stop, move |_host, request| {
                thread_calls.fetch_add(1, Ordering::Relaxed);
                match &request.method {
                    Method::Ping(_) => Ok(pong_response().into()),
                    Method::AgentListLocal(_) => {
                        Ok(agent_list_response(vec![agent("term-1")]).into())
                    }
                    Method::WorkspaceListLocal(_) => {
                        Ok(workspace_list_response(vec![workspace("ws-1", "tmp")]).into())
                    }
                    Method::TabList(_) => Ok(tab_list_response(vec![tab("tab-1", true)]).into()),
                    Method::LayoutExport(_) => Ok(layout_export_response("ws-1", "tab-1").into()),
                    _ => unreachable!("unexpected request"),
                }
            });
        });

        let event = rx.blocking_recv().unwrap();
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        let AppEvent::RemoteSourceSnapshot {
            host,
            agents,
            workspaces,
            capabilities,
            projections,
            tabs,
        } = event
        else {
            panic!("expected snapshot event");
        };
        assert_eq!(host, RemoteHostKey::new("jafar", "default"));
        assert!(capabilities.workspace_list_local);
        assert!(capabilities.workspace_create);
        assert!(capabilities.tab_list);
        assert!(capabilities.tab_create);
        assert!(capabilities.tab_focus);
        assert!(capabilities.tab_close);
        assert!(capabilities.layout_export);
        assert_eq!(agents[0].terminal_id, "term-1");
        let workspaces = workspaces.expect("workspace snapshot");
        assert_eq!(workspaces[0].workspace_id, "ws-1");
        assert_eq!(workspaces[0].label, "tmp");
        assert_eq!(projections.len(), 1);
        assert_eq!(projections[0].workspace_id, "ws-1");
        assert_eq!(projections[0].status, RemoteProjectionStatus::Available);
        assert_eq!(projections[0].layout.as_ref().unwrap().tab_id, "tab-1");
        assert_eq!(projections[0].tab_label.as_deref(), Some("active"));
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].tabs[0].tab_id, "tab-1");
        assert_eq!(calls.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn remote_supervisor_loop_publishes_prepared_bridge_state_on_ping() {
        // C5/test 2: a successful supervisor ping publishes the prepared bridge
        // state captured on the ping path through the AppEvent/reducer path
        // only. The supervisor thread never mutates AppState/RemoteSourceCache
        // directly; it sends an event, which the reducer applies.
        let (tx, mut rx) = mpsc::channel(8);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let host = RemoteHostConfig::new("jafar", "jafar", "default", true);
        let published_state = crate::remote::RemoteApiBridgeState {
            shell_path: "\"$HOME/.local/bin/herdr\"".to_string(),
            capabilities: crate::api::schema::FederationCapabilities::current(),
        };
        let expected_state = published_state.clone();

        let handle = thread::spawn(move || {
            remote_source_supervisor_loop_with(host, tx, thread_stop, move |_host, request| {
                match &request.method {
                    Method::Ping(_) => Ok(RemoteSourceSendResult {
                        response: pong_response(),
                        bridge_state: Some(published_state.clone()),
                    }),
                    Method::AgentListLocal(_) => {
                        Ok(agent_list_response(vec![agent("term-1")]).into())
                    }
                    Method::WorkspaceListLocal(_) => {
                        Ok(workspace_list_response(vec![workspace("ws-1", "tmp")]).into())
                    }
                    Method::TabList(_) => Ok(tab_list_response(vec![tab("tab-1", true)]).into()),
                    Method::LayoutExport(_) => Ok(layout_export_response("ws-1", "tab-1").into()),
                    _ => unreachable!("unexpected request"),
                }
            });
        });

        // The ping runs first and must publish the prepared bridge state event
        // before the snapshot event is published.
        let first = rx.blocking_recv().unwrap();
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        let AppEvent::RemoteSourceBridgeState { host, bridge_state } = first else {
            panic!("expected RemoteSourceBridgeState event first, got {first:?}");
        };
        assert_eq!(host, RemoteHostKey::new("jafar", "default"));
        assert_eq!(bridge_state.shell_path, expected_state.shell_path);
        assert_eq!(bridge_state.capabilities, expected_state.capabilities);
    }

    #[test]
    fn remote_supervisor_loop_does_not_publish_bridge_state_when_ping_omits_it() {
        // A response-only ping (no prepared state, e.g. an older/test sender)
        // must not publish a bridge-state event; the snapshot event is still
        // delivered. This proves the publish is gated on captured state.
        let (tx, mut rx) = mpsc::channel(8);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let host = RemoteHostConfig::new("jafar", "jafar", "default", true);

        let handle = thread::spawn(move || {
            remote_source_supervisor_loop_with(host, tx, thread_stop, move |_host, request| {
                match &request.method {
                    Method::Ping(_) => Ok(pong_response().into()),
                    Method::AgentListLocal(_) => {
                        Ok(agent_list_response(vec![agent("term-1")]).into())
                    }
                    Method::WorkspaceListLocal(_) => {
                        Ok(workspace_list_response(vec![workspace("ws-1", "tmp")]).into())
                    }
                    Method::TabList(_) => Ok(tab_list_response(vec![tab("tab-1", true)]).into()),
                    Method::LayoutExport(_) => Ok(layout_export_response("ws-1", "tab-1").into()),
                    _ => unreachable!("unexpected request"),
                }
            });
        });

        let first = rx.blocking_recv().unwrap();
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        // No prepared state captured -> first event is the snapshot, not a
        // bridge-state event.
        assert!(
            matches!(first, AppEvent::RemoteSourceSnapshot { .. }),
            "expected snapshot event first, got {first:?}"
        );
    }

    #[test]
    fn remote_supervisor_loop_skips_workspace_poll_when_capability_missing() {
        let (tx, mut rx) = mpsc::channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let calls = Arc::new(AtomicUsize::new(0));
        let thread_calls = Arc::clone(&calls);
        let host = RemoteHostConfig::new("jafar", "jafar", "default", true);

        let handle = thread::spawn(move || {
            remote_source_supervisor_loop_with(host, tx, thread_stop, move |_host, request| {
                thread_calls.fetch_add(1, Ordering::Relaxed);
                match &request.method {
                    Method::Ping(_) => Ok(pong_response_without_workspace_list_local().into()),
                    Method::AgentListLocal(_) => {
                        Ok(agent_list_response(vec![agent("term-1")]).into())
                    }
                    Method::WorkspaceListLocal(_) => panic!("workspace poll should be skipped"),
                    _ => unreachable!("unexpected request"),
                }
            });
        });

        let event = rx.blocking_recv().unwrap();
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        let AppEvent::RemoteSourceSnapshot {
            host,
            agents,
            workspaces,
            capabilities,
            projections,
            tabs,
        } = event
        else {
            panic!("expected snapshot event");
        };
        assert_eq!(host, RemoteHostKey::new("jafar", "default"));
        assert!(!capabilities.workspace_list_local);
        assert!(!capabilities.workspace_create);
        assert!(!capabilities.workspace_rename);
        assert!(!capabilities.tab_list);
        assert!(!capabilities.tab_create);
        assert!(!capabilities.tab_focus);
        assert!(!capabilities.tab_close);
        assert!(!capabilities.tab_rename);
        assert!(!capabilities.pane_split);
        assert!(!capabilities.pane_close);
        assert!(!capabilities.pane_rename);
        assert!(!capabilities.pane_focus);
        assert!(!capabilities.pane_focus_direction);
        assert!(!capabilities.layout_export);
        assert_eq!(agents[0].terminal_id, "term-1");
        assert_eq!(workspaces, None);
        assert!(projections.is_empty());
        assert!(tabs.is_empty());
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn remote_supervisor_loop_sends_disconnected_on_request_error() {
        let (tx, mut rx) = mpsc::channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let host = RemoteHostConfig::new("jafar", "jafar", "default", true);

        let handle = thread::spawn(move || {
            remote_source_supervisor_loop_with(host, tx, thread_stop, |_host, _request| {
                Err(io::Error::other("ssh failed"))
            });
        });

        let event = rx.blocking_recv().unwrap();
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        let AppEvent::RemoteSourceDisconnected { host, status } = event else {
            panic!("expected disconnected event");
        };
        assert_eq!(host, RemoteHostKey::new("jafar", "default"));
        assert_eq!(status, RemoteConnectionStatus::Unreachable);
    }

    #[test]
    fn remote_supervisor_loop_defers_snapshot_probe_after_failed_ping() {
        let (tx, mut rx) = mpsc::channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let calls = Arc::new(AtomicUsize::new(0));
        let thread_calls = Arc::clone(&calls);
        let host = RemoteHostConfig::new("jafar", "jafar", "default", true);

        let handle = thread::spawn(move || {
            remote_source_supervisor_loop_with(host, tx, thread_stop, move |_host, request| {
                thread_calls.fetch_add(1, Ordering::Relaxed);
                match &request.method {
                    Method::Ping(_) => {
                        Err(io::Error::new(io::ErrorKind::TimedOut, "ssh timed out"))
                    }
                    // A failed ping must defer next_snapshot to the retry
                    // interval, so none of these deeper probes should run while
                    // the host is offline. If deferral regresses, the panic
                    // surfaces through `handle.join()`.
                    Method::AgentListLocal(_)
                    | Method::WorkspaceListLocal(_)
                    | Method::TabList(_)
                    | Method::LayoutExport(_) => {
                        panic!("snapshot/projection probe must be deferred while ping fails")
                    }
                    _ => unreachable!("unexpected request"),
                }
            });
        });

        let event = rx.blocking_recv().unwrap();
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        let AppEvent::RemoteSourceDisconnected { host, status } = event else {
            panic!("expected disconnected event, not a snapshot");
        };
        assert_eq!(host, RemoteHostKey::new("jafar", "default"));
        assert_eq!(status, RemoteConnectionStatus::Unreachable);
        // Only the ping was attempted; the snapshot/projection probes were
        // deferred to the transient retry interval.
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn remote_supervisor_rejects_missing_federation_before_snapshot() {
        let (tx, mut rx) = mpsc::channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let calls = Arc::new(AtomicUsize::new(0));
        let thread_calls = Arc::clone(&calls);
        let host = RemoteHostConfig::new("jafar", "jafar", "default", true);

        let handle = thread::spawn(move || {
            remote_source_supervisor_loop_with(host, tx, thread_stop, move |_host, request| {
                thread_calls.fetch_add(1, Ordering::Relaxed);
                match &request.method {
                    Method::Ping(_) => Ok(old_pong_response_without_federation().into()),
                    Method::AgentListLocal(_) => panic!("snapshot should not be requested"),
                    Method::WorkspaceListLocal(_) => panic!("workspace should not be requested"),
                    _ => unreachable!("unexpected request"),
                }
            });
        });

        let event = rx.blocking_recv().unwrap();
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        let AppEvent::RemoteSourceDisconnected { host, status } = event else {
            panic!("expected disconnected event");
        };
        assert_eq!(host, RemoteHostKey::new("jafar", "default"));
        assert_eq!(status, RemoteConnectionStatus::NeedsUpdate);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn remote_supervisor_loop_marks_snapshot_invalid_data_as_needs_update() {
        let (tx, mut rx) = mpsc::channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let calls = Arc::new(AtomicUsize::new(0));
        let thread_calls = Arc::clone(&calls);
        let host = RemoteHostConfig::new("jafar", "jafar", "default", true);

        let handle = thread::spawn(move || {
            remote_source_supervisor_loop_with(host, tx, thread_stop, move |_host, request| {
                thread_calls.fetch_add(1, Ordering::Relaxed);
                match &request.method {
                    Method::Ping(_) => Ok(pong_response().into()),
                    Method::AgentListLocal(_) => Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "remote API ping did not advertise federation method agent.list",
                    )),
                    Method::WorkspaceListLocal(_) => panic!("workspace should not be requested"),
                    _ => unreachable!("unexpected request"),
                }
            });
        });

        let event = rx.blocking_recv().unwrap();
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        let AppEvent::RemoteSourceDisconnected { host, status } = event else {
            panic!("expected disconnected event");
        };
        assert_eq!(host, RemoteHostKey::new("jafar", "default"));
        assert_eq!(status, RemoteConnectionStatus::NeedsUpdate);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn remote_supervisor_loop_marks_bad_workspace_snapshot_as_needs_update() {
        let (tx, mut rx) = mpsc::channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let calls = Arc::new(AtomicUsize::new(0));
        let thread_calls = Arc::clone(&calls);
        let host = RemoteHostConfig::new("jafar", "jafar", "default", true);

        let handle = thread::spawn(move || {
            remote_source_supervisor_loop_with(host, tx, thread_stop, move |_host, request| {
                thread_calls.fetch_add(1, Ordering::Relaxed);
                match &request.method {
                    Method::Ping(_) => Ok(pong_response().into()),
                    Method::AgentListLocal(_) => {
                        Ok(agent_list_response(vec![agent("term-1")]).into())
                    }
                    Method::WorkspaceListLocal(_) => Ok("not json".to_string().into()),
                    _ => unreachable!("unexpected request"),
                }
            });
        });

        let event = rx.blocking_recv().unwrap();
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        let AppEvent::RemoteSourceDisconnected { host, status } = event else {
            panic!("expected disconnected event");
        };
        assert_eq!(host, RemoteHostKey::new("jafar", "default"));
        assert_eq!(status, RemoteConnectionStatus::NeedsUpdate);
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn remote_supervisor_loop_does_not_send_after_stop_during_request() {
        let (tx, mut rx) = mpsc::channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let send_stop = Arc::clone(&stop);
        let host = RemoteHostConfig::new("jafar", "jafar", "default", true);

        let handle = thread::spawn(move || {
            remote_source_supervisor_loop_with(host, tx, thread_stop, move |_host, _request| {
                send_stop.store(true, Ordering::Relaxed);
                Err(io::Error::other("stopped while request was running"))
            });
        });

        handle.join().unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn remote_supervisor_parses_agent_list_success() {
        let response = agent_list_response(vec![agent("term-1")]);

        let agents = parse_agent_list_response(&response).unwrap();

        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].terminal_id, "term-1");
    }

    #[test]
    fn remote_supervisor_parses_workspace_list_success() {
        let response = workspace_list_response(vec![workspace("ws-1", "tmp")]);

        let workspaces = parse_workspace_list_response(&response).unwrap();

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].workspace_id, "ws-1");
        assert_eq!(workspaces[0].label, "tmp");
    }

    #[test]
    fn remote_supervisor_rejects_error_response_for_agent_list() {
        let response = serde_json::to_string(&ErrorResponse {
            id: "remote-source.agent-list".to_string(),
            error: ErrorBody {
                code: "server_unavailable".to_string(),
                message: "no server".to_string(),
            },
        })
        .unwrap();

        let err = parse_agent_list_response(&response).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(err.to_string().contains("server_unavailable"));
    }

    #[test]
    fn remote_supervisor_rejects_malformed_agent_list_response() {
        let err = parse_agent_list_response("not json").unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn remote_supervisor_rejects_malformed_workspace_list_response() {
        let err = parse_workspace_list_response("not json").unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn remote_supervisor_rejects_wrong_success_response_type() {
        let response = serde_json::to_string(&SuccessResponse {
            id: "remote-source.agent-list".to_string(),
            result: ResponseResult::Ok {},
        })
        .unwrap();

        let err = parse_agent_list_response(&response).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("expected agent.list"));
    }

    #[test]
    fn remote_supervisor_rejects_wrong_workspace_success_response_type() {
        let response = serde_json::to_string(&SuccessResponse {
            id: "remote-source.workspace-list".to_string(),
            result: ResponseResult::Ok {},
        })
        .unwrap();

        let err = parse_workspace_list_response(&response).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("expected workspace.list_local"));
    }

    #[test]
    fn remote_supervisor_parse_ping_advertises_all_cached_capabilities() {
        // A remote advertising the full current federation method set converts to
        // cached capabilities with every cached field true, including the projected
        // pane split/close fields and the rename/focus/tab/workspace fields.
        let capabilities = parse_ping_response(&pong_response()).unwrap();
        assert!(capabilities.workspace_list_local);
        assert!(capabilities.workspace_create);
        assert!(capabilities.workspace_rename);
        assert!(capabilities.tab_list);
        assert!(capabilities.tab_create);
        assert!(capabilities.tab_focus);
        assert!(capabilities.tab_close);
        assert!(capabilities.tab_rename);
        assert!(capabilities.pane_split);
        assert!(capabilities.pane_close);
        assert!(capabilities.pane_rename);
        assert!(capabilities.pane_focus);
        assert!(capabilities.pane_focus_direction);
        assert!(capabilities.layout_export);
    }

    #[test]
    fn remote_supervisor_ping_succeeds_with_only_required_methods_and_optionals_false() {
        // A remote that advertises only the supervisor ping-required federation
        // methods (remote_api_bridge + agent_list_local) still pings successfully
        // and leaves every optional cached field -- including the projected pane
        // split/close fields -- false. The new fields must never become ping
        // prerequisites.
        let capabilities =
            parse_ping_response(&pong_response_without_workspace_list_local()).unwrap();
        assert!(!capabilities.workspace_list_local);
        assert!(!capabilities.workspace_create);
        assert!(!capabilities.workspace_rename);
        assert!(!capabilities.tab_list);
        assert!(!capabilities.tab_create);
        assert!(!capabilities.tab_focus);
        assert!(!capabilities.tab_close);
        assert!(!capabilities.tab_rename);
        assert!(!capabilities.pane_split);
        assert!(!capabilities.pane_close);
        assert!(!capabilities.pane_rename);
        assert!(!capabilities.pane_focus);
        assert!(!capabilities.pane_focus_direction);
        assert!(!capabilities.layout_export);
    }

    #[test]
    fn remote_supervisor_loop_keeps_host_connected_when_one_tab_list_fails() {
        let (tx, mut rx) = mpsc::channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let host = RemoteHostConfig::new("jafar", "jafar", "default", true);

        let handle = thread::spawn(move || {
            remote_source_supervisor_loop_with(host, tx, thread_stop, move |_host, request| {
                match &request.method {
                    Method::Ping(_) => Ok(pong_response().into()),
                    Method::AgentListLocal(_) => {
                        Ok(agent_list_response(vec![agent("term-1")]).into())
                    }
                    Method::WorkspaceListLocal(_) => {
                        Ok(workspace_list_response(vec![workspace("ws-1", "tmp")]).into())
                    }
                    Method::TabList(_) => Err(io::Error::other("tab list denied")),
                    Method::LayoutExport(_) => Ok(layout_export_response("ws-1", "tab-1").into()),
                    _ => unreachable!("unexpected request"),
                }
            });
        });

        let event = rx.blocking_recv().unwrap();
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        let AppEvent::RemoteSourceSnapshot {
            host,
            projections,
            tabs,
            ..
        } = event
        else {
            panic!("expected snapshot event, not disconnected");
        };
        assert_eq!(host, RemoteHostKey::new("jafar", "default"));
        assert_eq!(projections[0].status, RemoteProjectionStatus::Available);
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].workspace_id, "ws-1");
        assert_eq!(tabs[0].status, RemoteProjectionStatus::Unavailable);
    }

    #[test]
    fn remote_supervisor_loop_returns_snapshot_with_unavailable_projection_on_layout_export_failure(
    ) {
        let (tx, mut rx) = mpsc::channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let host = RemoteHostConfig::new("jafar", "jafar", "default", true);

        let handle = thread::spawn(move || {
            remote_source_supervisor_loop_with(host, tx, thread_stop, move |_host, request| {
                match &request.method {
                    Method::Ping(_) => Ok(pong_response().into()),
                    Method::AgentListLocal(_) => {
                        Ok(agent_list_response(vec![agent("term-1")]).into())
                    }
                    Method::WorkspaceListLocal(_) => {
                        Ok(workspace_list_response(vec![workspace("ws-1", "tmp")]).into())
                    }
                    Method::TabList(_) => Ok(tab_list_response(vec![tab("tab-1", true)]).into()),
                    Method::LayoutExport(_) => Err(io::Error::other("layout export denied")),
                    _ => unreachable!("unexpected request"),
                }
            });
        });

        let event = rx.blocking_recv().unwrap();
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        // A projection fetch failure must NOT drive RemoteSourceDisconnected; the
        // core snapshot is still delivered with an unavailable projection entry.
        let AppEvent::RemoteSourceSnapshot {
            host, projections, ..
        } = event
        else {
            panic!("expected snapshot event, not disconnected");
        };
        assert_eq!(host, RemoteHostKey::new("jafar", "default"));
        assert_eq!(projections.len(), 1);
        assert_eq!(projections[0].workspace_id, "ws-1");
        assert_eq!(projections[0].status, RemoteProjectionStatus::Unavailable);
        assert!(projections[0].layout.is_none());
    }
}
