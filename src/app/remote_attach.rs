use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Direction, Rect};

use super::{
    state::{Mode, PendingRemoteAttach, RemoteAttachPaneTarget, ToastKind, ToastNotification},
    App,
};
use crate::remote_source::RemoteAttachTarget;
use crate::remote_target::{plan_target_route, PlannedTargetRoute, RemoteTargetSelector};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteAttachPrecheckError {
    RemoteNotConfigured,
    RouteMismatch(String),
    TargetNotConnected(&'static str),
}

impl std::fmt::Display for RemoteAttachPrecheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RemoteNotConfigured => {
                write!(
                    f,
                    "remote federation is disabled or has no configured hosts"
                )
            }
            Self::RouteMismatch(detail) => write!(f, "remote attach target is invalid: {detail}"),
            Self::TargetNotConnected(status) => {
                write!(
                    f,
                    "remote host is {status}; wait for it to reconnect before attaching"
                )
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteAttachApplyError {
    PaneMissing,
    TerminalMissing,
    Precheck(RemoteAttachPrecheckError),
    CurrentExe(String),
    Spawn(String),
}

impl std::fmt::Display for RemoteAttachApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PaneMissing => write!(f, "focused pane no longer exists"),
            Self::TerminalMissing => write!(f, "focused pane terminal no longer exists"),
            Self::Precheck(err) => write!(f, "{err}"),
            Self::CurrentExe(err) => write!(f, "failed to locate current Herdr binary: {err}"),
            Self::Spawn(err) => write!(f, "failed to start remote attach: {err}"),
        }
    }
}

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

pub(crate) fn remote_attach_argv_from_exe(exe: &Path, target: &RemoteAttachTarget) -> Vec<String> {
    vec![
        exe.display().to_string(),
        "agent".to_string(),
        "attach".to_string(),
        format!("{}/terminal:{}", target.host, target.terminal_id),
    ]
}

fn remote_attach_target_string(target: &RemoteAttachTarget) -> String {
    format!("{}/terminal:{}", target.host, target.terminal_id)
}

fn remote_attach_display_label(target: &RemoteAttachTarget) -> String {
    let label = target.label.trim();
    if !label.is_empty() {
        return label.to_string();
    }
    format!("{}/terminal:{}", target.host, target.terminal_id)
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

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

impl App {
    pub(crate) fn drain_remote_attach_request(&mut self) {
        let Some(request) = self.state.request_remote_attach.take() else {
            return;
        };
        self.begin_remote_attach_request(request);
    }

    pub(crate) fn drain_remote_attach_in_new_split_request(&mut self) {
        let Some(target) = self.state.request_remote_attach_in_new_split.take() else {
            return;
        };
        self.begin_remote_attach_in_new_split_request(target);
    }

    pub(crate) fn drain_remote_detach_view_request(&mut self) {
        let Some(pane) = self.state.request_remote_detach_view.take() else {
            return;
        };
        self.detach_remote_attach_view_or_toast(pane);
    }

    pub(crate) fn begin_remote_attach_request(&mut self, request: PendingRemoteAttach) {
        if self.focus_existing_remote_attach(&request.target) {
            return;
        }

        if let Err(err) = self.precheck_remote_attach_target(&request.target) {
            toast_error(self, "attach unavailable", err.to_string());
            set_terminal_mode(self);
            return;
        }

        if self
            .state
            .remote_attach_pane_indices(&request.pane)
            .is_none()
        {
            toast_error(
                self,
                "attach unavailable",
                RemoteAttachApplyError::PaneMissing.to_string(),
            );
            set_terminal_mode(self);
            return;
        }

        if self.remote_attach_pane_is_safe(&request.pane) {
            self.apply_remote_attach_or_toast(request);
        } else {
            self.state.pending_remote_attach = Some(request);
            self.state.mode = Mode::ConfirmRemoteAttach;
        }
    }

    pub(crate) fn begin_remote_attach_in_new_split_request(&mut self, target: RemoteAttachTarget) {
        match self.apply_remote_attach_in_new_split(target) {
            Ok(()) => {}
            Err(err) => {
                toast_error(self, "attach unavailable", err.to_string());
                set_terminal_mode(self);
            }
        }
    }

    pub(crate) fn handle_confirm_remote_attach_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.confirm_remote_attach_accept(),
            KeyCode::Esc => self.confirm_remote_attach_cancel(),
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                self.confirm_remote_attach_cancel()
            }
            _ => {}
        }
    }

    pub(crate) fn handle_confirm_remote_attach_mouse(&mut self, mouse: MouseEvent) -> bool {
        if self.state.mode != Mode::ConfirmRemoteAttach {
            return false;
        }
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return true;
        }
        let Some(inner) =
            crate::ui::confirm_remote_attach_inner_rect(self.state.view.terminal_area)
        else {
            self.confirm_remote_attach_cancel();
            return true;
        };
        let (confirm, _cancel) = crate::ui::confirm_remote_attach_button_rects(inner);
        if rect_contains(confirm, mouse.column, mouse.row) {
            self.confirm_remote_attach_accept();
        } else {
            self.confirm_remote_attach_cancel();
        }
        true
    }

    pub(crate) fn confirm_remote_attach_accept(&mut self) {
        let Some(request) = self.state.pending_remote_attach.take() else {
            set_terminal_mode(self);
            return;
        };
        self.apply_remote_attach_or_toast(request);
    }

    pub(crate) fn confirm_remote_attach_cancel(&mut self) {
        self.state.pending_remote_attach = None;
        set_terminal_mode(self);
    }

    fn apply_remote_attach_or_toast(&mut self, request: PendingRemoteAttach) {
        match self.apply_remote_attach(request) {
            Ok(()) => {}
            Err(err) => {
                toast_error(self, "attach unavailable", err.to_string());
                set_terminal_mode(self);
            }
        }
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

    fn focus_existing_remote_attach(&mut self, target: &RemoteAttachTarget) -> bool {
        let Some((ws_idx, pane_id)) = self.state.find_remote_attach_pane(target) else {
            return false;
        };
        self.state.focus_pane_in_workspace(ws_idx, pane_id);
        set_terminal_mode(self);
        true
    }

    pub(crate) fn precheck_remote_attach_target(
        &self,
        target: &RemoteAttachTarget,
    ) -> Result<(), RemoteAttachPrecheckError> {
        if self.remote_hosts.list().is_empty() {
            return Err(RemoteAttachPrecheckError::RemoteNotConfigured);
        }

        match plan_target_route(&self.remote_hosts, &remote_attach_target_string(target)) {
            Ok(PlannedTargetRoute::Remote {
                host,
                target: RemoteTargetSelector::Terminal(terminal_id),
            }) if host.name == target.host
                && host.session == target.session
                && terminal_id == target.terminal_id => {}
            Ok(route) => {
                return Err(RemoteAttachPrecheckError::RouteMismatch(format!(
                    "unexpected route {route:?}"
                )));
            }
            Err(err) => return Err(RemoteAttachPrecheckError::RouteMismatch(err.to_string())),
        }

        // The host must be connected. Projection-derived terminal ids may not
        // be in the local agent cache; the authoritative remote attach server
        // rejects unknown/stale ids with a clear error, so a connected host is
        // sufficient to allow the attach attempt. This keeps cached agent
        // targets gated on the same host connection status.
        let host_key =
            crate::remote_source::RemoteHostKey::new(target.host.clone(), target.session.clone());
        let host_status = self.state.remote_sources.host_status(&host_key).ok_or(
            RemoteAttachPrecheckError::TargetNotConnected("disconnected"),
        )?;
        if !host_status.is_connected() {
            return Err(RemoteAttachPrecheckError::TargetNotConnected(
                host_status.stale_label().unwrap_or("disconnected"),
            ));
        }
        Ok(())
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

    pub(crate) fn remote_attach_pane_is_safe(&self, pane: &RemoteAttachPaneTarget) -> bool {
        let Some((ws_idx, _tab_idx)) = self.state.remote_attach_pane_indices(pane) else {
            return false;
        };
        let Some(terminal) = self.state.terminals.get(&pane.terminal_id) else {
            return false;
        };
        if terminal.remote_attach.is_some()
            || terminal.launch_argv.is_some()
            || terminal.is_agent_terminal()
        {
            return false;
        }
        self.state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane.pane_id)
            .and_then(crate::terminal::TerminalRuntime::foreground_is_pane_shell)
            == Some(true)
    }

    fn apply_remote_attach(
        &mut self,
        request: PendingRemoteAttach,
    ) -> Result<(), RemoteAttachApplyError> {
        let exe = std::env::current_exe()
            .map_err(|err| RemoteAttachApplyError::CurrentExe(err.to_string()))?;
        self.apply_remote_attach_with_exe(request, &exe)
    }

    fn apply_remote_attach_in_new_split(
        &mut self,
        target: RemoteAttachTarget,
    ) -> Result<(), RemoteAttachApplyError> {
        let exe = std::env::current_exe()
            .map_err(|err| RemoteAttachApplyError::CurrentExe(err.to_string()))?;
        self.apply_remote_attach_in_new_split_with_exe(target, &exe)
    }

    fn apply_remote_attach_in_new_split_with_exe(
        &mut self,
        target: RemoteAttachTarget,
        exe: &Path,
    ) -> Result<(), RemoteAttachApplyError> {
        if self.focus_existing_remote_attach(&target) {
            return Ok(());
        }
        self.precheck_remote_attach_target(&target)
            .map_err(RemoteAttachApplyError::Precheck)?;

        let ws_idx = self
            .state
            .active
            .ok_or(RemoteAttachApplyError::PaneMissing)?;
        let focused_pane = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(crate::workspace::Workspace::focused_pane_id)
            .ok_or(RemoteAttachApplyError::PaneMissing)?;
        let follow_cwd = self.state.workspaces.get(ws_idx).and_then(|ws| {
            let tab = ws.active_tab()?;
            tab.cwd_for_pane(focused_pane, &self.state.terminals, &self.terminal_runtimes)
        });
        let cwd = Some(self.resolve_new_terminal_cwd(follow_cwd));
        let (rows, cols) = self.state.estimate_pane_size();
        let new_rows = (rows / 2).max(4);
        let new_cols = (cols / 2).max(10);
        let previous_focus = self.state.current_pane_focus_target();
        let argv = remote_attach_argv_from_exe(exe, &target);
        let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
            return Err(RemoteAttachApplyError::PaneMissing);
        };
        let (_tab_idx, new_pane) = ws
            .split_pane_argv_command(
                focused_pane,
                Direction::Horizontal,
                new_rows,
                new_cols,
                cwd,
                &argv,
                Vec::new(),
                self.state.pane_scrollback_limit_bytes,
                self.state.host_terminal_theme,
                true,
            )
            .ok_or(RemoteAttachApplyError::PaneMissing)?
            .map_err(|err| RemoteAttachApplyError::Spawn(err.to_string()))?;
        let pane_id = new_pane.pane_id;
        let terminal_id = new_pane.terminal.id.clone();
        let mut terminal = new_pane.terminal;
        terminal.remote_attach = Some(target);
        terminal.respawn_shell_on_exit = false;
        terminal.launch_argv = None;
        terminal.runtime_only_remote_attach_view = true;
        self.terminal_runtimes
            .insert(terminal_id.clone(), new_pane.runtime);
        self.state.remove_alias_shadowed_by_new_pane(pane_id);
        self.state.terminals.insert(terminal_id, terminal);
        self.state
            .record_pane_focus_change(previous_focus, ws_idx, pane_id);
        // Remote attach placement is runtime-only. Do not schedule a session
        // save for a split that exists solely to host the local attach client.
        set_terminal_mode(self);
        self.render_dirty
            .store(true, std::sync::atomic::Ordering::Release);
        self.render_notify.notify_one();
        Ok(())
    }

    fn apply_remote_attach_with_exe(
        &mut self,
        request: PendingRemoteAttach,
        exe: &Path,
    ) -> Result<(), RemoteAttachApplyError> {
        if self.focus_existing_remote_attach(&request.target) {
            return Ok(());
        }
        self.precheck_remote_attach_target(&request.target)
            .map_err(RemoteAttachApplyError::Precheck)?;

        let (ws_idx, _tab_idx) = self
            .state
            .remote_attach_pane_indices(&request.pane)
            .ok_or(RemoteAttachApplyError::PaneMissing)?;
        let terminal_id = request.pane.terminal_id.clone();
        let terminal = self
            .state
            .terminals
            .get(&terminal_id)
            .ok_or(RemoteAttachApplyError::TerminalMissing)?;
        let cwd = terminal.cwd.clone();
        let (rows, cols) = self
            .terminal_runtimes
            .get(&terminal_id)
            .map(crate::terminal::TerminalRuntime::current_size)
            .unwrap_or_else(|| self.state.estimate_pane_size());
        let launch_env = self
            .pane_launch_env(ws_idx, request.pane.pane_id, Vec::new())
            .unwrap_or_default();
        let argv = remote_attach_argv_from_exe(exe, &request.target);
        let runtime = crate::terminal::TerminalRuntime::spawn_argv_command(
            request.pane.pane_id,
            rows,
            cols,
            cwd,
            &argv,
            &launch_env,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        )
        .map_err(|err| RemoteAttachApplyError::Spawn(err.to_string()))?;

        let old_runtime = self.terminal_runtimes.insert(terminal_id.clone(), runtime);
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.clear_agent_runtime_identity_for_replacement();
            terminal.remote_attach = Some(request.target.clone());
            terminal.respawn_shell_on_exit = true;
            terminal.launch_argv = None;
            terminal.runtime_only_remote_attach_view = false;
        }
        self.state
            .focus_pane_in_workspace(ws_idx, request.pane.pane_id);
        set_terminal_mode(self);
        self.render_dirty
            .store(true, std::sync::atomic::Ordering::Release);
        self.render_notify.notify_one();
        if let Some(old_runtime) = old_runtime {
            old_runtime.shutdown();
        }
        Ok(())
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

    fn pane_request(app: &mut App) -> PendingRemoteAttach {
        let workspace = Workspace::test_new("local");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        PendingRemoteAttach {
            target: target(),
            pane: RemoteAttachPaneTarget {
                workspace_id: app.state.workspaces[0].id.clone(),
                pane_id,
                terminal_id,
            },
        }
    }

    fn attached_pane(app: &mut App) -> RemoteAttachPaneTarget {
        let request = pane_request(app);
        let terminal = app
            .state
            .terminals
            .get_mut(&request.pane.terminal_id)
            .expect("terminal");
        terminal.remote_attach = Some(target());
        terminal.respawn_shell_on_exit = true;
        terminal.launch_argv = Some(vec![
            "herdr".into(),
            "agent".into(),
            "attach".into(),
            "jafar/terminal:term-1".into(),
        ]);
        request.pane
    }

    fn cache_connected_target(app: &mut App) {
        app.state.remote_sources.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![remote_agent("term-1")],
        );
    }

    #[test]
    fn remote_attach_argv_uses_direct_agent_attach_target() {
        let argv = remote_attach_argv_from_exe(Path::new("/bin/herdr"), &target());

        assert_eq!(
            argv,
            vec![
                "/bin/herdr".to_string(),
                "agent".to_string(),
                "attach".to_string(),
                "jafar/terminal:term-1".to_string()
            ]
        );
        assert!(!argv.iter().any(|arg| arg == "--takeover"));
    }

    #[test]
    fn remote_attach_precheck_accepts_connected_cached_target() {
        let mut app = app_with_remote("default");
        app.state.remote_sources.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![remote_agent("term-1")],
        );

        assert_eq!(app.precheck_remote_attach_target(&target()), Ok(()));
    }

    #[test]
    fn remote_attach_precheck_rejects_missing_remote_config() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );

        assert_eq!(
            app.precheck_remote_attach_target(&target()),
            Err(RemoteAttachPrecheckError::RemoteNotConfigured)
        );
    }

    #[test]
    fn remote_attach_precheck_rejects_route_session_mismatch() {
        let mut app = app_with_remote("agents");
        app.state.remote_sources.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![remote_agent("term-1")],
        );

        assert!(matches!(
            app.precheck_remote_attach_target(&target()),
            Err(RemoteAttachPrecheckError::RouteMismatch(_))
        ));
    }

    #[test]
    fn remote_attach_precheck_rejects_disconnected_host() {
        // No snapshot → host_status returns None → TargetNotConnected("disconnected").
        // This covers both an unknown terminal_id (projection-derived) and a
        // cached-but-host-unreachable case; the remote server validates the id.
        let mut app = app_with_remote("default");

        assert_eq!(
            app.precheck_remote_attach_target(&target()),
            Err(RemoteAttachPrecheckError::TargetNotConnected(
                "disconnected"
            ))
        );

        let host = RemoteHostKey::new("jafar", "default");
        app.state
            .remote_sources
            .replace_connected_snapshot(host.clone(), vec![remote_agent("term-1")]);
        app.state
            .remote_sources
            .mark_status(&host, RemoteConnectionStatus::Unreachable);

        assert_eq!(
            app.precheck_remote_attach_target(&target()),
            Err(RemoteAttachPrecheckError::TargetNotConnected("unreachable"))
        );
    }

    #[test]
    fn remote_attach_precheck_accepts_projection_terminal_id_not_in_agent_cache() {
        // Projection-derived terminal ids are not in the agent cache; the precheck
        // must allow them as long as the host is connected.
        let mut app = app_with_remote("default");
        app.state.remote_sources.replace_connected_snapshot(
            RemoteHostKey::new("jafar", "default"),
            vec![], // no agents in cache
        );

        let projection_target = RemoteAttachTarget {
            host: "jafar".into(),
            session: "default".into(),
            terminal_id: "proj-term-99".into(),
            label: "projection pane".into(),
        };

        assert_eq!(
            app.precheck_remote_attach_target(&projection_target),
            Ok(())
        );
    }

    #[test]
    fn remote_attach_request_for_unknown_safe_pane_opens_confirmation() {
        let mut app = app_with_remote("default");
        cache_connected_target(&mut app);
        let request = pane_request(&mut app);

        app.begin_remote_attach_request(request.clone());

        assert_eq!(app.state.mode, Mode::ConfirmRemoteAttach);
        assert_eq!(app.state.pending_remote_attach, Some(request));
    }

    #[test]
    fn remote_attach_confirm_accept_revalidates_missing_pane() {
        let mut app = app_with_remote("default");
        cache_connected_target(&mut app);
        let request = pane_request(&mut app);
        let terminal_id = request.pane.terminal_id.clone();

        app.begin_remote_attach_request(request);
        app.state.workspaces.clear();
        app.state.active = None;
        app.confirm_remote_attach_accept();

        assert_eq!(app.state.mode, Mode::Navigate);
        assert!(app.state.pending_remote_attach.is_none());
        assert!(app.terminal_runtimes.get(&terminal_id).is_none());
        assert!(app
            .state
            .terminals
            .get(&terminal_id)
            .and_then(|terminal| terminal.remote_attach.as_ref())
            .is_none());
        let toast = app.state.toast.as_ref().expect("missing pane toast");
        assert_eq!(toast.title, "attach unavailable");
        assert!(toast.context.contains("focused pane no longer exists"));
    }

    #[test]
    fn remote_attach_confirm_accept_revalidates_remote_status() {
        let mut app = app_with_remote("default");
        cache_connected_target(&mut app);
        let request = pane_request(&mut app);
        let terminal_id = request.pane.terminal_id.clone();

        app.begin_remote_attach_request(request);
        app.state.remote_sources.mark_status(
            &RemoteHostKey::new("jafar", "default"),
            RemoteConnectionStatus::Unreachable,
        );
        app.confirm_remote_attach_accept();

        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.pending_remote_attach.is_none());
        assert!(app.terminal_runtimes.get(&terminal_id).is_none());
        assert!(app
            .state
            .terminals
            .get(&terminal_id)
            .and_then(|terminal| terminal.remote_attach.as_ref())
            .is_none());
        let toast = app.state.toast.as_ref().expect("remote status toast");
        assert_eq!(toast.title, "attach unavailable");
        assert!(toast.context.contains("remote host is unreachable"));
    }

    #[test]
    fn remote_attach_spawn_failure_keeps_metadata_clear() {
        let mut app = app_with_remote("default");
        cache_connected_target(&mut app);
        let request = pane_request(&mut app);
        let terminal_id = request.pane.terminal_id.clone();

        let result = app.apply_remote_attach_with_exe(
            request,
            Path::new("/__herdr_missing_remote_attach_executable__"),
        );

        assert!(matches!(result, Err(RemoteAttachApplyError::Spawn(_))));
        assert!(app.terminal_runtimes.get(&terminal_id).is_none());
        let terminal = app.state.terminals.get(&terminal_id).unwrap();
        assert!(terminal.remote_attach.is_none());
        assert!(terminal.launch_argv.is_none());
    }

    #[tokio::test]
    async fn remote_attach_in_new_split_records_runtime_only_metadata() {
        let mut app = app_with_remote("default");
        cache_connected_target(&mut app);
        let request = pane_request(&mut app);
        let original_pane = request.pane.pane_id;
        let original_terminal_id = request.pane.terminal_id.clone();

        app.apply_remote_attach_in_new_split_with_exe(target(), Path::new("/bin/true"))
            .expect("remote attach split should spawn argv child");

        let ws = &app.state.workspaces[0];
        let focused_pane = ws.focused_pane_id().expect("focused split pane");
        assert_ne!(focused_pane, original_pane);
        assert_eq!(ws.tabs[0].panes.len(), 2);
        let focused_terminal_id = ws.terminal_id(focused_pane).cloned().unwrap();
        assert_ne!(focused_terminal_id, original_terminal_id);

        let original_terminal = app.state.terminals.get(&original_terminal_id).unwrap();
        assert!(original_terminal.remote_attach.is_none());
        assert!(original_terminal.launch_argv.is_none());

        let split_terminal = app.state.terminals.get(&focused_terminal_id).unwrap();
        assert_eq!(split_terminal.remote_attach, Some(target()));
        assert!(split_terminal.launch_argv.is_none());
        assert!(!split_terminal.respawn_shell_on_exit);
        assert!(split_terminal.runtime_only_remote_attach_view);
        assert!(!split_terminal.is_agent_terminal());
        let split_runtime = app
            .terminal_runtimes
            .remove(&focused_terminal_id)
            .expect("split attach runtime");
        split_runtime.shutdown();

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[tokio::test]
    async fn remote_attach_in_new_split_exit_closes_runtime_only_pane() {
        let mut app = app_with_remote("default");
        cache_connected_target(&mut app);
        let request = pane_request(&mut app);
        let original_pane = request.pane.pane_id;

        app.apply_remote_attach_in_new_split_with_exe(target(), Path::new("/bin/true"))
            .expect("remote attach split should spawn argv child");
        let attach_pane = app.state.workspaces[0]
            .focused_pane_id()
            .expect("focused attach pane");
        assert_ne!(attach_pane, original_pane);

        app.handle_internal_event(crate::events::AppEvent::PaneDied {
            pane_id: attach_pane,
        });

        assert!(app.state.workspaces[0]
            .find_tab_index_for_pane(attach_pane)
            .is_none());
        assert!(app.state.workspaces[0]
            .find_tab_index_for_pane(original_pane)
            .is_some());
        assert_eq!(app.state.workspaces[0].tabs[0].panes.len(), 1);

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[test]
    fn remote_attach_in_new_split_spawn_failure_rolls_back_split() {
        let mut app = app_with_remote("default");
        cache_connected_target(&mut app);
        let request = pane_request(&mut app);
        let original_terminal_id = request.pane.terminal_id.clone();

        let result = app.apply_remote_attach_in_new_split_with_exe(
            target(),
            Path::new("/__herdr_missing_remote_attach_executable__"),
        );

        assert!(matches!(result, Err(RemoteAttachApplyError::Spawn(_))));
        assert_eq!(app.state.workspaces[0].tabs[0].panes.len(), 1);
        assert_eq!(app.terminal_runtimes.len(), 0);
        let terminal = app.state.terminals.get(&original_terminal_id).unwrap();
        assert!(terminal.remote_attach.is_none());
        assert!(terminal.launch_argv.is_none());
    }

    #[tokio::test]
    async fn remote_attach_apply_records_runtime_only_metadata() {
        let mut app = app_with_remote("default");
        cache_connected_target(&mut app);
        let request = pane_request(&mut app);
        let terminal_id = request.pane.terminal_id.clone();

        app.apply_remote_attach_with_exe(request, Path::new("/usr/bin/true"))
            .expect("remote attach should spawn argv child");

        let terminal = app.state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.remote_attach, Some(target()));
        assert!(terminal.launch_argv.is_none());
        assert!(terminal.respawn_shell_on_exit);
        assert!(!terminal.runtime_only_remote_attach_view);
        assert!(!terminal.is_agent_terminal());
        let runtime = app
            .terminal_runtimes
            .remove(&terminal_id)
            .expect("attach runtime");
        runtime.shutdown();
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
        let request = pane_request(&mut app);
        let terminal_id = request.pane.terminal_id.clone();

        app.state.request_remote_detach_view = Some(request.pane);
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
