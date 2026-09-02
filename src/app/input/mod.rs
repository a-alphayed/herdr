//! Input handling — translates crossterm key/mouse events into state mutations.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::app::PaneClickState;
use crate::input::TerminalKey;
use ratatui::layout::Direction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollbarClickTarget {
    Thumb { grab_row_offset: u16 },
    Track { offset_from_bottom: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
enum WheelRouting {
    HostScroll,
    MouseReport,
    AlternateScroll,
}

const WORKSPACE_DRAG_THRESHOLD: u16 = 1;
const TAB_DRAG_THRESHOLD: u16 = 1;

fn modified_url_click_modifier() -> KeyModifiers {
    KeyModifiers::CONTROL
}

fn translate_host_glass_mouse(
    body: ratatui::layout::Rect,
    mouse: MouseEvent,
) -> Option<crate::protocol::ClientInputEvent> {
    if body.width == 0
        || body.height == 0
        || mouse.column < body.x
        || mouse.row < body.y
        || mouse.column >= body.x.saturating_add(body.width)
        || mouse.row >= body.y.saturating_add(body.height)
    {
        return None;
    }
    Some(crate::protocol::ClientInputEvent::Mouse {
        kind: crate::protocol::ClientMouseKind::from_crossterm(mouse.kind)?,
        column: mouse.column - body.x,
        row: mouse.row - body.y,
        modifiers: mouse.modifiers.bits(),
    })
}
#[cfg(test)]
#[test]
fn modified_url_click_modifier_matches_terminal_mouse_reporting() {
    assert_eq!(modified_url_click_modifier(), KeyModifiers::CONTROL);
}

mod copy_mode;
mod modal;
mod mouse;
mod navigate;
mod overlays;
mod selection;
mod settings;
mod sidebar;
mod terminal;

pub(crate) use self::{
    modal::{
        handle_global_menu_key, handle_keybind_help_key, handle_navigator_key,
        insert_navigator_search_text, insert_rename_input_text,
    },
    navigate::terminal_direct_navigation_action,
    settings::open_settings_at,
};
use self::{
    modal::{
        modal_action_from_key, ModalAction, ONBOARDING_WELCOME_ACTIONS, RELEASE_NOTES_ACTIONS,
    },
    mouse::MouseAction,
    settings::SettingsAction,
};
use super::state::{AppState, Mode};
use super::App;

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

impl App {
    pub(super) async fn handle_key(&mut self, key: TerminalKey) {
        let key_event = key.as_key_event();
        if modal_paste_target_active(&self.state) && is_modal_paste_shortcut(&key_event) {
            if let Some(text) = crate::platform::read_clipboard_text() {
                self.paste_into_active_text_input(&text);
            }
            return;
        }

        let previous_toast = self.state.toast.clone();
        match self.state.mode {
            Mode::Terminal => self.handle_terminal_key(key).await,
            Mode::Prefix => self.handle_prefix_key(key),
            Mode::Navigate => self.handle_navigate_key(key),
            Mode::Copy => self.handle_copy_mode_key(key),
            _ => match self.state.mode {
                Mode::Onboarding => self.handle_onboarding_key(key_event),
                Mode::ReleaseNotes => self.handle_release_notes_key(key_event),
                Mode::ProductAnnouncement => self.handle_product_announcement_key(key_event),
                Mode::Prefix | Mode::Navigate | Mode::Copy => unreachable!(),
                Mode::RenameWorkspace | Mode::RenameTab | Mode::RenamePane => {
                    self.handle_rename_key_via_api(key_event)
                }
                Mode::NewLinkedWorktree => self.handle_worktree_create_key(key_event),
                Mode::OpenExistingWorktree => self.handle_worktree_open_key(key_event),
                Mode::ConfirmRemoveWorktree => self.handle_worktree_remove_key(key_event),
                Mode::Resize => self.handle_resize_key_via_api(key),
                Mode::ConfirmClose => self.handle_confirm_close_key_via_api(key_event),
                Mode::ConfirmRemoteProjectedPaneClose => {
                    self.handle_confirm_remote_projected_pane_close_key(key_event)
                }
                Mode::ConfirmRemoteProjectedTabClose => {
                    self.handle_confirm_remote_projected_tab_close_key(key_event)
                }
                Mode::ContextMenu => {
                    self.handle_context_menu_key_via_api(key_event);
                }
                Mode::Settings => self.handle_settings_key(key_event),
                Mode::GlobalMenu => handle_global_menu_key(&mut self.state, key_event),
                Mode::KeybindHelp => handle_keybind_help_key(&mut self.state, key_event),
                Mode::Navigator => {
                    handle_navigator_key(&mut self.state, &self.terminal_runtimes, key_event)
                }
                Mode::Terminal => unreachable!(),
            },
        }
        self.drain_remote_detach_view_request();
        self.sync_toast_deadline(previous_toast);
    }

    pub(super) async fn handle_paste(&mut self, text: String) {
        if self.state.mode != Mode::Terminal {
            self.paste_into_active_text_input(&text);
            return;
        }

        // Glass uses the full-App structured input path. Until S2 removes the
        // projection input/action path, a selected remote source remains a
        // fail-closed authority boundary and never pastes into a local pane.
        if self.state.host_glass_surface_active() {
            let _ = self.route_host_glass_input(crate::protocol::ClientInputEvent::Paste { text });
            return;
        }
        if self.state.remote_projection_surface_active() {
            return;
        }

        if let Some(ws_idx) = self.state.active {
            if let Some(rt) = self
                .state
                .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
            {
                let _ = rt.send_paste(text).await;
            }
        }
    }

    pub(crate) fn paste_into_active_text_input(&mut self, text: &str) -> bool {
        match self.state.mode {
            Mode::RenameWorkspace | Mode::RenameTab | Mode::RenamePane => {
                insert_rename_input_text(&mut self.state, text);
                true
            }
            Mode::NewLinkedWorktree => {
                self.insert_worktree_create_text(text);
                true
            }
            Mode::OpenExistingWorktree => {
                if !self
                    .state
                    .worktree_open
                    .as_ref()
                    .is_some_and(|open| open.search_focused)
                {
                    return false;
                }
                self.insert_worktree_open_search_text(text);
                true
            }
            Mode::Navigator => {
                if !self.state.navigator.search_focused {
                    return false;
                }
                insert_navigator_search_text(&mut self.state, &self.terminal_runtimes, text);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn handle_onboarding_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Right | KeyCode::Char('l') => self.open_settings_from_onboarding(),
            _ => {
                if let Some(ModalAction::Continue) =
                    modal_action_from_key(&key, ONBOARDING_WELCOME_ACTIONS)
                {
                    self.open_settings_from_onboarding();
                }
            }
        }
    }

    pub(crate) fn handle_release_notes_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll_release_notes(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_release_notes(1),
            KeyCode::PageUp => self.scroll_release_notes(-8),
            KeyCode::PageDown => self.scroll_release_notes(8),
            KeyCode::Home => {
                if let Some(notes) = &mut self.state.release_notes {
                    notes.scroll = 0;
                }
            }
            KeyCode::End => {
                let max_scroll = self.state.release_notes_max_scroll();
                if let Some(notes) = &mut self.state.release_notes {
                    notes.scroll = max_scroll;
                }
            }
            _ => {
                if let Some(ModalAction::Close) = modal_action_from_key(&key, RELEASE_NOTES_ACTIONS)
                {
                    self.dismiss_release_notes();
                }
            }
        }
    }

    pub(crate) fn handle_product_announcement_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll_product_announcement(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_product_announcement(1),
            KeyCode::PageUp => self.scroll_product_announcement(-8),
            KeyCode::PageDown => self.scroll_product_announcement(8),
            KeyCode::Home => {
                if let Some(announcement) = &mut self.state.product_announcement {
                    announcement.scroll = 0;
                }
            }
            KeyCode::End => {
                let max_scroll = self.state.product_announcement_max_scroll();
                if let Some(announcement) = &mut self.state.product_announcement {
                    announcement.scroll = max_scroll;
                }
            }
            _ => {
                if let Some(ModalAction::Close) = modal_action_from_key(&key, RELEASE_NOTES_ACTIONS)
                {
                    self.dismiss_product_announcement();
                }
            }
        }
    }

    /// Keep the projection-era mouse authority boundary until S2 removes its
    /// action/data path. Presentation no longer consumes these hit areas, but
    /// a retained hit still consumes focused-pane terminal input fail-closed
    /// while right-click chrome and non-focused focus actions stay local.
    fn handle_remote_projection_terminal_mouse(&mut self, mouse: MouseEvent) -> bool {
        if self.state.mode != Mode::Terminal || self.state.selected_remote_space.is_none() {
            return false;
        }

        let Some(hit) = self
            .state
            .view
            .remote_projection_hit_areas
            .iter()
            .find(|hit| {
                mouse.column >= hit.rect.x
                    && mouse.column < hit.rect.x.saturating_add(hit.rect.width)
                    && mouse.row >= hit.rect.y
                    && mouse.row < hit.rect.y.saturating_add(hit.rect.height)
            })
        else {
            return false;
        };

        !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Right)) && hit.focused
    }

    /// Forward mouse input over the selected full-App glass using the exact
    /// PTY-free body geometry advertised in Hello/Resize. The persistent
    /// local indicator and host rail are never forwarded; the rail therefore
    /// remains the unconditional mouse escape hatch.
    fn handle_host_glass_mouse(&mut self, mouse: MouseEvent) -> bool {
        if self.state.mode != Mode::Terminal || !self.state.host_glass_surface_active() {
            return false;
        }

        let rail = self.state.view.host_rail_rect;
        if mouse.column >= rail.x
            && mouse.column < rail.x.saturating_add(rail.width)
            && mouse.row >= rail.y
            && mouse.row < rail.y.saturating_add(rail.height)
        {
            return false;
        }

        // Preserve the existing clickable local notification above glass.
        let toast = self.state.view.toast_hit_area;
        if self
            .state
            .toast
            .as_ref()
            .is_some_and(|toast| toast.target.is_some())
            && mouse.column >= toast.x
            && mouse.column < toast.x.saturating_add(toast.width)
            && mouse.row >= toast.y
            && mouse.row < toast.y.saturating_add(toast.height)
        {
            return false;
        }

        let area = self.state.view.terminal_area;
        if mouse.column < area.x
            || mouse.column >= area.x.saturating_add(area.width)
            || mouse.row < area.y
            || mouse.row >= area.y.saturating_add(area.height)
        {
            return false;
        }
        let body = crate::ui::host_glass_body_area(area);
        let Some(event) = translate_host_glass_mouse(body, mouse) else {
            // The one-row glass identity/status indicator is local chrome.
            return true;
        };
        let _ = self.route_host_glass_input(event);
        true
    }
    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) {
        if self.handle_overlay_mouse(mouse) {
            return;
        }

        if self.handle_host_glass_mouse(mouse) {
            return;
        }

        if self.handle_remote_projection_terminal_mouse(mouse) {
            return;
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.state.on_sidebar_divider(mouse.column, mouse.row)
        {
            let now = std::time::Instant::now();
            let is_double_click = self
                .last_sidebar_divider_click
                .is_some_and(|last| now.duration_since(last) <= super::SIDEBAR_DOUBLE_CLICK_WINDOW);
            self.last_sidebar_divider_click = Some(now);

            if is_double_click {
                self.state.sidebar_width = self.state.default_sidebar_width;
                self.state.sidebar_width_source =
                    crate::app::state::SidebarWidthSource::ConfigDefault;
                self.state.sidebar_width_auto = false;
                self.state.mark_session_dirty();
                self.state.drag = None;
                return;
            }
        }

        if self.handle_modified_url_click(mouse) {
            return;
        }

        let handled_pane_double_click = self.handle_pane_double_click(mouse);

        let previous_toast = self.state.toast.clone();
        let previous_agent_panel_sort = self.state.agent_panel_sort;
        let previous_settings_section = self.state.settings.section;
        if !handled_pane_double_click {
            let right_button = matches!(
                mouse.kind,
                MouseEventKind::Down(MouseButton::Right)
                    | MouseEventKind::Up(MouseButton::Right)
                    | MouseEventKind::Drag(MouseButton::Right)
            );
            let intentional_pane_press = matches!(
                mouse.kind,
                MouseEventKind::Down(MouseButton::Left | MouseButton::Middle)
            );
            if !right_button
                && intentional_pane_press
                && matches!(self.state.mode, Mode::Terminal | Mode::Resize)
            {
                if let (Some(ws_idx), Some(info)) = (
                    self.state.active,
                    self.state.pane_at(mouse.column, mouse.row).cloned(),
                ) {
                    self.focus_pane_internal_via_api(ws_idx, info.id);
                }
            }
            if let Some(action) = self.state.handle_mouse(&mut self.terminal_runtimes, mouse) {
                match action {
                    MouseAction::Settings(action) => match action {
                        SettingsAction::SaveTheme(name) => self.save_theme(&name),
                        SettingsAction::SaveSound(enabled) => self.save_sound(enabled),
                        SettingsAction::SaveToastDelivery(delivery) => {
                            self.save_toast_delivery(delivery)
                        }
                        SettingsAction::SaveAgentBorderLabels(enabled) => {
                            self.save_agent_border_labels(enabled)
                        }
                        SettingsAction::SavePaneHistory(enabled) => {
                            self.save_pane_history_persistence(enabled)
                        }
                        SettingsAction::SaveSwitchAsciiInputSourceInPrefix(enabled) => {
                            self.save_switch_ascii_input_source_in_prefix(enabled)
                        }
                        SettingsAction::SaveHostGlass(enabled) => self.save_host_glass(enabled),
                        SettingsAction::InstallRecommendedIntegrations => {
                            self.install_recommended_integrations()
                        }
                    },
                    MouseAction::FocusWorkspace { ws_idx } => {
                        self.focus_workspace_idx_via_api(ws_idx)
                    }
                    MouseAction::FocusTab { tab_idx } => self.focus_tab_idx_via_api(tab_idx),
                    MouseAction::FocusPane { ws_idx, pane_id } => {
                        self.focus_pane_internal_via_api(ws_idx, pane_id)
                    }
                    MouseAction::RemoteProjectedTabCreate { target } => {
                        self.create_remote_projected_tab_via_api(target);
                    }
                    MouseAction::RemoteProjectedTabFocus { target } => {
                        self.focus_remote_projected_tab_via_api(target);
                    }
                    MouseAction::RemoteProjectedPaneFocus { target } => {
                        self.focus_remote_projected_pane_via_api(target);
                    }
                    MouseAction::FocusToastTarget => self.focus_toast_target_via_api(),
                    MouseAction::MoveWorkspace {
                        source_ws_idx,
                        insert_idx,
                    } => self.move_workspace_via_api(source_ws_idx, insert_idx),
                    MouseAction::MoveTab {
                        ws_idx,
                        source_tab_idx,
                        insert_idx,
                    } => self.move_tab_via_api(ws_idx, source_tab_idx, insert_idx),
                    MouseAction::SetSplitRatio { path, ratio } => {
                        self.set_split_ratio_via_api(path, ratio)
                    }
                    MouseAction::RenameModal(action) => {
                        self.apply_rename_mouse_action_via_api(action)
                    }
                    MouseAction::ConfirmCloseAccept => self.confirm_close_accept_via_api(),
                    MouseAction::ContextMenu { menu, idx } => {
                        self.apply_context_menu_action_via_api(menu, idx)
                    }
                }
            }
        }
        self.drain_remote_detach_view_request();
        self.sync_toast_deadline(previous_toast);
        if previous_settings_section != crate::app::state::SettingsSection::Integrations
            && self.state.settings.section == crate::app::state::SettingsSection::Integrations
        {
            self.refresh_integration_recommendations();
        }
        if self.state.agent_panel_sort != previous_agent_panel_sort {
            self.save_agent_panel_sort(self.state.agent_panel_sort);
        }

        self.queue_pending_clipboard_write();

        // Sync autoscroll deadline with state (mouse handler may have
        // set or cleared selection_autoscroll during handle_mouse).
        if self.state.selection_autoscroll.is_none() {
            self.selection_autoscroll_deadline = None;
        } else if self.selection_autoscroll_deadline.is_none() {
            self.selection_autoscroll_deadline =
                Some(std::time::Instant::now() + super::SELECTION_AUTOSCROLL_INTERVAL);
        }
    }

    fn handle_modified_url_click(&mut self, mouse: MouseEvent) -> bool {
        if self.state.mode != Mode::Terminal
            || !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            || !mouse.modifiers.contains(modified_url_click_modifier())
        {
            return false;
        }

        if self.state.selected_remote_space.is_some() {
            return false;
        }

        let Some(info) = self.state.pane_at(mouse.column, mouse.row).cloned() else {
            return false;
        };
        let viewport_row = mouse.row.saturating_sub(info.inner_rect.y);
        let col = mouse.column.saturating_sub(info.inner_rect.x);
        let Some(url) =
            self.state
                .url_at_pane_cell(&self.terminal_runtimes, info.id, viewport_row, col)
        else {
            return false;
        };

        self.last_pane_click = None;
        match self.invoke_plugin_link_handler_for_url(&url, info.id) {
            Ok(true) => return true,
            Ok(false) => {}
            Err(err) => {
                tracing::warn!(err = %err, url = %url, "failed to invoke plugin link handler");
            }
        }
        if let Err(err) = crate::platform::open_url(&url) {
            tracing::warn!(err = %err, url = %url, "failed to open pane URL");
        }
        true
    }

    fn handle_pane_double_click(&mut self, mouse: MouseEvent) -> bool {
        // A pane press stops being a double-click candidate once it becomes
        // a drag or completes as a real text selection.
        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                self.last_pane_click = None;
                return false;
            }
            MouseEventKind::Up(MouseButton::Left)
                if self
                    .state
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.is_visible()) =>
            {
                self.last_pane_click = None;
                return false;
            }
            _ => {}
        }

        // Only terminal-pane left-clicks can start this gesture; other clicks
        // should keep their existing mouse behavior and clear stale candidates.
        let Some(click) = self.pane_click_candidate(mouse) else {
            return false;
        };

        // Require the second click to land near the first click in the same pane
        // and within the double-click window so adjacent interactions do not copy.
        if !self.take_pane_double_click(click) {
            return false;
        }

        // Preserve a short highlight after copying so the user gets visible
        // confirmation without leaving a persistent selection behind.
        self.copy_double_clicked_word(click)
    }

    fn pane_click_candidate(&mut self, mouse: MouseEvent) -> Option<PaneClickState> {
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return None;
        }

        if !mouse.modifiers.is_empty() {
            self.last_pane_click = None;
            return None;
        }

        if self.state.mode != Mode::Terminal {
            self.last_pane_click = None;
            return None;
        }

        let Some(info) = self.state.pane_at(mouse.column, mouse.row).cloned() else {
            self.last_pane_click = None;
            return None;
        };

        Some(PaneClickState {
            pane_id: info.id,
            viewport_row: mouse.row - info.inner_rect.y,
            col: mouse.column - info.inner_rect.x,
            at: std::time::Instant::now(),
        })
    }

    fn take_pane_double_click(&mut self, click: PaneClickState) -> bool {
        if !self
            .last_pane_click
            .is_some_and(|last| last.is_double_click_for(click))
        {
            self.last_pane_click = Some(click);
            return false;
        }

        self.last_pane_click = None;
        true
    }

    fn copy_double_clicked_word(&mut self, click: PaneClickState) -> bool {
        let copied = self.state.copy_word_at_pane_cell(
            &self.terminal_runtimes,
            click.pane_id,
            click.viewport_row,
            click.col,
        );
        if copied {
            self.selection_highlight_clear_deadline =
                Some(std::time::Instant::now() + super::PANE_COPY_HIGHLIGHT_DURATION);
        }
        copied
    }
}

pub(crate) fn is_modal_paste_shortcut(key: &KeyEvent) -> bool {
    if !matches!(key.code, KeyCode::Char('v' | 'V')) {
        return false;
    }

    #[cfg(target_os = "macos")]
    {
        key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::CONTROL)
    }

    #[cfg(not(target_os = "macos"))]
    {
        key.modifiers.contains(KeyModifiers::CONTROL)
    }
}

pub(crate) fn modal_paste_target_active(state: &AppState) -> bool {
    match state.mode {
        Mode::RenameWorkspace | Mode::RenameTab | Mode::RenamePane | Mode::NewLinkedWorktree => {
            true
        }
        Mode::OpenExistingWorktree => state
            .worktree_open
            .as_ref()
            .is_some_and(|open| open.search_focused),
        Mode::Navigator => state.navigator.search_focused,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Mouse handling
// ---------------------------------------------------------------------------

// Note: split_pane needs runtime (event_tx for PTY spawn), so it lives on App
impl AppState {
    pub(crate) fn split_pane(
        &mut self,
        terminal_runtimes: &mut crate::terminal::TerminalRuntimeRegistry,
        direction: Direction,
    ) {
        // Actual PTY spawning happens in Workspace::split_focused
        // which needs events channel — this is called from navigate_key
        // where we don't have async context, so the workspace handles it
        let (rows, cols) = self.estimate_pane_size();
        let new_rows = (rows / 2).max(4);
        let new_cols = (cols / 2).max(10);

        let follow_cwd = self
            .active
            .and_then(|i| self.workspaces.get(i))
            .and_then(|ws| {
                let tab = ws.active_tab()?;
                tab.cwd_for_pane(tab.layout.focused(), &self.terminals, terminal_runtimes)
            });
        let cwd = Some(super::creation::resolve_new_terminal_cwd(
            &self.new_terminal_cwd,
            follow_cwd,
        ));

        let previous_focus = self.current_pane_focus_target();
        if let Some(ws_idx) = self.active {
            let Some(ws) = self.workspaces.get_mut(ws_idx) else {
                return;
            };
            if let Ok(new_pane) = ws.split_focused(
                direction,
                new_rows,
                new_cols,
                cwd,
                self.pane_scrollback_limit_bytes,
                self.host_terminal_theme,
                crate::pane::PaneShellConfig::new(&self.default_shell, self.shell_mode),
                Vec::new(),
            ) {
                let new_id = new_pane.pane_id;
                terminal_runtimes.insert(new_pane.terminal.id.clone(), new_pane.runtime);
                self.remove_alias_shadowed_by_new_pane(new_id);
                self.terminals
                    .insert(new_pane.terminal.id.clone(), new_pane.terminal);
                self.record_pane_focus_change(previous_focus, ws_idx, new_id);
                self.mark_session_dirty();
                self.mode = Mode::Terminal;
            }
        }
    }
}

#[cfg(test)]
fn state_with_workspaces(names: &[&str]) -> AppState {
    let mut state = AppState::test_new();
    state.workspaces = names
        .iter()
        .map(|name| crate::workspace::Workspace::test_new(name))
        .collect();
    if !state.workspaces.is_empty() {
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Navigate;
    }
    state
}

#[cfg(test)]
fn app_for_mouse_test() -> App {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &crate::config::Config::default(),
        true,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state.mode = Mode::Terminal;
    app.state.update_available = None;
    app.state.latest_release_notes_available = false;
    app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 26, 20);
    app.state.view.sidebar_panel_rect = app.state.view.sidebar_rect;
    app.state.view.terminal_area = ratatui::layout::Rect::new(26, 0, 80, 20);
    app
}

#[cfg(test)]
fn mouse(
    kind: crossterm::event::MouseEventKind,
    col: u16,
    row: u16,
) -> crossterm::event::MouseEvent {
    crossterm::event::MouseEvent {
        kind,
        column: col,
        row,
        modifiers: crossterm::event::KeyModifiers::empty(),
    }
}

#[cfg(test)]
fn numbered_lines_bytes(count: usize) -> Vec<u8> {
    (0..count)
        .map(|i| format!("{i:06}\r\n"))
        .collect::<String>()
        .into_bytes()
}

#[cfg(test)]
fn capture_snapshot(state: &AppState) -> crate::persist::SessionSnapshot {
    let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
    crate::persist::capture(
        &state.workspaces,
        &state.terminals,
        &terminal_runtimes,
        state.active,
        state.selected,
        state.sidebar_width,
        state.sidebar_section_split,
        state.collapsed_space_keys.clone(),
    )
}

#[cfg(test)]
fn root_layout_ratio(snapshot: &crate::persist::SessionSnapshot) -> Option<f32> {
    match &snapshot.workspaces.first()?.tabs.first()?.layout {
        crate::persist::LayoutSnapshot::Split { ratio, .. } => Some(*ratio),
        crate::persist::LayoutSnapshot::Pane(_) => None,
    }
}

#[cfg(test)]
fn unique_temp_path(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("herdr-{name}-{}-{nanos}", std::process::id()))
}

#[cfg(test)]
#[cfg(unix)]
fn wait_for_file(path: &std::path::Path) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if let Ok(content) = std::fs::read_to_string(path) {
            if !content.is_empty() {
                return content;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("timed out waiting for {}", path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        )
    }

    #[test]
    fn host_glass_mouse_translation_is_body_local_for_buttons_and_scroll() {
        let body = ratatui::layout::Rect::new(26, 3, 80, 17);
        let down = translate_host_glass_mouse(
            body,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 31,
                row: 7,
                modifiers: KeyModifiers::ALT,
            },
        )
        .expect("glass body button");
        assert_eq!(
            down,
            crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Down(
                    crate::protocol::ClientMouseButton::Left,
                ),
                column: 5,
                row: 4,
                modifiers: KeyModifiers::ALT.bits(),
            }
        );
        assert_eq!(
            translate_host_glass_mouse(
                body,
                MouseEvent {
                    kind: MouseEventKind::ScrollUp,
                    column: 26,
                    row: 3,
                    modifiers: KeyModifiers::empty(),
                },
            ),
            Some(crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::ScrollUp,
                column: 0,
                row: 0,
                modifiers: 0,
            })
        );
        assert!(translate_host_glass_mouse(
            body,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 25,
                row: 3,
                modifiers: KeyModifiers::empty(),
            },
        )
        .is_none());
    }

    #[cfg(unix)]
    fn glass_input_test_app(
        status: crate::app::host_glass::GlassStatus,
    ) -> (
        App,
        std::sync::mpsc::Receiver<crate::protocol::ClientMessage>,
        crate::remote_source::RemoteHostKey,
    ) {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("local")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.host_glass_enabled = true;
        let host = crate::remote_source::RemoteHostKey::new(
            "remote-a",
            crate::session::DEFAULT_SESSION_NAME,
        );
        app.state.remote_sources.mark_status(
            &host,
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        app.state
            .select_sidebar_source(crate::app::state::SidebarSource::Remote(host.clone()));
        crate::ui::compute_view_with_runtime_registry(
            &mut app.state,
            &app.terminal_runtimes,
            ratatui::layout::Rect::new(0, 0, 106, 20),
        );
        let generation = app.state.begin_host_glass_generation(host.clone());
        assert!(app
            .state
            .set_host_glass_status(&host, generation, status, None));
        let receiver = app.host_glass_runtime.test_install_connected_stream(
            host.clone(),
            generation,
            Some(true),
        );
        (app, receiver, host)
    }

    #[test]
    #[cfg(unix)]
    fn live_glass_routes_body_mouse_but_keeps_host_rail_local() {
        let (mut app, receiver, _host) =
            glass_input_test_app(crate::app::host_glass::GlassStatus::Live);
        let body = crate::ui::host_glass_body_area(app.state.view.terminal_area);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            body.x + 4,
            body.y + 2,
        ));
        assert_eq!(
            receiver.try_recv().expect("glass body mouse forwarded"),
            crate::protocol::ClientMessage::InputEvents {
                events: vec![crate::protocol::ClientInputEvent::Mouse {
                    kind: crate::protocol::ClientMouseKind::Down(
                        crate::protocol::ClientMouseButton::Left,
                    ),
                    column: 4,
                    row: 2,
                    modifiers: 0,
                }]
            }
        );

        let local = crate::ui::host_list_row_areas(&app.state)
            .into_iter()
            .find(|row| row.source == crate::app::state::SidebarSource::Local)
            .expect("local host-rail row")
            .rect;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            local.x,
            local.y,
        ));

        assert_eq!(
            app.state.effective_sidebar_source(),
            crate::app::state::SidebarSource::Local
        );
        assert!(receiver.try_recv().is_err(), "rail click never goes remote");
    }

    #[test]
    #[cfg(unix)]
    fn host_glass_status_row_is_consumed_locally_and_never_forwarded() {
        let (mut app, receiver, _host) =
            glass_input_test_app(crate::app::host_glass::GlassStatus::Live);
        let status_row = app.state.view.terminal_area;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            status_row.x + 3,
            status_row.y,
        ));

        assert!(app.state.host_glass_surface_active());
        assert!(
            receiver.try_recv().is_err(),
            "local status row never goes remote"
        );
    }

    #[test]
    #[cfg(unix)]
    fn glass_escape_is_the_only_local_key_and_deselects_without_forwarding() {
        let (mut app, receiver, _host) =
            glass_input_test_app(crate::app::host_glass::GlassStatus::Live);
        let config: crate::config::Config = toml::from_str(
            r#"
[keys]
new_workspace = "ctrl+shift+f12"

[experimental]
host_glass = true
"#,
        )
        .expect("colliding glass escape config parses");
        app.state.keybinds = config.keybinds();
        assert_eq!(app.state.keybinds.host_glass_exit.bindings.len(), 1);
        assert!(app.state.keybinds.new_workspace.bindings.is_empty());

        app.handle_terminal_key_headless(crate::input::TerminalKey::new(
            KeyCode::Char('x'),
            KeyModifiers::empty(),
        ));
        assert_eq!(
            receiver.try_recv().expect("ordinary key passes through"),
            crate::protocol::ClientMessage::InputEvents {
                events: vec![crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Char('x'),
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Press,
                }]
            }
        );

        app.handle_terminal_key_headless(crate::input::TerminalKey::new(
            KeyCode::F(12),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));

        assert_eq!(
            app.state.effective_sidebar_source(),
            crate::app::state::SidebarSource::Local
        );
        assert!(receiver.try_recv().is_err(), "escape chord stays local");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn stale_glass_drops_key_paste_and_mouse_without_queue_and_sets_brief_cue() {
        let (mut app, receiver, host) =
            glass_input_test_app(crate::app::host_glass::GlassStatus::Stale {
                since: std::time::Instant::now(),
            });
        let body = crate::ui::host_glass_body_area(app.state.view.terminal_area);

        app.handle_terminal_key_headless(crate::input::TerminalKey::new(
            KeyCode::Char('k'),
            KeyModifiers::empty(),
        ));
        app.handle_paste("never queued".into()).await;
        app.handle_mouse(mouse(MouseEventKind::ScrollDown, body.x + 2, body.y + 1));

        assert!(receiver.try_recv().is_err());
        assert!(app.state.host_glass_states.get(&host).is_some_and(|glass| {
            glass.input_drop_cue == Some(crate::app::host_glass::GlassInputDropReason::Stale)
        }));
        let deadline = app
            .host_glass_input_drop_cue_deadline
            .expect("stale drop cue has a bounded deadline");
        assert!(app.handle_scheduled_tasks(deadline, false));
        assert!(app.host_glass_input_drop_cue_deadline.is_none());
        assert!(!app
            .state
            .host_glass_states
            .get(&host)
            .is_some_and(|glass| glass.input_drop_cue.is_some()));
        assert!(
            receiver.try_recv().is_err(),
            "expiration cannot replay input"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn first_attach_drops_key_paste_and_mouse_with_rendered_not_queued_cue() {
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("local");
        let pane_id = workspace.focused_pane_id().expect("focused local pane");
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.mouse_capture = false;
        app.state.host_glass_enabled = true;
        let host = crate::remote_source::RemoteHostKey::new(
            "remote-a",
            crate::session::DEFAULT_SESSION_NAME,
        );
        app.state.remote_sources.mark_status(
            &host,
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        let area = ratatui::layout::Rect::new(0, 0, 106, 20);
        crate::ui::compute_view_with_runtime_registry(&mut app.state, &app.terminal_runtimes, area);
        let local_info = app
            .state
            .pane_info_by_id(pane_id)
            .expect("local pane geometry before source switch")
            .clone();
        let (runtime, mut local_input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                local_info.inner_rect.width,
                local_info.inner_rect.height,
                0,
                b"\x1b[?1002hlocal input target",
                8,
            );
        app.state.insert_test_runtime(pane_id, runtime);

        // Match the real first-attach window: selection is authoritative now,
        // but compute/reconciliation has not created HostGlassState yet.
        app.state
            .select_sidebar_source(crate::app::state::SidebarSource::Remote(host.clone()));
        assert!(!app.state.host_glass_states.contains_key(&host));

        app.handle_terminal_key_headless(crate::input::TerminalKey::new(
            KeyCode::Char('k'),
            KeyModifiers::empty(),
        ));
        app.handle_paste("never queued".into()).await;
        let body = crate::ui::host_glass_body_area(app.state.view.terminal_area);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            body.x,
            body.y,
        ));

        assert!(
            local_input_rx.try_recv().is_err(),
            "first-attach input must never reach the stale local terminal runtime"
        );
        let glass = app
            .state
            .host_glass_states
            .get(&host)
            .expect("first-attach drop creates cue metadata");
        assert_eq!(glass.generation, 0);
        assert_eq!(
            glass.status,
            crate::app::host_glass::GlassStatus::Connecting
        );
        assert_eq!(
            glass.input_drop_cue,
            Some(crate::app::host_glass::GlassInputDropReason::Connecting)
        );
        assert!(app.host_glass_input_drop_cue_deadline.is_some());

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
                .expect("first-attach cue terminal");
        terminal
            .draw(|frame| crate::ui::render(&app.state, frame))
            .expect("render first-attach cue");
        let cue_row = body.y + body.height / 2;
        let cue = (body.x..body.x + body.width)
            .map(|x| terminal.backend().buffer()[(x, cue_row)].symbol())
            .collect::<String>();
        assert!(cue.contains("INPUT DROPPED"));
        assert!(cue.contains("glass is connecting"));
        assert!(cue.contains("not queued"));
    }

    #[test]
    #[cfg(unix)]
    fn glass_queue_full_and_closed_are_consumed_with_truthful_drop_cues() {
        let (mut app, receiver, host) =
            glass_input_test_app(crate::app::host_glass::GlassStatus::Live);
        let key = crate::input::TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty());

        for _ in 0..crate::app::host_glass::GLASS_OUTBOUND_CAPACITY {
            app.handle_terminal_key_headless(key);
        }
        app.handle_terminal_key_headless(key);
        assert_eq!(
            app.state
                .host_glass_states
                .get(&host)
                .and_then(|glass| glass.input_drop_cue),
            Some(crate::app::host_glass::GlassInputDropReason::QueueFull)
        );
        assert!(app.host_glass_input_drop_cue_deadline.is_some());

        drop(receiver);
        app.handle_terminal_key_headless(key);
        assert_eq!(
            app.state
                .host_glass_states
                .get(&host)
                .and_then(|glass| glass.input_drop_cue),
            Some(crate::app::host_glass::GlassInputDropReason::QueueClosed)
        );
        app.handle_terminal_key_headless(key);
        assert_eq!(
            app.state
                .host_glass_states
                .get(&host)
                .and_then(|glass| glass.input_drop_cue),
            Some(crate::app::host_glass::GlassInputDropReason::Disconnected)
        );
    }

    #[test]
    #[cfg(unix)]
    fn glass_missing_connecting_and_changed_streams_drop_locally_with_cues() {
        let (mut app, receiver, host) =
            glass_input_test_app(crate::app::host_glass::GlassStatus::Live);
        let key = crate::input::TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty());
        drop(receiver);
        app.host_glass_runtime = crate::app::host_glass::HostGlassRuntime::default();

        app.handle_terminal_key_headless(key);
        assert_eq!(
            app.state
                .host_glass_states
                .get(&host)
                .and_then(|glass| glass.input_drop_cue),
            Some(crate::app::host_glass::GlassInputDropReason::MissingStream)
        );

        let generation = app
            .state
            .host_glass_states
            .get(&host)
            .expect("glass metadata")
            .generation;
        assert!(app.state.set_host_glass_status(
            &host,
            generation,
            crate::app::host_glass::GlassStatus::Connecting,
            None,
        ));
        app.handle_terminal_key_headless(key);
        assert_eq!(
            app.state
                .host_glass_states
                .get(&host)
                .and_then(|glass| glass.input_drop_cue),
            Some(crate::app::host_glass::GlassInputDropReason::Connecting)
        );

        let mut runtime = crate::app::host_glass::HostGlassRuntime::default();
        let _receiver =
            runtime.test_install_connected_stream(host.clone(), generation, Some(false));
        app.host_glass_runtime = runtime;
        let next_generation = app.state.begin_host_glass_generation(host.clone());
        assert!(app.state.set_host_glass_status(
            &host,
            next_generation,
            crate::app::host_glass::GlassStatus::Live,
            None,
        ));
        app.handle_terminal_key_headless(key);
        assert_eq!(
            app.state
                .host_glass_states
                .get(&host)
                .and_then(|glass| glass.input_drop_cue),
            Some(crate::app::host_glass::GlassInputDropReason::GenerationChanged)
        );
    }

    #[tokio::test]
    async fn paste_routes_to_rename_modal_input() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::RenameTab;
        app.state.name_input = "2".into();
        app.state.name_input_replace_on_type = true;

        app.handle_paste("feature/logs".into()).await;

        assert_eq!(app.state.name_input, "feature/logs");
        assert!(!app.state.name_input_replace_on_type);
    }

    #[tokio::test]
    async fn paste_does_nothing_while_remote_space_projected() {
        let mut app = test_app();
        let mut ws = crate::workspace::Workspace::test_new("test");
        let root_pane = ws.tabs[0].root_pane;
        let (runtime, mut input_rx) = crate::terminal::TerminalRuntime::test_with_channel(20, 5);
        ws.insert_test_runtime(root_pane, runtime);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.selected_remote_space = Some(crate::remote_source::RemoteSpaceKey {
            host: "jafar".to_string(),
            session: "default".to_string(),
            workspace_id: "ws-remote".to_string(),
        });

        // The projection guard must short-circuit before any local runtime or
        // remote session receives the paste, so no bytes reach the runtime's
        // input channel.
        app.handle_paste("injected".into()).await;

        assert!(
            input_rx.try_recv().is_err(),
            "paste must not reach the focused pane runtime while a remote space is projected"
        );
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.selected_remote_space.is_some());
    }

    #[tokio::test]
    async fn paste_routes_to_new_linked_worktree_input() {
        let mut app = test_app();
        app.state.mode = Mode::NewLinkedWorktree;
        app.state.name_input = "generated-branch".into();
        app.state.name_input_replace_on_type = true;
        app.state.worktree_create = Some(crate::app::state::WorktreeCreateState {
            source_workspace_id: "source".into(),
            source_checkout_path: "/repo/herdr".into(),
            source_existing_membership: None,
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            branch: "generated-branch".into(),
            checkout_path: "/repo/herdr-generated-branch".into(),
            error: None,
            creating: false,
        });

        app.handle_paste("feature/linear-302".into()).await;

        assert_eq!(app.state.name_input, "feature/linear-302");
        assert_eq!(
            app.state
                .worktree_create
                .as_ref()
                .map(|create| create.branch.as_str()),
            Some("feature/linear-302")
        );
    }

    #[test]
    fn modal_paste_shortcut_matches_platform_primary_v() {
        #[cfg(target_os = "macos")]
        let modifiers = KeyModifiers::SUPER;
        #[cfg(not(target_os = "macos"))]
        let modifiers = KeyModifiers::CONTROL;

        assert!(is_modal_paste_shortcut(&KeyEvent::new(
            KeyCode::Char('v'),
            modifiers
        )));
        assert!(is_modal_paste_shortcut(&KeyEvent::new(
            KeyCode::Char('V'),
            modifiers | KeyModifiers::SHIFT
        )));
        assert!(!is_modal_paste_shortcut(&KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::ALT
        )));
    }

    #[test]
    fn modal_paste_target_is_active_only_for_text_inputs() {
        let mut state = AppState::test_new();

        state.mode = Mode::RenameTab;
        assert!(modal_paste_target_active(&state));

        state.mode = Mode::Navigator;
        state.navigator.search_focused = false;
        assert!(!modal_paste_target_active(&state));
        state.navigator.search_focused = true;
        assert!(modal_paste_target_active(&state));

        state.mode = Mode::ConfirmClose;
        assert!(!modal_paste_target_active(&state));
    }
}
