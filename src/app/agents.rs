use std::path::PathBuf;

use super::{terminal_targets::TerminalTargetError, App, Mode};
use crate::api::schema::{AgentStartParams, SplitDirection};

impl App {
    pub(super) fn collect_agent_infos(&self) -> Vec<crate::api::schema::AgentInfo> {
        let mut agents = self.collect_local_agent_infos();
        agents.extend(
            self.state
                .remote_sources
                .list_entries()
                .into_iter()
                .map(host_qualified_remote_agent_info),
        );
        agents
    }

    pub(crate) fn collect_local_agent_infos(&self) -> Vec<crate::api::schema::AgentInfo> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, ws)| {
                ws.tabs.iter().flat_map(move |tab| {
                    tab.layout
                        .pane_ids()
                        .into_iter()
                        .filter_map(move |pane_id| self.agent_info(ws_idx, pane_id))
                })
            })
            .collect()
    }

    pub(super) fn agent_info_for_target(
        &self,
        target: &str,
    ) -> Result<crate::api::schema::AgentInfo, TerminalTargetError> {
        let resolved = self.resolve_terminal_target(target)?;
        self.agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(|| TerminalTargetError::NotFound {
                target: target.to_string(),
            })
    }

    pub(super) fn focus_agent_target(
        &mut self,
        target: &str,
    ) -> Result<crate::api::schema::AgentInfo, TerminalTargetError> {
        let resolved = self.resolve_terminal_target(target)?;
        self.state
            .focus_pane_in_workspace(resolved.ws_idx, resolved.pane_id);
        self.state.mode = Mode::Terminal;
        self.agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(|| TerminalTargetError::NotFound {
                target: target.to_string(),
            })
    }

    pub(super) fn rename_agent_target(
        &mut self,
        target: &str,
        name: Option<String>,
    ) -> Result<crate::api::schema::AgentInfo, AgentRenameError> {
        let resolved = self
            .resolve_terminal_target(target)
            .map_err(AgentRenameError::Target)?;
        let normalized_name = name.and_then(|name| {
            let trimmed = name.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });

        if let Some(name) = normalized_name.as_deref() {
            let conflicts = self.agent_name_conflicts(name, &resolved.terminal_id);
            if !conflicts.is_empty() {
                return Err(AgentRenameError::DuplicateName {
                    name: name.to_string(),
                    candidates: conflicts,
                });
            }
        }

        let Some(terminal) = self
            .state
            .terminals
            .values_mut()
            .find(|terminal| terminal.id.to_string() == resolved.terminal_id)
        else {
            return Err(AgentRenameError::Target(TerminalTargetError::NotFound {
                target: target.to_string(),
            }));
        };
        match normalized_name {
            Some(name) => {
                terminal.set_agent_name(name.clone());
                terminal.set_manual_label(name);
            }
            None => terminal.clear_agent_name(),
        }
        self.state.mark_session_dirty();
        self.agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(|| {
                AgentRenameError::Target(TerminalTargetError::NotFound {
                    target: target.to_string(),
                })
            })
    }

    pub(super) fn start_agent(
        &mut self,
        params: AgentStartParams,
    ) -> Result<(crate::api::schema::AgentInfo, Vec<String>), AgentStartError> {
        let name = params.name.trim().to_string();
        if name.is_empty() {
            return Err(AgentStartError::InvalidName);
        }
        if params.argv.is_empty() {
            return Err(AgentStartError::EmptyArgv);
        }
        let conflicts = self.agent_name_conflicts(&name, "");
        if !conflicts.is_empty() {
            return Err(AgentStartError::DuplicateName {
                name,
                candidates: conflicts,
            });
        }

        let cwd = params
            .cwd
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        let argv = params.argv;
        let focus = params.focus;
        let (rows, cols) = self.state.estimate_pane_size();

        let (ws_idx, tab_idx, pane_id) = if params.new_workspace {
            self.spawn_agent_workspace(cwd, rows, cols, &argv, focus)?
        } else if let Some(tab_id) = params.tab_id {
            let (ws_idx, tab_idx) =
                self.parse_tab_id(&tab_id)
                    .ok_or_else(|| AgentStartError::TargetNotFound {
                        target: tab_id.clone(),
                    })?;
            if let Some(workspace_id) = params.workspace_id.as_deref() {
                let requested_ws_idx = self.parse_workspace_id(workspace_id).ok_or_else(|| {
                    AgentStartError::TargetNotFound {
                        target: workspace_id.to_string(),
                    }
                })?;
                if requested_ws_idx != ws_idx {
                    return Err(AgentStartError::PlacementConflict);
                }
            }
            let target_pane = self.state.workspaces[ws_idx].tabs[tab_idx].layout.focused();
            self.spawn_agent_split(
                ws_idx,
                target_pane,
                params.split.unwrap_or(SplitDirection::Right),
                cwd,
                &argv,
                focus,
            )?
        } else if let Some(workspace_id) = params.workspace_id {
            let ws_idx = self.parse_workspace_id(&workspace_id).ok_or_else(|| {
                AgentStartError::TargetNotFound {
                    target: workspace_id.clone(),
                }
            })?;
            let tab_idx = self.state.workspaces[ws_idx].active_tab;
            let target_pane = self.state.workspaces[ws_idx].tabs[tab_idx].layout.focused();
            self.spawn_agent_split(
                ws_idx,
                target_pane,
                params.split.unwrap_or(SplitDirection::Right),
                cwd,
                &argv,
                focus,
            )?
        } else if self.state.workspaces.is_empty() {
            self.spawn_agent_workspace(cwd, rows, cols, &argv, focus)?
        } else {
            let ws_idx = self.state.active.unwrap_or(0);
            let tab_idx = self.state.workspaces[ws_idx].active_tab;
            let target_pane = self.state.workspaces[ws_idx].tabs[tab_idx].layout.focused();
            self.spawn_agent_split(
                ws_idx,
                target_pane,
                params.split.unwrap_or(SplitDirection::Right),
                cwd,
                &argv,
                focus,
            )?
        };

        let terminal_id = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.terminal_id(pane_id))
            .cloned()
            .ok_or_else(|| AgentStartError::SpawnFailed("terminal disappeared".into()))?;
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return Err(AgentStartError::SpawnFailed("terminal disappeared".into()));
        };
        terminal.set_agent_name(name.clone());
        terminal.set_manual_label(name);
        self.state.mark_session_dirty();

        let agent = self
            .agent_info(ws_idx, pane_id)
            .ok_or_else(|| AgentStartError::SpawnFailed("agent disappeared".into()))?;
        debug_assert_eq!(agent.tab_id, self.public_tab_id(ws_idx, tab_idx).unwrap());
        Ok((agent, argv))
    }

    pub(super) fn agent_start_error_body(
        &self,
        err: AgentStartError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentStartError::InvalidName => crate::api::schema::ErrorBody {
                code: "invalid_agent_name".into(),
                message: "agent name must not be empty".into(),
            },
            AgentStartError::EmptyArgv => crate::api::schema::ErrorBody {
                code: "invalid_agent_argv".into(),
                message: "agent start argv must not be empty".into(),
            },
            AgentStartError::TargetNotFound { target } => crate::api::schema::ErrorBody {
                code: "agent_placement_not_found".into(),
                message: format!("agent placement target {target} not found"),
            },
            AgentStartError::PlacementConflict => crate::api::schema::ErrorBody {
                code: "agent_placement_conflict".into(),
                message: "--tab must belong to --workspace".into(),
            },
            AgentStartError::SpawnFailed(message) => crate::api::schema::ErrorBody {
                code: "agent_start_failed".into(),
                message,
            },
            AgentStartError::DuplicateName { name, candidates } => crate::api::schema::ErrorBody {
                code: "agent_name_taken".into(),
                message: format!(
                    "agent name {name} is already used; candidates: {}",
                    candidates
                        .into_iter()
                        .map(|candidate| format!(
                            "terminal_id={} pane_id={} workspace_id={} tab_id={} cwd={} status={:?}",
                            candidate.terminal_id,
                            candidate.pane_id,
                            candidate.workspace_id,
                            candidate.tab_id,
                            candidate.cwd.unwrap_or_else(|| "unknown".into()),
                            candidate.agent_status,
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            },
        }
    }

    pub(super) fn agent_target_error_body(
        &self,
        err: TerminalTargetError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            TerminalTargetError::NotFound { target } => crate::api::schema::ErrorBody {
                code: "agent_not_found".into(),
                message: format!("agent target {target} not found"),
            },
            TerminalTargetError::Ambiguous { target, candidates } => {
                crate::api::schema::ErrorBody {
                    code: "agent_target_ambiguous".into(),
                    message: format!(
                        "agent target {target} is ambiguous; candidates: {}",
                        candidates
                            .into_iter()
                            .map(|candidate| format!(
                                "terminal_id={} pane_id={} workspace_id={} tab_id={} cwd={} status={:?}",
                                candidate.terminal_id,
                                candidate.pane_id,
                                candidate.workspace_id,
                                candidate.tab_id,
                                candidate.cwd.unwrap_or_else(|| "unknown".into()),
                                candidate.agent_status,
                            ))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                }
            }
        }
    }

    pub(super) fn agent_rename_error_body(
        &self,
        err: AgentRenameError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentRenameError::Target(err) => self.agent_target_error_body(err),
            AgentRenameError::DuplicateName { name, candidates } => crate::api::schema::ErrorBody {
                code: "agent_name_taken".into(),
                message: format!(
                    "agent name {name} is already used; candidates: {}",
                    candidates
                        .into_iter()
                        .map(|candidate| format!(
                            "terminal_id={} pane_id={} workspace_id={} tab_id={} cwd={} status={:?}",
                            candidate.terminal_id,
                            candidate.pane_id,
                            candidate.workspace_id,
                            candidate.tab_id,
                            candidate.cwd.unwrap_or_else(|| "unknown".into()),
                            candidate.agent_status,
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            },
        }
    }

    fn spawn_agent_workspace(
        &mut self,
        cwd: PathBuf,
        rows: u16,
        cols: u16,
        argv: &[String],
        focus: bool,
    ) -> Result<(usize, usize, crate::layout::PaneId), AgentStartError> {
        let (ws, terminal, runtime) = crate::workspace::Workspace::new_argv_command(
            cwd,
            rows,
            cols,
            argv,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        )
        .map_err(|err| AgentStartError::SpawnFailed(err.to_string()))?;
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        self.state.workspaces.push(ws);
        let ws_idx = self.state.workspaces.len() - 1;
        self.state
            .remove_alias_shadowed_by_new_pane(self.state.workspaces[ws_idx].tabs[0].root_pane);
        if focus || self.state.active.is_none() {
            self.state.switch_workspace(ws_idx);
            self.state.mode = Mode::Terminal;
        }
        self.schedule_session_save();
        let pane_id = self.state.workspaces[ws_idx].tabs[0].root_pane;
        Ok((ws_idx, 0, pane_id))
    }

    fn spawn_agent_split(
        &mut self,
        ws_idx: usize,
        target_pane: crate::layout::PaneId,
        split: SplitDirection,
        cwd: PathBuf,
        argv: &[String],
        focus: bool,
    ) -> Result<(usize, usize, crate::layout::PaneId), AgentStartError> {
        let (rows, cols) = self.state.estimate_pane_size();
        let previous_focus = self.state.current_pane_focus_target();
        let direction = match split {
            SplitDirection::Right => ratatui::layout::Direction::Horizontal,
            SplitDirection::Down => ratatui::layout::Direction::Vertical,
        };
        let result = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| {
                ws.split_pane_argv_command(
                    target_pane,
                    direction,
                    rows,
                    cols,
                    Some(cwd),
                    argv,
                    self.state.pane_scrollback_limit_bytes,
                    self.state.host_terminal_theme,
                    focus,
                )
            })
            .ok_or_else(|| AgentStartError::TargetNotFound {
                target: target_pane.raw().to_string(),
            })?
            .map_err(|err| AgentStartError::SpawnFailed(err.to_string()))?;
        self.terminal_runtimes
            .insert(result.1.terminal.id.clone(), result.1.runtime);
        self.state
            .remove_alias_shadowed_by_new_pane(result.1.pane_id);
        self.state
            .terminals
            .insert(result.1.terminal.id.clone(), result.1.terminal);
        if focus {
            self.state.switch_workspace_tab(ws_idx, result.0);
            self.state
                .record_pane_focus_change(previous_focus, ws_idx, result.1.pane_id);
            self.state.mode = Mode::Terminal;
        }
        self.schedule_session_save();
        Ok((ws_idx, result.0, result.1.pane_id))
    }

    fn agent_info(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::AgentInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_state = ws.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane_state.attached_terminal_id)?;
        if terminal.remote_attach.is_some() {
            return None;
        }
        if !terminal.is_agent_terminal() {
            return None;
        }
        let pane = self.pane_info(ws_idx, pane_id)?;
        Some(crate::api::schema::AgentInfo {
            terminal_id: pane.terminal_id,
            name: terminal.agent_name.clone(),
            agent: pane.agent,
            title: pane.title,
            display_agent: pane.display_agent,
            agent_status: pane.agent_status,
            custom_status: pane.custom_status,
            state_labels: pane.state_labels,
            agent_session: pane.agent_session,
            workspace_id: pane.workspace_id,
            tab_id: pane.tab_id,
            pane_id: pane.pane_id,
            focused: pane.focused,
            cwd: pane.cwd,
            foreground_cwd: pane.foreground_cwd,
            revision: pane.revision,
        })
    }

    fn agent_name_conflicts(
        &self,
        name: &str,
        except_terminal_id: &str,
    ) -> Vec<crate::api::schema::AgentInfo> {
        self.collect_local_agent_infos()
            .into_iter()
            .filter(|agent| {
                agent.name.as_deref() == Some(name) && agent.terminal_id != except_terminal_id
            })
            .collect()
    }
}

pub(super) enum AgentStartError {
    InvalidName,
    EmptyArgv,
    TargetNotFound {
        target: String,
    },
    PlacementConflict,
    SpawnFailed(String),
    DuplicateName {
        name: String,
        candidates: Vec<crate::api::schema::AgentInfo>,
    },
}

pub(crate) fn host_qualified_remote_agent_info(
    entry: crate::remote_source::RemoteAgentEntry,
) -> crate::api::schema::AgentInfo {
    let stale = entry.stale();
    let mut agent = entry.agent;
    agent.name = prefix_remote_label(&entry.host.host, agent.name);
    agent.display_agent = prefix_remote_label(&entry.host.host, agent.display_agent);
    agent.agent = prefix_remote_label(&entry.host.host, agent.agent);
    agent.title = prefix_remote_label(&entry.host.host, agent.title);
    if stale {
        agent.custom_status = entry.status.stale_label().map(str::to_string);
    }
    agent
}

fn prefix_remote_label(host: &str, label: Option<String>) -> Option<String> {
    label.map(|label| format!("{host}/{label}"))
}

pub(super) enum AgentRenameError {
    Target(TerminalTargetError),
    DuplicateName {
        name: String,
        candidates: Vec<crate::api::schema::AgentInfo>,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::api::schema::{AgentInfo, AgentStatus};
    use crate::app::App;
    use crate::remote_source::RemoteHostKey;

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn remote_agent(terminal_id: &str, pane_id: &str, label: &str) -> AgentInfo {
        AgentInfo {
            terminal_id: terminal_id.to_string(),
            name: Some(label.to_string()),
            agent: Some(label.to_string()),
            title: Some(format!("{label} title")),
            display_agent: Some(format!("{label} display")),
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

    #[tokio::test]
    async fn agent_start_new_workspace_flag_ignores_active_workspace_default() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("existing")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let started = app.start_agent(crate::api::schema::AgentStartParams {
            host: None,
            name: "worker".to_string(),
            cwd: None,
            workspace_id: None,
            tab_id: None,
            split: None,
            focus: false,
            new_workspace: true,
            argv: vec!["/usr/bin/true".to_string()],
        });
        let (agent, argv) = match started {
            Ok(started) => started,
            Err(_) => panic!("expected agent start to succeed"),
        };

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(agent.name.as_deref(), Some("worker"));
        assert_eq!(agent.workspace_id, app.public_workspace_id(1));
        assert_eq!(argv, vec!["/usr/bin/true"]);

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
    }

    #[test]
    fn collect_agent_infos_keeps_local_agents_first_and_unchanged() {
        let mut app = test_app();
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

        let agents = app.collect_agent_infos();

        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].terminal_id, local_terminal_id.to_string());
        assert_eq!(agents[0].name.as_deref(), Some("local-codex"));
        assert_eq!(agents[1].terminal_id, "remote-term");
    }

    #[test]
    fn local_agent_name_conflicts_ignore_remote_cache_entries() {
        let mut app = test_app();
        app.state.remote_sources.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![remote_agent("remote-term", "remote-pane", "codex")],
        );

        let conflicts = app.agent_name_conflicts("jafar/codex", "");

        assert!(conflicts.is_empty());
    }

    #[test]
    fn local_agent_infos_ignore_remote_attach_terminals() {
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("attach");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("codex".to_string());
        terminal.remote_attach = Some(crate::remote_source::RemoteAttachTarget {
            host: "jafar".into(),
            session: "default".into(),
            terminal_id: "remote-term".into(),
            label: "jafar/codex".into(),
        });

        assert!(app.collect_local_agent_infos().is_empty());
        assert!(app.agent_name_conflicts("codex", "").is_empty());
    }

    #[test]
    fn collect_agent_infos_appends_host_qualified_remote_labels() {
        let mut app = test_app();
        app.state.remote_sources.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![remote_agent("remote-term", "remote-pane", "codex")],
        );

        let agents = app.collect_agent_infos();

        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name.as_deref(), Some("jafar/codex"));
        assert_eq!(agents[0].agent.as_deref(), Some("jafar/codex"));
        assert_eq!(
            agents[0].display_agent.as_deref(),
            Some("jafar/codex display")
        );
        assert_eq!(agents[0].title.as_deref(), Some("jafar/codex title"));
        assert_eq!(agents[0].terminal_id, "remote-term");
        assert_eq!(agents[0].pane_id, "remote-pane");
    }

    #[test]
    fn collect_agent_infos_keeps_disconnected_remote_cache_entries() {
        let mut app = test_app();
        let host = RemoteHostKey::new("jafar", "default");
        let mut agent = remote_agent("remote-term", "remote-pane", "codex");
        agent.custom_status = Some("busy".to_string());
        app.state
            .remote_sources
            .replace_connected_snapshot(host.clone(), vec![agent]);
        app.state.remote_sources.mark_status(
            &host,
            crate::remote_source::RemoteConnectionStatus::NeedsUpdate,
        );

        let agents = app.collect_agent_infos();

        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name.as_deref(), Some("jafar/codex"));
        assert_eq!(agents[0].custom_status.as_deref(), Some("needs update"));
        assert_eq!(agents[0].terminal_id, "remote-term");
    }

    #[test]
    fn collect_agent_infos_ignores_remote_host_status_without_agents() {
        let mut app = test_app();
        app.state.remote_sources.mark_status(
            &RemoteHostKey::new("jafar", "default"),
            crate::remote_source::RemoteConnectionStatus::Unreachable,
        );

        let agents = app.collect_agent_infos();

        assert!(agents.is_empty());
    }

    #[test]
    fn collect_agent_infos_does_not_synthesize_remote_label_or_local_ids() {
        let mut app = test_app();
        let mut agent = remote_agent("remote-term", "remote-pane", "codex");
        agent.name = None;
        agent.display_agent = None;
        app.state
            .remote_sources
            .replace_connected_snapshot(RemoteHostKey::new("jafar", "default"), vec![agent]);

        let agents = app.collect_agent_infos();

        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, None);
        assert_eq!(agents[0].display_agent, None);
        assert_eq!(agents[0].agent.as_deref(), Some("jafar/codex"));
        assert_eq!(agents[0].pane_id, "remote-pane");
        assert_eq!(agents[0].terminal_id, "remote-term");
    }
}
