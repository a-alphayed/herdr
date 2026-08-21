//! Deferred (off-loop) dispatch for host-qualified remote AGENT CONTROL
//! requests.
//!
//! Slow or sleeping remote hosts must not stall the local TUI or headless
//! server request loop while their SSH bridge runs. This seam moves remote
//! agent control bridge dispatch off both local API loops *after* the existing
//! in-memory route/cache/policy gates pass:
//!
//! 1. A **pure planner** ([`App::plan_deferred_remote_agent_request`]) inspects
//!    the request against `remote_hosts` / `remote_sources` only. It performs
//!    route planning, cached target resolution, the `agent.teardown` confirm
//!    gate, the `agent.start --host` connection-policy guard and cached
//!    non-connected precheck, and the remote-mutating connected checks. It
//!    never spawns a thread and never touches SSH. It returns one of:
//!    [`DeferredRemoteAgentPlan::NotHandled`] (run the normal synchronous local
//!    path), [`DeferredRemoteAgentPlan::Immediate`] (a pre-dispatch guard
//!    failed; send this response on the current loop), or
//!    [`DeferredRemoteAgentPlan::Deferred`] (guards passed; dispatch
//!    off-loop).
//! 2. A **dispatch descriptor** ([`RemoteAgentDispatchDescriptor`]) is fully
//!    owned: `RemoteHostConfig`, the rewritten `Request`, and a response
//!    rewrite mode. The worker needs no further access to App state.
//! 3. An **injectable dispatch starter** runs the bridge on a background thread
//!    by default and sends a response through the one-shot `respond_to`
//!    channel, always attempting a response so no client hangs on a
//!    worker/bridge/rewrite error. Tests inject a fake starter to prove the
//!    loop continues while the worker is blocked.
//!
//! Scope: only `agent.read`, `agent.focus`, `agent.send`, `agent.submit`,
//! `agent.teardown` for host-qualified remote targets, and `agent.start` when
//! `params.host` is set. Local/bare/non-remote targets return
//! [`DeferredRemoteAgentPlan::NotHandled`] and use the existing synchronous
//! local path unchanged. `agent.get`/`agent.list` remain cache-only and never
//! use a worker. Remote host authority is preserved: the local node only
//! routes/proxies the rewritten request.

use crate::api::schema::{AgentStartParams, Method, Request};
use crate::app::App;
use crate::remote_source::RemoteHostKey;
use crate::remote_target::{PlannedTargetRoute, RemoteHostConfig, RemoteRoutePlanError};

use super::agents::{
    remote_agent_focus_request, remote_agent_read_request, remote_agent_resolve_error_body,
    remote_agent_send_request, remote_agent_start_host_policy_guard,
    remote_agent_start_host_precheck, remote_agent_start_request, remote_agent_submit_request,
    remote_agent_teardown_request, rewrite_remote_agent_start_response,
};
use super::remote_helpers::{remote_route_plan_error_body, rewrite_remote_response_id};
use super::responses::{encode_error, encode_error_body};

/// Outcome of planning a host-qualified remote-agent API request off the
/// App/headless loop. The planner is pure: it never spawns a thread and never
/// touches SSH.
#[derive(Debug)]
pub(crate) enum DeferredRemoteAgentPlan {
    /// The request is not a host-qualified remote-agent method handled by this
    /// seam. The caller must run it through the normal synchronous local path.
    NotHandled,
    /// A pre-dispatch guard fired (confirm gate, route/resolve error, manual
    /// connection policy, or cached non-connected status). The caller sends
    /// this response immediately on the current loop without spawning a worker.
    Immediate(String),
    /// Route/cache/policy guards passed. Dispatch this descriptor off-loop.
    ///
    /// Boxed: the descriptor (~584 bytes) would otherwise dominate the enum
    /// size and bloat every `NotHandled`/`Immediate` value on the loop.
    Deferred(Box<RemoteAgentDispatchDescriptor>),
}

/// Result of [`App::handle_deferred_remote_agent_api_request`]. On
/// [`DeferredRemoteAgentOutcome::NotHandled`] ownership of the request and
/// response channel returns to the caller so it can run the synchronous path.
#[derive(Debug)]
pub(crate) enum DeferredRemoteAgentOutcome {
    NotHandled {
        // Boxed: a `Request` is ~512 bytes; boxing keeps this outcome from
        // dominating the enum while preserving owned move semantics back to
        // the synchronous path.
        request: Box<Request>,
        respond_to: std::sync::mpsc::Sender<String>,
    },
    /// A deferred worker was started, or an immediate guard response was sent.
    /// The caller must NOT run the synchronous path.
    Handled,
}

/// How the deferred worker rewrites the remote response before sending it to
/// the local client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteAgentResponseRewrite {
    /// Replace only the response `id` with the local request id. Used for
    /// `agent.focus`/`read`/`send`/`submit`/`teardown`.
    Id,
    /// Replace the response `id` and host-qualify the started-agent label
    /// fields. Used for `agent.start --host`.
    AgentStart,
}

/// Owned, self-contained description of a deferred remote-agent bridge
/// dispatch. The worker runs the bridge and sends a response through the
/// one-shot `respond_to` channel without any further access to App state, so a
/// slow/sleeping host cannot stall the local loop.
#[derive(Debug)]
pub(crate) struct RemoteAgentDispatchDescriptor {
    pub(crate) host: RemoteHostConfig,
    pub(crate) request: Request,
    pub(crate) rewrite: RemoteAgentResponseRewrite,
    /// Optional supervisor-prepared bridge state reused to skip per-request
    /// remote binary preparation and capability/ping probes. Populated only
    /// when the host is `Connected` with cached prepared state; `None` means
    /// the worker uses the existing full non-interactive bridge path. This is
    /// data reuse, not connection pooling: a fresh SSH process still runs per
    /// request, and the actual remote API request still fails authoritatively
    /// on drift.
    pub(crate) bridge_state: Option<crate::remote::RemoteApiBridgeState>,
}

/// Signature of the bridge call the worker makes to the remote host. Defaults
/// to [`real_remote_agent_bridge`]; tests inject a fake to exercise the worker
/// error/response path deterministically without real SSH. The optional cached
/// prepared bridge state is forwarded so the real bridge can reuse it.
pub(crate) type RemoteAgentBridge = fn(
    &RemoteHostConfig,
    &Request,
    Option<&crate::remote::RemoteApiBridgeState>,
) -> std::io::Result<String>;

/// Signature of the dispatch starter. The default implementation
/// ([`spawn_remote_agent_dispatch`]) spawns a background thread that runs the
/// bridge. Tests inject a fake starter to prove the loop continues while the
/// worker is blocked/not completed.
pub(crate) type RemoteAgentDispatchStarter =
    fn(RemoteAgentDispatchDescriptor, std::sync::mpsc::Sender<String>);

/// Per-(host alias, session) in-flight cap for deferred remote-agent bridge
/// dispatch. Bounds how many concurrent SSH request bridge workers one
/// configured host/session may run when many host-qualified remote-agent
/// requests clear the existing route/cache/policy gates. Keyed by configured
/// remote alias + session, not by method, agent target, display label, or
/// global process count.
pub(crate) const REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT: usize = 4;

/// Small independently testable limiter with RAII permits. Production reads a
/// process-global instance via [`remote_agent_bridge_limiter`]; unit tests use
/// local instances so they never mutate or saturate the production global.
pub(crate) struct RemoteAgentBridgeLimiter {
    limit: usize,
    in_flight: std::sync::Mutex<std::collections::BTreeMap<RemoteHostKey, usize>>,
}

impl RemoteAgentBridgeLimiter {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            in_flight: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        }
    }

    /// Configured per-(host, session) cap.
    pub(crate) fn limit(&self) -> usize {
        self.limit
    }

    /// Active in-flight dispatch count for one (host, session). Inspection /
    /// test only.
    #[cfg(test)]
    pub(crate) fn in_flight(&self, key: &RemoteHostKey) -> usize {
        self.lock().get(key).copied().unwrap_or(0)
    }

    /// Try to acquire an in-flight permit for `(host, session)`. Returns
    /// `None` if the cap is saturated; the caller must respond with
    /// `remote_bridge_busy` and must not spawn a worker.
    pub(crate) fn try_acquire(&self, key: &RemoteHostKey) -> Option<RemoteAgentBridgePermit<'_>> {
        let mut map = self.lock();
        let count = map.get(key).copied().unwrap_or(0);
        if count >= self.limit {
            None
        } else {
            map.insert(key.clone(), count + 1);
            Some(RemoteAgentBridgePermit {
                limiter: self,
                key: key.clone(),
            })
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, std::collections::BTreeMap<RemoteHostKey, usize>> {
        self.in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn release(&self, key: &RemoteHostKey) {
        let mut map = self.lock();
        if let Some(count) = map.get_mut(key) {
            if *count > 0 {
                *count -= 1;
            }
            if *count == 0 {
                map.remove(key);
            }
        }
    }
}

/// RAII permit for one in-flight remote-agent bridge dispatch. Releases the
/// per-(host, session) slot on drop, including through panic unwind, so a
/// worker that succeeds, errors, or panics always frees its slot.
pub(crate) struct RemoteAgentBridgePermit<'a> {
    limiter: &'a RemoteAgentBridgeLimiter,
    key: RemoteHostKey,
}

impl Drop for RemoteAgentBridgePermit<'_> {
    fn drop(&mut self) {
        self.limiter.release(&self.key);
    }
}

/// Intermediate route/resolve result used by the planner before building a
/// dispatch descriptor.
enum RemoteAgentTargetRoute {
    /// Target is local (or no remote hosts configured). Caller runs the
    /// synchronous local path.
    Local,
    /// A route/resolve/connected guard failed; send this response immediately.
    Immediate(String),
    /// Cleared for off-loop dispatch.
    Ready {
        host: RemoteHostConfig,
        terminal_id: String,
        /// Cached prepared bridge state, present only when the host is
        /// `Connected` with cached state; the descriptor carries it through to
        /// the worker so it can skip per-request preparation/probes.
        bridge_state: Option<crate::remote::RemoteApiBridgeState>,
    },
}

impl App {
    /// Try to plan a host-qualified remote-agent request off the loop. Pure:
    /// no thread spawn, no SSH. Returns [`DeferredRemoteAgentPlan::NotHandled`]
    /// for anything this seam does not own (local/bare targets, cache-only
    /// reads, non-agent methods).
    pub(crate) fn plan_deferred_remote_agent_request(
        &self,
        request: &Request,
    ) -> DeferredRemoteAgentPlan {
        let request_id = request.id.clone();

        match &request.method {
            // `agent.teardown` confirm gate fires first for BOTH local and
            // remote: a missing `confirm: true` must return
            // `confirmation_required` immediately, before route planning,
            // target resolution, connected checks, or worker spawn.
            //
            // Phase G.8 telemetry intentionally does NOT instrument this arm:
            // it fires before a configured host/session is known (it wins for
            // local targets too), so there is no safe remote host identity to
            // log yet. Do not add a routed-action log call here.
            Method::AgentTeardown(params) if !params.confirm => {
                DeferredRemoteAgentPlan::Immediate(encode_error(
                    request_id,
                    "confirmation_required",
                    "agent.teardown is destructive; pass confirm: true to proceed".to_string(),
                ))
            }

            Method::AgentFocus(target) => self.plan_remote_agent_method(
                &request_id,
                "agent.focus",
                &target.target,
                /* require_connected */ true,
                |host, terminal_id, bridge_state| RemoteAgentDispatchDescriptor {
                    host,
                    request: remote_agent_focus_request(
                        request_id.clone(),
                        target.clone(),
                        &terminal_id,
                    ),
                    rewrite: RemoteAgentResponseRewrite::Id,
                    bridge_state,
                },
            ),

            // `agent.read` preserves existing behavior: resolve the cached
            // remote target, rewrite to the authoritative terminal id, and
            // dispatch even for stale/non-connected cached entries because
            // read is safe/idempotent. Only route/resolve errors are
            // immediate; the slow reachability work still runs off-loop.
            Method::AgentRead(params) => self.plan_remote_agent_method(
                &request_id,
                "agent.read",
                &params.target,
                /* require_connected */ false,
                |host, terminal_id, bridge_state| RemoteAgentDispatchDescriptor {
                    host,
                    request: remote_agent_read_request(
                        request_id.clone(),
                        params.clone(),
                        &terminal_id,
                    ),
                    rewrite: RemoteAgentResponseRewrite::Id,
                    bridge_state,
                },
            ),

            Method::AgentSend(params) => self.plan_remote_agent_method(
                &request_id,
                "agent.send",
                &params.target,
                /* require_connected */ true,
                |host, terminal_id, bridge_state| RemoteAgentDispatchDescriptor {
                    host,
                    request: remote_agent_send_request(
                        request_id.clone(),
                        params.clone(),
                        &terminal_id,
                    ),
                    rewrite: RemoteAgentResponseRewrite::Id,
                    bridge_state,
                },
            ),

            Method::AgentSubmit(params) => self.plan_remote_agent_method(
                &request_id,
                "agent.submit",
                &params.target,
                /* require_connected */ true,
                |host, terminal_id, bridge_state| RemoteAgentDispatchDescriptor {
                    host,
                    request: remote_agent_submit_request(
                        request_id.clone(),
                        params.clone(),
                        &terminal_id,
                    ),
                    rewrite: RemoteAgentResponseRewrite::Id,
                    bridge_state,
                },
            ),

            // Confirmation was already enforced above; when `confirm: true`
            // the connected check still applies before forwarding. The
            // authoritative terminal id is resolved and substituted by the
            // builder, which always forwards `confirm: true`.
            Method::AgentTeardown(params) => self.plan_remote_agent_method(
                &request_id,
                "agent.teardown",
                &params.target,
                /* require_connected */ true,
                |host, terminal_id, bridge_state| RemoteAgentDispatchDescriptor {
                    host,
                    request: remote_agent_teardown_request(request_id.clone(), &terminal_id),
                    rewrite: RemoteAgentResponseRewrite::Id,
                    bridge_state,
                },
            ),

            Method::AgentStart(params) if params.host.is_some() => {
                self.plan_remote_agent_start(&request_id, params.clone())
            }

            // `agent.start` without `host`, `agent.get`/`list`/`list_local`,
            // `agent.rename`/`explain`, and every non-agent method stay on the
            // synchronous local path.
            _ => DeferredRemoteAgentPlan::NotHandled,
        }
    }

    /// Top-level deferred entry point for the TUI runtime and headless server
    /// loops. Drains-independent: callers must have drained internal events
    /// once so `remote_sources` is as fresh as the current sync path before
    /// calling. Returns [`DeferredRemoteAgentOutcome::NotHandled`] with the
    /// request and response channel when this seam does not own the request,
    /// so the caller can run the synchronous local path unchanged.
    pub(crate) fn handle_deferred_remote_agent_api_request(
        &mut self,
        request: Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> DeferredRemoteAgentOutcome {
        match self.plan_deferred_remote_agent_request(&request) {
            DeferredRemoteAgentPlan::NotHandled => DeferredRemoteAgentOutcome::NotHandled {
                request: Box::new(request),
                respond_to,
            },
            DeferredRemoteAgentPlan::Immediate(response) => {
                let _ = respond_to.send(response);
                DeferredRemoteAgentOutcome::Handled
            }
            DeferredRemoteAgentPlan::Deferred(descriptor) => {
                // `descriptor` is `Box<RemoteAgentDispatchDescriptor>`; unbox
                // to hand the starter its owned value.
                (self.remote_agent_dispatch_starter)(*descriptor, respond_to);
                DeferredRemoteAgentOutcome::Handled
            }
        }
    }

    /// Route, resolve, and connected-check a host-qualified remote agent
    /// target, then build a dispatch descriptor via `build_descriptor`.
    /// `require_connected` controls whether the remote-mutating connected
    /// precheck runs (`agent.read` is safe/idempotent and may dispatch even for
    /// stale/non-connected cached entries). `method` is the canonical API
    /// method name used for Phase G.8 routed-action telemetry only; it never
    /// affects the plan result.
    fn plan_remote_agent_method(
        &self,
        request_id: &str,
        method: &'static str,
        target: &str,
        require_connected: bool,
        build_descriptor: impl FnOnce(
            RemoteHostConfig,
            String,
            Option<crate::remote::RemoteApiBridgeState>,
        ) -> RemoteAgentDispatchDescriptor,
    ) -> DeferredRemoteAgentPlan {
        match self.plan_remote_agent_target_route(request_id, method, target, require_connected) {
            RemoteAgentTargetRoute::Local => DeferredRemoteAgentPlan::NotHandled,
            RemoteAgentTargetRoute::Immediate(response) => {
                DeferredRemoteAgentPlan::Immediate(response)
            }
            RemoteAgentTargetRoute::Ready {
                host,
                terminal_id,
                bridge_state,
            } => DeferredRemoteAgentPlan::Deferred(Box::new(build_descriptor(
                host,
                terminal_id,
                bridge_state,
            ))),
        }
    }

    /// `agent.start --host` planner. Applies the Manual connection-policy guard
    /// before any SSH/API dispatch, then the cached non-connected precheck.
    /// `manual` fails locally with the existing distinct policy error;
    /// `on_demand` with no cached non-connected entry can dispatch; a cached
    /// disconnected/unreachable/needs-update status fails fast.
    fn plan_remote_agent_start(
        &self,
        request_id: &str,
        params: AgentStartParams,
    ) -> DeferredRemoteAgentPlan {
        let host_alias = params
            .host
            .as_deref()
            .expect("caller guards params.host.is_some()");
        let Some(host) = self.remote_hosts.get(host_alias).cloned() else {
            // Unknown host: alias parsed from params but not configured. No
            // safe host/session identity yet, so intentionally not logged.
            return DeferredRemoteAgentPlan::Immediate(encode_error_body(
                request_id.to_string(),
                remote_route_plan_error_body(RemoteRoutePlanError::UnknownHost(
                    host_alias.to_string(),
                )),
            ));
        };
        if let Err(err) = remote_agent_start_host_policy_guard(&host) {
            crate::logging::remote_route_planned(
                request_id,
                "agent.start",
                &host.name,
                &host.session,
                "fail_fast",
                Some(&err.code),
            );
            return DeferredRemoteAgentPlan::Immediate(encode_error_body(
                request_id.to_string(),
                err,
            ));
        }
        if let Err(err) = remote_agent_start_host_precheck(&self.state.remote_sources, &host) {
            crate::logging::remote_route_planned(
                request_id,
                "agent.start",
                &host.name,
                &host.session,
                "fail_fast",
                Some(&err.code),
            );
            return DeferredRemoteAgentPlan::Immediate(encode_error_body(
                request_id.to_string(),
                err,
            ));
        }
        crate::logging::remote_route_planned(
            request_id,
            "agent.start",
            &host.name,
            &host.session,
            "deferred",
            None,
        );
        let request = remote_agent_start_request(request_id.to_string(), params);
        // Prepared state is included only when the host is `Connected` with
        // cached state. For on-demand/no-cache `agent.start --host` this returns
        // `None`, so the worker falls back to the full non-interactive bridge path.
        let bridge_state = self
            .state
            .remote_sources
            .connected_bridge_state(&RemoteHostKey::new(host.name.clone(), host.session.clone()));
        DeferredRemoteAgentPlan::Deferred(Box::new(RemoteAgentDispatchDescriptor {
            host,
            request,
            rewrite: RemoteAgentResponseRewrite::AgentStart,
            bridge_state,
        }))
    }

    /// Shared route/resolve/(optional) connected-check helper. Returns the
    /// authoritative remote host and resolved terminal id when cleared to
    /// dispatch, an immediate error response for any guard failure, or `Local`
    /// when the target is not host-qualified remote (caller runs the sync
    /// path).
    ///
    /// `method` is for Phase G.8 telemetry only and never affects the result.
    /// Telemetry is emitted only once a configured host/session is known:
    /// resolve errors and cached non-connected statuses (guard failures past
    /// route planning) and the deferred plan result are logged; target parse /
    /// unknown-host errors that occur before a configured host/session is
    /// known are intentionally left unlogged in this slice.
    fn plan_remote_agent_target_route(
        &self,
        request_id: &str,
        method: &'static str,
        target: &str,
        require_connected: bool,
    ) -> RemoteAgentTargetRoute {
        let route = match self.plan_agent_api_target(target) {
            Ok(route) => route,
            Err(err) => {
                // Target parse / unknown-host error before a configured
                // host/session is known: intentionally not logged in this slice.
                return RemoteAgentTargetRoute::Immediate(encode_error_body(
                    request_id.to_string(),
                    remote_route_plan_error_body(err),
                ));
            }
        };
        let PlannedTargetRoute::Remote {
            host,
            target: selector,
        } = route
        else {
            return RemoteAgentTargetRoute::Local;
        };

        let resolved = match crate::remote_target::resolve_remote_agent_target(
            &self.state.remote_sources,
            &host,
            &selector,
        ) {
            Ok(resolved) => resolved,
            Err(err) => {
                let body = remote_agent_resolve_error_body(err);
                crate::logging::remote_route_planned(
                    request_id,
                    method,
                    &host.name,
                    &host.session,
                    "fail_fast",
                    Some(&body.code),
                );
                return RemoteAgentTargetRoute::Immediate(encode_error_body(
                    request_id.to_string(),
                    body,
                ));
            }
        };
        if require_connected {
            if let Err(err) = Self::remote_agent_resolved_connected_or_error(&host, &resolved) {
                crate::logging::remote_route_planned(
                    request_id,
                    method,
                    &host.name,
                    &host.session,
                    "fail_fast",
                    Some(&err.code),
                );
                return RemoteAgentTargetRoute::Immediate(encode_error_body(
                    request_id.to_string(),
                    err,
                ));
            }
        }
        crate::logging::remote_route_planned(
            request_id,
            method,
            &host.name,
            &host.session,
            "deferred",
            None,
        );
        // Prepared state is included only when the host is `Connected` with
        // cached state. For stale/non-connected `agent.read` (and any
        // non-connected resolved target) this returns `None`, so the worker
        // falls back to the full non-interactive bridge path.
        let bridge_state = self
            .state
            .remote_sources
            .connected_bridge_state(&RemoteHostKey::new(host.name.clone(), host.session.clone()));
        RemoteAgentTargetRoute::Ready {
            host,
            terminal_id: resolved.entry.agent.terminal_id.clone(),
            bridge_state,
        }
    }
}

/// Production bridge: runs the real SSH-bridged JSON API request to the remote
/// host non-interactively. When cached supervisor-prepared bridge state is
/// available it reuses that prepared data to skip per-request remote binary
/// preparation and capability/ping probes; otherwise it uses the existing full
/// non-interactive bridge path. A fresh SSH process still runs per request in
/// the one-shot paths; this reuses prepared data, not a persistent connection.
///
/// When the prepared state also advertises the Phase G.10 persistent-bridge
/// capability, the request is first attempted through the bounded idle bridge
/// pool ([`crate::remote::try_pooled_remote_api_request`]). That call returns
/// `Ok(None)` when pool checkout/start fails *before any byte is written* — in
/// that case this falls back to the one-shot prepared path (still pre-write,
/// safe). Once the pool has begun a write, any failure is already mapped to an
/// `Err` by the pool and is returned here as `remote_request_failed` by the
/// worker, with no retry and no fallback (uniform for every routed method).
pub(crate) fn real_remote_agent_bridge(
    host: &RemoteHostConfig,
    request: &Request,
    bridge_state: Option<&crate::remote::RemoteApiBridgeState>,
) -> std::io::Result<String> {
    if let Some(state) = bridge_state {
        if state.capabilities.supports_method(
            crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE_PERSISTENT,
        ) {
            if let Some(response) =
                crate::remote::try_pooled_remote_api_request(host, state, request)?
            {
                return Ok(response);
            }
            // Pool checkout/start failed before write: fall through to the
            // one-shot prepared path below (still pre-write, safe).
        }
    }
    match bridge_state {
        Some(state) => {
            crate::remote::send_remote_api_request_with_prepared_state(host, state, request)
        }
        None => crate::remote::send_remote_api_request_to_host_noninteractive(host, request),
    }
}

/// Canonical dot method name for a routed remote-agent method, used for Phase
/// G.8 telemetry only. Limited to the routed agent methods handled by this
/// seam; never records params or values. Returns a stable fallback for any
/// other method so the helper stays total.
fn remote_agent_method_name(method: &Method) -> &'static str {
    match method {
        Method::AgentRead(_) => "agent.read",
        Method::AgentFocus(_) => "agent.focus",
        Method::AgentSend(_) => "agent.send",
        Method::AgentSubmit(_) => "agent.submit",
        Method::AgentTeardown(_) => "agent.teardown",
        Method::AgentStart(_) => "agent.start",
        _ => "agent",
    }
}

/// Extract the controlled `error.code` scalar from a rewritten remote API
/// response when it is an authoritative remote-side error. Returns `None` for
/// success responses and malformed JSON. Telemetry-only: the returned code is
/// a controlled scalar label, never the raw `message`, body, or payload.
fn remote_response_error_code(response: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(response).ok()?;
    value
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(|code| code.as_str())
        .map(str::to_string)
}

/// Process-global limiter bound to [`REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT`].
/// `RemoteAgentDispatchStarter` is a `fn` pointer with no extra state, so the
/// default starter reads this global. Tests exercise the limiter through local
/// instances ([`acquire_remote_agent_bridge_permit_or_busy`]) instead of
/// mutating or saturating this global.
static REMOTE_AGENT_BRIDGE_LIMITER: std::sync::OnceLock<RemoteAgentBridgeLimiter> =
    std::sync::OnceLock::new();

pub(crate) fn remote_agent_bridge_limiter() -> &'static RemoteAgentBridgeLimiter {
    REMOTE_AGENT_BRIDGE_LIMITER
        .get_or_init(|| RemoteAgentBridgeLimiter::new(REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT))
}

/// Default dispatch starter: delegate to the testable
/// [`spawn_remote_agent_dispatch_with_limiter`] core with the process-global
/// limiter and the real SSH bridge. Acquire a per-(host, session) in-flight
/// permit, then spawn a background thread that runs the bridge and sends a
/// response through the one-shot channel. If the cap is saturated, an
/// immediate `remote_bridge_busy` response is sent without spawning a worker.
/// Always attempts to send a response so a client never hangs on a
/// worker/bridge/rewrite error or a saturated cap.
pub(crate) fn spawn_remote_agent_dispatch(
    descriptor: RemoteAgentDispatchDescriptor,
    respond_to: std::sync::mpsc::Sender<String>,
) {
    spawn_remote_agent_dispatch_with_limiter(
        descriptor,
        respond_to,
        remote_agent_bridge_limiter(),
        real_remote_agent_bridge,
    );
}

/// Testable core of the default dispatch starter: acquire a per-(host,
/// session) in-flight permit from the supplied limiter, then spawn a
/// background thread that runs the injectable bridge and sends a response
/// through the one-shot channel. If the cap is saturated, an immediate
/// `remote_bridge_busy` response is sent without spawning a worker. Always
/// attempts to send a response so a client never hangs on a
/// worker/bridge/rewrite error or a saturated cap.
///
/// `limiter` is `&'static` so the RAII permit (which borrows it) is `Send` and
/// may move into the worker thread. Production passes the process-global
/// limiter ([`remote_agent_bridge_limiter`]); tests pass a leaked local
/// limiter (`Box::leak`) so they never touch or saturate the production
/// global. The worker runs `bridge` (the real bridge in production, an
/// injectable fake in tests) instead of the real-bridge wrapper, preserving
/// the existing acquire/spawn/release behavior.
fn spawn_remote_agent_dispatch_with_limiter(
    descriptor: RemoteAgentDispatchDescriptor,
    respond_to: std::sync::mpsc::Sender<String>,
    limiter: &'static RemoteAgentBridgeLimiter,
    bridge: RemoteAgentBridge,
) {
    let Some(permit) =
        acquire_remote_agent_bridge_permit_or_busy(limiter, &descriptor, &respond_to)
    else {
        return;
    };
    // Permit acquired: log dispatch start before spawning the worker.
    crate::logging::remote_route_dispatch_started(
        &descriptor.request.id,
        remote_agent_method_name(&descriptor.request.method),
        &descriptor.host.name,
        &descriptor.host.session,
    );
    std::thread::spawn(move || {
        // RAII guard: releases the per-(host, session) slot on normal return
        // (bridge success, bridge error mapped to `remote_request_failed`,
        // rewrite error) and on panic unwind. `permit` borrows the limiter for
        // `'static`, so it is `Send` and may move into the worker thread.
        let _permit = permit;
        run_remote_agent_dispatch_with_bridge(descriptor, respond_to, bridge);
    });
}

/// Acquire a per-(host, session) in-flight permit for `descriptor`'s configured
/// host/session. On saturation send an immediate `remote_bridge_busy` response
/// on `respond_to` and return `None` (the caller must not spawn a worker). On
/// success return the RAII permit; the caller moves it into the worker closure
/// so the slot releases on bridge success, bridge error, rewrite error, and
/// panic unwind. Takes the limiter by shared reference so unit tests can use
/// local limiter instances without touching the production global; the returned
/// permit borrows only the limiter, leaving `descriptor`/`respond_to` free to
/// move into the worker closure.
pub(crate) fn acquire_remote_agent_bridge_permit_or_busy<'a>(
    limiter: &'a RemoteAgentBridgeLimiter,
    descriptor: &RemoteAgentDispatchDescriptor,
    respond_to: &std::sync::mpsc::Sender<String>,
) -> Option<RemoteAgentBridgePermit<'a>> {
    let key = RemoteHostKey::new(&descriptor.host.name, &descriptor.host.session);
    match limiter.try_acquire(&key) {
        Some(permit) => Some(permit),
        None => {
            // Phase G.7 limiter saturation: one immediate `remote_bridge_busy`
            // outcome, no worker spawned, nothing queued.
            crate::logging::remote_route_busy(
                &descriptor.request.id,
                remote_agent_method_name(&descriptor.request.method),
                &descriptor.host.name,
                &descriptor.host.session,
                limiter.limit(),
            );
            let _ = respond_to.send(remote_bridge_busy_response(
                &descriptor.request.id,
                &key,
                limiter.limit(),
            ));
            None
        }
    }
}

/// Build the `remote_bridge_busy` error response sent when the per-(host,
/// session) in-flight cap is saturated. Preserves the local request id and
/// names the host/session and active limit.
pub(crate) fn remote_bridge_busy_response(id: &str, key: &RemoteHostKey, limit: usize) -> String {
    encode_error(
        id.to_string(),
        "remote_bridge_busy",
        format!(
            "remote agent bridge dispatch limit reached for {host}/{session}: \
             {limit} in-flight dispatches already in progress",
            host = key.host,
            session = key.session,
        ),
    )
}

/// Testable core: run one dispatch with an injectable bridge. A bridge or
/// rewrite error still sends a `remote_request_failed` response through the
/// channel so the client never hangs. Production dispatches through
/// [`spawn_remote_agent_dispatch_with_limiter`] with [`real_remote_agent_bridge`];
/// tests inject a fake bridge.
pub(crate) fn run_remote_agent_dispatch_with_bridge(
    descriptor: RemoteAgentDispatchDescriptor,
    respond_to: std::sync::mpsc::Sender<String>,
    bridge: RemoteAgentBridge,
) {
    let local_id = descriptor.request.id.clone();
    let method = remote_agent_method_name(&descriptor.request.method);
    let host_name = descriptor.host.name.clone();
    let rewrite = descriptor.rewrite;
    let bridge_state = descriptor.bridge_state.as_ref();
    let bridged =
        bridge(&descriptor.host, &descriptor.request, bridge_state).and_then(|response| {
            match rewrite {
                RemoteAgentResponseRewrite::Id => rewrite_remote_response_id(&response, &local_id),
                RemoteAgentResponseRewrite::AgentStart => {
                    rewrite_remote_agent_start_response(&response, &local_id, &host_name)
                }
            }
        });
    let response = match bridged {
        Ok(response) => response,
        Err(err) => encode_error(local_id, "remote_request_failed", err.to_string()),
    };
    // Observability only: completion outcome derived from the controlled
    // `error.code` scalar on the final response. A bridge/rewrite failure was
    // mapped above to the fixed `remote_request_failed` code; an authoritative
    // remote API error surfaces its own controlled `error.code`. Never logs
    // raw messages, payloads, response bodies, or request bodies.
    let remote_error = remote_response_error_code(&response);
    crate::logging::remote_route_completed(
        &descriptor.request.id,
        method,
        &descriptor.host.name,
        &descriptor.host.session,
        remote_error.as_deref(),
    );
    let _ = respond_to.send(response);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        AgentReadParams, AgentSendParams, AgentStartParams, AgentSubmitParams, AgentTarget,
        AgentTeardownParams, EmptyParams, ErrorResponse, ReadFormat, ReadSource, SuccessResponse,
    };
    use crate::remote_source::{RemoteConnectionStatus, RemoteHostKey};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn test_app(config: &crate::config::Config) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(config, true, None, api_rx, crate::api::EventHub::default())
    }

    fn remote_enabled_app() -> App {
        let mut config = crate::config::Config::default();
        config.remote.enabled = true;
        config.remote.hosts = vec![crate::remote_target::RemoteHostConfig::new(
            "jafar", "jafar", "default", true,
        )];
        test_app(&config)
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
            state_labels: HashMap::new(),
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

    fn seed_connected_agent(app: &mut App, terminal_id: &str, name: &str) {
        app.state.remote_sources.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![standalone_remote_agent(terminal_id, name)],
        );
    }

    fn seed_stale_agent(app: &mut App, status: RemoteConnectionStatus) {
        let host = RemoteHostKey::new("jafar", "default");
        app.state.remote_sources.replace_connected_snapshot(
            host.clone(),
            vec![standalone_remote_agent("term-1", "codex")],
        );
        app.state.remote_sources.mark_status(&host, status);
    }

    fn read_params(target: &str) -> AgentReadParams {
        AgentReadParams {
            target: target.to_string(),
            source: ReadSource::Recent,
            lines: None,
            format: ReadFormat::Text,
            strip_ansi: true,
        }
    }

    fn base_start_params(host: Option<&str>) -> AgentStartParams {
        AgentStartParams {
            host: host.map(str::to_string),
            name: "codex".to_string(),
            cwd: None,
            workspace_id: None,
            tab_id: None,
            split: None,
            focus: false,
            new_workspace: false,
            argv: vec!["codex".to_string()],
            env: Default::default(),
        }
    }

    #[test]
    fn remote_agent_read_plans_dispatch_to_authoritative_terminal_id_without_ssh() {
        let mut app = remote_enabled_app();
        seed_connected_agent(&mut app, "term-1", "codex");

        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentRead(read_params("jafar/codex")),
        });

        let descriptor = match plan {
            DeferredRemoteAgentPlan::Deferred(descriptor) => descriptor,
            other => panic!("expected Deferred, got {other:?}"),
        };
        let Method::AgentRead(params) = &descriptor.request.method else {
            panic!("expected agent.read");
        };
        // Target rewritten to the authoritative remote terminal id; planning
        // touched no SSH (the real bridge is never installed or invoked here).
        assert_eq!(params.target, "term-1");
        assert_eq!(descriptor.request.id, "req");
        assert_eq!(descriptor.rewrite, RemoteAgentResponseRewrite::Id);
    }

    #[test]
    fn remote_agent_read_does_not_require_connected_status() {
        // Read is safe/idempotent: a stale/non-connected cached agent still
        // plans a dispatch (only the slow reachability work moves off-loop).
        let mut app = remote_enabled_app();
        seed_stale_agent(&mut app, RemoteConnectionStatus::Disconnected);

        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentRead(read_params("jafar/codex")),
        });
        assert!(matches!(plan, DeferredRemoteAgentPlan::Deferred(_)));
    }

    #[test]
    fn remote_agent_send_fails_fast_for_cached_disconnected_without_spawn() {
        let mut app = remote_enabled_app();
        seed_stale_agent(&mut app, RemoteConnectionStatus::Unreachable);

        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentSend(AgentSendParams {
                target: "jafar/codex".to_string(),
                text: "hi".to_string(),
            }),
        });
        let response = match plan {
            DeferredRemoteAgentPlan::Immediate(response) => response,
            other => panic!("expected Immediate, got {other:?}"),
        };
        let parsed: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req");
        assert_eq!(parsed.error.code, "remote_host_not_connected");
        assert!(parsed.error.message.contains("unreachable"));
    }

    #[test]
    fn remote_agent_submit_fails_fast_for_cached_needs_update_without_spawn() {
        let mut app = remote_enabled_app();
        seed_stale_agent(&mut app, RemoteConnectionStatus::NeedsUpdate);

        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentSubmit(AgentSubmitParams {
                target: "jafar/codex".to_string(),
                text: "continue".to_string(),
            }),
        });
        let response = match plan {
            DeferredRemoteAgentPlan::Immediate(response) => response,
            other => panic!("expected Immediate, got {other:?}"),
        };
        let parsed: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.error.code, "remote_host_not_connected");
    }

    #[test]
    fn remote_agent_teardown_unconfirmed_returns_confirmation_required_before_routing() {
        let mut app = remote_enabled_app();
        // Seed a disconnected agent so that, if the confirm gate did not fire
        // first, the connected precheck would also fail. The confirm gate must
        // win and fire before route planning/connected checks/spawn.
        seed_stale_agent(&mut app, RemoteConnectionStatus::Disconnected);

        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentTeardown(AgentTeardownParams {
                target: "jafar/codex".to_string(),
                confirm: false,
            }),
        });
        let response = match plan {
            DeferredRemoteAgentPlan::Immediate(response) => response,
            other => panic!("expected Immediate, got {other:?}"),
        };
        let parsed: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.error.code, "confirmation_required");
    }

    #[test]
    fn remote_agent_focus_with_connected_agent_plans_dispatch() {
        let mut app = remote_enabled_app();
        seed_connected_agent(&mut app, "term-1", "codex");

        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentFocus(AgentTarget {
                target: "jafar/codex".to_string(),
            }),
        });
        let descriptor = match plan {
            DeferredRemoteAgentPlan::Deferred(descriptor) => descriptor,
            other => panic!("expected Deferred, got {other:?}"),
        };
        let Method::AgentFocus(target) = &descriptor.request.method else {
            panic!("expected agent.focus");
        };
        assert_eq!(target.target, "term-1");
    }

    #[test]
    fn agent_start_host_manual_policy_returns_distinct_error_before_dispatch() {
        let mut config = crate::config::Config::default();
        config.remote.enabled = true;
        config.remote.hosts =
            vec![
                crate::remote_target::RemoteHostConfig::new("jafar", "jafar", "default", true)
                    .with_connection_policy(crate::remote_target::RemoteConnectionPolicy::Manual),
            ];
        let app = test_app(&config);

        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentStart(base_start_params(Some("jafar"))),
        });
        let response = match plan {
            DeferredRemoteAgentPlan::Immediate(response) => response,
            other => panic!("expected Immediate, got {other:?}"),
        };
        let parsed: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.error.code, "remote_host_connection_policy_manual");
    }

    #[test]
    fn agent_start_host_on_demand_without_cache_plans_dispatch() {
        let mut config = crate::config::Config::default();
        config.remote.enabled = true;
        config.remote.hosts = vec![crate::remote_target::RemoteHostConfig::new(
            "jafar", "jafar", "default", false,
        )];
        let app = test_app(&config);

        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentStart(base_start_params(Some("jafar"))),
        });
        let descriptor = match plan {
            DeferredRemoteAgentPlan::Deferred(descriptor) => descriptor,
            other => panic!("expected Deferred, got {other:?}"),
        };
        assert_eq!(descriptor.rewrite, RemoteAgentResponseRewrite::AgentStart);
        let Method::AgentStart(params) = &descriptor.request.method else {
            panic!("expected agent.start");
        };
        // Host stripped and new_workspace defaulted for a bare remote start.
        assert!(params.host.is_none());
        assert!(params.new_workspace);
    }

    #[test]
    fn agent_start_host_cached_disconnected_fails_fast_before_dispatch() {
        let mut app = remote_enabled_app();
        seed_stale_agent(&mut app, RemoteConnectionStatus::Disconnected);

        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentStart(base_start_params(Some("jafar"))),
        });
        let response = match plan {
            DeferredRemoteAgentPlan::Immediate(response) => response,
            other => panic!("expected Immediate, got {other:?}"),
        };
        let parsed: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.error.code, "remote_host_not_connected");
    }

    fn seeded_bridge_state() -> crate::remote::RemoteApiBridgeState {
        crate::remote::RemoteApiBridgeState {
            shell_path: "\"$HOME/.local/bin/herdr\"".to_string(),
            capabilities: crate::api::schema::FederationCapabilities::current(),
        }
    }

    fn seed_connected_agent_with_bridge_state(app: &mut App, terminal_id: &str, name: &str) {
        let host = RemoteHostKey::new("jafar", "default");
        app.state.remote_sources.replace_connected_snapshot(
            host.clone(),
            vec![standalone_remote_agent(terminal_id, name)],
        );
        app.state
            .remote_sources
            .set_connected_bridge_state(&host, seeded_bridge_state());
    }

    #[test]
    fn routed_descriptor_includes_prepared_state_for_connected_host() {
        // C5/test 4: connected routed actions (agent.read here) include the
        // cached prepared bridge state so the worker can skip per-request prep.
        let mut app = remote_enabled_app();
        seed_connected_agent_with_bridge_state(&mut app, "term-1", "codex");

        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentRead(read_params("jafar/codex")),
        });
        let descriptor = match plan {
            DeferredRemoteAgentPlan::Deferred(descriptor) => descriptor,
            other => panic!("expected Deferred, got {other:?}"),
        };
        let bridge_state = descriptor
            .bridge_state
            .as_ref()
            .expect("connected routed action must carry prepared state");
        assert_eq!(bridge_state.shell_path, seeded_bridge_state().shell_path);
        assert_eq!(
            bridge_state.capabilities,
            seeded_bridge_state().capabilities
        );
    }

    #[test]
    fn routed_descriptor_omits_prepared_state_for_stale_agent_read() {
        // C5/test 4: stale/non-connected agent.read must NOT carry prepared
        // state; the worker falls back to the full non-interactive bridge path.
        let mut app = remote_enabled_app();
        seed_stale_agent(&mut app, RemoteConnectionStatus::Disconnected);

        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentRead(read_params("jafar/codex")),
        });
        let descriptor = match plan {
            DeferredRemoteAgentPlan::Deferred(descriptor) => descriptor,
            other => panic!("expected Deferred, got {other:?}"),
        };
        assert!(
            descriptor.bridge_state.is_none(),
            "stale agent.read must not carry prepared state"
        );
    }

    #[test]
    fn agent_start_host_connected_with_cache_includes_prepared_state() {
        // C5/test 4: connected agent.start --host with cached prepared state
        // includes it. On-demand/no-cache omits it (next test).
        //
        // The host is connected with prepared state but no agent snapshot yet:
        // this mirrors a successful supervisor ping that published bridge state
        // before the first agent snapshot arrived. `set_connected_bridge_state`
        // is the reducer path for `AppEvent::RemoteSourceBridgeState` and marks
        // the host `Connected` while storing the prepared state, so the
        // `agent.start --host` connected precheck clears.
        let mut config = crate::config::Config::default();
        config.remote.enabled = true;
        config.remote.hosts = vec![crate::remote_target::RemoteHostConfig::new(
            "jafar", "jafar", "default", true,
        )];
        let mut app = test_app(&config);
        let host = RemoteHostKey::new("jafar", "default");
        app.state
            .remote_sources
            .set_connected_bridge_state(&host, seeded_bridge_state());

        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentStart(base_start_params(Some("jafar"))),
        });
        let descriptor = match plan {
            DeferredRemoteAgentPlan::Deferred(descriptor) => descriptor,
            other => panic!("expected Deferred, got {other:?}"),
        };
        assert!(
            descriptor.bridge_state.is_some(),
            "connected agent.start --host with cache must carry prepared state"
        );
    }

    #[test]
    fn agent_start_host_on_demand_without_cache_omits_prepared_state() {
        // C5/test 4: on-demand/no-cache agent.start --host omits prepared
        // state; the worker uses the full non-interactive bridge path.
        let mut config = crate::config::Config::default();
        config.remote.enabled = true;
        config.remote.hosts = vec![crate::remote_target::RemoteHostConfig::new(
            "jafar", "jafar", "default", false,
        )];
        let app = test_app(&config);

        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentStart(base_start_params(Some("jafar"))),
        });
        let descriptor = match plan {
            DeferredRemoteAgentPlan::Deferred(descriptor) => descriptor,
            other => panic!("expected Deferred, got {other:?}"),
        };
        assert!(
            descriptor.bridge_state.is_none(),
            "on-demand/no-cache agent.start --host must not carry prepared state"
        );
    }

    #[test]
    fn worker_with_cached_prepared_state_preserves_rewrite_error_and_telemetry_semantics() {
        // C5/test 5: a descriptor carrying cached prepared state still maps a
        // bridge error to remote_request_failed and rewrites the id. The fake
        // bridge asserts the prepared state was forwarded to the bridge seam.
        static BRIDGE_SAW_STATE: AtomicBool = AtomicBool::new(false);
        fn state_aware_bridge(
            _host: &RemoteHostConfig,
            _request: &Request,
            state: Option<&crate::remote::RemoteApiBridgeState>,
        ) -> std::io::Result<String> {
            if state.is_some() {
                BRIDGE_SAW_STATE.store(true, Ordering::SeqCst);
            }
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "simulated bridge timeout",
            ))
        }

        let descriptor = RemoteAgentDispatchDescriptor {
            host: RemoteHostConfig::new("jafar", "jafar", "default", true),
            request: Request {
                id: "req".to_string(),
                method: Method::AgentRead(read_params("term-1")),
            },
            rewrite: RemoteAgentResponseRewrite::Id,
            bridge_state: Some(seeded_bridge_state()),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        run_remote_agent_dispatch_with_bridge(descriptor, tx, state_aware_bridge);

        let response = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let parsed: ErrorResponse = serde_json::from_str(&response).unwrap();
        // Response id rewrite + remote_request_failed mapping preserved.
        assert_eq!(parsed.id, "req");
        assert_eq!(parsed.error.code, "remote_request_failed");
        assert!(parsed.error.message.contains("simulated bridge timeout"));
        // The prepared state was forwarded through the bridge seam.
        assert!(BRIDGE_SAW_STATE.load(Ordering::SeqCst));
    }

    #[test]
    fn local_and_bare_targets_return_not_handled() {
        let mut app = remote_enabled_app();
        // A local workspace/terminal so bare agent targets resolve locally.
        let workspace = crate::workspace::Workspace::test_new("local");
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();

        // Bare agent.read (no remote host qualifier) is NotHandled.
        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentRead(read_params("missing")),
        });
        assert!(matches!(plan, DeferredRemoteAgentPlan::NotHandled));

        // agent.start without host is NotHandled.
        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentStart(base_start_params(None)),
        });
        assert!(matches!(plan, DeferredRemoteAgentPlan::NotHandled));

        // agent.get is NotHandled (cache-only, no worker).
        let plan = app.plan_deferred_remote_agent_request(&Request {
            id: "req".to_string(),
            method: Method::AgentGet(AgentTarget {
                target: "jafar/codex".to_string(),
            }),
        });
        assert!(matches!(plan, DeferredRemoteAgentPlan::NotHandled));
    }

    #[test]
    fn worker_bridge_error_returns_remote_request_failed_through_channel() {
        // Deterministic: a fake bridge that always errors. The worker must map
        // it to remote_request_failed and still send a response.
        fn failing_bridge(
            _host: &RemoteHostConfig,
            _request: &Request,
            _state: Option<&crate::remote::RemoteApiBridgeState>,
        ) -> std::io::Result<String> {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "simulated bridge timeout",
            ))
        }

        let descriptor = RemoteAgentDispatchDescriptor {
            host: crate::remote_target::RemoteHostConfig::new("jafar", "jafar", "default", true),
            request: Request {
                id: "req".to_string(),
                method: Method::AgentRead(read_params("term-1")),
            },
            rewrite: RemoteAgentResponseRewrite::Id,
            bridge_state: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        run_remote_agent_dispatch_with_bridge(descriptor, tx, failing_bridge);

        let response = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let parsed: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req");
        assert_eq!(parsed.error.code, "remote_request_failed");
        assert!(parsed.error.message.contains("simulated bridge timeout"));
    }

    #[test]
    fn worker_rewrite_error_returns_remote_request_failed_through_channel() {
        // A fake bridge that returns a malformed response so the id rewrite
        // fails. The worker must still send a remote_request_failed response.
        fn malformed_bridge(
            _host: &RemoteHostConfig,
            _request: &Request,
            _state: Option<&crate::remote::RemoteApiBridgeState>,
        ) -> std::io::Result<String> {
            Ok("not json".to_string())
        }

        let descriptor = RemoteAgentDispatchDescriptor {
            host: crate::remote_target::RemoteHostConfig::new("jafar", "jafar", "default", true),
            request: Request {
                id: "req".to_string(),
                method: Method::AgentRead(read_params("term-1")),
            },
            rewrite: RemoteAgentResponseRewrite::Id,
            bridge_state: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        run_remote_agent_dispatch_with_bridge(descriptor, tx, malformed_bridge);

        let response = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let parsed: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req");
        assert_eq!(parsed.error.code, "remote_request_failed");
    }

    /// Prove a remote agent request is deferred off the App loop while a fake
    /// worker is blocked, and a subsequent local request is still handled.
    #[test]
    fn remote_agent_request_defers_and_loop_continues_with_blocked_worker() {
        static STARTED: AtomicBool = AtomicBool::new(false);
        STARTED.store(false, Ordering::SeqCst);

        let mut app = remote_enabled_app();
        seed_connected_agent(&mut app, "term-1", "codex");

        // Fake starter: record invocation and spawn a worker that blocks
        // forever (never sends a response) to simulate a slow/sleeping host.
        // The starter itself returns immediately so the loop is not held.
        app.remote_agent_dispatch_starter = |_descriptor, _respond_to| {
            STARTED.store(true, Ordering::SeqCst);
            std::thread::spawn(|| {
                let (_tx, rx) = std::sync::mpsc::channel::<()>();
                let _ = rx.recv(); // blocks until the test process exits
            });
        };

        let (respond_to, _never_recv) = std::sync::mpsc::channel::<String>();
        let remote_message = crate::api::ApiRequestMessage {
            request: Request {
                id: "remote".to_string(),
                method: Method::AgentRead(read_params("jafar/codex")),
            },
            respond_to,
        };

        // Drive the real TUI message handler so the proof covers the actual
        // call site, not just the planner.
        let changed = app.handle_api_request_message(remote_message);
        // A deferred read does not change local UI; the bookkeeping flag may
        // be either, so assert only that the loop returned without blocking
        // (we got here) and the fake worker was actually started.
        let _ = changed;
        assert!(STARTED.load(Ordering::SeqCst));

        // A subsequent local request must still be handled on the loop even
        // though the fake worker is still blocked. Use the message handler
        // again so the loop-continuation proof is end-to-end.
        let (local_respond_to, local_rx) = std::sync::mpsc::channel::<String>();
        let local_message = crate::api::ApiRequestMessage {
            request: Request {
                id: "local".to_string(),
                method: Method::AgentListLocal(EmptyParams::default()),
            },
            respond_to: local_respond_to,
        };
        app.handle_api_request_message(local_message);
        let response = local_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("local request must be handled while worker is blocked");
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "local");
    }

    #[test]
    fn limiter_rejects_nth_plus_one_acquire_for_one_host_session() {
        let limiter = RemoteAgentBridgeLimiter::new(REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT);
        let key = RemoteHostKey::new("jafar", "default");

        // Hold every acquired permit so the slots stay saturated; a dropped
        // permit would release its slot immediately.
        let held: Vec<_> = (0..REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT)
            .map(|_| limiter.try_acquire(&key).expect("acquire within limit"))
            .collect();
        // N+1 is rejected: the cap is saturated and no slot is handed out.
        assert!(limiter.try_acquire(&key).is_none());
        assert_eq!(limiter.in_flight(&key), REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT);
        drop(held);
    }

    #[test]
    fn limiter_permits_are_per_host_session() {
        let limiter = RemoteAgentBridgeLimiter::new(REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT);
        let key_a = RemoteHostKey::new("jafar", "default");
        let key_b = RemoteHostKey::new("work-mini", "default");

        // Saturate host A.
        let held_a: Vec<_> = (0..REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT)
            .map(|_| limiter.try_acquire(&key_a).unwrap())
            .collect();
        assert!(limiter.try_acquire(&key_a).is_none());

        // Host B is independent: saturating A does not block B. Hold the permit
        // so the slot stays occupied for the in-flight assertion.
        let permit_b = limiter.try_acquire(&key_b).expect("host B independent");
        assert_eq!(
            limiter.in_flight(&key_a),
            REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT
        );
        assert_eq!(limiter.in_flight(&key_b), 1);

        drop(permit_b);
        drop(held_a);
    }

    #[test]
    fn limiter_permit_releases_on_drop() {
        let limiter = RemoteAgentBridgeLimiter::new(1);
        let key = RemoteHostKey::new("jafar", "default");

        let permit = limiter.try_acquire(&key).unwrap();
        assert!(limiter.try_acquire(&key).is_none());
        drop(permit);
        // The slot is freed; a new acquire succeeds.
        assert!(limiter.try_acquire(&key).is_some());
    }

    #[test]
    fn limiter_permit_releases_on_worker_completion() {
        let limiter = RemoteAgentBridgeLimiter::new(REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT);
        let key = RemoteHostKey::new("jafar", "default");

        // Saturate the cap.
        let mut held: Vec<_> = (0..REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT)
            .map(|_| limiter.try_acquire(&key).unwrap())
            .collect();
        assert!(limiter.try_acquire(&key).is_none());

        // Move one permit into a worker closure. `thread::scope` keeps the
        // borrow valid until the worker completes; the permit releases when the
        // closure returns, modeling a worker that finished (success or error).
        let permit = held.pop().expect("held permits non-empty");
        std::thread::scope(|s| {
            s.spawn(move || {
                let _permit = permit;
            });
        });

        // The worker released its slot; the cap is no longer saturated.
        assert!(limiter.try_acquire(&key).is_some());
        drop(held);
    }

    #[test]
    fn saturated_cap_returns_remote_bridge_busy_without_permit() {
        let limiter = RemoteAgentBridgeLimiter::new(REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT);
        let key = RemoteHostKey::new("jafar", "default");

        // Saturate the (host, session) cap with locally-held permits. Tests
        // never touch the production global limiter.
        let held: Vec<_> = (0..REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT)
            .map(|_| limiter.try_acquire(&key).unwrap())
            .collect();
        assert_eq!(limiter.in_flight(&key), REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT);

        let descriptor = RemoteAgentDispatchDescriptor {
            host: RemoteHostConfig::new("jafar", "jafar", "default", true),
            request: Request {
                id: "req".to_string(),
                method: Method::AgentRead(read_params("term-1")),
            },
            rewrite: RemoteAgentResponseRewrite::Id,
            bridge_state: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        // This is the exact acquire step the default dispatch starter runs
        // before spawning a worker. With the cap saturated it must hand out no
        // permit (so no worker is spawned) and send `remote_bridge_busy`.
        let permit = acquire_remote_agent_bridge_permit_or_busy(&limiter, &descriptor, &tx);
        assert!(permit.is_none(), "saturated cap must not hand out a permit");
        // No permit was acquired: the in-flight count is unchanged at the cap.
        assert_eq!(limiter.in_flight(&key), REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT);

        let response = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("busy response must be sent immediately");
        let parsed: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req");
        assert_eq!(parsed.error.code, "remote_bridge_busy");
        assert!(parsed.error.message.contains("jafar"));
        assert!(parsed.error.message.contains("default"));
        assert!(parsed
            .error
            .message
            .contains(&REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT.to_string()));

        drop(held);
    }

    #[test]
    fn permit_releases_after_worker_bridge_error_and_mapping_stays_remote_request_failed() {
        // Prove that holding a permit across a worker that actually runs does
        // not change the existing `remote_request_failed` mapping, and that the
        // permit releases on the bridge-error completion path. This mirrors the
        // `spawn_remote_agent_dispatch_with_limiter` worker closure body
        // without touching real SSH or the production global limiter.
        fn failing_bridge(
            _host: &RemoteHostConfig,
            _request: &Request,
            _state: Option<&crate::remote::RemoteApiBridgeState>,
        ) -> std::io::Result<String> {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "simulated bridge timeout",
            ))
        }

        let limiter = RemoteAgentBridgeLimiter::new(1);
        let key = RemoteHostKey::new("jafar", "default");
        let permit = limiter.try_acquire(&key).unwrap();
        assert!(limiter.try_acquire(&key).is_none());

        let descriptor = RemoteAgentDispatchDescriptor {
            host: RemoteHostConfig::new("jafar", "jafar", "default", true),
            request: Request {
                id: "req".to_string(),
                method: Method::AgentRead(read_params("term-1")),
            },
            rewrite: RemoteAgentResponseRewrite::Id,
            bridge_state: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();

        // Worker closure body, inline (no real thread/SSH): the permit is held
        // while the bridge runs and drops when the block ends.
        {
            let _permit_guard = permit;
            run_remote_agent_dispatch_with_bridge(descriptor, tx, failing_bridge);
        }
        // The worker completed with a bridge error; its slot is freed.
        assert!(limiter.try_acquire(&key).is_some());

        let response = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("worker must send a response");
        let parsed: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req");
        assert_eq!(parsed.error.code, "remote_request_failed");
        assert!(parsed.error.message.contains("simulated bridge timeout"));
    }

    #[test]
    fn with_limiter_saturated_sends_busy_and_does_not_invoke_bridge() {
        // Directly exercise the extracted starter core with a local (leaked)
        // test limiter so the production global is never touched. A saturated
        // cap must send `remote_bridge_busy` immediately and must not spawn a
        // worker, so the injectable bridge is never invoked.
        static BRIDGE_INVOKED: AtomicBool = AtomicBool::new(false);
        fn tracking_bridge(
            _host: &RemoteHostConfig,
            _request: &Request,
            _state: Option<&crate::remote::RemoteApiBridgeState>,
        ) -> std::io::Result<String> {
            BRIDGE_INVOKED.store(true, Ordering::SeqCst);
            Ok(r#"{"id":"ignored","result":{"type":"ok"}}"#.to_string())
        }

        let limiter: &'static RemoteAgentBridgeLimiter =
            Box::leak(Box::new(RemoteAgentBridgeLimiter::new(1)));
        let key = RemoteHostKey::new("jafar", "default");

        // Saturate the single-slot cap with a locally-held permit.
        let _held = limiter.try_acquire(&key).expect("acquire within limit");

        let descriptor = RemoteAgentDispatchDescriptor {
            host: RemoteHostConfig::new("jafar", "jafar", "default", true),
            request: Request {
                id: "req".to_string(),
                method: Method::AgentRead(read_params("term-1")),
            },
            rewrite: RemoteAgentResponseRewrite::Id,
            bridge_state: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_remote_agent_dispatch_with_limiter(descriptor, tx, limiter, tracking_bridge);

        let response = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("busy response must be sent immediately");
        let parsed: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req");
        assert_eq!(parsed.error.code, "remote_bridge_busy");
        // No worker was spawned, so the bridge was never invoked.
        assert!(!BRIDGE_INVOKED.load(Ordering::SeqCst));
        // The in-flight count is unchanged at the cap.
        assert_eq!(limiter.in_flight(&key), 1);
    }

    #[test]
    fn with_limiter_success_runs_injectable_bridge_and_releases_permit() {
        // Directly exercise the extracted starter core with a local (leaked)
        // test limiter: a successful acquire spawns a worker that runs the
        // injectable fake bridge, sends a rewritten success response, and
        // releases the permit on completion.
        static BRIDGE_INVOKED: AtomicBool = AtomicBool::new(false);
        fn fake_bridge(
            _host: &RemoteHostConfig,
            _request: &Request,
            _state: Option<&crate::remote::RemoteApiBridgeState>,
        ) -> std::io::Result<String> {
            BRIDGE_INVOKED.store(true, Ordering::SeqCst);
            Ok(r#"{"id":"ignored","result":{"type":"ok"}}"#.to_string())
        }

        let limiter: &'static RemoteAgentBridgeLimiter =
            Box::leak(Box::new(RemoteAgentBridgeLimiter::new(1)));
        let key = RemoteHostKey::new("jafar", "default");

        let descriptor = RemoteAgentDispatchDescriptor {
            host: RemoteHostConfig::new("jafar", "jafar", "default", true),
            request: Request {
                id: "req".to_string(),
                method: Method::AgentRead(read_params("term-1")),
            },
            rewrite: RemoteAgentResponseRewrite::Id,
            bridge_state: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_remote_agent_dispatch_with_limiter(descriptor, tx, limiter, fake_bridge);

        let response = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("worker must send a response");
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req");
        assert!(BRIDGE_INVOKED.load(Ordering::SeqCst));

        // The worker spawned in a background thread; after it sends the
        // response it drops the permit on closure return. Poll until the slot
        // frees, then prove a new acquire succeeds.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while limiter.in_flight(&key) != 0 {
            if std::time::Instant::now() >= deadline {
                panic!("permit was not released after worker completion");
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(limiter.try_acquire(&key).is_some());
    }

    #[test]
    fn remote_agent_method_name_classifies_routed_methods() {
        // Telemetry name only: never records params/values. The canonical dot
        // names must match what each planner call site passes.
        assert_eq!(
            remote_agent_method_name(&Method::AgentRead(read_params("t"))),
            "agent.read"
        );
        assert_eq!(
            remote_agent_method_name(&Method::AgentFocus(AgentTarget {
                target: "t".to_string(),
            })),
            "agent.focus"
        );
        assert_eq!(
            remote_agent_method_name(&Method::AgentSend(AgentSendParams {
                target: "t".to_string(),
                text: "secret-text".to_string(),
            })),
            "agent.send"
        );
        assert_eq!(
            remote_agent_method_name(&Method::AgentSubmit(AgentSubmitParams {
                target: "t".to_string(),
                text: "secret-text".to_string(),
            })),
            "agent.submit"
        );
        assert_eq!(
            remote_agent_method_name(&Method::AgentTeardown(AgentTeardownParams {
                target: "t".to_string(),
                confirm: true,
            })),
            "agent.teardown"
        );
        assert_eq!(
            remote_agent_method_name(&Method::AgentStart(base_start_params(Some("jafar")))),
            "agent.start"
        );
    }

    #[test]
    fn remote_response_error_code_extracts_controlled_code_only() {
        // Success response: no error code.
        assert!(remote_response_error_code(r#"{"id":"r","result":{"type":"ok"}}"#).is_none());
        // Authoritative remote API error: controlled `error.code` scalar only.
        assert_eq!(
            remote_response_error_code(
                r#"{"id":"r","error":{"code":"agent_not_found","message":"leaky detail"}}"#
            )
            .as_deref(),
            Some("agent_not_found")
        );
        // Local bridge/rewrite failure mapped by the worker: fixed label.
        assert_eq!(
            remote_response_error_code(
                r#"{"id":"r","error":{"code":"remote_request_failed","message":"x"}}"#
            )
            .as_deref(),
            Some("remote_request_failed")
        );
        // Malformed JSON is not a remote error.
        assert!(remote_response_error_code("not json").is_none());
        // Missing `error.code` is not a remote error.
        assert!(remote_response_error_code(r#"{"id":"r","error":{}}"#).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn remote_bridge_pool_cap_equals_app_layer_limiter() {
        // Phase G.10 cross-layer invariant: the remote-layer pool cap
        // (`REMOTE_AGENT_BRIDGE_POOL_MAX_PER_HOST`) must equal the app-layer
        // per-(host, session) in-flight limiter
        // (`REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT`). The limiter is acquired before
        // the pool is consulted, and the pool cap staying equal to it means
        // `active + idle` can never exceed the limiter that gates dispatch.
        // This module can reach both consts, so the equality is pinned here.
        assert_eq! {
            crate::remote::REMOTE_AGENT_BRIDGE_POOL_MAX_PER_HOST,
            REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT
        };
    }
}
