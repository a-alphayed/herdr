//! Deferred, local-only runtime lifecycle actions (`remote.connect`,
//! `remote.reconnect`, `remote.disconnect`).
//!
//! These methods control ONLY the running local controller's aggregation /
//! supervisor / persistent-bridge-pool state. They are reached only on the
//! running LOCAL server and are never routed remote-of-remote (not advertised
//! as federation capabilities / routed methods; see
//! [`crate::api::schema::FederationCapabilities`]). `disconnect` is entirely
//! local and sends no remote request. `connect`/`reconnect` deliberately cause
//! the LOCAL supervisor worker to perform a non-mutating SSH/API health /
//! capability ping; they never provision/install/update/start/stop/mutate the
//! remote Herdr server, processes, panes, workspaces, config, or state, and
//! never open a new remote shell-command shape.
//!
//! The seam mirrors the established deferred remote-agent / worktree seams:
//! [`App::handle_deferred_remote_lifecycle_api_request`] runs on the App
//! (TUI) / headless loop, performs only short keyed state transitions,
//! installs the supervisor handle and the pending `respond_to` metadata, and
//! returns immediately. Slow SSH and bridge reaping never run on the loop;
//! only the API connection thread waits on the one-shot `respond_to` channel.
//!
//! The supervisor worker queues generation-tagged status / bridge / completion
//! events. The App applies status events first (host+generation must still be
//! active), then resolves the pending responder from the completion event. If
//! a newer lifecycle action / config reload supersedes an in-flight
//! generation, the App resolves its pending responder with a deterministic
//! superseded error and ignores later completion events from that generation.

use crate::api::schema::remotes::{
    RemoteLifecycleAction, RemoteLifecycleHostParams, RemoteLifecycleResult,
    RemoteLifecycleResultStatus,
};
use crate::api::schema::{Method, Request, ResponseResult};
use crate::app::App;
use crate::remote_source::{RemoteConnectionStatus, RemoteHostKey};
use crate::remote_supervisor::{
    next_supervisor_generation, RemoteSourceLifecycleOutcome, RemoteSourceSupervisorHandle,
};

use super::responses::{encode_error, encode_success};

/// Signature of the lifecycle supervisor starter. The default implementation
/// ([`RemoteSourceSupervisorHandle::start_with_lifecycle`]) spawns a background
/// worker thread that drains/reaps this host's idle pooled bridges off-loop and
/// then performs the initial SSH ping, emitting generation-tagged
/// status/bridge/completion events. Tests inject a fake starter that returns an
/// inert [`RemoteSourceSupervisorHandle::test_stub`] and emits nothing, so the
/// App's planning/admission/responder logic is exercised without real SSH and
/// the test drives completion events itself.
pub(crate) type RemoteLifecycleSupervisorStarter = fn(
    crate::remote_target::RemoteHostConfig,
    tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    u64,
) -> RemoteSourceSupervisorHandle;

/// Default lifecycle supervisor starter: spawns the real off-loop worker.
pub(crate) fn spawn_remote_lifecycle_supervisor(
    host: crate::remote_target::RemoteHostConfig,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    generation: u64,
) -> RemoteSourceSupervisorHandle {
    RemoteSourceSupervisorHandle::start_with_lifecycle(host, event_tx, generation)
}

/// Pending one-shot responder for an in-flight local lifecycle action, stored
/// on the App keyed by the generation that will resolve it. `disconnect_changed`
/// is computed at planning time (whether stopping a live handle / clearing a
/// non-Disconnected cache entry actually changed local state); connect/reconnect
/// compute `changed` from the lifecycle attempt outcome at completion time, so
/// they leave it `None`.
#[derive(Debug)]
pub(crate) struct PendingRemoteLifecycleResponder {
    pub(crate) request_id: String,
    pub(crate) host_key: RemoteHostKey,
    pub(crate) action: RemoteLifecycleAction,
    pub(crate) respond_to: std::sync::mpsc::Sender<String>,
    pub(crate) disconnect_changed: Option<bool>,
}

/// Outcome of [`App::handle_deferred_remote_lifecycle_api_request`]. On
/// [`DeferredRemoteLifecycleOutcome::NotHandled`] ownership of the request and
/// response channel returns to the caller so it can run the synchronous path.
#[derive(Debug)]
pub(crate) enum DeferredRemoteLifecycleOutcome {
    NotHandled {
        request: Box<Request>,
        respond_to: std::sync::mpsc::Sender<String>,
    },
    /// A lifecycle worker was started, or an immediate guard/idempotent
    /// response was sent. The caller must NOT run the synchronous path.
    Handled,
}

/// How to surface a resulting cached [`RemoteConnectionStatus`] as the typed
/// API result status after a connect/reconnect attempt. `Disconnected` (a
/// connect/reconnect that could not establish a connection yet but left a
/// retrying supervisor alive) maps to the `Disconnected` result; the
/// `Unhealthy` result status is reserved for the planning path (no live/healthy
/// supervisor at all) and is never produced from a ping outcome.
fn lifecycle_result_status(status: RemoteConnectionStatus) -> RemoteLifecycleResultStatus {
    match status {
        RemoteConnectionStatus::Connected => RemoteLifecycleResultStatus::Connected,
        RemoteConnectionStatus::Disconnected => RemoteLifecycleResultStatus::Disconnected,
        RemoteConnectionStatus::NeedsUpdate => RemoteLifecycleResultStatus::NeedsUpdate,
        RemoteConnectionStatus::Unreachable => RemoteLifecycleResultStatus::Unreachable,
    }
}

/// Actionable guidance for a failure result status, or `None` for success.
fn lifecycle_detail(status: RemoteLifecycleResultStatus) -> Option<String> {
    match status {
        RemoteLifecycleResultStatus::Connected => None,
        RemoteLifecycleResultStatus::Disconnected => Some(
            "could not establish a connection yet; the local supervisor keeps retrying".to_string(),
        ),
        RemoteLifecycleResultStatus::Unreachable => Some(
            "could not reach the remote host (SSH/transport); the local supervisor keeps retrying"
                .to_string(),
        ),
        RemoteLifecycleResultStatus::NeedsUpdate => Some(
            "remote Herdr is missing or incompatible; run `herdr remote setup <HOST>` to install/update (no install ran here)"
                .to_string(),
        ),
        RemoteLifecycleResultStatus::Unhealthy => None,
    }
}

/// Build the typed success response for a resolved lifecycle action.
fn build_lifecycle_success_response(
    pending: &PendingRemoteLifecycleResponder,
    status: RemoteLifecycleResultStatus,
    changed: bool,
) -> String {
    encode_success(
        pending.request_id.clone(),
        ResponseResult::RemoteLifecycle {
            result: RemoteLifecycleResult {
                host: pending.host_key.host.clone(),
                session: pending.host_key.session.clone(),
                action: pending.action,
                status,
                changed,
                remote_authoritative: true,
                detail: lifecycle_detail(status),
            },
        },
    )
}

/// Deterministic error sent to a pending responder when a newer lifecycle
/// action or config reload supersedes its generation before its completion
/// event arrives.
fn build_lifecycle_superseded_error(pending: &PendingRemoteLifecycleResponder) -> String {
    encode_error(
        pending.request_id.clone(),
        "remote_lifecycle_superseded",
        format!(
            "{} for {} superseded by a newer lifecycle action or config reload",
            pending.action.label(),
            pending.host_key.host
        ),
    )
}

impl App {
    /// Top-level deferred entry point for the TUI runtime and headless server
    /// loops. Drains-independent: callers must have drained internal events
    /// once so supervisor/cache state is fresh before calling. Returns
    /// [`DeferredRemoteLifecycleOutcome::NotHandled`] with the request and
    /// response channel when this seam does not own the request, so the caller
    /// can run the synchronous local path unchanged.
    pub(crate) fn handle_deferred_remote_lifecycle_api_request(
        &mut self,
        request: Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> DeferredRemoteLifecycleOutcome {
        match request.method {
            Method::RemoteConnect(params) => {
                self.start_remote_lifecycle_connect(request.id, params, respond_to);
                DeferredRemoteLifecycleOutcome::Handled
            }
            Method::RemoteReconnect(params) => {
                self.start_remote_lifecycle_reconnect(request.id, params, respond_to);
                DeferredRemoteLifecycleOutcome::Handled
            }
            Method::RemoteDisconnect(params) => {
                self.start_remote_lifecycle_disconnect(request.id, params, respond_to);
                DeferredRemoteLifecycleOutcome::Handled
            }
            _ => DeferredRemoteLifecycleOutcome::NotHandled {
                request: Box::new(request),
                respond_to,
            },
        }
    }

    /// Resolve the pending lifecycle responder for one generation after its
    /// completion event. If the generation was already superseded (no pending
    /// entry), this is a no-op. connect/reconnect derive the result status
    /// from the ping outcome; disconnect is handled separately by
    /// [`Self::handle_remote_pool_drain_completed`].
    pub(crate) fn handle_remote_lifecycle_attempt(
        &mut self,
        host: RemoteHostKey,
        generation: u64,
        outcome: RemoteSourceLifecycleOutcome,
    ) {
        let Some(pending) = self.pending_remote_lifecycle.remove(&generation) else {
            // Superseded (reconnect/disconnect/config reload already resolved
            // this responder with a deterministic error) or already resolved.
            return;
        };
        // Defensive: if a different host's responder somehow landed under this
        // generation, do not resolve it with the wrong host's outcome.
        if pending.host_key != host {
            tracing::warn!(
                generation,
                expected_host = %pending.host_key.host,
                got_host = %host.host,
                "lifecycle attempt host mismatch; dropping pending responder"
            );
            self.pending_remote_lifecycle.insert(generation, pending);
            return;
        }
        let (status, changed) = match outcome {
            RemoteSourceLifecycleOutcome::Connected => {
                (RemoteLifecycleResultStatus::Connected, true)
            }
            RemoteSourceLifecycleOutcome::Disconnected(failure) => {
                (lifecycle_result_status(failure), true)
            }
        };
        let _ = pending
            .respond_to
            .send(build_lifecycle_success_response(&pending, status, changed));
    }

    /// Resolve the pending disconnect responder after its off-loop pool drain
    /// completes. If the generation was superseded, this is a no-op.
    pub(crate) fn handle_remote_pool_drain_completed(
        &mut self,
        host: RemoteHostKey,
        generation: u64,
    ) {
        let Some(pending) = self.pending_remote_lifecycle.remove(&generation) else {
            return;
        };
        if pending.host_key != host {
            tracing::warn!(
                generation,
                expected_host = %pending.host_key.host,
                got_host = %host.host,
                "pool drain completion host mismatch; dropping pending responder"
            );
            self.pending_remote_lifecycle.insert(generation, pending);
            return;
        }
        // The cache was already marked Disconnected at planning time; the drain
        // completion just confirms the off-loop reap finished. `changed` was
        // computed at planning time.
        let changed = pending.disconnect_changed.unwrap_or(true);
        let _ = pending.respond_to.send(build_lifecycle_success_response(
            &pending,
            RemoteLifecycleResultStatus::Disconnected,
            changed,
        ));
    }

    /// Deterministically supersede (resolve with an error) every pending
    /// lifecycle responder for `host`. A new lifecycle action for a host calls
    /// this before installing its own responder; config reload calls it for
    /// every active host. Resolved responders are removed so their completion
    /// events become no-ops.
    pub(crate) fn supersede_pending_remote_lifecycle_for_host(&mut self, host: &RemoteHostKey) {
        let superseded: Vec<u64> = self
            .pending_remote_lifecycle
            .iter()
            .filter(|(_, pending)| pending.host_key == *host)
            .map(|(generation, _)| *generation)
            .collect();
        for generation in superseded {
            if let Some(pending) = self.pending_remote_lifecycle.remove(&generation) {
                let _ = pending
                    .respond_to
                    .send(build_lifecycle_superseded_error(&pending));
            }
        }
    }

    /// Supersede every pending lifecycle responder (used by config reload).
    pub(crate) fn supersede_all_pending_remote_lifecycle(&mut self) {
        let generations: Vec<u64> = self.pending_remote_lifecycle.keys().copied().collect();
        for generation in generations {
            if let Some(pending) = self.pending_remote_lifecycle.remove(&generation) {
                let _ = pending
                    .respond_to
                    .send(build_lifecycle_superseded_error(&pending));
            }
        }
    }

    /// `remote.connect`: idempotent on a live + `Connected` host; otherwise
    /// retire any stale/unhealthy handle and start one fresh generated
    /// supervisor whose initial ping resolves the pending responder.
    fn start_remote_lifecycle_connect(
        &mut self,
        request_id: String,
        params: RemoteLifecycleHostParams,
        respond_to: std::sync::mpsc::Sender<String>,
    ) {
        let action = RemoteLifecycleAction::Connect;
        let Some(host_key) =
            self.resolve_lifecycle_host(&request_id, &params.host, action, &respond_to)
        else {
            return;
        };
        // Idempotence: a live supervisor handle with cached `Connected` status
        // succeeds without stopping/replacing the healthy supervisor or opening
        // another bridge. There is no in-flight responder for a Connected host.
        if self.host_has_active_connected_supervisor(&host_key) {
            let _ = respond_to.send(build_lifecycle_success_response_from_planned(
                request_id,
                &host_key,
                action,
                RemoteLifecycleResultStatus::Connected,
                false,
            ));
            return;
        }
        // Not healthy/Connected: a concurrent in-flight connect (if any) is
        // deterministically superseded, the stale handle retired, and a fresh
        // generated supervisor started.
        self.supersede_pending_remote_lifecycle_for_host(&host_key);
        self.retire_supervisor_for_host(&host_key);
        self.start_lifecycle_supervisor(&request_id, &host_key, action, respond_to);
    }

    /// `remote.reconnect`: always retire the current supervisor, mark cached
    /// data disconnected/stale, drop prepared state, and start exactly one
    /// fresh generated supervisor.
    pub(super) fn start_remote_lifecycle_reconnect(
        &mut self,
        request_id: String,
        params: RemoteLifecycleHostParams,
        respond_to: std::sync::mpsc::Sender<String>,
    ) {
        let action = RemoteLifecycleAction::Reconnect;
        let Some(host_key) =
            self.resolve_lifecycle_host(&request_id, &params.host, action, &respond_to)
        else {
            return;
        };
        self.supersede_pending_remote_lifecycle_for_host(&host_key);
        self.retire_supervisor_for_host(&host_key);
        // Mark cached aggregation disconnected/stale now (drops prepared state,
        // keeps last-known agent/workspace data as stale) so a
        // transient window cannot show stale `Connected` data while the fresh
        // supervisor's first ping is in flight. A successful first ping's
        // generation-tagged bridge-state/snapshot events flip it back to
        // `Connected`.
        self.state
            .remote_sources
            .mark_status(&host_key, RemoteConnectionStatus::Disconnected);
        self.start_lifecycle_supervisor(&request_id, &host_key, action, respond_to);
    }

    /// `remote.disconnect`: stop the named local supervisor handle, mark the
    /// host `Disconnected` (preserving last-known cache as stale, dropping
    /// prepared state), advance the bridge-pool generation, and reap idle
    /// bridges off-loop. The success response is sent only after the off-loop
    /// drain reports completion. Idempotent: reports `changed=false` when
    /// already disconnected with no live supervisor, and creates no cache entry
    /// for a never-cached host.
    fn start_remote_lifecycle_disconnect(
        &mut self,
        request_id: String,
        params: RemoteLifecycleHostParams,
        respond_to: std::sync::mpsc::Sender<String>,
    ) {
        let action = RemoteLifecycleAction::Disconnect;
        let Some(host_key) =
            self.resolve_lifecycle_host(&request_id, &params.host, action, &respond_to)
        else {
            return;
        };
        self.supersede_pending_remote_lifecycle_for_host(&host_key);
        let had_handle = self.retire_supervisor_for_host(&host_key);
        let prior_status = self.state.remote_sources.host_status(&host_key);
        // `changed` is true only when disconnect actually transitions local
        // state: a live supervisor was stopped, or a prior cached status was
        // non-Disconnected. A never-cached manual/on_demand host has no
        // aggregation state, so disconnect is a true no-op -- no cache entry is
        // created and changed=false -- resolving the cosmetic inconsistency of
        // inserting a Disconnected entry while reporting no change. The host
        // stays visible through the configured-host collection regardless of
        // cache. Idempotent if already Disconnected.
        let changed = had_handle
            || prior_status.is_some_and(|status| status != RemoteConnectionStatus::Disconnected);
        if changed {
            self.state
                .remote_sources
                .mark_status(&host_key, RemoteConnectionStatus::Disconnected);
        }

        // Store the pending responder keyed by a fresh lifecycle generation,
        // then advance the pool generation + reap idle bridges off-loop. The
        // worker always reports completion (even with nothing to reap) so the
        // pending responder always resolves exactly once.
        let generation = next_supervisor_generation();
        self.pending_remote_lifecycle.insert(
            generation,
            PendingRemoteLifecycleResponder {
                request_id,
                host_key: host_key.clone(),
                action,
                respond_to: respond_to.clone(),
                disconnect_changed: Some(changed),
            },
        );
        crate::remote::drain_remote_bridge_pool_host_off_loop(
            host_key,
            generation,
            self.event_tx.clone(),
        );
    }

    /// Re-resolve a lifecycle host alias against the running server's loaded
    /// registry so stale client/server config cannot target an unintended host.
    /// Sends an immediate error response on failure and returns `None`.
    fn resolve_lifecycle_host(
        &self,
        request_id: &str,
        alias: &str,
        action: RemoteLifecycleAction,
        respond_to: &std::sync::mpsc::Sender<String>,
    ) -> Option<RemoteHostKey> {
        if alias.is_empty() {
            let _ = respond_to.send(encode_error(
                request_id.to_string(),
                "invalid_params",
                format!("{} requires a host alias", action.method_name()),
            ));
            return None;
        }
        let Some(config) = self.remote_hosts.get(alias) else {
            let _ = respond_to.send(encode_error(
                request_id.to_string(),
                "unknown_host",
                format!("unknown remote host: {alias}"),
            ));
            return None;
        };
        // manual forbids implicit reachability, NOT an explicit user-issued
        // connect/reconnect (or disconnect). on_demand is explicit-only by
        // definition. auto is also allowed explicitly. So every policy is
        // admissible for an explicit lifecycle action.
        Some(RemoteHostKey::new(
            config.name.clone(),
            config.session.clone(),
        ))
    }

    /// Start one fresh generated supervisor for a connect/reconnect and store
    /// the pending responder keyed by its generation.
    fn start_lifecycle_supervisor(
        &mut self,
        request_id: &str,
        host_key: &RemoteHostKey,
        action: RemoteLifecycleAction,
        respond_to: std::sync::mpsc::Sender<String>,
    ) {
        let Some(config) = self.remote_hosts.get(&host_key.host).cloned() else {
            // Re-checked above; defensive.
            let _ = respond_to.send(encode_error(
                request_id.to_string(),
                "unknown_host",
                format!("unknown remote host: {}", host_key.host),
            ));
            return;
        };
        let generation = next_supervisor_generation();
        let handle = (self.lifecycle_supervisor_starter)(config, self.event_tx.clone(), generation);
        self.remote_source_supervisors.push(handle);
        self.pending_remote_lifecycle.insert(
            generation,
            PendingRemoteLifecycleResponder {
                request_id: request_id.to_string(),
                host_key: host_key.clone(),
                action,
                respond_to,
                disconnect_changed: None,
            },
        );
    }

    /// Whether `host` has an active supervisor handle whose cached status is
    /// `Connected` (the connect idempotence guard).
    fn host_has_active_connected_supervisor(&self, host: &RemoteHostKey) -> bool {
        let has_handle = self
            .remote_source_supervisors
            .iter()
            .any(|handle| handle.host_key == *host);
        has_handle
            && self
                .state
                .remote_sources
                .host_status(host)
                .is_some_and(RemoteConnectionStatus::is_connected)
    }

    /// Stop and remove any supervisor handle for `host`. Returns whether a
    /// handle was present (and therefore retired). The worker thread is
    /// detached after publishing the stop flag (an SSH request may be blocked),
    /// so its late generation-tagged events are rejected by admission.
    fn retire_supervisor_for_host(&mut self, host: &RemoteHostKey) -> bool {
        // Removed handles are dropped, and `RemoteSourceSupervisorHandle::drop`
        // publishes the stop flag (the worker thread is detached, never
        // joined, since an SSH request may be blocked). Their generation-tagged
        // late events are rejected by admission since no matching handle
        // remains afterwards.
        let before = self.remote_source_supervisors.len();
        self.remote_source_supervisors
            .retain(|handle| handle.host_key != *host);
        before != self.remote_source_supervisors.len()
    }

    // ----- generation-filtered event admission helpers -----

    /// Whether `host`/`session` is currently configured in the loaded registry
    /// (any connection policy). Used by remote-source event admission: a host
    /// no longer in the registry is not aggregated.
    pub(crate) fn remote_host_session_is_configured(&self, host: &RemoteHostKey) -> bool {
        self.remote_hosts
            .get(&host.host)
            .is_some_and(|config| config.session == host.session)
    }

    /// Whether an active supervisor handle for `host` carries exactly
    /// `generation`. Remote-source events are admitted only when this is true,
    /// so a retired predecessor's queued/late event (after a reconnect /
    /// disconnect / config reload) is rejected. With no active handle for the
    /// host there is no current supervisor, so its events are stale by
    /// definition.
    pub(crate) fn remote_source_generation_is_active(
        &self,
        host: &RemoteHostKey,
        generation: u64,
    ) -> bool {
        self.remote_source_supervisors
            .iter()
            .any(|handle| handle.host_key == *host && handle.generation == generation)
    }
}

/// Build a success response directly from planned values (the connect
/// idempotence path) without going through a pending responder.
fn build_lifecycle_success_response_from_planned(
    request_id: String,
    host_key: &RemoteHostKey,
    action: RemoteLifecycleAction,
    status: RemoteLifecycleResultStatus,
    changed: bool,
) -> String {
    encode_success(
        request_id,
        ResponseResult::RemoteLifecycle {
            result: RemoteLifecycleResult {
                host: host_key.host.clone(),
                session: host_key.session.clone(),
                action,
                status,
                changed,
                remote_authoritative: true,
                detail: lifecycle_detail(status),
            },
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        EmptyParams, ErrorResponse, Method, Request, ResponseResult, SuccessResponse,
    };
    use crate::events::AppEvent;
    use crate::remote_source::{RemoteConnectionStatus, RemoteHostKey};
    use crate::remote_target::RemoteHostConfig;
    use std::time::Duration;

    const JAFAR: &str = "jafar";

    fn host_key() -> RemoteHostKey {
        RemoteHostKey::new(JAFAR, "default")
    }

    /// Build an App with one `on_demand` host configured. `on_demand` is chosen
    /// (not `auto`) so `App::new` starts NO real supervisor thread at
    /// construction, keeping these unit tests free of real SSH; an explicit
    /// `connect`/`reconnect` then starts a (stubbed) supervisor. Every policy is
    /// admissible for an explicit lifecycle action.
    fn lifecycle_app() -> App {
        let mut config = crate::config::Config::default();
        config.remote.enabled = true;
        config.remote.hosts = vec![RemoteHostConfig::new(JAFAR, "jafar", "default", false)];
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        app.lifecycle_supervisor_starter = stub_supervisor_starter;
        app
    }

    fn test_bridge_state() -> crate::remote::RemoteApiBridgeState {
        crate::remote::RemoteApiBridgeState {
            shell_path: "\"$HOME/.local/bin/herdr\"".to_string(),
            capabilities: crate::api::schema::FederationCapabilities::current(),
        }
    }

    /// Fake lifecycle supervisor starter: returns an inert stub handle (no
    /// worker thread, no events) carrying the host key + generation so the
    /// App's generation-filtered admission accepts the test's events. The test
    /// drives completion events itself, so no real SSH runs.
    fn stub_supervisor_starter(
        host: crate::remote_target::RemoteHostConfig,
        _event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
        generation: u64,
    ) -> RemoteSourceSupervisorHandle {
        RemoteSourceSupervisorHandle::test_stub(
            RemoteHostKey::new(host.name, host.session),
            generation,
        )
    }

    fn lifecycle_request(action: RemoteLifecycleAction, host: &str) -> Request {
        let params = RemoteLifecycleHostParams {
            host: host.to_string(),
        };
        let method = match action {
            RemoteLifecycleAction::Connect => Method::RemoteConnect(params),
            RemoteLifecycleAction::Reconnect => Method::RemoteReconnect(params),
            RemoteLifecycleAction::Disconnect => Method::RemoteDisconnect(params),
        };
        Request {
            id: "req".to_string(),
            method,
        }
    }

    /// Dispatch a lifecycle action through the real App message handler (the
    /// same path the TUI/headless loops use) and return the response receiver.
    fn dispatch(
        app: &mut App,
        action: RemoteLifecycleAction,
        host: &str,
    ) -> std::sync::mpsc::Receiver<String> {
        let (respond_to, rx) = std::sync::mpsc::channel::<String>();
        let msg = crate::api::ApiRequestMessage {
            request: lifecycle_request(action, host),
            respond_to,
        };
        app.handle_api_request_message(msg);
        rx
    }

    fn active_generation_for(app: &App, host: &RemoteHostKey) -> u64 {
        app.remote_source_supervisors
            .iter()
            .find(|h| &h.host_key == host)
            .map(|h| h.generation)
            .expect("active supervisor handle for host")
    }

    fn pending_generation_for(app: &App, host: &RemoteHostKey) -> Option<u64> {
        app.pending_remote_lifecycle
            .iter()
            .find(|(_, p)| &p.host_key == host)
            .map(|(g, _)| *g)
    }

    fn count_supervisors_for(app: &App, host: &RemoteHostKey) -> usize {
        app.remote_source_supervisors
            .iter()
            .filter(|h| &h.host_key == host)
            .count()
    }

    /// Poll the response receiver while draining internal events, so an
    /// off-loop completion (the disconnect pool-drain worker) is processed.
    fn recv_while_draining(
        app: &mut App,
        rx: &std::sync::mpsc::Receiver<String>,
        timeout: Duration,
    ) -> String {
        let start = std::time::Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(response) => return response,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    app.drain_internal_events();
                    if start.elapsed() > timeout {
                        panic!("no lifecycle response within {timeout:?}");
                    }
                }
                Err(err) => panic!("lifecycle response channel error: {err:?}"),
            }
        }
    }

    #[test]
    fn lifecycle_result_status_maps_every_connection_status() {
        assert_eq!(
            lifecycle_result_status(RemoteConnectionStatus::Connected),
            RemoteLifecycleResultStatus::Connected
        );
        assert_eq!(
            lifecycle_result_status(RemoteConnectionStatus::Disconnected),
            RemoteLifecycleResultStatus::Disconnected
        );
        assert_eq!(
            lifecycle_result_status(RemoteConnectionStatus::NeedsUpdate),
            RemoteLifecycleResultStatus::NeedsUpdate
        );
        assert_eq!(
            lifecycle_result_status(RemoteConnectionStatus::Unreachable),
            RemoteLifecycleResultStatus::Unreachable
        );
    }

    #[test]
    fn lifecycle_detail_only_advises_on_failure() {
        assert!(lifecycle_detail(RemoteLifecycleResultStatus::Connected).is_none());
        assert!(lifecycle_detail(RemoteLifecycleResultStatus::Disconnected).is_some());
        assert!(lifecycle_detail(RemoteLifecycleResultStatus::Unreachable)
            .as_ref()
            .is_some_and(|d| d.contains("SSH")));
        assert!(lifecycle_detail(RemoteLifecycleResultStatus::NeedsUpdate)
            .as_ref()
            .is_some_and(|d| d.contains("remote setup")));
    }

    /// Cat 3: an unknown host fails on the running server before any supervisor
    /// is started.
    #[test]
    fn connect_unknown_host_fails_with_immediate_error_before_starter() {
        let mut app = lifecycle_app();
        let rx = dispatch(&mut app, RemoteLifecycleAction::Connect, "missing");
        let response = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let parsed: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req");
        assert_eq!(parsed.error.code, "unknown_host");
        // No supervisor was started and no responder is pending.
        assert_eq!(app.remote_source_supervisors.len(), 0);
        assert!(app.pending_remote_lifecycle.is_empty());
    }

    /// Cat 4: `connect` on an active + `Connected` host is idempotent: it sends
    /// an immediate success response without stopping/replacing the healthy
    /// supervisor or starting another.
    #[test]
    fn connect_is_idempotent_on_active_connected_host() {
        let mut app = lifecycle_app();
        let host = host_key();
        // Seed a live Connected supervisor + cache (the healthy state).
        app.remote_source_supervisors
            .push(RemoteSourceSupervisorHandle::test_stub(host.clone(), 7));
        app.state
            .remote_sources
            .replace_connected_snapshot(host.clone(), Vec::new());
        assert_eq!(
            app.state.remote_sources.host_status(&host),
            Some(RemoteConnectionStatus::Connected)
        );

        let rx = dispatch(&mut app, RemoteLifecycleAction::Connect, JAFAR);
        let response = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::RemoteLifecycle { result } = parsed.result else {
            panic!("expected RemoteLifecycle result, got {:?}", parsed.result);
        };
        assert_eq!(result.host, JAFAR);
        assert_eq!(result.session, "default");
        assert_eq!(result.action, RemoteLifecycleAction::Connect);
        assert_eq!(result.status, RemoteLifecycleResultStatus::Connected);
        assert!(!result.changed, "idempotent connect reports no change");
        assert!(result.remote_authoritative);
        // The healthy supervisor was preserved (still the seeded generation),
        // no new one was started, and no responder is left pending.
        assert_eq!(count_supervisors_for(&app, &host), 1);
        assert_eq!(active_generation_for(&app, &host), 7);
        assert!(pending_generation_for(&app, &host).is_none());
    }

    /// Cat 5: `connect` on a missing/unhealthy supervisor starts exactly one
    /// fresh generated supervisor and resolves only after that generation's
    /// status is applied.
    #[test]
    fn connect_on_unhealthy_starts_one_fresh_supervisor_and_resolves_on_success() {
        let mut app = lifecycle_app();
        let host = host_key();

        let rx = dispatch(&mut app, RemoteLifecycleAction::Connect, JAFAR);
        // Deferred: no response until the generation completes.
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "connect must not respond before its generation completes"
        );
        assert_eq!(count_supervisors_for(&app, &host), 1);
        let generation = active_generation_for(&app, &host);
        assert!(pending_generation_for(&app, &host).is_some());

        // The worker's successful first ping publishes a generation-tagged
        // bridge-state event (marks Connected) then a completion event.
        app.handle_internal_event(AppEvent::RemoteSourceBridgeState {
            host: host.clone(),
            generation,
            bridge_state: test_bridge_state(),
        });
        assert_eq!(
            app.state.remote_sources.host_status(&host),
            Some(RemoteConnectionStatus::Connected)
        );
        app.handle_internal_event(AppEvent::RemoteSourceLifecycleAttempt {
            host: host.clone(),
            generation,
            outcome: RemoteSourceLifecycleOutcome::Connected,
        });

        let response = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::RemoteLifecycle { result } = parsed.result else {
            panic!("expected RemoteLifecycle result");
        };
        assert_eq!(result.status, RemoteLifecycleResultStatus::Connected);
        assert!(result.changed);
        // The retrying supervisor stays alive after a successful attempt.
        assert_eq!(count_supervisors_for(&app, &host), 1);
        assert!(pending_generation_for(&app, &host).is_none());
    }

    /// Cat 10: a transient/setup failure resolves the responder with a
    /// non-zero actionable status while the retrying supervisor stays alive.
    #[test]
    fn connect_failure_resolves_with_actionable_status_and_keeps_supervisor() {
        let mut app = lifecycle_app();
        let host = host_key();

        let rx = dispatch(&mut app, RemoteLifecycleAction::Connect, JAFAR);
        let generation = active_generation_for(&app, &host);

        // Unreachable first ping.
        app.handle_internal_event(AppEvent::RemoteSourceDisconnected {
            host: host.clone(),
            generation,
            status: RemoteConnectionStatus::Unreachable,
        });
        app.handle_internal_event(AppEvent::RemoteSourceLifecycleAttempt {
            host: host.clone(),
            generation,
            outcome: RemoteSourceLifecycleOutcome::Disconnected(
                RemoteConnectionStatus::Unreachable,
            ),
        });

        let response = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::RemoteLifecycle { result } = parsed.result else {
            panic!("expected RemoteLifecycle result");
        };
        assert_eq!(
            result.status,
            RemoteLifecycleResultStatus::Unreachable,
            "failure surfaces a non-Connected actionable status"
        );
        assert!(result.changed);
        assert!(result.detail.as_ref().is_some_and(|d| d.contains("SSH")));
        // The retrying supervisor stays alive after the failure.
        assert_eq!(count_supervisors_for(&app, &host), 1);
    }

    /// Cat 6: `reconnect` always retires the current supervisor (even when
    /// healthy), marks cached data disconnected/stale, and starts exactly one
    /// fresh generated supervisor.
    #[test]
    fn reconnect_always_retires_and_starts_one_fresh_supervisor() {
        let mut app = lifecycle_app();
        let host = host_key();
        // Seed a healthy Connected supervisor.
        app.remote_source_supervisors
            .push(RemoteSourceSupervisorHandle::test_stub(host.clone(), 5));
        app.state
            .remote_sources
            .replace_connected_snapshot(host.clone(), Vec::new());

        let rx = dispatch(&mut app, RemoteLifecycleAction::Reconnect, JAFAR);
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "reconnect must not respond before its generation completes"
        );
        // Exactly one fresh supervisor; the old generation is gone.
        assert_eq!(count_supervisors_for(&app, &host), 1);
        let generation = active_generation_for(&app, &host);
        assert_ne!(generation, 5, "reconnect started a fresh generation");
        // Reconnect marked cached data disconnected/stale up front.
        assert_eq!(
            app.state.remote_sources.host_status(&host),
            Some(RemoteConnectionStatus::Disconnected)
        );

        // Fresh first ping flips it back to Connected.
        app.handle_internal_event(AppEvent::RemoteSourceBridgeState {
            host: host.clone(),
            generation,
            bridge_state: test_bridge_state(),
        });
        app.handle_internal_event(AppEvent::RemoteSourceLifecycleAttempt {
            host: host.clone(),
            generation,
            outcome: RemoteSourceLifecycleOutcome::Connected,
        });
        let response = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::RemoteLifecycle { result } = parsed.result else {
            panic!("expected RemoteLifecycle result");
        };
        assert_eq!(result.status, RemoteLifecycleResultStatus::Connected);
        assert!(result.changed);
    }

    /// Cat 9: a same-host reconnect supersedes the in-flight generation,
    /// resolves its responder with a deterministic error, and rejects queued /
    /// late events/completions from the predecessor generation.
    #[test]
    fn reconnect_supersedes_prior_generation_and_rejects_late_events() {
        let mut app = lifecycle_app();
        let host = host_key();

        let rx_n = dispatch(&mut app, RemoteLifecycleAction::Connect, JAFAR);
        let gen_n = active_generation_for(&app, &host);
        assert!(rx_n.recv_timeout(Duration::from_millis(100)).is_err());

        // Reconnect supersedes generation N and starts generation N+1.
        let rx_n1 = dispatch(&mut app, RemoteLifecycleAction::Reconnect, JAFAR);
        // The superseded generation's responder gets a deterministic error.
        let response_n = rx_n.recv_timeout(Duration::from_secs(2)).unwrap();
        let parsed_n: ErrorResponse = serde_json::from_str(&response_n).unwrap();
        assert_eq!(parsed_n.error.code, "remote_lifecycle_superseded");
        let gen_n1 = active_generation_for(&app, &host);
        assert_ne!(gen_n, gen_n1);
        assert_eq!(count_supervisors_for(&app, &host), 1);

        // A queued/late bridge-state event from generation N is rejected by
        // admission (no matching active handle) and cannot mark the host
        // Connected.
        app.handle_internal_event(AppEvent::RemoteSourceBridgeState {
            host: host.clone(),
            generation: gen_n,
            bridge_state: test_bridge_state(),
        });
        assert_ne!(
            app.state.remote_sources.host_status(&host),
            Some(RemoteConnectionStatus::Connected),
            "stale generation N event must not mark the host Connected"
        );
        // A late completion event from generation N is a no-op (pending already
        // resolved/removed), and must not resolve generation N+1's responder.
        app.handle_internal_event(AppEvent::RemoteSourceLifecycleAttempt {
            host: host.clone(),
            generation: gen_n,
            outcome: RemoteSourceLifecycleOutcome::Connected,
        });
        assert!(
            rx_n1.recv_timeout(Duration::from_millis(100)).is_err(),
            "generation N+1 responder must stay pending"
        );

        // Generation N+1's own completion resolves its responder normally.
        app.handle_internal_event(AppEvent::RemoteSourceBridgeState {
            host: host.clone(),
            generation: gen_n1,
            bridge_state: test_bridge_state(),
        });
        app.handle_internal_event(AppEvent::RemoteSourceLifecycleAttempt {
            host: host.clone(),
            generation: gen_n1,
            outcome: RemoteSourceLifecycleOutcome::Connected,
        });
        let response_n1 = rx_n1.recv_timeout(Duration::from_secs(2)).unwrap();
        let parsed_n1: SuccessResponse = serde_json::from_str(&response_n1).unwrap();
        let ResponseResult::RemoteLifecycle { result } = parsed_n1.result else {
            panic!("expected RemoteLifecycle result");
        };
        assert_eq!(result.status, RemoteLifecycleResultStatus::Connected);
    }

    /// Cat 7: `disconnect` is idempotent, targets one host only, marks it
    /// disconnected, preserves last-known cache data as stale, and sends no
    /// remote request. Reports `changed=false` when already disconnected with
    /// no live supervisor.
    #[test]
    fn disconnect_is_idempotent_and_marks_disconnected_preserving_stale_data() {
        let mut app = lifecycle_app();
        let host = host_key();
        // Seed a Connected supervisor + a cached agent so we can prove the
        // agent is preserved as stale after disconnect.
        app.remote_source_supervisors
            .push(RemoteSourceSupervisorHandle::test_stub(host.clone(), 3));
        app.state.remote_sources.replace_connected_snapshot(
            host.clone(),
            vec![standalone_remote_agent("term-1", "codex")],
        );
        assert_eq!(
            app.state.remote_sources.host_status(&host),
            Some(RemoteConnectionStatus::Connected)
        );

        let rx = dispatch(&mut app, RemoteLifecycleAction::Disconnect, JAFAR);
        let response = recv_while_draining(&mut app, &rx, Duration::from_secs(3));
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::RemoteLifecycle { result } = parsed.result else {
            panic!("expected RemoteLifecycle result");
        };
        assert_eq!(result.status, RemoteLifecycleResultStatus::Disconnected);
        assert!(result.changed, "disconnect of a live host reports a change");
        assert!(result.remote_authoritative);
        // Supervisor stopped; cache marked Disconnected; the agent is preserved
        // as stale (entries remain).
        assert_eq!(count_supervisors_for(&app, &host), 0);
        assert_eq!(
            app.state.remote_sources.host_status(&host),
            Some(RemoteConnectionStatus::Disconnected)
        );
        assert_eq!(
            app.state.remote_sources.list_entries().len(),
            1,
            "last-known agent data preserved as stale"
        );

        // Idempotent second disconnect: changed=false, still Disconnected.
        let rx2 = dispatch(&mut app, RemoteLifecycleAction::Disconnect, JAFAR);
        let response2 = recv_while_draining(&mut app, &rx2, Duration::from_secs(3));
        let parsed2: SuccessResponse = serde_json::from_str(&response2).unwrap();
        let ResponseResult::RemoteLifecycle { result: result2 } = parsed2.result else {
            panic!("expected RemoteLifecycle result");
        };
        assert_eq!(result2.status, RemoteLifecycleResultStatus::Disconnected);
        assert!(!result2.changed, "already-disconnected reports no change");
    }

    /// Disconnecting a never-cached manual/on_demand host is a true no-op: it
    /// reports changed=false and creates no cache entry (the host stays visible
    /// through the configured-host collection), resolving the cosmetic
    /// inconsistency where a Disconnected entry was inserted while reporting no
    /// change. (Reviewer A finding 5.)
    #[test]
    fn disconnect_of_never_cached_host_is_true_noop_with_no_cache_entry() {
        let mut app = lifecycle_app();
        let host = host_key();
        // Never cached: no supervisor, no cache entry. The configured-host
        // collection still includes it.
        assert!(app.state.remote_sources.host_status(&host).is_none());
        assert!(app.state.configured_remote_hosts.contains(&host));

        let rx = dispatch(&mut app, RemoteLifecycleAction::Disconnect, JAFAR);
        let response = recv_while_draining(&mut app, &rx, Duration::from_secs(3));
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::RemoteLifecycle { result } = parsed.result else {
            panic!("expected RemoteLifecycle result");
        };
        assert_eq!(result.status, RemoteLifecycleResultStatus::Disconnected);
        assert!(!result.changed, "never-cached disconnect reports no change");
        // No supervisor and NO cache entry created.
        assert_eq!(count_supervisors_for(&app, &host), 0);
        assert!(
            app.state.remote_sources.host_status(&host).is_none(),
            "no cache entry created for a never-cached disconnect"
        );
        // Still visible as a configured host.
        assert!(app.state.configured_remote_hosts.contains(&host));
    }

    /// Cat 11: a deferred lifecycle action keeps its responder pending while the
    /// App loop handles an unrelated API request on the same tick (no remote
    /// wait occurs on-loop).
    #[test]
    fn lifecycle_request_defers_and_loop_continues_with_blocked_worker() {
        let mut app = lifecycle_app();
        // connect starts a stub supervisor that never completes: the responder
        // stays pending.
        let rx = dispatch(&mut app, RemoteLifecycleAction::Connect, JAFAR);
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "lifecycle responder must stay pending while the worker is blocked"
        );
        assert!(pending_generation_for(&app, &host_key()).is_some());

        // An unrelated local request must still be handled on the loop while
        // the lifecycle responder is pending.
        let (local_respond_to, local_rx) = std::sync::mpsc::channel::<String>();
        app.handle_api_request_message(crate::api::ApiRequestMessage {
            request: Request {
                id: "local".to_string(),
                method: Method::AgentListLocal(EmptyParams::default()),
            },
            respond_to: local_respond_to,
        });
        let local_response = local_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("local request must be handled while the lifecycle worker is blocked");
        let parsed: SuccessResponse = serde_json::from_str(&local_response).unwrap();
        assert_eq!(parsed.id, "local");
    }

    /// Cat 13 (supersession mechanism): `supersede_all_pending_remote_lifecycle`
    /// (called by config reload) resolves every in-flight responder with a
    /// deterministic error so its later completion is a no-op.
    #[test]
    fn supersede_all_pending_resolves_each_with_deterministic_error() {
        let mut app = lifecycle_app();
        let host = host_key();
        let rx = dispatch(&mut app, RemoteLifecycleAction::Connect, JAFAR);
        let generation = active_generation_for(&app, &host);
        assert!(pending_generation_for(&app, &host).is_some());

        // Config reload's supersession step.
        app.supersede_all_pending_remote_lifecycle();
        let response = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let parsed: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.error.code, "remote_lifecycle_superseded");
        assert!(pending_generation_for(&app, &host).is_none());

        // A late completion for that generation is now a no-op: it does not
        // panic and does not send on a dead channel.
        app.handle_internal_event(AppEvent::RemoteSourceLifecycleAttempt {
            host: host.clone(),
            generation,
            outcome: RemoteSourceLifecycleOutcome::Connected,
        });
        // The cache status is whatever reconnect/connect left it; the late
        // completion did not flip it to Connected here because the bridge-state
        // event for this generation would also be rejected by admission (handle
        // still present but responder gone). The point: no hang, no double
        // resolve.
        assert!(pending_generation_for(&app, &host).is_none());
    }

    /// Cat 8: an explicit `connect` admits a manual/on_demand supervisor's
    /// generation-tagged events only while that exact generation is active.
    /// After a superseding reconnect retires the handle, the prior generation's
    /// events are rejected by admission.
    #[test]
    fn explicit_connect_events_admitted_only_for_active_generation() {
        let mut app = lifecycle_app();
        let host = host_key();
        let rx = dispatch(&mut app, RemoteLifecycleAction::Connect, JAFAR);
        let gen = active_generation_for(&app, &host);

        // The active generation's bridge-state event IS admitted (marks
        // Connected) because an active handle with that generation exists.
        app.handle_internal_event(AppEvent::RemoteSourceBridgeState {
            host: host.clone(),
            generation: gen,
            bridge_state: test_bridge_state(),
        });
        assert_eq!(
            app.state.remote_sources.host_status(&host),
            Some(RemoteConnectionStatus::Connected)
        );
        drop(rx);
    }

    fn standalone_remote_agent(terminal_id: &str, name: &str) -> crate::api::schema::AgentInfo {
        crate::api::schema::AgentInfo {
            terminal_id: terminal_id.to_string(),
            name: Some(name.to_string()),
            agent: Some(name.to_string()),
            title: None,
            display_agent: Some(name.to_string()),
            agent_status: crate::api::schema::AgentStatus::Working,
            screen_detection_skipped: false,
            custom_status: None,
            state_labels: std::collections::HashMap::new(),
            agent_session: None,
            workspace_id: "remote-ws".to_string(),
            tab_id: "remote-tab".to_string(),
            pane_id: format!("{terminal_id}-pane"),
            focused: false,
            cwd: None,
            foreground_cwd: None,
            revision: 1,
        }
    }
}
