use std::path::PathBuf;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, Method, Request, ResponseResult, SuccessResponse,
    WorkspaceCreateParams, WorkspaceMoveParams, WorkspaceRenameParams, WorkspaceTarget,
};
use crate::app::App;

use super::remote_helpers::{
    remote_capability_unavailable_body, remote_route_plan_error_body,
    rewrite_remote_response_id_value,
};
use super::responses::{encode_error, encode_error_body, encode_success};
use crate::remote_target::{
    parse_target_route, resolve_remote_workspace_target, RemoteTargetSelector,
    RemoteWorkspaceResolveError, TargetRoute,
};

pub(super) fn remote_workspace_resolve_error_body(
    err: RemoteWorkspaceResolveError,
) -> crate::api::schema::ErrorBody {
    let code = match &err {
        RemoteWorkspaceResolveError::NotFound { .. } => "remote_workspace_not_found",
        RemoteWorkspaceResolveError::MetadataUnavailable { .. } => "remote_workspace_unavailable",
        RemoteWorkspaceResolveError::UnsupportedSelector { .. } => "remote_target_error",
    };
    crate::api::schema::ErrorBody {
        code: code.to_string(),
        message: err.to_string(),
    }
}

impl App {
    pub(super) fn handle_workspace_list(&mut self, id: String) -> String {
        encode_success(id, self.local_workspace_list_result())
    }

    pub(super) fn handle_workspace_list_local(&mut self, id: String) -> String {
        encode_success(id, self.local_workspace_list_result())
    }

    fn local_workspace_list_result(&self) -> ResponseResult {
        ResponseResult::WorkspaceList {
            workspaces: self.workspace_list_info(),
        }
    }

    pub(super) fn handle_workspace_get(&mut self, id: String, target: WorkspaceTarget) -> String {
        let Some(index) = self.parse_workspace_id(&target.workspace_id) else {
            return workspace_not_found(id, &target.workspace_id);
        };
        let Some(_) = self.state.workspaces.get(index) else {
            return workspace_not_found(id, &target.workspace_id);
        };

        encode_success(
            id,
            ResponseResult::WorkspaceInfo {
                workspace: self.workspace_info(index),
            },
        )
    }

    pub(super) fn handle_workspace_create(
        &mut self,
        id: String,
        params: WorkspaceCreateParams,
    ) -> String {
        let cwd = params.cwd.map(PathBuf::from).unwrap_or_else(|| {
            let follow_cwd = self
                .workspace_creation_source()
                .and_then(|ws_idx| self.seed_cwd_from_workspace(ws_idx));
            self.resolve_new_terminal_cwd(follow_cwd)
        });
        let extra_env = match super::env::normalize_launch_env(params.env) {
            Ok(env) => env,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        match self.create_workspace_with_launch_env(cwd, params.focus, extra_env) {
            Ok(index) => {
                if let Some(label) = params.label {
                    if let Some(workspace) = self.state.workspaces.get_mut(index) {
                        workspace.set_custom_name(label);
                        crate::logging::workspace_renamed(&workspace.id);
                    }
                }
                self.emit_workspace_open_events(index);
                encode_success(
                    id,
                    self.workspace_created_result(index)
                        .expect("new workspace should produce a complete create response"),
                )
            }
            Err(err) => encode_error(id, "workspace_create_failed", err.to_string()),
        }
    }

    pub(super) fn handle_workspace_focus(&mut self, id: String, target: WorkspaceTarget) -> String {
        let Some(index) = self.parse_workspace_id(&target.workspace_id) else {
            return workspace_not_found(id, &target.workspace_id);
        };
        if self.state.workspaces.get(index).is_none() {
            return workspace_not_found(id, &target.workspace_id);
        }
        self.state.switch_workspace(index);

        encode_success(
            id,
            ResponseResult::WorkspaceInfo {
                workspace: self.workspace_info(index),
            },
        )
    }

    pub(super) fn plan_workspace_target_remote_route(
        &self,
        workspace_id: &str,
    ) -> Result<
        Option<(crate::remote_target::RemoteHostConfig, RemoteTargetSelector)>,
        crate::remote_target::RemoteRoutePlanError,
    > {
        if self.remote_hosts.list().is_empty() {
            return Ok(None);
        }
        let Some((host_alias, _)) = workspace_id.split_once('/') else {
            return Ok(None);
        };
        let Some(host) = self.remote_hosts.get(host_alias).cloned() else {
            return Ok(None);
        };
        match parse_target_route(workspace_id)? {
            TargetRoute::Local { .. } => Ok(None),
            TargetRoute::Remote { target, .. } => Ok(Some((host, target))),
        }
    }

    fn handle_remote_workspace_rename(
        &mut self,
        id: String,
        host: crate::remote_target::RemoteHostConfig,
        selector: RemoteTargetSelector,
        params: WorkspaceRenameParams,
    ) -> String {
        self.handle_remote_workspace_rename_with_sender(
            id,
            host,
            selector,
            params,
            crate::remote::send_remote_api_request_to_host_noninteractive,
        )
    }

    fn handle_remote_workspace_rename_with_sender<F>(
        &mut self,
        id: String,
        host: crate::remote_target::RemoteHostConfig,
        selector: RemoteTargetSelector,
        params: WorkspaceRenameParams,
        send: F,
    ) -> String
    where
        F: FnOnce(&crate::remote_target::RemoteHostConfig, &Request) -> std::io::Result<String>,
    {
        let host_key =
            crate::remote_source::RemoteHostKey::new(host.name.clone(), host.session.clone());
        let host_status = self.state.remote_sources.host_status(&host_key);
        if !host_status.is_some_and(|status| status.is_connected()) {
            let status = host_status
                .and_then(|status| status.stale_label())
                .unwrap_or("disconnected")
                .to_string();
            return encode_error_body(
                id,
                crate::api::schema::ErrorBody {
                    code: "remote_host_not_connected".to_string(),
                    message: format!(
                        "remote host {} is {status}; wait for it to reconnect before mutating a remote workspace",
                        host.name
                    ),
                },
            );
        }
        if !self
            .state
            .remote_sources
            .host_capabilities(&host_key)
            .supports_route_method(crate::api::schema::FederationCapabilities::WORKSPACE_RENAME)
        {
            return encode_error_body(
                id,
                remote_capability_unavailable_body(
                    &host.name,
                    crate::api::schema::FederationCapabilities::WORKSPACE_RENAME,
                ),
            );
        }
        let resolved =
            match resolve_remote_workspace_target(&self.state.remote_sources, &host, &selector) {
                Ok(resolved) => resolved,
                Err(err) => return encode_error_body(id, remote_workspace_resolve_error_body(err)),
            };
        let request = Request {
            id: id.clone(),
            method: Method::WorkspaceRename(WorkspaceRenameParams {
                workspace_id: resolved.workspace.workspace_id.clone(),
                label: params.label,
            }),
        };
        let response_value = match send(&resolved.host, &request)
            .and_then(|response| rewrite_remote_response_id_value(&response, &id))
        {
            Ok(value) => value,
            Err(err) => return encode_error(id, "remote_request_failed", err.to_string()),
        };
        if let Ok(success) = serde_json::from_value::<SuccessResponse>(response_value.clone()) {
            if let ResponseResult::WorkspaceInfo { workspace } = success.result {
                self.state
                    .remote_sources
                    .upsert_workspace(host_key, workspace);
            }
        }
        serde_json::to_string(&response_value)
            .unwrap_or_else(|err| encode_error(id, "remote_request_failed", err.to_string()))
    }

    pub(super) fn handle_workspace_rename(
        &mut self,
        id: String,
        params: WorkspaceRenameParams,
    ) -> String {
        match self.plan_workspace_target_remote_route(&params.workspace_id.clone()) {
            Ok(Some((host, selector))) => {
                return self.handle_remote_workspace_rename(id, host, selector, params);
            }
            Ok(None) => {}
            Err(err) => return encode_error_body(id, remote_route_plan_error_body(err)),
        }
        let Some(index) = self.parse_workspace_id(&params.workspace_id) else {
            return workspace_not_found(id, &params.workspace_id);
        };
        let Some(ws) = self.state.workspaces.get_mut(index) else {
            return workspace_not_found(id, &params.workspace_id);
        };
        ws.set_custom_name(params.label.clone());
        crate::logging::workspace_renamed(&ws.id);
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::WorkspaceRenamed,
            data: EventData::WorkspaceRenamed {
                workspace_id: self.public_workspace_id(index),
                label: params.label,
            },
        });

        encode_success(
            id,
            ResponseResult::WorkspaceInfo {
                workspace: self.workspace_info(index),
            },
        )
    }

    pub(super) fn handle_workspace_move(
        &mut self,
        id: String,
        params: WorkspaceMoveParams,
    ) -> String {
        let Some(index) = self.parse_workspace_id(&params.workspace_id) else {
            return workspace_not_found(id, &params.workspace_id);
        };
        if self.state.workspaces.get(index).is_none() {
            return workspace_not_found(id, &params.workspace_id);
        }
        if params.insert_index > self.state.workspaces.len() {
            return encode_error(
                id,
                "workspace_move_failed",
                format!("insert_index {} is out of bounds", params.insert_index),
            );
        }

        let workspace_id = self.public_workspace_id(index);
        let insert_index = params.insert_index;
        let moved = self.state.move_workspace(index, insert_index);
        let workspaces = self.workspace_list_info();
        if moved {
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceMoved,
                data: EventData::WorkspaceMoved {
                    workspace_id,
                    insert_index,
                    workspaces: workspaces.clone(),
                },
            });
        }

        encode_success(id, ResponseResult::WorkspaceList { workspaces })
    }

    pub(super) fn handle_workspace_close(&mut self, id: String, target: WorkspaceTarget) -> String {
        let Some(index) = self.parse_workspace_id(&target.workspace_id) else {
            return workspace_not_found(id, &target.workspace_id);
        };
        if self.state.workspaces.get(index).is_none() {
            return workspace_not_found(id, &target.workspace_id);
        }
        let workspace_id = self.public_workspace_id(index);
        let workspace = self.workspace_info(index);
        let pane_ids = self
            .state
            .workspaces
            .get(index)
            .map(|ws| {
                ws.tabs
                    .iter()
                    .flat_map(|tab| tab.layout.pane_ids())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.state.selected = index;
        self.state.close_selected_workspace();
        for pane_id in pane_ids {
            self.state.plugin_panes.remove(&pane_id);
        }
        self.shutdown_detached_terminal_runtimes();
        self.emit_event(EventEnvelope {
            event: EventKind::WorkspaceClosed,
            data: EventData::WorkspaceClosed {
                workspace_id,
                workspace: Some(workspace),
            },
        });

        encode_success(id, ResponseResult::Ok {})
    }

    fn workspace_list_info(&self) -> Vec<crate::api::schema::WorkspaceInfo> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .map(|(idx, _)| self.workspace_info(idx))
            .collect()
    }
}

fn workspace_not_found(id: String, workspace_id: &str) -> String {
    encode_error(
        id,
        "workspace_not_found",
        format!("workspace {workspace_id} not found"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{api::schema::SuccessResponse, config::Config, workspace::Workspace};

    fn app_with_linked_worktree() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("issue")];
        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });
        app
    }

    #[test]
    fn api_workspace_close_closes_linked_worktree_workspace_only() {
        let mut app = app_with_linked_worktree();

        let response = app.handle_workspace_close(
            "req".into(),
            WorkspaceTarget {
                workspace_id: app.state.workspaces[0].id.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(app.state.request_remove_linked_worktree, None);
        assert!(app.state.workspaces.is_empty());
    }

    #[test]
    fn api_workspace_list_local_returns_authoritative_local_workspaces() {
        let mut app = app_with_linked_worktree();

        let response = app.handle_workspace_list_local("req".into());

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        let ResponseResult::WorkspaceList { workspaces } = success.result else {
            panic!("expected workspace list");
        };
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].label, "issue");
        assert_eq!(workspaces[0].workspace_id, app.state.workspaces[0].id);
    }

    #[test]
    fn api_workspace_close_event_includes_final_worktree_snapshot() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = app_with_linked_worktree().state.workspaces;
        let workspace_id = app.state.workspaces[0].id.clone();

        let response = app.handle_workspace_close(
            "req".into(),
            WorkspaceTarget {
                workspace_id: workspace_id.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::WorkspaceClosed {
                    workspace_id: closed_id,
                    workspace: Some(workspace),
                } if closed_id == &workspace_id
                    && workspace
                        .worktree
                        .as_ref()
                        .is_some_and(|worktree| worktree.is_linked_worktree)
            )
        }));
    }

    #[test]
    fn api_workspace_move_reorders_workspaces() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
        ];
        app.state.active = Some(0);
        app.state.selected = 0;
        let moved_id = app.public_workspace_id(0);

        let response = app.handle_workspace_move(
            "req".into(),
            WorkspaceMoveParams {
                workspace_id: moved_id.clone(),
                insert_index: 3,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorkspaceList { workspaces } = success.result else {
            panic!("expected workspace list");
        };
        assert_eq!(workspaces[2].workspace_id, moved_id);
        assert_eq!(app.state.workspaces[2].display_name(), "one");
        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::WorkspaceMoved {
                    workspace_id,
                    insert_index: 3,
                    workspaces,
                } if workspace_id == &moved_id
                    && workspaces[2].workspace_id == moved_id
            )
        }));
    }

    #[test]
    fn api_workspace_move_noop_does_not_emit_event() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        let moved_id = app.public_workspace_id(0);

        let response = app.handle_workspace_move(
            "req".into(),
            WorkspaceMoveParams {
                workspace_id: moved_id.clone(),
                insert_index: 1,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorkspaceList { workspaces } = success.result else {
            panic!("expected workspace list");
        };
        assert_eq!(workspaces[0].workspace_id, moved_id);
        assert!(event_hub.events_after(0).is_empty());
    }

    fn config_with_remote_host() -> Config {
        let mut config = Config::default();
        config.remote.enabled = true;
        config.remote.hosts = vec![crate::remote_target::RemoteHostConfig::new(
            "jafar", "jafar", "default", true,
        )];
        config
    }

    fn seed_remote_workspace_metadata(app: &mut App) -> crate::remote_source::RemoteHostKey {
        use crate::api::schema::{AgentStatus, WorkspaceInfo};
        let host_key = crate::remote_source::RemoteHostKey::new("jafar", "default");
        app.state.remote_sources.replace_workspace_snapshot(
            host_key.clone(),
            vec![WorkspaceInfo {
                workspace_id: "remote-ws".to_string(),
                number: 1,
                label: "remote workspace".to_string(),
                focused: false,
                pane_count: 1,
                tab_count: 1,
                active_tab_id: "remote-tab".to_string(),
                agent_status: AgentStatus::Unknown,
                worktree: None,
            }],
        );
        host_key
    }

    #[test]
    fn workspace_rename_without_remote_hosts_keeps_slash_target_local() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );

        let response = app.handle_workspace_rename(
            "req".into(),
            WorkspaceRenameParams {
                workspace_id: "jafar/workspace:remote-ws".to_string(),
                label: "new name".to_string(),
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(error.error.code, "workspace_not_found");
    }

    #[test]
    fn workspace_rename_remote_disconnected_host_rejects_before_send() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &config_with_remote_host(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let host_key = seed_remote_workspace_metadata(&mut app);
        app.state.remote_sources.mark_status(
            &host_key,
            crate::remote_source::RemoteConnectionStatus::Unreachable,
        );
        let host = app.remote_hosts.get("jafar").cloned().unwrap();

        let response = app.handle_remote_workspace_rename_with_sender(
            "req".into(),
            host,
            RemoteTargetSelector::Workspace("remote-ws".to_string()),
            WorkspaceRenameParams {
                workspace_id: "jafar/workspace:remote-ws".to_string(),
                label: "new name".to_string(),
            },
            |_host, _request| panic!("disconnected host must not send a remote request"),
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(error.error.code, "remote_host_not_connected");
        assert!(error.error.message.contains("unreachable"));
    }

    #[test]
    fn workspace_rename_missing_capability_rejects_before_send() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &config_with_remote_host(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        seed_remote_workspace_metadata(&mut app);
        // capabilities default to all-false — workspace_rename is not advertised
        let host = app.remote_hosts.get("jafar").cloned().unwrap();

        let response = app.handle_remote_workspace_rename_with_sender(
            "req".into(),
            host,
            RemoteTargetSelector::Workspace("remote-ws".to_string()),
            WorkspaceRenameParams {
                workspace_id: "jafar/workspace:remote-ws".to_string(),
                label: "new name".to_string(),
            },
            |_host, _request| panic!("missing capability must not send a remote request"),
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(error.error.code, "remote_capability_unavailable");
        assert!(error
            .error
            .message
            .contains(crate::api::schema::FederationCapabilities::WORKSPACE_RENAME));
    }

    #[test]
    fn workspace_rename_missing_metadata_rejects_before_send() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &config_with_remote_host(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        // host is connected but workspace metadata was never seeded
        let host_key = crate::remote_source::RemoteHostKey::new("jafar", "default");
        app.state
            .remote_sources
            .replace_connected_snapshot(host_key.clone(), Vec::new());
        app.state.remote_sources.set_capabilities(
            &host_key,
            crate::remote_source::RemoteSourceCapabilities {
                workspace_rename: true,
                ..Default::default()
            },
        );
        let host = app.remote_hosts.get("jafar").cloned().unwrap();

        let response = app.handle_remote_workspace_rename_with_sender(
            "req".into(),
            host,
            RemoteTargetSelector::Workspace("remote-ws".to_string()),
            WorkspaceRenameParams {
                workspace_id: "jafar/workspace:remote-ws".to_string(),
                label: "new name".to_string(),
            },
            |_host, _request| panic!("missing metadata must not send a remote request"),
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(error.error.code, "remote_workspace_unavailable");
    }

    #[test]
    fn workspace_rename_connected_remote_forwards_raw_workspace_id_and_updates_cache() {
        use crate::api::schema::{AgentStatus, WorkspaceInfo};
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &config_with_remote_host(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let host_key = seed_remote_workspace_metadata(&mut app);
        app.state.remote_sources.set_capabilities(
            &host_key,
            crate::remote_source::RemoteSourceCapabilities {
                workspace_rename: true,
                ..Default::default()
            },
        );
        let host = app.remote_hosts.get("jafar").cloned().unwrap();
        let captured = std::cell::RefCell::new(None::<Request>);

        let renamed_workspace = WorkspaceInfo {
            workspace_id: "remote-ws".to_string(),
            number: 1,
            label: "renamed".to_string(),
            focused: false,
            pane_count: 1,
            tab_count: 1,
            active_tab_id: "remote-tab".to_string(),
            agent_status: AgentStatus::Unknown,
            worktree: None,
        };
        let rename_response = serde_json::to_string(&SuccessResponse {
            id: "remote-req".to_string(),
            result: ResponseResult::WorkspaceInfo {
                workspace: renamed_workspace.clone(),
            },
        })
        .unwrap();

        let response = app.handle_remote_workspace_rename_with_sender(
            "local-req".into(),
            host,
            RemoteTargetSelector::Workspace("remote-ws".to_string()),
            WorkspaceRenameParams {
                workspace_id: "jafar/workspace:remote-ws".to_string(),
                label: "renamed".to_string(),
            },
            |sent_host, request| {
                assert_eq!(sent_host.name, "jafar");
                *captured.borrow_mut() = Some(request.clone());
                Ok(rename_response.clone())
            },
        );

        // Response id is rewritten to local request id
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["id"], "local-req");

        // Forwarded request uses raw workspace id (not host-qualified)
        let request = captured.into_inner().expect("request captured");
        let Method::WorkspaceRename(rename_params) = request.method else {
            panic!("expected workspace.rename");
        };
        assert_eq!(rename_params.workspace_id, "remote-ws");
        assert_eq!(rename_params.label, "renamed");

        // Cache was updated with renamed workspace
        let entries = app
            .state
            .remote_sources
            .workspace_entries_for_host(&host_key)
            .expect("workspace cache");
        assert!(entries
            .iter()
            .any(|e| e.workspace.label == "renamed" && e.workspace.workspace_id == "remote-ws"));
    }
}
