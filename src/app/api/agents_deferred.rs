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
}

/// Signature of the bridge call the worker makes to the remote host. Defaults
/// to [`real_remote_agent_bridge`]; tests inject a fake to exercise the worker
/// error/response path deterministically without real SSH.
pub(crate) type RemoteAgentBridge = fn(&RemoteHostConfig, &Request) -> std::io::Result<String>;

/// Signature of the dispatch starter. The default implementation
/// ([`spawn_remote_agent_dispatch`]) spawns a background thread that runs the
/// bridge. Tests inject a fake starter to prove the loop continues while the
/// worker is blocked/not completed.
pub(crate) type RemoteAgentDispatchStarter =
    fn(RemoteAgentDispatchDescriptor, std::sync::mpsc::Sender<String>);

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
            Method::AgentTeardown(params) if !params.confirm => {
                DeferredRemoteAgentPlan::Immediate(encode_error(
                    request_id,
                    "confirmation_required",
                    "agent.teardown is destructive; pass confirm: true to proceed".to_string(),
                ))
            }

            Method::AgentFocus(target) => self.plan_remote_agent_method(
                &request_id,
                &target.target,
                /* require_connected */ true,
                |host, terminal_id| RemoteAgentDispatchDescriptor {
                    host,
                    request: remote_agent_focus_request(
                        request_id.clone(),
                        target.clone(),
                        &terminal_id,
                    ),
                    rewrite: RemoteAgentResponseRewrite::Id,
                },
            ),

            // `agent.read` preserves existing behavior: resolve the cached
            // remote target, rewrite to the authoritative terminal id, and
            // dispatch even for stale/non-connected cached entries because
            // read is safe/idempotent. Only route/resolve errors are
            // immediate; the slow reachability work still runs off-loop.
            Method::AgentRead(params) => self.plan_remote_agent_method(
                &request_id,
                &params.target,
                /* require_connected */ false,
                |host, terminal_id| RemoteAgentDispatchDescriptor {
                    host,
                    request: remote_agent_read_request(
                        request_id.clone(),
                        params.clone(),
                        &terminal_id,
                    ),
                    rewrite: RemoteAgentResponseRewrite::Id,
                },
            ),

            Method::AgentSend(params) => self.plan_remote_agent_method(
                &request_id,
                &params.target,
                /* require_connected */ true,
                |host, terminal_id| RemoteAgentDispatchDescriptor {
                    host,
                    request: remote_agent_send_request(
                        request_id.clone(),
                        params.clone(),
                        &terminal_id,
                    ),
                    rewrite: RemoteAgentResponseRewrite::Id,
                },
            ),

            Method::AgentSubmit(params) => self.plan_remote_agent_method(
                &request_id,
                &params.target,
                /* require_connected */ true,
                |host, terminal_id| RemoteAgentDispatchDescriptor {
                    host,
                    request: remote_agent_submit_request(
                        request_id.clone(),
                        params.clone(),
                        &terminal_id,
                    ),
                    rewrite: RemoteAgentResponseRewrite::Id,
                },
            ),

            // Confirmation was already enforced above; when `confirm: true`
            // the connected check still applies before forwarding. The
            // authoritative terminal id is resolved and substituted by the
            // builder, which always forwards `confirm: true`.
            Method::AgentTeardown(params) => self.plan_remote_agent_method(
                &request_id,
                &params.target,
                /* require_connected */ true,
                |host, terminal_id| RemoteAgentDispatchDescriptor {
                    host,
                    request: remote_agent_teardown_request(request_id.clone(), &terminal_id),
                    rewrite: RemoteAgentResponseRewrite::Id,
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
    /// stale/non-connected cached entries).
    fn plan_remote_agent_method(
        &self,
        request_id: &str,
        target: &str,
        require_connected: bool,
        build_descriptor: impl FnOnce(RemoteHostConfig, String) -> RemoteAgentDispatchDescriptor,
    ) -> DeferredRemoteAgentPlan {
        match self.plan_remote_agent_target_route(request_id, target, require_connected) {
            RemoteAgentTargetRoute::Local => DeferredRemoteAgentPlan::NotHandled,
            RemoteAgentTargetRoute::Immediate(response) => {
                DeferredRemoteAgentPlan::Immediate(response)
            }
            RemoteAgentTargetRoute::Ready { host, terminal_id } => {
                DeferredRemoteAgentPlan::Deferred(Box::new(build_descriptor(host, terminal_id)))
            }
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
            return DeferredRemoteAgentPlan::Immediate(encode_error_body(
                request_id.to_string(),
                remote_route_plan_error_body(RemoteRoutePlanError::UnknownHost(
                    host_alias.to_string(),
                )),
            ));
        };
        if let Err(err) = remote_agent_start_host_policy_guard(&host) {
            return DeferredRemoteAgentPlan::Immediate(encode_error_body(
                request_id.to_string(),
                err,
            ));
        }
        if let Err(err) = remote_agent_start_host_precheck(&self.state.remote_sources, &host) {
            return DeferredRemoteAgentPlan::Immediate(encode_error_body(
                request_id.to_string(),
                err,
            ));
        }
        let request = remote_agent_start_request(request_id.to_string(), params);
        DeferredRemoteAgentPlan::Deferred(Box::new(RemoteAgentDispatchDescriptor {
            host,
            request,
            rewrite: RemoteAgentResponseRewrite::AgentStart,
        }))
    }

    /// Shared route/resolve/(optional) connected-check helper. Returns the
    /// authoritative remote host and resolved terminal id when cleared to
    /// dispatch, an immediate error response for any guard failure, or `Local`
    /// when the target is not host-qualified remote (caller runs the sync
    /// path).
    fn plan_remote_agent_target_route(
        &self,
        request_id: &str,
        target: &str,
        require_connected: bool,
    ) -> RemoteAgentTargetRoute {
        let route = match self.plan_agent_api_target(target) {
            Ok(route) => route,
            Err(err) => {
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
                return RemoteAgentTargetRoute::Immediate(encode_error_body(
                    request_id.to_string(),
                    remote_agent_resolve_error_body(err),
                ));
            }
        };
        if require_connected {
            if let Err(err) = Self::remote_agent_resolved_connected_or_error(&host, &resolved) {
                return RemoteAgentTargetRoute::Immediate(encode_error_body(
                    request_id.to_string(),
                    err,
                ));
            }
        }
        RemoteAgentTargetRoute::Ready {
            host,
            terminal_id: resolved.entry.agent.terminal_id.clone(),
        }
    }
}

/// Production bridge: runs the real SSH-bridged JSON API request to the remote
/// host non-interactively.
pub(crate) fn real_remote_agent_bridge(
    host: &RemoteHostConfig,
    request: &Request,
) -> std::io::Result<String> {
    crate::remote::send_remote_api_request_to_host_noninteractive(host, request)
}

/// Default dispatch starter: spawn a background thread that runs the bridge
/// and sends a response through the one-shot channel. Always attempts to send a
/// response so a client never hangs on a worker/bridge/rewrite error.
pub(crate) fn spawn_remote_agent_dispatch(
    descriptor: RemoteAgentDispatchDescriptor,
    respond_to: std::sync::mpsc::Sender<String>,
) {
    std::thread::spawn(move || run_remote_agent_dispatch(descriptor, respond_to));
}

/// Run one remote-agent bridge dispatch to completion using the real bridge and
/// send the response. Maps any bridge/rewrite error to the existing
/// `remote_request_failed` error shape so the client always receives a
/// response.
pub(crate) fn run_remote_agent_dispatch(
    descriptor: RemoteAgentDispatchDescriptor,
    respond_to: std::sync::mpsc::Sender<String>,
) {
    run_remote_agent_dispatch_with_bridge(descriptor, respond_to, real_remote_agent_bridge);
}

/// Testable core: run one dispatch with an injectable bridge. A bridge or
/// rewrite error still sends a `remote_request_failed` response through the
/// channel so the client never hangs.
pub(crate) fn run_remote_agent_dispatch_with_bridge(
    descriptor: RemoteAgentDispatchDescriptor,
    respond_to: std::sync::mpsc::Sender<String>,
    bridge: RemoteAgentBridge,
) {
    let local_id = descriptor.request.id.clone();
    let host_name = descriptor.host.name.clone();
    let rewrite = descriptor.rewrite;
    let response =
        bridge(&descriptor.host, &descriptor.request).and_then(|response| match rewrite {
            RemoteAgentResponseRewrite::Id => rewrite_remote_response_id(&response, &local_id),
            RemoteAgentResponseRewrite::AgentStart => {
                rewrite_remote_agent_start_response(&response, &local_id, &host_name)
            }
        });
    let response = match response {
        Ok(response) => response,
        Err(err) => encode_error(local_id, "remote_request_failed", err.to_string()),
    };
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
        fn failing_bridge(_host: &RemoteHostConfig, _request: &Request) -> std::io::Result<String> {
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
}
