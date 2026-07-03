use std::io;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use super::{
    state::{PendingRemoteWorkspaceCreate, ToastKind, ToastNotification},
    App,
};
use crate::api::client::{parse_response_value, ApiClientError};
use crate::api::schema::{Method, Request, ResponseResult, WorkspaceCreateParams, WorkspaceInfo};
use crate::events::AppEvent;
use crate::remote_source::{RemoteHostKey, RemoteSpaceKey};
use crate::remote_target::RemoteHostConfig;

const REMOTE_WORKSPACE_CREATE_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) fn remote_workspace_create_request(token: u64) -> Request {
    Request {
        id: format!("remote-workspace.create.{token}"),
        method: Method::WorkspaceCreate(WorkspaceCreateParams {
            cwd: None,
            focus: true,
            label: None,
            env: Default::default(),
        }),
    }
}

pub(crate) fn parse_remote_workspace_create_response(response: &str) -> io::Result<WorkspaceInfo> {
    let value: serde_json::Value = serde_json::from_str(response).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid remote workspace create response JSON: {err}"),
        )
    })?;
    let result = parse_response_value(value)
        .map(|response| response.result)
        .map_err(remote_api_client_error)?;
    match result {
        ResponseResult::WorkspaceCreated {
            workspace,
            tab: _,
            root_pane: _,
        } => Ok(workspace),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected workspace.create response, got {other:?}"),
        )),
    }
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

fn remote_host_label(host: &RemoteHostKey) -> String {
    if host.session == crate::session::DEFAULT_SESSION_NAME {
        host.host.clone()
    } else {
        format!("{}/{}", host.host, host.session)
    }
}

fn remote_workspace_label(workspace: &WorkspaceInfo) -> String {
    let label = workspace.label.trim();
    if label.is_empty() {
        workspace.workspace_id.clone()
    } else {
        label.to_string()
    }
}

fn workspace_create_unavailable_message(host: &RemoteHostKey) -> String {
    format!(
        "{} does not advertise workspace create",
        remote_host_label(host)
    )
}

fn show_toast(app: &mut App, kind: ToastKind, title: String, context: String) {
    app.state.toast = Some(ToastNotification {
        kind,
        title,
        context,
        position: None,
        target: None,
    });
}

fn create_failure_is_unavailable(message: &str) -> bool {
    message.contains("does not advertise federation method workspace_create")
}

impl App {
    pub(crate) fn drain_remote_workspace_create_request(&mut self) {
        let Some(host) = self.state.request_remote_workspace_create.take() else {
            return;
        };
        self.begin_remote_workspace_create_request(host, Instant::now());
    }

    fn begin_remote_workspace_create_request(&mut self, host: RemoteHostKey, now: Instant) {
        self.begin_remote_workspace_create_request_with_dispatch(
            host,
            now,
            spawn_remote_workspace_create_worker,
        );
    }

    pub(crate) fn begin_remote_workspace_create_request_with_dispatch<D>(
        &mut self,
        host: RemoteHostKey,
        now: Instant,
        dispatch: D,
    ) where
        D: FnOnce(RemoteHostConfig, RemoteHostKey, u64, Request, mpsc::Sender<AppEvent>),
    {
        let host_label = remote_host_label(&host);
        if self
            .state
            .pending_remote_workspace_creates
            .contains_key(&host)
        {
            show_toast(
                self,
                ToastKind::NeedsAttention,
                format!("Create already pending on {host_label}"),
                "Wait for the current remote create request to finish".to_string(),
            );
            return;
        }

        let Some(config) = self.remote_hosts.get(&host.host).cloned() else {
            show_toast(
                self,
                ToastKind::NeedsAttention,
                "create unavailable".to_string(),
                format!("{host_label} is not configured"),
            );
            return;
        };
        if config.session != host.session || !config.auto_connect {
            show_toast(
                self,
                ToastKind::NeedsAttention,
                "create unavailable".to_string(),
                format!("{host_label} is not a connected direct remote source"),
            );
            return;
        }
        let status = self
            .state
            .remote_sources
            .host_status(&host)
            .unwrap_or(crate::remote_source::RemoteConnectionStatus::Disconnected);
        if !status.is_connected() {
            let status = status.stale_label().unwrap_or("disconnected");
            show_toast(
                self,
                ToastKind::NeedsAttention,
                "create unavailable".to_string(),
                format!("{host_label} is {status}"),
            );
            return;
        }
        if !self
            .state
            .remote_sources
            .host_supports_workspace_create(&host)
        {
            show_toast(
                self,
                ToastKind::NeedsAttention,
                "create unavailable".to_string(),
                workspace_create_unavailable_message(&host),
            );
            return;
        }

        let token = self.state.next_remote_workspace_create_token;
        let Some(next_token) = token.checked_add(1) else {
            show_toast(
                self,
                ToastKind::NeedsAttention,
                "create unavailable".to_string(),
                "remote workspace create token space is exhausted".to_string(),
            );
            return;
        };
        self.state.next_remote_workspace_create_token = next_token;
        let request = remote_workspace_create_request(token);
        self.state.pending_remote_workspace_creates.insert(
            host.clone(),
            PendingRemoteWorkspaceCreate {
                token,
                deadline: now + REMOTE_WORKSPACE_CREATE_TIMEOUT,
            },
        );
        dispatch(config, host, token, request, self.event_tx.clone());
    }

    pub(crate) fn next_remote_workspace_create_deadline(&self) -> Option<Instant> {
        self.state
            .pending_remote_workspace_creates
            .values()
            .map(|pending| pending.deadline)
            .min()
    }

    pub(crate) fn handle_remote_workspace_create_timeouts(&mut self, now: Instant) -> bool {
        let events = self
            .state
            .pending_remote_workspace_creates
            .iter()
            .filter_map(|(host, pending)| {
                (now >= pending.deadline).then_some(AppEvent::RemoteWorkspaceCreateTimedOut {
                    host: host.clone(),
                    token: pending.token,
                })
            })
            .collect::<Vec<_>>();
        if events.is_empty() {
            return false;
        }

        for event in events {
            self.state.handle_app_event(event);
        }
        true
    }
}

fn spawn_remote_workspace_create_worker(
    config: RemoteHostConfig,
    host: RemoteHostKey,
    token: u64,
    request: Request,
    event_tx: mpsc::Sender<AppEvent>,
) {
    std::thread::spawn(move || {
        let event =
            match crate::remote::send_remote_api_request_to_host_noninteractive(&config, &request)
                .and_then(|response| parse_remote_workspace_create_response(&response))
            {
                Ok(workspace) => AppEvent::RemoteWorkspaceCreateSucceeded {
                    host,
                    token,
                    workspace,
                },
                Err(err) => AppEvent::RemoteWorkspaceCreateFailed {
                    host,
                    token,
                    message: err.to_string(),
                },
            };
        let _ = event_tx.blocking_send(event);
    });
}

impl crate::app::state::AppState {
    pub(crate) fn handle_remote_workspace_create_succeeded(
        &mut self,
        host: RemoteHostKey,
        token: u64,
        workspace: WorkspaceInfo,
    ) {
        let Some(pending) = self.pending_remote_workspace_creates.get(&host) else {
            return;
        };
        if pending.token != token {
            return;
        }
        self.pending_remote_workspace_creates.remove(&host);

        let workspace_id = workspace.workspace_id.clone();
        let workspace_label = remote_workspace_label(&workspace);
        self.remote_sources
            .upsert_workspace(host.clone(), workspace);
        self.selected_remote_space = Some(RemoteSpaceKey {
            host: host.host.clone(),
            session: host.session.clone(),
            workspace_id,
        });
        self.selected_remote_agent = None;
        self.toast = Some(ToastNotification {
            kind: ToastKind::Finished,
            title: format!("Created space on {}", remote_host_label(&host)),
            context: workspace_label,
            position: None,
            target: None,
        });
    }

    pub(crate) fn handle_remote_workspace_create_failed(
        &mut self,
        host: RemoteHostKey,
        token: u64,
        message: String,
    ) {
        let Some(pending) = self.pending_remote_workspace_creates.get(&host) else {
            return;
        };
        if pending.token != token {
            return;
        }
        self.pending_remote_workspace_creates.remove(&host);

        if create_failure_is_unavailable(&message) {
            self.toast = Some(ToastNotification {
                kind: ToastKind::NeedsAttention,
                title: "create unavailable".to_string(),
                context: format!("{}: {message}", remote_host_label(&host)),
                position: None,
                target: None,
            });
        } else {
            self.toast = Some(ToastNotification {
                kind: ToastKind::NeedsAttention,
                title: format!(
                    "Create on {} may not have completed",
                    remote_host_label(&host)
                ),
                context: message,
                position: None,
                target: None,
            });
        }
    }

    pub(crate) fn handle_remote_workspace_create_timed_out(
        &mut self,
        host: RemoteHostKey,
        token: u64,
    ) {
        let Some(pending) = self.pending_remote_workspace_creates.get(&host) else {
            return;
        };
        if pending.token != token {
            return;
        }
        self.pending_remote_workspace_creates.remove(&host);

        let host_label = remote_host_label(&host);
        self.toast = Some(ToastNotification {
            kind: ToastKind::NeedsAttention,
            title: format!("Create on {host_label} may not have completed"),
            context: format!("Check {host_label} spaces"),
            position: None,
            target: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Instant;

    use super::*;
    use crate::api::schema::{AgentStatus, PaneInfo, SuccessResponse, TabInfo, WorkspaceInfo};
    use crate::app::state::AppState;
    use crate::config::Config;
    use crate::remote_source::{RemoteConnectionStatus, RemoteSourceCapabilities};

    fn workspace(workspace_id: &str, label: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            workspace_id: workspace_id.to_string(),
            number: 1,
            label: label.to_string(),
            focused: true,
            pane_count: 1,
            tab_count: 1,
            active_tab_id: "tab-1".to_string(),
            agent_status: AgentStatus::Unknown,
            worktree: None,
        }
    }

    fn tab() -> TabInfo {
        TabInfo {
            tab_id: "tab-1".to_string(),
            workspace_id: "ws-1".to_string(),
            number: 1,
            label: "1".to_string(),
            focused: true,
            pane_count: 1,
            agent_status: AgentStatus::Unknown,
        }
    }

    fn pane() -> PaneInfo {
        PaneInfo {
            pane_id: "pane-1".to_string(),
            terminal_id: "term-1".to_string(),
            workspace_id: "ws-1".to_string(),
            tab_id: "tab-1".to_string(),
            cwd: None,
            foreground_cwd: None,
            label: None,
            focused: true,
            agent: None,
            agent_status: AgentStatus::Unknown,
            title: None,
            display_agent: None,
            custom_status: None,
            state_labels: std::collections::HashMap::new(),
            agent_session: None,
            revision: 1,
        }
    }

    fn host() -> RemoteHostKey {
        RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME)
    }

    fn app_with_remote_host() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut config = Config::default();
        config.remote.enabled = true;
        config.remote.hosts = vec![RemoteHostConfig::new(
            "jafar",
            "jafar",
            crate::session::DEFAULT_SESSION_NAME,
            true,
        )];
        App::new(&config, true, None, api_rx, crate::api::EventHub::default())
    }

    fn mark_host_create_capable(app: &mut App, host: &RemoteHostKey) {
        app.state
            .remote_sources
            .replace_connected_snapshot(host.clone(), Vec::new());
        app.state.remote_sources.set_capabilities(
            host,
            RemoteSourceCapabilities {
                workspace_list_local: false,
                workspace_create: true,
                tab_list: false,
                layout_export: false,
            },
        );
        assert_eq!(
            app.state.remote_sources.host_status(host),
            Some(RemoteConnectionStatus::Connected)
        );
    }

    #[test]
    fn remote_workspace_create_request_uses_remote_defaults() {
        let request = remote_workspace_create_request(42);

        let Method::WorkspaceCreate(params) = request.method else {
            panic!("expected workspace.create");
        };
        assert_eq!(request.id, "remote-workspace.create.42");
        assert_eq!(params.cwd, None);
        assert_eq!(params.label, None);
        assert!(params.focus);
    }

    #[test]
    fn remote_workspace_create_response_accepts_workspace_created() {
        let response = serde_json::to_string(&SuccessResponse {
            id: "remote-workspace.create.1".to_string(),
            result: ResponseResult::WorkspaceCreated {
                workspace: workspace("ws-1", "tmp"),
                tab: tab(),
                root_pane: pane(),
            },
        })
        .unwrap();

        let workspace = parse_remote_workspace_create_response(&response).unwrap();

        assert_eq!(workspace.workspace_id, "ws-1");
        assert_eq!(workspace.label, "tmp");
    }

    #[test]
    fn remote_workspace_create_response_rejects_wrong_type_and_malformed_json() {
        let wrong = serde_json::to_string(&SuccessResponse {
            id: "remote-workspace.create.1".to_string(),
            result: ResponseResult::Ok {},
        })
        .unwrap();

        let err = parse_remote_workspace_create_response(&wrong).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("expected workspace.create"));

        let err = parse_remote_workspace_create_response("not json").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn remote_workspace_create_dispatches_once_and_duplicate_pending_does_not_dispatch() {
        let mut app = app_with_remote_host();
        let host = host();
        mark_host_create_capable(&mut app, &host);
        let calls = Rc::new(RefCell::new(Vec::<Request>::new()));
        let first_calls = Rc::clone(&calls);
        let expected_host = host.clone();
        let now = Instant::now();

        app.begin_remote_workspace_create_request_with_dispatch(
            host.clone(),
            now,
            move |_config, dispatch_host, token, request, _event_tx| {
                assert_eq!(dispatch_host, expected_host);
                assert_eq!(token, 1);
                first_calls.borrow_mut().push(request);
            },
        );
        let second_calls = Rc::clone(&calls);
        app.begin_remote_workspace_create_request_with_dispatch(
            host.clone(),
            now,
            move |_config, _host, _token, request, _event_tx| {
                second_calls.borrow_mut().push(request);
            },
        );

        let calls = calls.borrow();
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0].method, Method::WorkspaceCreate(_)));
        assert_eq!(
            app.state
                .pending_remote_workspace_creates
                .get(&host)
                .map(|pending| pending.token),
            Some(1)
        );
        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.title.as_str()),
            Some("Create already pending on jafar")
        );
    }

    #[test]
    fn remote_workspace_create_unavailable_without_capability_does_not_dispatch() {
        let mut app = app_with_remote_host();
        let host = host();
        app.state
            .remote_sources
            .replace_connected_snapshot(host.clone(), Vec::new());
        let mut dispatched = false;

        app.begin_remote_workspace_create_request_with_dispatch(
            host,
            Instant::now(),
            |_config, _host, _token, _request, _event_tx| {
                dispatched = true;
            },
        );

        assert!(!dispatched);
        assert!(app.state.pending_remote_workspace_creates.is_empty());
        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.title.as_str()),
            Some("create unavailable")
        );
        assert!(app
            .state
            .toast
            .as_ref()
            .is_some_and(|toast| toast.context.contains("workspace create")));
    }

    #[test]
    fn remote_workspace_create_matching_success_upserts_selects_and_clears_pending() {
        let mut state = AppState::test_new();
        let host = host();
        state.pending_remote_workspace_creates.insert(
            host.clone(),
            PendingRemoteWorkspaceCreate {
                token: 7,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        );
        state
            .remote_sources
            .replace_workspace_snapshot(host.clone(), vec![workspace("existing", "old")]);

        state.handle_app_event(AppEvent::RemoteWorkspaceCreateSucceeded {
            host: host.clone(),
            token: 7,
            workspace: workspace("new-ws", "tmp"),
        });

        assert!(state.pending_remote_workspace_creates.is_empty());
        assert_eq!(
            state.selected_remote_space,
            Some(RemoteSpaceKey {
                host: "jafar".to_string(),
                session: crate::session::DEFAULT_SESSION_NAME.to_string(),
                workspace_id: "new-ws".to_string(),
            })
        );
        let labels = state
            .remote_sources
            .workspace_entries_for_host(&host)
            .expect("workspace snapshot")
            .into_iter()
            .map(|entry| entry.workspace.label)
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["old", "tmp"]);
        assert_eq!(
            state.toast.as_ref().map(|toast| toast.title.as_str()),
            Some("Created space on jafar")
        );
    }

    #[test]
    fn remote_workspace_create_matching_failure_and_timeout_clear_without_cache_mutation() {
        let mut state = AppState::test_new();
        let host = host();
        state
            .remote_sources
            .replace_workspace_snapshot(host.clone(), vec![workspace("existing", "old")]);
        state.pending_remote_workspace_creates.insert(
            host.clone(),
            PendingRemoteWorkspaceCreate {
                token: 7,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        );

        state.handle_app_event(AppEvent::RemoteWorkspaceCreateFailed {
            host: host.clone(),
            token: 7,
            message: "ssh failed after dispatch".to_string(),
        });

        assert!(state.pending_remote_workspace_creates.is_empty());
        assert_eq!(
            state
                .remote_sources
                .workspace_entries_for_host(&host)
                .expect("workspace snapshot")
                .len(),
            1
        );
        assert_eq!(
            state.toast.as_ref().map(|toast| toast.title.as_str()),
            Some("Create on jafar may not have completed")
        );

        state.pending_remote_workspace_creates.insert(
            host.clone(),
            PendingRemoteWorkspaceCreate {
                token: 8,
                deadline: Instant::now(),
            },
        );
        state.handle_app_event(AppEvent::RemoteWorkspaceCreateTimedOut {
            host: host.clone(),
            token: 8,
        });

        assert!(state.pending_remote_workspace_creates.is_empty());
        assert_eq!(
            state
                .remote_sources
                .workspace_entries_for_host(&host)
                .expect("workspace snapshot")
                .len(),
            1
        );
        assert_eq!(
            state.toast.as_ref().map(|toast| toast.title.as_str()),
            Some("Create on jafar may not have completed")
        );
        assert!(state
            .toast
            .as_ref()
            .is_some_and(|toast| toast.context == "Check jafar spaces"));
    }

    #[test]
    fn remote_workspace_create_stale_tokens_are_noop() {
        let mut state = AppState::test_new();
        let host = host();
        state.pending_remote_workspace_creates.insert(
            host.clone(),
            PendingRemoteWorkspaceCreate {
                token: 9,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        );

        state.handle_app_event(AppEvent::RemoteWorkspaceCreateSucceeded {
            host: host.clone(),
            token: 8,
            workspace: workspace("new-ws", "tmp"),
        });
        state.handle_app_event(AppEvent::RemoteWorkspaceCreateFailed {
            host: host.clone(),
            token: 8,
            message: "late failure".to_string(),
        });
        state.handle_app_event(AppEvent::RemoteWorkspaceCreateTimedOut {
            host: host.clone(),
            token: 8,
        });

        assert_eq!(
            state
                .pending_remote_workspace_creates
                .get(&host)
                .map(|pending| pending.token),
            Some(9)
        );
        assert!(state
            .remote_sources
            .workspace_entries_for_host(&host)
            .is_none());
        assert!(state.selected_remote_space.is_none());
        assert!(state.toast.is_none());
    }

    #[test]
    fn remote_workspace_create_missing_capability_failure_uses_unavailable_toast() {
        let mut state = AppState::test_new();
        let host = host();
        state.pending_remote_workspace_creates.insert(
            host.clone(),
            PendingRemoteWorkspaceCreate {
                token: 7,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        );

        state.handle_app_event(AppEvent::RemoteWorkspaceCreateFailed {
            host,
            token: 7,
            message: "remote host jafar does not advertise federation method workspace_create"
                .to_string(),
        });

        assert_eq!(
            state.toast.as_ref().map(|toast| toast.title.as_str()),
            Some("create unavailable")
        );
    }

    #[test]
    fn remote_workspace_create_api_failure_uses_uncertain_toast() {
        let mut state = AppState::test_new();
        let host = host();
        state.pending_remote_workspace_creates.insert(
            host.clone(),
            PendingRemoteWorkspaceCreate {
                token: 7,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        );

        state.handle_app_event(AppEvent::RemoteWorkspaceCreateFailed {
            host,
            token: 7,
            message: "remote API error workspace_create_failed: cwd unavailable".to_string(),
        });

        assert_eq!(
            state.toast.as_ref().map(|toast| toast.title.as_str()),
            Some("Create on jafar may not have completed")
        );
    }
}
