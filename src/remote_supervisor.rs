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
use crate::api::schema::{AgentInfo, EmptyParams, Method, PingParams, Request, ResponseResult};
use crate::events::AppEvent;
use crate::remote_source::{RemoteConnectionStatus, RemoteHostKey};
use crate::remote_target::{RemoteHostConfig, RemoteHostRegistry};

const REMOTE_SOURCE_PING_INTERVAL: Duration = Duration::from_secs(30);
const REMOTE_SOURCE_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(60);
const REMOTE_SOURCE_RETRY_INTERVAL: Duration = Duration::from_secs(15);
const REMOTE_SOURCE_INCOMPATIBLE_RETRY_INTERVAL: Duration = Duration::from_secs(5 * 60);
const REMOTE_SOURCE_STOP_POLL_INTERVAL: Duration = Duration::from_millis(250);

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
        .filter(|host| host.auto_connect)
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
        crate::remote::send_remote_api_request_to_host_noninteractive,
    );
}

fn remote_source_supervisor_loop_with<F>(
    host: RemoteHostConfig,
    event_tx: mpsc::Sender<AppEvent>,
    stop: Arc<AtomicBool>,
    send: F,
) where
    F: Fn(&RemoteHostConfig, &Request) -> io::Result<String>,
{
    let host_key = RemoteHostKey::new(host.name.clone(), host.session.clone());
    let mut next_ping = Instant::now();
    let mut next_snapshot = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= next_ping {
            match send_ping(&host, &send) {
                Ok(()) => next_ping = now + REMOTE_SOURCE_PING_INTERVAL,
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
                    let retry_interval = ping_failure_retry_interval(&err);
                    next_ping = now + retry_interval;
                    next_snapshot = next_snapshot.max(now + retry_interval);
                }
            }
        }

        let now = Instant::now();
        if now >= next_snapshot {
            match send_agent_list(&host, &send) {
                Ok(agents) => {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let _ = event_tx.blocking_send(AppEvent::RemoteSourceSnapshot {
                        host: host_key.clone(),
                        agents,
                    });
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
                    next_snapshot = now + REMOTE_SOURCE_RETRY_INTERVAL;
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

fn ping_failure_retry_interval(err: &io::Error) -> Duration {
    match crate::remote::classify_remote_failure(err) {
        crate::remote::RemoteFailureClass::NeedsUpdate => REMOTE_SOURCE_INCOMPATIBLE_RETRY_INTERVAL,
        crate::remote::RemoteFailureClass::Unreachable
        | crate::remote::RemoteFailureClass::Unknown => REMOTE_SOURCE_RETRY_INTERVAL,
    }
}

fn remote_source_failure_status(err: &io::Error) -> RemoteConnectionStatus {
    match crate::remote::classify_remote_failure(err) {
        crate::remote::RemoteFailureClass::NeedsUpdate => RemoteConnectionStatus::NeedsUpdate,
        crate::remote::RemoteFailureClass::Unreachable => RemoteConnectionStatus::Unreachable,
        crate::remote::RemoteFailureClass::Unknown => RemoteConnectionStatus::Disconnected,
    }
}

fn send_ping<F>(host: &RemoteHostConfig, send: &F) -> io::Result<()>
where
    F: Fn(&RemoteHostConfig, &Request) -> io::Result<String>,
{
    let response = send(host, &ping_request())?;
    parse_ping_response(&response)
}

fn send_agent_list<F>(host: &RemoteHostConfig, send: &F) -> io::Result<Vec<AgentInfo>>
where
    F: Fn(&RemoteHostConfig, &Request) -> io::Result<String>,
{
    let response = send(host, &agent_list_request())?;
    parse_agent_list_response(&response)
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

pub(crate) fn parse_ping_response(response: &str) -> io::Result<()> {
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
            Ok(())
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected pong response, got {other:?}"),
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

    use crate::api::schema::{AgentStatus, ErrorBody, ErrorResponse, SuccessResponse};

    use super::*;

    fn agent(terminal_id: &str) -> AgentInfo {
        AgentInfo {
            terminal_id: terminal_id.to_string(),
            name: Some("codex".to_string()),
            agent: Some("codex".to_string()),
            title: None,
            display_agent: Some("Codex".to_string()),
            agent_status: AgentStatus::Working,
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
                    federation: None,
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

    #[test]
    fn remote_supervisor_filters_auto_connect_hosts() {
        let registry = RemoteHostRegistry::from_configs(vec![
            RemoteHostConfig::new("jafar", "jafar", "default", true),
            RemoteHostConfig::new("manual", "manual", "default", false),
        ])
        .unwrap();

        let hosts = auto_connect_hosts(&registry);

        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].name, "jafar");
    }

    #[test]
    fn remote_supervisor_builds_ping_and_agent_list_requests_without_subscriptions() {
        assert!(matches!(ping_request().method, Method::Ping(_)));
        assert!(matches!(
            agent_list_request().method,
            Method::AgentListLocal(_)
        ));
    }

    #[test]
    fn remote_supervisor_ping_backoff_keeps_transient_failures_short() {
        let err = io::Error::new(io::ErrorKind::TimedOut, "ssh timed out");

        assert_eq!(
            ping_failure_retry_interval(&err),
            REMOTE_SOURCE_RETRY_INTERVAL
        );
    }

    #[test]
    fn remote_supervisor_ping_backoff_uses_long_interval_for_invalid_data() {
        let err = io::Error::new(
            io::ErrorKind::InvalidData,
            "remote API ping did not advertise federation support",
        );

        assert_eq!(
            ping_failure_retry_interval(&err),
            REMOTE_SOURCE_INCOMPATIBLE_RETRY_INTERVAL
        );
        assert!(REMOTE_SOURCE_INCOMPATIBLE_RETRY_INTERVAL > REMOTE_SOURCE_RETRY_INTERVAL);
    }

    #[test]
    fn remote_supervisor_ping_backoff_uses_long_interval_for_missing_binary() {
        let err = io::Error::new(
            io::ErrorKind::NotFound,
            "compatible herdr binary was not found",
        );

        assert_eq!(
            ping_failure_retry_interval(&err),
            REMOTE_SOURCE_INCOMPATIBLE_RETRY_INTERVAL
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
                    Method::Ping(_) => Ok(pong_response()),
                    Method::AgentListLocal(_) => Ok(agent_list_response(vec![agent("term-1")])),
                    _ => unreachable!("unexpected request"),
                }
            });
        });

        let event = rx.blocking_recv().unwrap();
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        let AppEvent::RemoteSourceSnapshot { host, agents } = event else {
            panic!("expected snapshot event");
        };
        assert_eq!(host, RemoteHostKey::new("jafar", "default"));
        assert_eq!(agents[0].terminal_id, "term-1");
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
                    Method::Ping(_) => Ok(old_pong_response_without_federation()),
                    Method::AgentListLocal(_) => panic!("snapshot should not be requested"),
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
                    Method::Ping(_) => Ok(pong_response()),
                    Method::AgentListLocal(_) => Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "remote API ping did not advertise federation method agent.list",
                    )),
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
}
