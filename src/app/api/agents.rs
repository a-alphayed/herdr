use bytes::Bytes;

use crate::api::schema::{
    AgentReadParams, AgentRenameParams, AgentSendParams, AgentStartParams, AgentTarget, ErrorBody,
    Method, PaneReadResult, ReadFormat, ReadSource, Request, ResponseResult,
};
use crate::app::App;

use super::responses::{encode_error, encode_error_body, encode_success};
use crate::remote_target::{
    plan_target_route, resolve_remote_agent_target, PlannedTargetRoute, RemoteAgentResolveError,
    RemoteRoutePlanError, RemoteTargetSelector,
};

impl App {
    pub(super) fn handle_agent_list(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::AgentList {
                agents: self.collect_agent_infos(),
            },
        )
    }

    pub(super) fn handle_agent_list_local(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::AgentList {
                agents: self.collect_local_agent_infos(),
            },
        )
    }

    pub(super) fn handle_agent_get(&mut self, id: String, target: AgentTarget) -> String {
        match self.plan_agent_api_target(&target.target) {
            Ok(PlannedTargetRoute::Local { .. }) => {
                let agent = match self.agent_info_for_target(&target.target) {
                    Ok(agent) => agent,
                    Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
                };

                encode_success(id, ResponseResult::AgentInfo { agent })
            }
            Ok(PlannedTargetRoute::Remote { host, target }) => {
                match resolve_remote_agent_target(&self.state.remote_sources, &host, &target) {
                    Ok(resolved) => encode_success(
                        id,
                        ResponseResult::AgentInfo {
                            agent: crate::app::agents::host_qualified_remote_agent_info(
                                resolved.entry,
                            ),
                        },
                    ),
                    Err(err) => encode_error_body(id, remote_agent_resolve_error_body(err)),
                }
            }
            Err(err) => encode_error_body(id, remote_route_plan_error_body(err)),
        }
    }

    pub(super) fn handle_agent_focus(&mut self, id: String, target: AgentTarget) -> String {
        match self.plan_agent_api_target(&target.target) {
            Ok(PlannedTargetRoute::Local { .. }) => {}
            Ok(PlannedTargetRoute::Remote {
                host,
                target: selector,
            }) => {
                return self.handle_remote_agent_focus(id, host, selector, target);
            }
            Err(err) => return encode_error_body(id, remote_route_plan_error_body(err)),
        }

        let agent = match self.focus_agent_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_rename(&mut self, id: String, params: AgentRenameParams) -> String {
        let agent = match self.rename_agent_target(&params.target, params.name) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_rename_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_start(&mut self, id: String, params: AgentStartParams) -> String {
        if let Some(host_alias) = params.host.as_deref() {
            let Some(host) = self.remote_hosts.get(host_alias).cloned() else {
                return encode_error_body(
                    id,
                    remote_route_plan_error_body(RemoteRoutePlanError::UnknownHost(
                        host_alias.to_string(),
                    )),
                );
            };
            return self.handle_remote_agent_start(id, host, params);
        }

        let (agent, argv) = match self.start_agent(params) {
            Ok(started) => started,
            Err(err) => return encode_error_body(id, self.agent_start_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentStarted { agent, argv })
    }

    pub(super) fn handle_agent_read(&mut self, id: String, params: AgentReadParams) -> String {
        match self.plan_agent_api_target(&params.target) {
            Ok(PlannedTargetRoute::Local { .. }) => {}
            Ok(PlannedTargetRoute::Remote { host, target }) => {
                return self.handle_remote_agent_read(id, host, target, params);
            }
            Err(err) => return encode_error_body(id, remote_route_plan_error_body(err)),
        }

        let resolved = match self.resolve_terminal_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some((pane, workspace_id)) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)
        else {
            return agent_not_found(id, &params.target);
        };
        let requested_lines = params.lines.unwrap_or(80).min(1000) as usize;
        let text = match params.format {
            ReadFormat::Text => match params.source {
                ReadSource::Visible => pane.visible_text(),
                ReadSource::Recent => pane.recent_text(requested_lines),
                ReadSource::RecentUnwrapped => pane.recent_unwrapped_text(requested_lines),
            },
            ReadFormat::Ansi => match params.source {
                ReadSource::Visible => pane.visible_ansi(),
                ReadSource::Recent => pane.recent_ansi(requested_lines),
                ReadSource::RecentUnwrapped => pane.recent_unwrapped_ansi(requested_lines),
            },
        };

        encode_success(
            id,
            ResponseResult::PaneRead {
                read: PaneReadResult {
                    pane_id: self
                        .public_pane_id(resolved.ws_idx, resolved.pane_id)
                        .unwrap_or_else(|| params.target.clone()),
                    workspace_id,
                    tab_id: self
                        .public_tab_id(resolved.ws_idx, resolved.tab_idx)
                        .unwrap(),
                    source: params.source,
                    format: params.format,
                    text,
                    revision: 0,
                    truncated: false,
                },
            },
        )
    }

    pub(super) fn handle_agent_send(&mut self, id: String, params: AgentSendParams) -> String {
        match self.plan_agent_api_target(&params.target) {
            Ok(PlannedTargetRoute::Local { .. }) => {}
            Ok(PlannedTargetRoute::Remote { host, target }) => {
                return self.handle_remote_agent_send(id, host, target, params);
            }
            Err(err) => return encode_error_body(id, remote_route_plan_error_body(err)),
        }

        let resolved = match self.resolve_terminal_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &params.target);
        };
        if let Err(err) = runtime.try_send_bytes(Bytes::from(params.text)) {
            return encode_error(id, "agent_send_failed", err.to_string());
        }

        encode_success(id, ResponseResult::Ok {})
    }

    fn plan_agent_api_target(
        &self,
        target: &str,
    ) -> Result<PlannedTargetRoute, RemoteRoutePlanError> {
        if self.remote_hosts.list().is_empty() {
            return Ok(PlannedTargetRoute::Local {
                target: target.to_string(),
            });
        }
        plan_target_route(&self.remote_hosts, target)
    }

    fn handle_remote_agent_focus(
        &self,
        id: String,
        host: crate::remote_target::RemoteHostConfig,
        selector: RemoteTargetSelector,
        target: AgentTarget,
    ) -> String {
        let resolved =
            match resolve_remote_agent_target(&self.state.remote_sources, &host, &selector) {
                Ok(resolved) => resolved,
                Err(err) => return encode_error_body(id, remote_agent_resolve_error_body(err)),
            };
        let request = remote_agent_focus_request(
            id.clone(),
            target,
            resolved.entry.agent.terminal_id.as_str(),
        );
        match crate::remote::send_remote_api_request_to_host_noninteractive(&host, &request)
            .and_then(|response| rewrite_response_id(&response, &id))
        {
            Ok(response) => response,
            Err(err) => encode_error(id, "remote_request_failed", err.to_string()),
        }
    }

    fn handle_remote_agent_read(
        &self,
        id: String,
        host: crate::remote_target::RemoteHostConfig,
        selector: RemoteTargetSelector,
        params: AgentReadParams,
    ) -> String {
        let resolved =
            match resolve_remote_agent_target(&self.state.remote_sources, &host, &selector) {
                Ok(resolved) => resolved,
                Err(err) => return encode_error_body(id, remote_agent_resolve_error_body(err)),
            };
        let request = remote_agent_read_request(
            id.clone(),
            params,
            resolved.entry.agent.terminal_id.as_str(),
        );
        match crate::remote::send_remote_api_request_to_host_noninteractive(&host, &request)
            .and_then(|response| rewrite_response_id(&response, &id))
        {
            Ok(response) => response,
            Err(err) => encode_error(id, "remote_request_failed", err.to_string()),
        }
    }

    fn handle_remote_agent_send(
        &self,
        id: String,
        host: crate::remote_target::RemoteHostConfig,
        selector: RemoteTargetSelector,
        params: AgentSendParams,
    ) -> String {
        let resolved =
            match resolve_remote_agent_target(&self.state.remote_sources, &host, &selector) {
                Ok(resolved) => resolved,
                Err(err) => return encode_error_body(id, remote_agent_resolve_error_body(err)),
            };
        let request = remote_agent_send_request(
            id.clone(),
            params,
            resolved.entry.agent.terminal_id.as_str(),
        );
        match crate::remote::send_remote_api_request_to_host_noninteractive(&host, &request)
            .and_then(|response| rewrite_response_id(&response, &id))
        {
            Ok(response) => response,
            Err(err) => encode_error(id, "remote_request_failed", err.to_string()),
        }
    }

    fn handle_remote_agent_start(
        &self,
        id: String,
        host: crate::remote_target::RemoteHostConfig,
        params: AgentStartParams,
    ) -> String {
        let request = remote_agent_start_request(id.clone(), params);
        match crate::remote::send_remote_api_request_to_host_noninteractive(&host, &request)
            .and_then(|response| rewrite_remote_agent_start_response(&response, &id, &host.name))
        {
            Ok(response) => response,
            Err(err) => encode_error(id, "remote_request_failed", err.to_string()),
        }
    }
}

fn agent_not_found(id: String, target: &str) -> String {
    encode_error(
        id,
        "agent_not_found",
        format!("agent target {target} not found"),
    )
}

fn remote_agent_focus_request(id: String, mut target: AgentTarget, terminal_id: &str) -> Request {
    target.target = terminal_id.to_string();
    Request {
        id,
        method: Method::AgentFocus(target),
    }
}

fn remote_agent_read_request(
    id: String,
    mut params: AgentReadParams,
    terminal_id: &str,
) -> Request {
    params.target = terminal_id.to_string();
    Request {
        id,
        method: Method::AgentRead(params),
    }
}

fn remote_agent_send_request(
    id: String,
    mut params: AgentSendParams,
    terminal_id: &str,
) -> Request {
    params.target = terminal_id.to_string();
    Request {
        id,
        method: Method::AgentSend(params),
    }
}

pub(crate) fn remote_agent_start_request(id: String, mut params: AgentStartParams) -> Request {
    params.host = None;
    params.new_workspace = params.workspace_id.is_none() && params.tab_id.is_none();
    Request {
        id,
        method: Method::AgentStart(params),
    }
}

fn rewrite_response_id(response: &str, id: &str) -> std::io::Result<String> {
    let mut value: serde_json::Value = serde_json::from_str(response).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid remote API response JSON: {err}"),
        )
    })?;
    let Some(object) = value.as_object_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "remote API response must be a JSON object",
        ));
    };
    if !object.contains_key("result") && !object.contains_key("error") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "remote API response must contain result or error",
        ));
    }
    object.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    serde_json::to_string(&value).map_err(std::io::Error::other)
}

pub(crate) fn rewrite_remote_agent_start_response(
    response: &str,
    id: &str,
    host: &str,
) -> std::io::Result<String> {
    let mut value: serde_json::Value = serde_json::from_str(response).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid remote API response JSON: {err}"),
        )
    })?;
    let Some(object) = value.as_object_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "remote API response must be a JSON object",
        ));
    };
    if !object.contains_key("result") && !object.contains_key("error") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "remote API response must contain result or error",
        ));
    }
    object.insert("id".to_string(), serde_json::Value::String(id.to_string()));

    if let Some(agent) = object
        .get_mut("result")
        .and_then(serde_json::Value::as_object_mut)
        .filter(|result| {
            result
                .get("type")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|kind| kind == "agent_started")
        })
        .and_then(|result| result.get_mut("agent"))
        .and_then(serde_json::Value::as_object_mut)
    {
        prefix_remote_agent_label_field(agent, "name", host);
        prefix_remote_agent_label_field(agent, "display_agent", host);
        prefix_remote_agent_label_field(agent, "agent", host);
        prefix_remote_agent_label_field(agent, "title", host);
    }

    serde_json::to_string(&value).map_err(std::io::Error::other)
}

fn prefix_remote_agent_label_field(
    agent: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
    host: &str,
) {
    let Some(value) = agent.get_mut(field) else {
        return;
    };
    let Some(label) = value.as_str() else {
        return;
    };
    *value = serde_json::Value::String(format!("{host}/{label}"));
}

fn remote_route_plan_error_body(err: RemoteRoutePlanError) -> ErrorBody {
    match err {
        RemoteRoutePlanError::Parse(err) => ErrorBody {
            code: "remote_target_error".to_string(),
            message: err.to_string(),
        },
        RemoteRoutePlanError::UnknownHost(host) => ErrorBody {
            code: "remote_target_error".to_string(),
            message: format!("unknown remote host: {host}"),
        },
    }
}

fn remote_agent_resolve_error_body(err: RemoteAgentResolveError) -> ErrorBody {
    match err {
        RemoteAgentResolveError::NotFound { target } => ErrorBody {
            code: "remote_agent_not_found".to_string(),
            message: format!("remote agent target not found: {target:?}"),
        },
        RemoteAgentResolveError::Ambiguous { candidates, .. } => ErrorBody {
            code: "remote_agent_ambiguous".to_string(),
            message: format!(
                "remote agent target matched {} agents: {}",
                candidates.len(),
                candidates
                    .into_iter()
                    .map(|candidate| candidate.terminal_id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
        RemoteAgentResolveError::UnsupportedSelector { target } => ErrorBody {
            code: "remote_target_error".to_string(),
            message: format!("remote selector is not a single-agent target: {target:?}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::api::schema::{
        AgentInfo, AgentReadParams, AgentStartParams, AgentStatus, EmptyParams, ErrorBody,
        ErrorResponse, SuccessResponse,
    };
    use crate::remote_source::RemoteHostKey;

    use super::*;

    fn test_app(config: &crate::config::Config) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(config, true, None, api_rx, crate::api::EventHub::default())
    }

    fn remote_agent(terminal_id: &str, pane_id: &str, name: &str) -> AgentInfo {
        AgentInfo {
            terminal_id: terminal_id.to_string(),
            name: Some(name.to_string()),
            agent: Some(name.to_string()),
            title: None,
            display_agent: Some(name.to_string()),
            agent_status: AgentStatus::Working,
            custom_status: None,
            state_labels: HashMap::new(),
            agent_session: None,
            workspace_id: "remote-ws".to_string(),
            tab_id: "remote-tab".to_string(),
            pane_id: pane_id.to_string(),
            focused: false,
            cwd: None,
            foreground_cwd: None,
            revision: 1,
        }
    }

    #[test]
    fn remote_agent_get_returns_cached_remote_agent() {
        let mut config = crate::config::Config::default();
        config.remote.enabled = true;
        config.remote.hosts = vec![crate::remote_target::RemoteHostConfig::new(
            "jafar", "jafar", "default", true,
        )];
        let mut app = test_app(&config);
        app.state.remote_sources.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![remote_agent("term-1", "pane-1", "codex")],
        );

        let response = app.handle_agent_get(
            "local-id".to_string(),
            AgentTarget {
                target: "jafar/codex".to_string(),
            },
        );
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(parsed.id, "local-id");
        let ResponseResult::AgentInfo { agent } = parsed.result else {
            panic!("expected agent info");
        };
        assert_eq!(agent.name.as_deref(), Some("jafar/codex"));
        assert_eq!(agent.display_agent.as_deref(), Some("jafar/codex"));
        assert_eq!(agent.agent.as_deref(), Some("jafar/codex"));
        assert_eq!(agent.terminal_id, "term-1");
        assert_eq!(agent.pane_id, "pane-1");
        assert_eq!(agent.workspace_id, "remote-ws");
        assert_eq!(agent.tab_id, "remote-tab");
    }

    #[test]
    fn remote_agent_get_marks_stale_cached_agent_disconnected() {
        let mut config = crate::config::Config::default();
        config.remote.enabled = true;
        config.remote.hosts = vec![crate::remote_target::RemoteHostConfig::new(
            "jafar", "jafar", "default", true,
        )];
        let mut app = test_app(&config);
        let host = RemoteHostKey::new("jafar", "default");
        let mut agent = remote_agent("term-1", "pane-1", "codex");
        agent.custom_status = Some("busy".to_string());
        app.state
            .remote_sources
            .replace_connected_snapshot(host.clone(), vec![agent]);
        app.state.remote_sources.mark_disconnected(&host);

        let response = app.handle_agent_get(
            "local-id".to_string(),
            AgentTarget {
                target: "jafar/codex".to_string(),
            },
        );
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentInfo { agent } = parsed.result else {
            panic!("expected agent info");
        };

        assert_eq!(agent.name.as_deref(), Some("jafar/codex"));
        assert_eq!(agent.custom_status.as_deref(), Some("disconnected"));
    }

    #[test]
    fn agent_list_local_returns_only_local_agents() {
        let mut app = test_app(&crate::config::Config::default());
        let workspace = crate::workspace::Workspace::test_new("local");
        let local_pane_id = workspace.tabs[0].root_pane;
        let local_terminal_id = workspace.terminal_id(local_pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state
            .terminals
            .get_mut(&local_terminal_id)
            .unwrap()
            .set_agent_name("local-codex".to_string());
        app.state.remote_sources.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![remote_agent("remote-term", "remote-pane", "codex")],
        );

        let response = app.handle_agent_list_local("local-id".to_string());
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentList { agents } = parsed.result else {
            panic!("expected agent list");
        };

        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].terminal_id, local_terminal_id.to_string());
        assert_eq!(agents[0].name.as_deref(), Some("local-codex"));
    }

    #[test]
    fn public_agent_list_aggregates_local_and_remote_cache() {
        let mut app = test_app(&crate::config::Config::default());
        let workspace = crate::workspace::Workspace::test_new("local");
        let local_pane_id = workspace.tabs[0].root_pane;
        let local_terminal_id = workspace.terminal_id(local_pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state
            .terminals
            .get_mut(&local_terminal_id)
            .unwrap()
            .set_agent_name("local-codex".to_string());
        app.state.remote_sources.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![remote_agent("remote-term", "remote-pane", "codex")],
        );

        let response = app.handle_agent_list("local-id".to_string());
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentList { agents } = parsed.result else {
            panic!("expected agent list");
        };

        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].name.as_deref(), Some("local-codex"));
        assert_eq!(agents[1].name.as_deref(), Some("jafar/codex"));
    }

    #[test]
    fn agent_list_local_method_dispatch_returns_only_local_agents() {
        let mut app = test_app(&crate::config::Config::default());
        app.state.remote_sources.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![remote_agent("remote-term", "remote-pane", "codex")],
        );
        let request = Request {
            id: "local-id".to_string(),
            method: Method::AgentListLocal(EmptyParams::default()),
        };

        let response = app.handle_api_request(request);
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentList { agents } = parsed.result else {
            panic!("expected agent list");
        };

        assert!(agents.is_empty());
    }

    #[test]
    fn bare_agent_get_keeps_local_target_errors() {
        let mut app = test_app(&crate::config::Config::default());

        let response = app.handle_agent_get(
            "local-id".to_string(),
            AgentTarget {
                target: "missing".to_string(),
            },
        );
        let parsed: ErrorResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(parsed.id, "local-id");
        assert_eq!(parsed.error.code, "agent_not_found");
    }

    #[test]
    fn slash_target_without_remote_hosts_keeps_local_target_errors() {
        let mut app = test_app(&crate::config::Config::default());

        let response = app.handle_agent_get(
            "local-id".to_string(),
            AgentTarget {
                target: "local/name".to_string(),
            },
        );
        let parsed: ErrorResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(parsed.id, "local-id");
        assert_eq!(parsed.error.code, "agent_not_found");
    }

    #[test]
    fn remote_agent_focus_request_uses_resolved_terminal_id() {
        let request = remote_agent_focus_request(
            "req-1".to_string(),
            AgentTarget {
                target: "jafar/codex".to_string(),
            },
            "term-1",
        );

        let Method::AgentFocus(params) = request.method else {
            panic!("expected agent.focus");
        };
        assert_eq!(request.id, "req-1");
        assert_eq!(params.target, "term-1");
    }

    #[test]
    fn remote_agent_read_request_uses_resolved_terminal_id() {
        let request = remote_agent_read_request(
            "req-1".to_string(),
            AgentReadParams {
                target: "jafar/codex".to_string(),
                source: ReadSource::Recent,
                lines: Some(20),
                format: ReadFormat::Text,
                strip_ansi: true,
            },
            "term-1",
        );

        let Method::AgentRead(params) = request.method else {
            panic!("expected agent.read");
        };
        assert_eq!(request.id, "req-1");
        assert_eq!(params.target, "term-1");
        assert_eq!(params.lines, Some(20));
    }

    #[test]
    fn remote_agent_send_request_uses_resolved_terminal_id() {
        let request = remote_agent_send_request(
            "req-1".to_string(),
            AgentSendParams {
                target: "jafar/codex".to_string(),
                text: "hello".to_string(),
            },
            "term-1",
        );

        let Method::AgentSend(params) = request.method else {
            panic!("expected agent.send");
        };
        assert_eq!(request.id, "req-1");
        assert_eq!(params.target, "term-1");
        assert_eq!(params.text, "hello");
    }

    #[test]
    fn remote_agent_start_request_strips_host_and_defaults_to_new_workspace() {
        let request = remote_agent_start_request(
            "req-1".to_string(),
            AgentStartParams {
                host: Some("jafar".to_string()),
                name: "codex".to_string(),
                cwd: Some("/remote/project".to_string()),
                workspace_id: None,
                tab_id: None,
                split: None,
                focus: false,
                new_workspace: false,
                argv: vec!["codex".to_string()],
            },
        );

        let Method::AgentStart(params) = request.method else {
            panic!("expected agent.start");
        };
        assert_eq!(request.id, "req-1");
        assert_eq!(params.host, None);
        assert_eq!(params.cwd.as_deref(), Some("/remote/project"));
        assert!(params.new_workspace);
    }

    #[test]
    fn remote_agent_start_request_keeps_explicit_remote_placement() {
        let request = remote_agent_start_request(
            "req-1".to_string(),
            AgentStartParams {
                host: Some("jafar".to_string()),
                name: "codex".to_string(),
                cwd: None,
                workspace_id: Some("remote-ws".to_string()),
                tab_id: None,
                split: None,
                focus: false,
                new_workspace: false,
                argv: vec!["codex".to_string()],
            },
        );

        let Method::AgentStart(params) = request.method else {
            panic!("expected agent.start");
        };
        assert_eq!(params.host, None);
        assert_eq!(params.workspace_id.as_deref(), Some("remote-ws"));
        assert!(!params.new_workspace);
    }

    #[test]
    fn remote_agent_start_request_clears_incoming_new_workspace_with_placement() {
        let request = remote_agent_start_request(
            "req-1".to_string(),
            AgentStartParams {
                host: Some("jafar".to_string()),
                name: "codex".to_string(),
                cwd: None,
                workspace_id: Some("remote-ws".to_string()),
                tab_id: None,
                split: None,
                focus: false,
                new_workspace: true,
                argv: vec!["codex".to_string()],
            },
        );

        let Method::AgentStart(params) = request.method else {
            panic!("expected agent.start");
        };
        assert_eq!(params.host, None);
        assert_eq!(params.workspace_id.as_deref(), Some("remote-ws"));
        assert!(!params.new_workspace);
    }

    #[test]
    fn remote_agent_start_response_rewrites_id_and_host_qualifies_agent() {
        let response = serde_json::to_string(&SuccessResponse {
            id: "remote-id".to_string(),
            result: ResponseResult::AgentStarted {
                agent: remote_agent("term-1", "pane-1", "codex"),
                argv: vec!["codex".to_string()],
            },
        })
        .unwrap();

        let rewritten =
            rewrite_remote_agent_start_response(&response, "local-id", "jafar").unwrap();
        let parsed: SuccessResponse = serde_json::from_str(&rewritten).unwrap();

        assert_eq!(parsed.id, "local-id");
        let ResponseResult::AgentStarted { agent, argv } = parsed.result else {
            panic!("expected agent_started");
        };
        assert_eq!(agent.name.as_deref(), Some("jafar/codex"));
        assert_eq!(agent.display_agent.as_deref(), Some("jafar/codex"));
        assert_eq!(agent.agent.as_deref(), Some("jafar/codex"));
        assert_eq!(agent.title, None);
        assert_eq!(argv, vec!["codex"]);
    }

    #[test]
    fn rewrite_response_id_preserves_success_body() {
        let response = serde_json::to_string(&SuccessResponse {
            id: "remote-id".to_string(),
            result: ResponseResult::Ok {},
        })
        .unwrap();

        let rewritten = rewrite_response_id(&response, "local-id").unwrap();
        let parsed: SuccessResponse = serde_json::from_str(&rewritten).unwrap();

        assert_eq!(parsed.id, "local-id");
        assert_eq!(parsed.result, ResponseResult::Ok {});
    }

    #[test]
    fn rewrite_response_id_preserves_error_body() {
        let response = serde_json::to_string(&ErrorResponse {
            id: "remote-id".to_string(),
            error: ErrorBody {
                code: "remote_error".to_string(),
                message: "failed remotely".to_string(),
            },
        })
        .unwrap();

        let rewritten = rewrite_response_id(&response, "local-id").unwrap();
        let parsed: ErrorResponse = serde_json::from_str(&rewritten).unwrap();

        assert_eq!(parsed.id, "local-id");
        assert_eq!(parsed.error.code, "remote_error");
        assert_eq!(parsed.error.message, "failed remotely");
    }

    #[test]
    fn rewrite_response_id_rejects_malformed_json() {
        let err = rewrite_response_id("not json", "local-id").unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn rewrite_response_id_rejects_non_api_json() {
        let err = rewrite_response_id(r#"{"id":"remote-id"}"#, "local-id").unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}
