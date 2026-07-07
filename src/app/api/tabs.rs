use std::path::PathBuf;

use crate::api::schema::{
    ErrorBody, EventData, EventEnvelope, EventKind, Method, Request, ResponseResult,
    SuccessResponse, TabCloseParams, TabCreateParams, TabListParams, TabMoveParams,
    TabRenameParams, TabTarget,
};
use crate::app::{App, Mode};

use super::remote_helpers::{remote_route_plan_error_body, rewrite_remote_response_id_value};
use super::responses::{encode_error, encode_error_body, encode_success};
use crate::remote_target::{
    parse_target_route, resolve_remote_tab_target, resolve_remote_workspace_target,
    RemoteTabResolveError, RemoteTargetSelector, RemoteWorkspaceResolveError, TargetRoute,
};

fn remote_tab_create_request(
    id: String,
    mut params: TabCreateParams,
    workspace_id: &str,
) -> Request {
    params.workspace_id = Some(workspace_id.to_string());
    Request {
        id,
        method: Method::TabCreate(params),
    }
}

fn remote_tab_focus_request(id: String, tab_id: &str) -> Request {
    Request {
        id,
        method: Method::TabFocus(TabTarget {
            tab_id: tab_id.to_string(),
        }),
    }
}

fn remote_tab_close_request(id: String, tab_id: &str) -> Request {
    Request {
        id,
        method: Method::TabClose(TabCloseParams {
            tab_id: tab_id.to_string(),
            confirm: true,
        }),
    }
}

fn remote_workspace_resolve_error_body(err: RemoteWorkspaceResolveError) -> ErrorBody {
    let code = match &err {
        RemoteWorkspaceResolveError::NotFound { .. } => "remote_workspace_not_found",
        RemoteWorkspaceResolveError::MetadataUnavailable { .. } => "remote_workspace_unavailable",
        RemoteWorkspaceResolveError::UnsupportedSelector { .. } => "remote_target_error",
    };
    ErrorBody {
        code: code.to_string(),
        message: err.to_string(),
    }
}

fn remote_tab_resolve_error_body(err: RemoteTabResolveError) -> ErrorBody {
    let code = match &err {
        RemoteTabResolveError::NotFound { .. } => "remote_tab_not_found",
        RemoteTabResolveError::MetadataUnavailable { .. } => "remote_tab_metadata_unavailable",
        RemoteTabResolveError::MetadataStale { .. } => "remote_tab_metadata_stale",
        RemoteTabResolveError::UnsupportedSelector { .. } => "remote_target_error",
    };
    ErrorBody {
        code: code.to_string(),
        message: err.to_string(),
    }
}

fn remote_host_not_connected_body(host: &str, status: String, noun: &str) -> ErrorBody {
    ErrorBody {
        code: "remote_host_not_connected".to_string(),
        message: format!(
            "remote host {host} is {status}; wait for it to reconnect before mutating a remote {noun}"
        ),
    }
}

fn remote_capability_unavailable_body(host: &str, method: &str) -> ErrorBody {
    ErrorBody {
        code: "remote_capability_unavailable".to_string(),
        message: format!("remote host {host} does not advertise federation method {method}"),
    }
}

fn parse_remote_success_response_value(value: serde_json::Value) -> Option<SuccessResponse> {
    serde_json::from_value(value).ok()
}

impl App {
    fn configured_remote_route_for_target(
        &self,
        target: &str,
    ) -> Result<
        Option<(crate::remote_target::RemoteHostConfig, RemoteTargetSelector)>,
        crate::remote_target::RemoteRoutePlanError,
    > {
        if self.remote_hosts.list().is_empty() {
            return Ok(None);
        }
        let Some((host_alias, _)) = target.split_once('/') else {
            return Ok(None);
        };
        let Some(host) = self.remote_hosts.get(host_alias).cloned() else {
            return Ok(None);
        };
        match parse_target_route(target)? {
            TargetRoute::Local { .. } => Ok(None),
            TargetRoute::Remote { target, .. } => Ok(Some((host, target))),
        }
    }

    fn plan_tab_create_remote_route(
        &self,
        params: &TabCreateParams,
    ) -> Result<
        Option<(crate::remote_target::RemoteHostConfig, RemoteTargetSelector)>,
        crate::remote_target::RemoteRoutePlanError,
    > {
        let Some(workspace_id) = params.workspace_id.as_deref() else {
            return Ok(None);
        };
        self.configured_remote_route_for_target(workspace_id)
    }

    fn plan_tab_target_remote_route(
        &self,
        tab_id: &str,
    ) -> Result<
        Option<(crate::remote_target::RemoteHostConfig, RemoteTargetSelector)>,
        crate::remote_target::RemoteRoutePlanError,
    > {
        self.configured_remote_route_for_target(tab_id)
    }

    fn remote_host_connected_or_error(
        &self,
        host: &crate::remote_target::RemoteHostConfig,
        noun: &str,
    ) -> Result<crate::remote_source::RemoteHostKey, ErrorBody> {
        let host_key =
            crate::remote_source::RemoteHostKey::new(host.name.clone(), host.session.clone());
        let host_status = self.state.remote_sources.host_status(&host_key);
        if host_status.is_some_and(|status| status.is_connected()) {
            Ok(host_key)
        } else {
            let status = host_status
                .and_then(|status| status.stale_label())
                .unwrap_or("disconnected")
                .to_string();
            Err(remote_host_not_connected_body(&host.name, status, noun))
        }
    }

    fn handle_remote_tab_create(
        &mut self,
        id: String,
        host: crate::remote_target::RemoteHostConfig,
        selector: RemoteTargetSelector,
        params: TabCreateParams,
    ) -> String {
        self.handle_remote_tab_create_with_sender(
            id,
            host,
            selector,
            params,
            crate::remote::send_remote_api_request_to_host_noninteractive,
        )
    }

    fn handle_remote_tab_create_with_sender<F>(
        &mut self,
        id: String,
        host: crate::remote_target::RemoteHostConfig,
        selector: RemoteTargetSelector,
        params: TabCreateParams,
        mut send: F,
    ) -> String
    where
        F: FnMut(&crate::remote_target::RemoteHostConfig, &Request) -> std::io::Result<String>,
    {
        let host_key = match self.remote_host_connected_or_error(&host, "tab") {
            Ok(host_key) => host_key,
            Err(err) => return encode_error_body(id, err),
        };
        if !self
            .state
            .remote_sources
            .host_capabilities(&host_key)
            .tab_create
        {
            return encode_error_body(
                id,
                remote_capability_unavailable_body(
                    &host.name,
                    crate::api::schema::FederationCapabilities::TAB_CREATE,
                ),
            );
        }
        let resolved =
            match resolve_remote_workspace_target(&self.state.remote_sources, &host, &selector) {
                Ok(resolved) => resolved,
                Err(err) => return encode_error_body(id, remote_workspace_resolve_error_body(err)),
            };
        let workspace_id = resolved.workspace.workspace_id.clone();
        let request = remote_tab_create_request(id.clone(), params, &workspace_id);
        let response_value = match send(&resolved.host, &request)
            .and_then(|response| rewrite_remote_response_id_value(&response, &id))
        {
            Ok(value) => value,
            Err(err) => return encode_error(id, "remote_request_failed", err.to_string()),
        };
        if let Some(success) = parse_remote_success_response_value(response_value.clone()) {
            if let ResponseResult::TabCreated { tab, .. } = success.result {
                self.state.remote_sources.upsert_tab(&host_key, tab.clone());
                self.refresh_remote_workspace_tabs_and_projection(
                    &resolved.host,
                    &host_key,
                    &tab.workspace_id,
                    Some(&tab.tab_id),
                    &mut send,
                );
            }
        }
        serde_json::to_string(&response_value)
            .unwrap_or_else(|err| encode_error(id, "remote_request_failed", err.to_string()))
    }

    fn handle_remote_tab_focus(
        &mut self,
        id: String,
        host: crate::remote_target::RemoteHostConfig,
        selector: RemoteTargetSelector,
    ) -> String {
        self.handle_remote_tab_focus_with_sender(
            id,
            host,
            selector,
            crate::remote::send_remote_api_request_to_host_noninteractive,
        )
    }

    fn handle_remote_tab_focus_with_sender<F>(
        &mut self,
        id: String,
        host: crate::remote_target::RemoteHostConfig,
        selector: RemoteTargetSelector,
        mut send: F,
    ) -> String
    where
        F: FnMut(&crate::remote_target::RemoteHostConfig, &Request) -> std::io::Result<String>,
    {
        let host_key = match self.remote_host_connected_or_error(&host, "tab") {
            Ok(host_key) => host_key,
            Err(err) => return encode_error_body(id, err),
        };
        if !self
            .state
            .remote_sources
            .host_capabilities(&host_key)
            .tab_focus
        {
            return encode_error_body(
                id,
                remote_capability_unavailable_body(
                    &host.name,
                    crate::api::schema::FederationCapabilities::TAB_FOCUS,
                ),
            );
        }
        let resolved = match resolve_remote_tab_target(&self.state.remote_sources, &host, &selector)
        {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, remote_tab_resolve_error_body(err)),
        };
        let request = remote_tab_focus_request(id.clone(), &resolved.tab.tab_id);
        let response_value = match send(&resolved.host, &request)
            .and_then(|response| rewrite_remote_response_id_value(&response, &id))
        {
            Ok(value) => value,
            Err(err) => return encode_error(id, "remote_request_failed", err.to_string()),
        };
        if let Some(success) = parse_remote_success_response_value(response_value.clone()) {
            if let ResponseResult::TabInfo { tab } = success.result {
                self.state.remote_sources.upsert_tab(&host_key, tab.clone());
                self.refresh_remote_workspace_tabs_and_projection(
                    &resolved.host,
                    &host_key,
                    &tab.workspace_id,
                    Some(&tab.tab_id),
                    &mut send,
                );
            }
        }
        serde_json::to_string(&response_value)
            .unwrap_or_else(|err| encode_error(id, "remote_request_failed", err.to_string()))
    }

    fn handle_remote_tab_close(
        &mut self,
        id: String,
        host: crate::remote_target::RemoteHostConfig,
        selector: RemoteTargetSelector,
    ) -> String {
        self.handle_remote_tab_close_with_sender(
            id,
            host,
            selector,
            crate::remote::send_remote_api_request_to_host_noninteractive,
        )
    }

    fn handle_remote_tab_close_with_sender<F>(
        &mut self,
        id: String,
        host: crate::remote_target::RemoteHostConfig,
        selector: RemoteTargetSelector,
        mut send: F,
    ) -> String
    where
        F: FnMut(&crate::remote_target::RemoteHostConfig, &Request) -> std::io::Result<String>,
    {
        let host_key = match self.remote_host_connected_or_error(&host, "tab") {
            Ok(host_key) => host_key,
            Err(err) => return encode_error_body(id, err),
        };
        if !self
            .state
            .remote_sources
            .host_capabilities(&host_key)
            .tab_close
        {
            return encode_error_body(
                id,
                remote_capability_unavailable_body(
                    &host.name,
                    crate::api::schema::FederationCapabilities::TAB_CLOSE,
                ),
            );
        }
        let resolved = match resolve_remote_tab_target(&self.state.remote_sources, &host, &selector)
        {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, remote_tab_resolve_error_body(err)),
        };
        let workspace_id = resolved.workspace_id.clone();
        let tab_id = resolved.tab.tab_id.clone();
        let request = remote_tab_close_request(id.clone(), &tab_id);
        let response_value = match send(&resolved.host, &request)
            .and_then(|response| rewrite_remote_response_id_value(&response, &id))
        {
            Ok(value) => value,
            Err(err) => return encode_error(id, "remote_request_failed", err.to_string()),
        };
        if parse_remote_success_response_value(response_value.clone()).is_some() {
            self.state.remote_sources.remove_tab(&host_key, &tab_id);
            self.state
                .remote_sources
                .mark_tab_snapshot_unavailable(&host_key, &workspace_id);
            self.refresh_remote_workspace_tabs_and_projection(
                &resolved.host,
                &host_key,
                &workspace_id,
                None,
                &mut send,
            );
        }
        serde_json::to_string(&response_value)
            .unwrap_or_else(|err| encode_error(id, "remote_request_failed", err.to_string()))
    }

    fn refresh_remote_workspace_tabs_and_projection<F>(
        &mut self,
        host: &crate::remote_target::RemoteHostConfig,
        host_key: &crate::remote_source::RemoteHostKey,
        workspace_id: &str,
        preferred_tab_id: Option<&str>,
        send: &mut F,
    ) where
        F: FnMut(&crate::remote_target::RemoteHostConfig, &Request) -> std::io::Result<String>,
    {
        let capabilities = self.state.remote_sources.host_capabilities(host_key);
        if !capabilities.tab_list {
            self.state
                .remote_sources
                .mark_tab_snapshot_unavailable(host_key, workspace_id);
            return;
        }

        let tabs = match send(
            host,
            &crate::remote_supervisor::tab_list_request(workspace_id),
        )
        .and_then(|response| crate::remote_supervisor::parse_tab_list_response(&response))
        {
            Ok(tabs) => {
                self.state.remote_sources.replace_tab_snapshot(
                    host_key,
                    workspace_id,
                    tabs.clone(),
                );
                tabs
            }
            Err(_) => {
                self.state
                    .remote_sources
                    .mark_tab_snapshot_unavailable(host_key, workspace_id);
                self.state.remote_sources.upsert_projection_snapshot(
                    host_key,
                    crate::remote_source::RemoteProjectionSnapshot {
                        workspace_id: workspace_id.to_string(),
                        tab_id: preferred_tab_id.map(str::to_string),
                        tab_label: None,
                        status: crate::remote_source::RemoteProjectionStatus::Unavailable,
                        layout: None,
                    },
                );
                return;
            }
        };

        let active_tab = tabs
            .iter()
            .find(|tab| tab.focused)
            .or_else(|| {
                preferred_tab_id
                    .and_then(|preferred| tabs.iter().find(|tab| tab.tab_id == preferred))
            })
            .or_else(|| tabs.first());
        let Some(active_tab) = active_tab else {
            self.state.remote_sources.upsert_projection_snapshot(
                host_key,
                crate::remote_source::RemoteProjectionSnapshot {
                    workspace_id: workspace_id.to_string(),
                    tab_id: None,
                    tab_label: None,
                    status: crate::remote_source::RemoteProjectionStatus::Unavailable,
                    layout: None,
                },
            );
            return;
        };

        if !capabilities.layout_export {
            self.state.remote_sources.upsert_projection_snapshot(
                host_key,
                crate::remote_source::RemoteProjectionSnapshot {
                    workspace_id: workspace_id.to_string(),
                    tab_id: Some(active_tab.tab_id.clone()),
                    tab_label: Some(active_tab.label.clone()),
                    status: crate::remote_source::RemoteProjectionStatus::Unavailable,
                    layout: None,
                },
            );
            return;
        }

        let layout = send(
            host,
            &crate::remote_supervisor::layout_export_request(&active_tab.tab_id),
        )
        .and_then(
            |response| match serde_json::from_str::<SuccessResponse>(&response) {
                Ok(SuccessResponse {
                    result: ResponseResult::LayoutExport { layout },
                    ..
                }) => Ok(layout),
                Ok(other) => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("expected layout.export response, got {:?}", other.result),
                )),
                Err(err) => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid remote API response JSON: {err}"),
                )),
            },
        );
        let (status, layout) = match layout {
            Ok(layout) => (
                crate::remote_source::RemoteProjectionStatus::Available,
                Some(layout),
            ),
            Err(_) => (
                crate::remote_source::RemoteProjectionStatus::Unavailable,
                None,
            ),
        };
        self.state.remote_sources.upsert_projection_snapshot(
            host_key,
            crate::remote_source::RemoteProjectionSnapshot {
                workspace_id: workspace_id.to_string(),
                tab_id: Some(active_tab.tab_id.clone()),
                tab_label: Some(active_tab.label.clone()),
                status,
                layout,
            },
        );
    }

    pub(super) fn handle_tab_list(&mut self, id: String, params: TabListParams) -> String {
        let tabs = if let Some(workspace_id) = params.workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                return workspace_not_found(id, &workspace_id);
            };
            let Some(_) = self.state.workspaces.get(ws_idx) else {
                return workspace_not_found(id, &workspace_id);
            };
            self.tab_list_info(ws_idx)
        } else {
            let mut tabs = Vec::new();
            for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
                for tab_idx in 0..ws.tabs.len() {
                    if let Some(tab) = self.tab_info(ws_idx, tab_idx) {
                        tabs.push(tab);
                    }
                }
            }
            tabs
        };

        encode_success(id, ResponseResult::TabList { tabs })
    }

    pub(super) fn handle_tab_get(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
            return tab_not_found(id, &target.tab_id);
        };

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_create(&mut self, id: String, params: TabCreateParams) -> String {
        match self.plan_tab_create_remote_route(&params) {
            Ok(Some((host, selector))) => {
                return self.handle_remote_tab_create(id, host, selector, params);
            }
            Ok(None) => {}
            Err(err) => return encode_error_body(id, remote_route_plan_error_body(err)),
        }

        let TabCreateParams {
            workspace_id,
            cwd,
            focus,
            label,
            env,
        } = params;
        let ws_idx = if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                return workspace_not_found(id, &workspace_id);
            };
            ws_idx
        } else if let Some(active) = self.state.active {
            active
        } else {
            return encode_error(id, "workspace_not_found", "no active workspace");
        };
        let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| {
            self.resolve_new_terminal_cwd(self.focused_pane_cwd_in_workspace(ws_idx))
        });
        let (rows, cols) = self.state.estimate_pane_size();
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let extra_env = match super::env::normalize_launch_env(env) {
            Ok(env) => env,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        let result = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .ok_or_else(|| std::io::Error::other("workspace disappeared"))
            .and_then(|ws| {
                ws.create_tab(
                    rows,
                    cols,
                    cwd,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode),
                    extra_env,
                )
            });
        match result {
            Ok((tab_idx, terminal, runtime)) => {
                self.terminal_runtimes.insert(terminal.id.clone(), runtime);
                self.state.terminals.insert(terminal.id.clone(), terminal);
                self.state.remove_alias_shadowed_by_new_pane(
                    self.state.workspaces[ws_idx].tabs[tab_idx].root_pane,
                );
                if let Some(label) = label {
                    let workspace_id = self.state.workspaces[ws_idx].id.clone();
                    let tab_id = self.public_tab_id(ws_idx, tab_idx).unwrap_or_else(|| {
                        crate::workspace::public_tab_id_for_number(&workspace_id, tab_idx + 1)
                    });
                    if let Some(tab) = self
                        .state
                        .workspaces
                        .get_mut(ws_idx)
                        .and_then(|ws| ws.tabs.get_mut(tab_idx))
                    {
                        tab.set_custom_name(label);
                        crate::logging::tab_renamed(&workspace_id, &tab_id);
                    }
                }
                if focus {
                    self.state.switch_workspace_tab(ws_idx, tab_idx);
                    self.state.mode = Mode::Terminal;
                }
                self.schedule_session_save();
                self.emit_tab_created_events(ws_idx, tab_idx);
                encode_success(
                    id,
                    self.tab_created_result(ws_idx, tab_idx)
                        .expect("new tab should produce a complete create response"),
                )
            }
            Err(err) => encode_error(id, "tab_create_failed", err.to_string()),
        }
    }

    pub(super) fn handle_tab_focus(&mut self, id: String, target: TabTarget) -> String {
        match self.plan_tab_target_remote_route(&target.tab_id) {
            Ok(Some((host, selector))) => {
                return self.handle_remote_tab_focus(id, host, selector);
            }
            Ok(None) => {}
            Err(err) => return encode_error_body(id, remote_route_plan_error_body(err)),
        }

        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        self.state.switch_workspace_tab(ws_idx, tab_idx);
        let tab = self.tab_info(ws_idx, tab_idx).unwrap();

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    fn handle_remote_tab_rename(
        &mut self,
        id: String,
        host: crate::remote_target::RemoteHostConfig,
        selector: RemoteTargetSelector,
        params: TabRenameParams,
    ) -> String {
        self.handle_remote_tab_rename_with_sender(
            id,
            host,
            selector,
            params,
            crate::remote::send_remote_api_request_to_host_noninteractive,
        )
    }

    fn handle_remote_tab_rename_with_sender<F>(
        &mut self,
        id: String,
        host: crate::remote_target::RemoteHostConfig,
        selector: RemoteTargetSelector,
        params: TabRenameParams,
        mut send: F,
    ) -> String
    where
        F: FnMut(&crate::remote_target::RemoteHostConfig, &Request) -> std::io::Result<String>,
    {
        let host_key = match self.remote_host_connected_or_error(&host, "tab") {
            Ok(host_key) => host_key,
            Err(err) => return encode_error_body(id, err),
        };
        if !self
            .state
            .remote_sources
            .host_capabilities(&host_key)
            .tab_rename
        {
            return encode_error_body(
                id,
                remote_capability_unavailable_body(
                    &host.name,
                    crate::api::schema::FederationCapabilities::TAB_RENAME,
                ),
            );
        }
        let resolved = match resolve_remote_tab_target(&self.state.remote_sources, &host, &selector)
        {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, remote_tab_resolve_error_body(err)),
        };
        let tab_id = resolved.tab.tab_id.clone();
        let workspace_id = resolved.workspace_id.clone();
        let request = Request {
            id: id.clone(),
            method: Method::TabRename(TabRenameParams {
                tab_id: tab_id.clone(),
                label: params.label,
            }),
        };
        let response_value = match send(&resolved.host, &request)
            .and_then(|response| rewrite_remote_response_id_value(&response, &id))
        {
            Ok(value) => value,
            Err(err) => return encode_error(id, "remote_request_failed", err.to_string()),
        };
        if let Some(success) = parse_remote_success_response_value(response_value.clone()) {
            if let ResponseResult::TabInfo { tab } = success.result {
                self.state.remote_sources.upsert_tab(&host_key, tab);
                self.refresh_remote_workspace_tabs_and_projection(
                    &resolved.host,
                    &host_key,
                    &workspace_id,
                    Some(&tab_id),
                    &mut send,
                );
            }
        }
        serde_json::to_string(&response_value)
            .unwrap_or_else(|err| encode_error(id, "remote_request_failed", err.to_string()))
    }

    pub(super) fn handle_tab_rename(&mut self, id: String, params: TabRenameParams) -> String {
        match self.plan_tab_target_remote_route(&params.tab_id) {
            Ok(Some((host, selector))) => {
                return self.handle_remote_tab_rename(id, host, selector, params);
            }
            Ok(None) => {}
            Err(err) => return encode_error_body(id, remote_route_plan_error_body(err)),
        }
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let workspace_id = self.state.workspaces[ws_idx].id.clone();
        let tab_id = self.public_tab_id(ws_idx, tab_idx).unwrap_or_else(|| {
            crate::workspace::public_tab_id_for_number(&workspace_id, tab_idx + 1)
        });
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return tab_not_found(id, &params.tab_id);
        };
        tab.set_custom_name(params.label.clone());
        crate::logging::tab_renamed(&workspace_id, &tab_id);
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::TabRenamed,
            data: EventData::TabRenamed {
                tab_id: self.public_tab_id(ws_idx, tab_idx).unwrap(),
                workspace_id: self.public_workspace_id(ws_idx),
                label: params.label,
            },
        });
        let tab = self.tab_info(ws_idx, tab_idx).unwrap();

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_move(&mut self, id: String, params: TabMoveParams) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return tab_not_found(id, &params.tab_id);
        };
        if params.insert_index > ws.tabs.len() {
            return encode_error(
                id,
                "tab_move_failed",
                format!("insert_index {} is out of bounds", params.insert_index),
            );
        }

        let tab_id = self
            .public_tab_id(ws_idx, tab_idx)
            .unwrap_or_else(|| crate::workspace::public_tab_id_for_number(&ws.id, tab_idx + 1));
        let workspace_id = self.public_workspace_id(ws_idx);
        let insert_index = params.insert_index;
        let moved = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .is_some_and(|ws| ws.move_tab(tab_idx, insert_index));
        let tabs = self.tab_list_info(ws_idx);
        if moved {
            self.schedule_session_save();
            if self.state.active == Some(ws_idx) {
                self.state.tab_scroll_follow_active = true;
                self.state.refresh_tab_bar_view();
            }
            self.emit_event(EventEnvelope {
                event: EventKind::TabMoved,
                data: EventData::TabMoved {
                    tab_id,
                    workspace_id,
                    insert_index,
                    tabs: tabs.clone(),
                },
            });
        }

        encode_success(id, ResponseResult::TabList { tabs })
    }

    pub(super) fn handle_tab_close(&mut self, id: String, target: TabCloseParams) -> String {
        match self.plan_tab_target_remote_route(&target.tab_id) {
            Ok(Some((host, selector))) => {
                if !target.confirm {
                    return encode_error(
                        id,
                        "confirmation_required",
                        format!(
                            "tab.close on remote target {} is destructive; pass confirm=true to proceed",
                            target.tab_id
                        ),
                    );
                }
                return self.handle_remote_tab_close(id, host, selector);
            }
            Ok(None) => {}
            Err(err) => return encode_error_body(id, remote_route_plan_error_body(err)),
        }

        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return tab_not_found(id, &target.tab_id);
        };
        let workspace_id = self.public_workspace_id(ws_idx);
        let terminal_ids = self.state.terminal_ids_for_tab(ws_idx, tab_idx);
        let pane_ids = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
            .map(|tab| tab.layout.pane_ids())
            .unwrap_or_default();
        let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
            return tab_not_found(id, &target.tab_id);
        };
        if ws.tabs.len() <= 1 {
            return encode_error(
                id,
                "tab_close_failed",
                "cannot close the last tab in a workspace",
            );
        }
        if !ws.close_tab(tab_idx) {
            return encode_error(
                id,
                "tab_close_failed",
                format!("tab {} could not be closed", target.tab_id),
            );
        }
        for pane_id in pane_ids {
            self.state.plugin_panes.remove(&pane_id);
        }
        self.state.remove_unattached_terminal_ids(terminal_ids);
        self.shutdown_detached_terminal_runtimes();
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::TabClosed,
            data: EventData::TabClosed {
                tab_id,
                workspace_id,
            },
        });

        encode_success(id, ResponseResult::Ok {})
    }

    fn tab_list_info(&self, ws_idx: usize) -> Vec<crate::api::schema::TabInfo> {
        self.state
            .workspaces
            .get(ws_idx)
            .map(|ws| {
                (0..ws.tabs.len())
                    .filter_map(|idx| self.tab_info(ws_idx, idx))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn workspace_not_found(id: String, workspace_id: &str) -> String {
    encode_error(
        id,
        "workspace_not_found",
        format!("workspace {workspace_id} not found"),
    )
}

fn tab_not_found(id: String, tab_id: &str) -> String {
    encode_error(id, "tab_not_found", format!("tab {tab_id} not found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::schema::{
            AgentStatus, ErrorResponse, LayoutDescription, LayoutNode, LayoutPane, PaneInfo,
            SuccessResponse, TabInfo, WorkspaceInfo,
        },
        config::{Config, ShellModeConfig},
        remote_source::{RemoteConnectionStatus, RemoteHostKey},
        workspace::Workspace,
    };

    #[cfg(windows)]
    fn exiting_test_command() -> &'static str {
        "C:\\Windows\\System32\\whoami.exe"
    }

    #[cfg(not(windows))]
    fn exiting_test_command() -> &'static str {
        "/usr/bin/true"
    }

    fn shutdown_test_runtimes(app: &mut App) {
        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
    }

    fn config_with_remote_host() -> Config {
        let mut config = Config::default();
        config.remote.enabled = true;
        config.remote.hosts = vec![crate::remote_target::RemoteHostConfig::new(
            "jafar", "jafar", "default", true,
        )];
        config
    }

    fn app_with_remote_host() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &config_with_remote_host(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn remote_workspace() -> WorkspaceInfo {
        WorkspaceInfo {
            workspace_id: "remote-ws".to_string(),
            number: 1,
            label: "remote".to_string(),
            focused: true,
            pane_count: 1,
            tab_count: 2,
            active_tab_id: "remote-tab-1".to_string(),
            agent_status: AgentStatus::Unknown,
            worktree: None,
        }
    }

    fn remote_tab(tab_id: &str, focused: bool) -> TabInfo {
        TabInfo {
            tab_id: tab_id.to_string(),
            workspace_id: "remote-ws".to_string(),
            number: if tab_id.ends_with('1') { 1 } else { 2 },
            label: tab_id.to_string(),
            focused,
            pane_count: 1,
            agent_status: AgentStatus::Unknown,
        }
    }

    fn remote_layout(tab_id: &str) -> LayoutDescription {
        LayoutDescription {
            workspace_id: "remote-ws".to_string(),
            tab_id: tab_id.to_string(),
            zoomed: false,
            focused_pane_id: "remote-pane".to_string(),
            root: LayoutNode::Pane {
                pane: LayoutPane {
                    pane_id: Some("remote-pane".to_string()),
                    terminal_id: Some("remote-term".to_string()),
                    ..Default::default()
                },
            },
        }
    }

    fn remote_tab_created_response(id: &str, tab: TabInfo) -> String {
        serde_json::to_string(&SuccessResponse {
            id: id.to_string(),
            result: ResponseResult::TabCreated {
                tab,
                root_pane: PaneInfo {
                    pane_id: "remote-pane".to_string(),
                    terminal_id: "remote-term".to_string(),
                    workspace_id: "remote-ws".to_string(),
                    tab_id: "remote-tab-new".to_string(),
                    focused: true,
                    cwd: None,
                    foreground_cwd: None,
                    label: None,
                    agent: None,
                    title: None,
                    display_agent: None,
                    agent_status: AgentStatus::Unknown,
                    custom_status: None,
                    state_labels: Default::default(),
                    agent_session: None,
                    revision: 1,
                },
            },
        })
        .unwrap()
    }

    fn remote_tab_info_response(id: &str, tab: TabInfo) -> String {
        serde_json::to_string(&SuccessResponse {
            id: id.to_string(),
            result: ResponseResult::TabInfo { tab },
        })
        .unwrap()
    }

    fn remote_ok_response(id: &str) -> String {
        serde_json::to_string(&SuccessResponse {
            id: id.to_string(),
            result: ResponseResult::Ok {},
        })
        .unwrap()
    }

    fn remote_tab_list_response(tabs: Vec<TabInfo>) -> String {
        serde_json::to_string(&SuccessResponse {
            id: "remote-source.tab-list".to_string(),
            result: ResponseResult::TabList { tabs },
        })
        .unwrap()
    }

    fn remote_layout_response(tab_id: &str) -> String {
        serde_json::to_string(&SuccessResponse {
            id: "remote-source.layout-export".to_string(),
            result: ResponseResult::LayoutExport {
                layout: remote_layout(tab_id),
            },
        })
        .unwrap()
    }

    fn seed_remote_tab_cache(app: &mut App) -> RemoteHostKey {
        let host = RemoteHostKey::new("jafar", "default");
        app.state
            .remote_sources
            .replace_connected_snapshot(host.clone(), Vec::new());
        app.state
            .remote_sources
            .replace_workspace_snapshot(host.clone(), vec![remote_workspace()]);
        app.state.remote_sources.replace_tab_snapshot(
            &host,
            "remote-ws",
            vec![
                remote_tab("remote-tab-1", true),
                remote_tab("remote-tab-2", false),
            ],
        );
        app.state.remote_sources.set_capabilities(
            &host,
            crate::remote_source::RemoteSourceCapabilities {
                workspace_list_local: true,
                tab_list: true,
                tab_create: true,
                tab_focus: true,
                tab_close: true,
                layout_export: true,
                ..Default::default()
            },
        );
        host
    }

    fn error_code(response: &str) -> String {
        let error: ErrorResponse = serde_json::from_str(response).unwrap();
        error.error.code
    }

    #[test]
    fn api_tab_move_reorders_tabs_in_target_workspace() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        let mut workspace = Workspace::test_new("tabs");
        workspace.test_add_tab(Some("two"));
        workspace.test_add_tab(Some("three"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        let moved_root = app.state.workspaces[0].tabs[0].root_pane;
        let moved_id = app.public_tab_id(0, 0).unwrap();

        let response = app.handle_tab_move(
            "req".into(),
            TabMoveParams {
                tab_id: moved_id.clone(),
                insert_index: 3,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabList { tabs } = success.result else {
            panic!("expected tab list");
        };
        assert_eq!(app.state.workspaces[0].tabs[2].root_pane, moved_root);
        assert_eq!(tabs[2].tab_id, app.public_tab_id(0, 2).unwrap());
        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::TabMoved {
                    tab_id,
                    workspace_id,
                    insert_index: 3,
                    tabs,
                } if tab_id == &moved_id
                    && workspace_id == &app.public_workspace_id(0)
                    && tabs[2].tab_id == moved_id
            )
        }));
    }

    #[test]
    fn tab_focus_without_remote_hosts_keeps_slash_target_local() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub);

        let response = app.handle_tab_focus(
            "req".into(),
            TabTarget {
                tab_id: "jafar/tab:remote-tab".to_string(),
            },
        );

        assert_eq!(error_code(&response), "tab_not_found");
    }

    #[test]
    fn tab_focus_unknown_host_keeps_slash_target_local() {
        let mut app = app_with_remote_host();
        seed_remote_tab_cache(&mut app);

        let response = app.handle_tab_focus(
            "req".into(),
            TabTarget {
                tab_id: "logs/tab:remote-tab".to_string(),
            },
        );

        assert_eq!(error_code(&response), "tab_not_found");
    }

    #[test]
    fn remote_tab_create_rewrites_workspace_and_passes_remote_fields_through() {
        let mut app = app_with_remote_host();
        let host_key = seed_remote_tab_cache(&mut app);
        let host = app.remote_hosts.get("jafar").unwrap().clone();
        let selector = RemoteTargetSelector::Workspace("remote-ws".to_string());
        let mut calls = Vec::new();
        let response = app.handle_remote_tab_create_with_sender(
            "req".into(),
            host,
            selector,
            TabCreateParams {
                workspace_id: Some("jafar/workspace:remote-ws".to_string()),
                cwd: Some("/remote/project".to_string()),
                focus: true,
                label: Some("shell".to_string()),
                env: [("REMOTE_ONLY".to_string(), "$HOME/not-expanded".to_string())]
                    .into_iter()
                    .collect(),
            },
            |_host, request| {
                calls.push(request.clone());
                match &request.method {
                    Method::TabCreate(params) => {
                        assert_eq!(params.workspace_id.as_deref(), Some("remote-ws"));
                        assert_eq!(params.cwd.as_deref(), Some("/remote/project"));
                        assert_eq!(
                            params.env.get("REMOTE_ONLY").map(String::as_str),
                            Some("$HOME/not-expanded")
                        );
                        Ok(remote_tab_created_response(
                            "remote-create",
                            remote_tab("remote-tab-new", true),
                        ))
                    }
                    Method::TabList(_) => Ok(remote_tab_list_response(vec![remote_tab(
                        "remote-tab-new",
                        true,
                    )])),
                    Method::LayoutExport(_) => Ok(remote_layout_response("remote-tab-new")),
                    other => panic!("unexpected request: {other:?}"),
                }
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert!(matches!(success.result, ResponseResult::TabCreated { .. }));
        assert_eq!(calls.len(), 3);
        let key = crate::remote_source::RemoteSpaceKey {
            host: "jafar".to_string(),
            session: "default".to_string(),
            workspace_id: "remote-ws".to_string(),
        };
        let tabs = app
            .state
            .remote_sources
            .tab_snapshot_for_space(&key)
            .unwrap();
        assert_eq!(tabs.tabs[0].tab_id, "remote-tab-new");
        assert_eq!(
            app.state.remote_sources.host_status(&host_key),
            Some(RemoteConnectionStatus::Connected)
        );
    }

    #[test]
    fn remote_tab_create_does_not_fetch_unadvertised_metadata_refreshes() {
        let mut app = app_with_remote_host();
        let host_key = seed_remote_tab_cache(&mut app);
        app.state.remote_sources.set_capabilities(
            &host_key,
            crate::remote_source::RemoteSourceCapabilities {
                workspace_list_local: true,
                tab_create: true,
                tab_focus: true,
                tab_close: true,
                ..Default::default()
            },
        );
        let host = app.remote_hosts.get("jafar").unwrap().clone();
        let mut calls = 0;

        let response = app.handle_remote_tab_create_with_sender(
            "req".into(),
            host,
            RemoteTargetSelector::Workspace("remote-ws".to_string()),
            TabCreateParams {
                workspace_id: Some("jafar/workspace:remote-ws".to_string()),
                cwd: None,
                focus: true,
                label: None,
                env: Default::default(),
            },
            |_host, request| {
                calls += 1;
                match &request.method {
                    Method::TabCreate(_) => Ok(remote_tab_created_response(
                        "remote-create",
                        remote_tab("remote-tab-new", true),
                    )),
                    other => panic!("unexpected metadata refresh request: {other:?}"),
                }
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(calls, 1);
        let tabs = app
            .state
            .remote_sources
            .tab_snapshot_for_space(&crate::remote_source::RemoteSpaceKey {
                host: "jafar".to_string(),
                session: "default".to_string(),
                workspace_id: "remote-ws".to_string(),
            })
            .unwrap();
        assert_eq!(
            tabs.status,
            crate::remote_source::RemoteProjectionStatus::StaleLastKnown
        );
    }

    #[test]
    fn remote_tab_focus_forwards_raw_tab_id_and_rewrites_response_id() {
        let mut app = app_with_remote_host();
        seed_remote_tab_cache(&mut app);
        let host = app.remote_hosts.get("jafar").unwrap().clone();
        let mut forwarded_tab_id = None;

        let response = app.handle_remote_tab_focus_with_sender(
            "req".into(),
            host,
            RemoteTargetSelector::Tab("remote-tab-2".to_string()),
            |_host, request| match &request.method {
                Method::TabFocus(target) => {
                    forwarded_tab_id = Some(target.tab_id.clone());
                    Ok(remote_tab_info_response(
                        "remote-focus",
                        remote_tab("remote-tab-2", true),
                    ))
                }
                Method::TabList(_) => Ok(remote_tab_list_response(vec![remote_tab(
                    "remote-tab-2",
                    true,
                )])),
                Method::LayoutExport(_) => Ok(remote_layout_response("remote-tab-2")),
                other => panic!("unexpected request: {other:?}"),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(forwarded_tab_id.as_deref(), Some("remote-tab-2"));
    }

    #[test]
    fn remote_tab_close_requires_confirm_before_status_or_cache_resolution() {
        let mut app = app_with_remote_host();

        let response = app.handle_tab_close(
            "req".into(),
            TabCloseParams {
                tab_id: "jafar/tab:remote-tab-1".to_string(),
                confirm: false,
            },
        );

        assert_eq!(error_code(&response), "confirmation_required");
    }

    #[test]
    fn remote_tab_close_stale_metadata_does_not_send() {
        let mut app = app_with_remote_host();
        let host_key = seed_remote_tab_cache(&mut app);
        app.state
            .remote_sources
            .mark_status(&host_key, RemoteConnectionStatus::Disconnected);

        let response = app.handle_tab_close(
            "req".into(),
            TabCloseParams {
                tab_id: "jafar/tab:remote-tab-1".to_string(),
                confirm: true,
            },
        );

        assert_eq!(error_code(&response), "remote_host_not_connected");
    }

    #[test]
    fn remote_tab_close_forwards_confirmed_raw_id_and_refreshes_cache() {
        let mut app = app_with_remote_host();
        seed_remote_tab_cache(&mut app);
        let host = app.remote_hosts.get("jafar").unwrap().clone();
        let mut forwarded_confirm = None;

        let response = app.handle_remote_tab_close_with_sender(
            "req".into(),
            host,
            RemoteTargetSelector::Tab("remote-tab-2".to_string()),
            |_host, request| match &request.method {
                Method::TabClose(params) => {
                    forwarded_confirm = Some((params.tab_id.clone(), params.confirm));
                    Ok(remote_ok_response("remote-close"))
                }
                Method::TabList(_) => Ok(remote_tab_list_response(vec![remote_tab(
                    "remote-tab-1",
                    true,
                )])),
                Method::LayoutExport(_) => Ok(remote_layout_response("remote-tab-1")),
                other => panic!("unexpected request: {other:?}"),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(forwarded_confirm, Some(("remote-tab-2".to_string(), true)));
        let key = crate::remote_source::RemoteSpaceKey {
            host: "jafar".to_string(),
            session: "default".to_string(),
            workspace_id: "remote-ws".to_string(),
        };
        let tabs = app
            .state
            .remote_sources
            .tab_snapshot_for_space(&key)
            .unwrap();
        assert_eq!(
            tabs.tabs
                .iter()
                .map(|tab| tab.tab_id.as_str())
                .collect::<Vec<_>>(),
            vec!["remote-tab-1"]
        );
    }

    #[test]
    fn remote_tab_request_failure_surfaces_remote_request_failed() {
        let mut app = app_with_remote_host();
        seed_remote_tab_cache(&mut app);
        let host = app.remote_hosts.get("jafar").unwrap().clone();

        let response = app.handle_remote_tab_focus_with_sender(
            "req".into(),
            host,
            RemoteTargetSelector::Tab("remote-tab-1".to_string()),
            |_host, request| match &request.method {
                Method::TabFocus(_) => Err(std::io::Error::other("ssh failed")),
                other => panic!("unexpected request: {other:?}"),
            },
        );

        assert_eq!(error_code(&response), "remote_request_failed");
    }

    #[tokio::test]
    async fn tab_create_follows_cached_focused_pane_cwd_without_runtime() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub);
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        let workspace = Workspace::test_new("tabs");
        let focused_pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let cached_cwd = std::env::temp_dir();
        let terminal_id = app.state.workspaces[0]
            .terminal_id(focused_pane)
            .cloned()
            .unwrap();
        app.state.terminals.get_mut(&terminal_id).unwrap().cwd = cached_cwd.clone();

        let response = app.handle_tab_create(
            "req".into(),
            TabCreateParams {
                workspace_id: None,
                cwd: None,
                focus: false,
                label: None,
                env: Default::default(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::TabCreated { .. }));
        let created = &app.state.workspaces[0].tabs[1];
        let created_terminal_id = created.terminal_id(created.root_pane).unwrap();
        let created_cwd = &app.state.terminals.get(created_terminal_id).unwrap().cwd;
        assert_eq!(
            crate::worktree::canonical_or_original(created_cwd),
            crate::worktree::canonical_or_original(&cached_cwd)
        );
        shutdown_test_runtimes(&mut app);
    }

    fn seed_remote_tab_rename_cache(app: &mut App) -> RemoteHostKey {
        let host_key = seed_remote_tab_cache(app);
        app.state.remote_sources.set_capabilities(
            &host_key,
            crate::remote_source::RemoteSourceCapabilities {
                workspace_list_local: true,
                tab_list: true,
                tab_create: true,
                tab_focus: true,
                tab_close: true,
                tab_rename: true,
                layout_export: true,
                ..Default::default()
            },
        );
        host_key
    }

    #[test]
    fn tab_rename_without_remote_hosts_keeps_slash_target_local() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );

        let response = app.handle_tab_rename(
            "req".into(),
            TabRenameParams {
                tab_id: "jafar/tab:remote-tab-1".to_string(),
                label: "new name".to_string(),
            },
        );
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(error.error.code, "tab_not_found");
    }

    #[test]
    fn tab_rename_missing_capability_rejects_before_send() {
        let mut app = app_with_remote_host();
        // seed cache without tab_rename capability
        seed_remote_tab_cache(&mut app);
        let host = app.remote_hosts.get("jafar").cloned().unwrap();

        let response = app.handle_remote_tab_rename_with_sender(
            "req".into(),
            host,
            RemoteTargetSelector::Tab("remote-tab-1".to_string()),
            TabRenameParams {
                tab_id: "jafar/tab:remote-tab-1".to_string(),
                label: "new name".to_string(),
            },
            |_host, _request| panic!("missing capability must not send"),
        );
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(error.error.code, "remote_capability_unavailable");
        assert!(error
            .error
            .message
            .contains(crate::api::schema::FederationCapabilities::TAB_RENAME));
    }

    #[test]
    fn tab_rename_stale_metadata_rejects_before_send() {
        let mut app = app_with_remote_host();
        let host_key = seed_remote_tab_rename_cache(&mut app);
        app.state
            .remote_sources
            .mark_status(&host_key, RemoteConnectionStatus::Disconnected);
        let host = app.remote_hosts.get("jafar").cloned().unwrap();

        let response = app.handle_remote_tab_rename_with_sender(
            "req".into(),
            host,
            RemoteTargetSelector::Tab("remote-tab-1".to_string()),
            TabRenameParams {
                tab_id: "jafar/tab:remote-tab-1".to_string(),
                label: "new name".to_string(),
            },
            |_host, _request| panic!("disconnected host must not send"),
        );
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(error.error.code, "remote_host_not_connected");
    }

    #[test]
    fn tab_rename_success_forwards_raw_tab_id_and_updates_cache() {
        let mut app = app_with_remote_host();
        let host_key = seed_remote_tab_rename_cache(&mut app);
        let host = app.remote_hosts.get("jafar").cloned().unwrap();
        let captured = std::cell::RefCell::new(None::<Request>);

        let renamed_tab = remote_tab("remote-tab-1", true);
        let tab_info_response = remote_tab_info_response("remote-req", renamed_tab.clone());

        let response = app.handle_remote_tab_rename_with_sender(
            "local-req".into(),
            host,
            RemoteTargetSelector::Tab("remote-tab-1".to_string()),
            TabRenameParams {
                tab_id: "jafar/tab:remote-tab-1".to_string(),
                label: "renamed".to_string(),
            },
            |sent_host, request| {
                assert_eq!(sent_host.name, "jafar");
                if captured.borrow().is_none() {
                    *captured.borrow_mut() = Some(request.clone());
                }
                Ok(tab_info_response.clone())
            },
        );

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["id"], "local-req");

        let request = captured.into_inner().expect("request captured");
        let Method::TabRename(rename_params) = request.method else {
            panic!("expected tab.rename");
        };
        assert_eq!(rename_params.tab_id, "remote-tab-1");
        assert_eq!(rename_params.label, "renamed");

        // Cache was updated (upsert_tab re-inserts the entry)
        let snapshots = app.state.remote_sources.tab_snapshots_for_host(&host_key);
        assert!(!snapshots.is_empty());
    }
}
