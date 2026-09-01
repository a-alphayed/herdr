use super::{
    state::{Mode, RemoteAttachPaneTarget, ToastKind, ToastNotification},
    App,
};
use crate::remote_source::RemoteAttachTarget;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteDetachViewError {
    PaneMissing,
    TerminalMissing,
    NotAttached,
    Spawn(String),
}

impl std::fmt::Display for RemoteDetachViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PaneMissing => write!(f, "view already gone"),
            Self::TerminalMissing => write!(f, "view terminal no longer exists"),
            Self::NotAttached => write!(f, "pane is not attached to a remote view"),
            Self::Spawn(err) => write!(f, "failed to restore local shell: {err}"),
        }
    }
}

fn remote_attach_display_label(target: &RemoteAttachTarget) -> String {
    let label = target.label.trim();
    if !label.is_empty() {
        return label.to_string();
    }
    let key = target.key();
    format!("{}/terminal:{}", key.host, key.terminal_id)
}

fn toast_error(app: &mut App, title: &str, detail: impl Into<String>) {
    app.state.toast = Some(ToastNotification {
        kind: ToastKind::NeedsAttention,
        title: title.to_string(),
        context: detail.into(),
        position: None,
        target: None,
    });
    app.sync_toast_deadline(None);
}

fn toast_finished(app: &mut App, title: &str, detail: impl Into<String>) {
    app.state.toast = Some(ToastNotification {
        kind: ToastKind::Finished,
        title: title.to_string(),
        context: detail.into(),
        position: None,
        target: None,
    });
    app.sync_toast_deadline(None);
}

fn set_terminal_mode(app: &mut App) {
    app.state.mode = if app.state.active.is_some() {
        Mode::Terminal
    } else {
        Mode::Navigate
    };
}

impl App {
    pub(crate) fn drain_remote_detach_view_request(&mut self) {
        let Some(pane) = self.state.request_remote_detach_view.take() else {
            return;
        };
        self.detach_remote_attach_view_or_toast(pane);
    }

    fn detach_remote_attach_view_or_toast(&mut self, pane: RemoteAttachPaneTarget) {
        match self.detach_remote_attach_view(pane) {
            Ok(label) => toast_finished(self, "Detached view", format!("{label} is still running")),
            Err(
                RemoteDetachViewError::PaneMissing
                | RemoteDetachViewError::TerminalMissing
                | RemoteDetachViewError::NotAttached,
            ) => set_terminal_mode(self),
            Err(err) => {
                toast_error(self, "detach failed", err.to_string());
                set_terminal_mode(self);
            }
        }
    }

    fn detach_remote_attach_view(
        &mut self,
        pane: RemoteAttachPaneTarget,
    ) -> Result<String, RemoteDetachViewError> {
        let (ws_idx, _tab_idx) = self
            .state
            .remote_attach_pane_indices(&pane)
            .ok_or(RemoteDetachViewError::PaneMissing)?;
        let terminal_id = pane.terminal_id.clone();
        let terminal = self
            .state
            .terminals
            .get(&terminal_id)
            .ok_or(RemoteDetachViewError::TerminalMissing)?;
        let remote_attach = terminal
            .remote_attach
            .clone()
            .ok_or(RemoteDetachViewError::NotAttached)?;
        let label = remote_attach_display_label(&remote_attach);
        let cwd = terminal.cwd.clone();
        let (rows, cols) = self
            .terminal_runtimes
            .get(&terminal_id)
            .map(crate::terminal::TerminalRuntime::current_size)
            .unwrap_or_else(|| self.state.estimate_pane_size());
        let launch_env = self
            .pane_launch_env(ws_idx, pane.pane_id, Vec::new())
            .unwrap_or_default();

        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane.pane_id,
            rows,
            cols,
            cwd,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            &launch_env,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        )
        .map_err(|err| RemoteDetachViewError::Spawn(err.to_string()))?;

        let old_runtime = self.terminal_runtimes.insert(terminal_id.clone(), runtime);
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.clear_agent_runtime_identity_after_respawn();
        }
        self.state.focus_pane_in_workspace(ws_idx, pane.pane_id);
        set_terminal_mode(self);
        self.schedule_session_save();
        self.render_dirty
            .store(true, std::sync::atomic::Ordering::Release);
        self.render_notify.notify_one();
        if let Some(old_runtime) = old_runtime {
            old_runtime.shutdown();
        }
        Ok(label)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::api::schema::{AgentInfo, AgentStatus};
    use crate::config::Config;
    use crate::remote_source::{RemoteConnectionStatus, RemoteHostKey};
    use crate::remote_target::RemoteHostConfig;
    use crate::workspace::Workspace;

    fn target() -> RemoteAttachTarget {
        RemoteAttachTarget {
            host: "jafar".into(),
            session: "default".into(),
            terminal_id: "term-1".into(),
            label: "jafar/codex".into(),
        }
    }

    fn remote_agent(terminal_id: &str) -> AgentInfo {
        AgentInfo {
            terminal_id: terminal_id.to_string(),
            name: Some("codex".into()),
            agent: Some("codex".into()),
            title: None,
            display_agent: Some("codex".into()),
            agent_status: AgentStatus::Working,
            screen_detection_skipped: false,
            custom_status: None,
            state_labels: HashMap::new(),
            agent_session: None,
            workspace_id: "remote-ws".into(),
            tab_id: "remote-tab".into(),
            pane_id: "remote-pane".into(),
            focused: false,
            cwd: None,
            foreground_cwd: None,
            revision: 1,
        }
    }

    fn app_with_remote(session: &str) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut config = Config::default();
        config.remote.enabled = true;
        config.remote.hosts = vec![RemoteHostConfig::new("jafar", "jafar", session, true)];
        App::new(&config, true, None, api_rx, crate::api::EventHub::default())
    }

    fn local_pane(app: &mut App) -> RemoteAttachPaneTarget {
        let workspace = Workspace::test_new("local");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        RemoteAttachPaneTarget {
            workspace_id: app.state.workspaces[0].id.clone(),
            pane_id,
            terminal_id,
        }
    }

    fn attached_pane(app: &mut App) -> RemoteAttachPaneTarget {
        let pane = local_pane(app);
        let terminal = app
            .state
            .terminals
            .get_mut(&pane.terminal_id)
            .expect("terminal");
        terminal.remote_attach = Some(target());
        terminal.respawn_shell_on_exit = true;
        terminal.launch_argv = Some(vec![
            "herdr".into(),
            "agent".into(),
            "attach".into(),
            "jafar/terminal:term-1".into(),
        ]);
        pane
    }

    fn cache_connected_target(app: &mut App) {
        app.state.remote_sources.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![remote_agent("term-1")],
        );
    }

    #[tokio::test]
    async fn remote_detach_view_swaps_shell_and_clears_local_metadata() {
        let mut app = app_with_remote("default");
        cache_connected_target(&mut app);
        let pane = attached_pane(&mut app);
        let pane_id = pane.pane_id;
        let terminal_id = pane.terminal_id.clone();
        let (old_runtime, _old_rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        let old_token = old_runtime.runtime_token();
        app.terminal_runtimes
            .insert(terminal_id.clone(), old_runtime);

        app.state.request_remote_detach_view = Some(pane);
        app.drain_remote_detach_view_request();

        let terminal = app.state.terminals.get(&terminal_id).unwrap();
        assert!(terminal.remote_attach.is_none());
        assert!(!terminal.respawn_shell_on_exit);
        assert!(terminal.launch_argv.is_none());
        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(
            app.state
                .toast
                .as_ref()
                .map(|toast| { (toast.title.as_str(), toast.context.as_str()) }),
            Some(("Detached view", "jafar/codex is still running"))
        );
        let new_token = app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("replacement shell runtime")
            .runtime_token();
        assert_ne!(new_token, old_token);

        app.handle_internal_event(crate::events::AppEvent::PaneRuntimeDied {
            pane_id,
            runtime_token: old_token,
        });

        assert_eq!(
            app.terminal_runtimes
                .get(&terminal_id)
                .map(crate::terminal::TerminalRuntime::runtime_token),
            Some(new_token)
        );
        assert!(app.find_pane(pane_id).is_some());

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[tokio::test]
    async fn remote_detach_view_preserves_remote_cache_and_works_when_stale() {
        let mut app = app_with_remote("default");
        cache_connected_target(&mut app);
        let host = RemoteHostKey::new("jafar", "default");
        app.state
            .remote_sources
            .mark_status(&host, RemoteConnectionStatus::Unreachable);
        let pane = attached_pane(&mut app);
        let terminal_id = pane.terminal_id.clone();
        let (old_runtime, _old_rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes
            .insert(terminal_id.clone(), old_runtime);

        app.state.request_remote_detach_view = Some(pane);
        app.drain_remote_detach_view_request();

        let entry = app
            .state
            .remote_sources
            .agent(&target().key())
            .expect("remote cache entry should remain");
        assert_eq!(entry.status, RemoteConnectionStatus::Unreachable);
        assert!(app
            .state
            .terminals
            .get(&terminal_id)
            .and_then(|terminal| terminal.remote_attach.as_ref())
            .is_none());

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[tokio::test]
    async fn remote_detach_view_spawn_failure_preserves_attach_runtime_and_metadata() {
        let mut app = app_with_remote("default");
        app.state.default_shell = "/__herdr_missing_detach_shell__".into();
        app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
        let pane = attached_pane(&mut app);
        let terminal_id = pane.terminal_id.clone();
        let (old_runtime, _old_rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        let old_token = old_runtime.runtime_token();
        app.terminal_runtimes
            .insert(terminal_id.clone(), old_runtime);

        app.state.request_remote_detach_view = Some(pane);
        app.drain_remote_detach_view_request();

        assert_eq!(
            app.terminal_runtimes
                .get(&terminal_id)
                .map(crate::terminal::TerminalRuntime::runtime_token),
            Some(old_token)
        );
        let terminal = app.state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.remote_attach, Some(target()));
        assert!(terminal.respawn_shell_on_exit);
        assert!(terminal.launch_argv.is_some());
        let toast = app.state.toast.as_ref().expect("failure toast");
        assert_eq!(toast.title, "detach failed");
        assert!(toast.context.contains("failed to restore local shell"));

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[test]
    fn remote_detach_view_nonattached_pane_is_noop() {
        let mut app = app_with_remote("default");
        let pane = local_pane(&mut app);
        let terminal_id = pane.terminal_id.clone();

        app.state.request_remote_detach_view = Some(pane);
        app.drain_remote_detach_view_request();

        assert!(app.terminal_runtimes.get(&terminal_id).is_none());
        assert!(app.state.toast.is_none());
        assert!(app
            .state
            .terminals
            .get(&terminal_id)
            .and_then(|terminal| terminal.remote_attach.as_ref())
            .is_none());
    }

    #[test]
    fn remote_detach_view_missing_pane_is_noop() {
        let mut app = app_with_remote("default");
        let pane = attached_pane(&mut app);
        let terminal_id = pane.terminal_id.clone();
        app.state.workspaces.clear();
        app.state.active = None;

        app.state.request_remote_detach_view = Some(pane);
        app.drain_remote_detach_view_request();

        assert!(app.terminal_runtimes.get(&terminal_id).is_none());
        assert!(app.state.toast.is_none());
    }
}
