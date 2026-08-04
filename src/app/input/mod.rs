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

fn translate_remote_projection_mouse(
    hit: &crate::app::state::RemoteProjectionHitArea,
    mouse: MouseEvent,
) -> Option<crate::protocol::ClientInputEvent> {
    let inner_x = hit.rect.x.saturating_add(1);
    let inner_y = hit.rect.y.saturating_add(1);
    let inner_width = hit.rect.width.saturating_sub(2);
    let inner_height = hit.rect.height.saturating_sub(2);
    if inner_width == 0
        || inner_height == 0
        || mouse.column < inner_x
        || mouse.row < inner_y
        || mouse.column >= inner_x.saturating_add(inner_width)
        || mouse.row >= inner_y.saturating_add(inner_height)
    {
        return None;
    }
    let kind = crate::protocol::ClientMouseKind::from_crossterm(mouse.kind)?;
    Some(crate::protocol::ClientInputEvent::Mouse {
        kind,
        column: mouse
            .column
            .saturating_sub(inner_x)
            .min(inner_width.saturating_sub(1)),
        row: mouse
            .row
            .saturating_sub(inner_y)
            .min(inner_height.saturating_sub(1)),
        modifiers: mouse.modifiers.bits(),
    })
}

/// Translate screen coordinates into the exact visible copy grid of one
/// projected hit area — the `min(pane interior, frame)` rectangle the render
/// loop actually draws. Returns None when the point is on the pane
/// border/title, in blank pane interior outside a smaller frame, on a pane
/// region beyond a clipped larger frame, or the grid is degenerate. Points
/// outside the rendered grid never clamp onto an unseen edge cell.
fn projected_frame_cell(
    hit: &crate::app::state::RemoteProjectionHitArea,
    frame: &crate::protocol::FrameData,
    column: u16,
    row: u16,
) -> Option<(u16, u16)> {
    let (inner_x, inner_y, copy_width, copy_height) =
        crate::selection::projected_visible_grid(hit.rect, frame.width, frame.height);
    if copy_width == 0
        || copy_height == 0
        || column < inner_x
        || row < inner_y
        || column >= inner_x.saturating_add(copy_width)
        || row >= inner_y.saturating_add(copy_height)
    {
        return None;
    }
    Some((row - inner_y, column - inner_x))
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
                Mode::ConfirmRemoteAttach => self.handle_confirm_remote_attach_key(key_event),
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
        self.drain_remote_attach_request();
        self.drain_remote_attach_in_new_split_request();
        self.drain_remote_detach_view_request();
        self.drain_remote_workspace_create_request();
        self.sync_toast_deadline(previous_toast);
    }

    pub(super) async fn handle_paste(&mut self, text: String) {
        if self.state.mode != Mode::Terminal {
            self.paste_into_active_text_input(&text);
            return;
        }

        // A selected remote source routes paste only through the in-place
        // controller stream. Unsupported/stale/owned states consume
        // fail-closed and never paste into a local terminal runtime.
        if self.state.remote_projection_surface_active() {
            let _ = self.remote_projection_runtime.send_input(
                &self.state,
                crate::protocol::ClientInputEvent::Paste { text },
                &self.event_tx,
            );
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

    /// Route terminal-mode mouse events inside the authoritative focused
    /// projected pane to its controller stream. Coordinates are translated to
    /// pane-interior space and clipped. Pane borders/tab/sidebar/global UI stay
    /// local; clicks on a non-focused projected pane keep using the existing
    /// capability-gated remote `pane.focus` action. Any unsupported/stale/owned
    /// projected terminal hit is consumed fail-closed and never reaches a local
    /// pane.
    ///
    /// A left-drag over an exact cached frame starts a local projected
    /// selection instead of remote forwarding when the pane can accept the
    /// gesture locally: read-only/owned/stale streams always can; a writable
    /// live control stream can only after the authoritative runtime explicitly
    /// reported no application mouse tracking. Enabled or not-yet-known keeps
    /// the existing structured remote mouse forwarding. Once a projected
    /// selection begins, its drag/up lifecycle stays local even if the
    /// stream's mouse mode changes mid-gesture, and a completed drag copies
    /// through the existing local clipboard event path exactly once.
    fn handle_remote_projection_terminal_mouse(&mut self, mouse: MouseEvent) -> bool {
        // An in-progress projected selection gesture owns the rest of its
        // lifecycle locally: once a projected left selection starts, its
        // remaining left Drag/Up are consumed until Up even when a
        // generation/source/space/target/mouse-mode change cleared the
        // selection overlay mid-gesture. Up always releases ownership; with
        // the exact selection/frame gone it copies nothing and sends nothing
        // remote. Ownership is deliberately tracked apart from the overlay
        // so a live replacement stream can never receive the tail of a
        // locally-started gesture.
        if self.state.projected_selection_gesture_active {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    self.update_projected_selection_drag(mouse.column, mouse.row);
                    return true;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.finish_projected_selection();
                    return true;
                }
                _ => {}
            }
        }

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
            .cloned()
        else {
            return false;
        };

        // Right click remains local projected-pane context chrome. A left click
        // on a non-focused leaf remains the existing remote focus mutation.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Right))
            || (matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) && !hit.focused)
        {
            return false;
        }
        if !hit.focused {
            return true;
        }
        if !hit.live {
            // A stale/read-only projection has no remote input route, but its
            // last-known cached frame stays locally selectable and copyable
            // when Herdr receives the gesture.
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                && self.state.mouse_capture
            {
                let _ = self.start_projected_selection_at(&hit, mouse.column, mouse.row);
            }
            return true;
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && mouse.modifiers.contains(modified_url_click_modifier())
            && self
                .state
                .remote_projection_url_at(mouse.column, mouse.row)
                .is_some()
        {
            // Let the local/global URL affordance below handle this OSC-8
            // link; do not send the click to the remote terminal as well.
            return false;
        }

        // A left press starts a local projected selection only when the exact
        // cached frame is selectable; otherwise the gesture keeps the existing
        // structured remote mouse forwarding below.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.state.mouse_capture
            && self.start_projected_selection_at(&hit, mouse.column, mouse.row)
        {
            return true;
        }

        let Some(event) = translate_remote_projection_mouse(&hit, mouse) else {
            // Border/title hits are local projection chrome, never terminal
            // input and never local-pane fallthrough.
            return true;
        };
        let _ = self
            .remote_projection_runtime
            .send_input(&self.state, event, &self.event_tx);
        true
    }

    /// Start a local projected-frame selection for an exact terminal hit.
    /// Returns true when the gesture was claimed for local selection; false
    /// preserves the existing structured remote forwarding / fail-closed
    /// consumption. Selection requires `ui.mouse_capture`, an exact cached
    /// frame for the hit's terminal, and — for a writable live control
    /// stream — an explicit authoritative report that application mouse
    /// tracking is off. Read-only/owned/stale streams are selectable without
    /// any remote input route.
    fn start_projected_selection_at(
        &mut self,
        hit: &crate::app::state::RemoteProjectionHitArea,
        column: u16,
        row: u16,
    ) -> bool {
        let Some(selected) = self.state.selected_remote_space.as_ref() else {
            return false;
        };
        let Some(terminal_id) = hit.terminal_id.as_deref() else {
            return false;
        };
        let Some(entry) = self.state.remote_projection_frame(selected, terminal_id) else {
            return false;
        };
        let Some(frame) = entry.frame.as_ref() else {
            return false;
        };
        // A writable live controller defers to the authoritative mouse-capture
        // report; enabled or not-yet-known keeps structured remote forwarding.
        if entry.status.accepts_input()
            && entry.role == crate::remote_source::RemoteProjectionStreamRole::Control
            && self
                .remote_projection_runtime
                .focused_projected_control_mouse_capture(&self.state)
                != Some(false)
        {
            return false;
        }
        let Some((frame_row, frame_col)) = projected_frame_cell(hit, frame, column, row) else {
            return false;
        };
        let key = crate::remote_source::RemoteProjectionTerminalKey {
            host: selected.host.clone(),
            session: selected.session.clone(),
            workspace_id: selected.workspace_id.clone(),
            terminal_id: terminal_id.to_owned(),
        };
        let selection = crate::selection::ProjectedSelection::anchor(key, frame_row, frame_col);
        self.state.start_projected_selection(selection);
        true
    }

    /// Extend the in-progress projected selection, clamped to the exact
    /// visible copy grid (`min(pane interior, frame)`) of the terminal that
    /// owns it, so only rendered cells can be highlighted or copied. The
    /// drag may leave the copied grid/pane; clamping is symmetric — explicit
    /// top/left/right/bottom out-of-bounds detection, never
    /// `saturating_sub` asymmetry. A missing/replaced hit or frame fails
    /// closed by leaving the selection untouched; the gesture itself stays
    /// locally consumed.
    fn update_projected_selection_drag(&mut self, column: u16, row: u16) {
        let Some(key) = self
            .state
            .projected_selection
            .as_ref()
            .map(|selection| selection.key.clone())
        else {
            return;
        };
        let Some(hit) = self
            .state
            .view
            .remote_projection_hit_areas
            .iter()
            .find(|hit| {
                hit.terminal_id.as_deref() == Some(key.terminal_id.as_str())
                    && hit.host == key.host
                    && hit.session == key.session
            })
            .cloned()
        else {
            return;
        };
        let Some((inner_x, inner_y, copy_width, copy_height)) = self
            .state
            .remote_projection_frames
            .get(&key)
            .and_then(|entry| entry.frame.as_ref())
            .map(|frame| {
                crate::selection::projected_visible_grid(hit.rect, frame.width, frame.height)
            })
        else {
            return;
        };
        if copy_width == 0 || copy_height == 0 {
            return;
        }
        let out_left = column < inner_x;
        let out_top = row < inner_y;
        let out_right = column >= inner_x.saturating_add(copy_width);
        let out_bottom = row >= inner_y.saturating_add(copy_height);
        let raw_row = row.saturating_sub(inner_y).min(copy_height - 1);
        let raw_col = column.saturating_sub(inner_x).min(copy_width - 1);
        if let Some(selection) = self.state.projected_selection.as_mut() {
            selection.drag(raw_row, raw_col, copy_width, copy_height);
            // The pointer left the anchor cell but clamping pinned the cursor
            // back onto it (any visible grid edge): the gesture still counts
            // as a drag.
            if selection.was_just_click() && (out_left || out_top || out_right || out_bottom) {
                selection.force_dragging();
            }
        }
    }

    /// Release the in-progress projected selection. Mouse-up always
    /// releases local gesture ownership, whether or not the selection
    /// overlay survived the gesture. A plain click just clears the overlay;
    /// a real drag extracts visible text from the exact current cached frame
    /// and queues one local `ClipboardWrite`, but only while the whole
    /// selected range still fits the pane's rendered copy grid. Malformed,
    /// stale, mismatched, cleared, resized-out, or empty content fails
    /// closed with no copy and no remote input. The selection overlay is
    /// cleared either way.
    fn finish_projected_selection(&mut self) {
        self.state.projected_selection_gesture_active = false;
        let Some(mut selection) = self.state.projected_selection.take() else {
            // The exact selection/frame was cleared mid-gesture (generation,
            // source, or target change): copy nothing, send nothing remote.
            return;
        };
        if !selection.finish() {
            return;
        }
        let Some(text) = self.projected_selection_visible_text(&selection) else {
            return;
        };
        self.state.request_clipboard_write = Some(text.into_bytes());
        // This handler consumed the gesture before `handle_mouse` reached its
        // common queue, so the event is queued here — exactly once, and the
        // drained request cannot be queued a second time.
        self.queue_pending_clipboard_write();
        tracing::info!("copied projected selection to local clipboard");
    }

    /// Extract the selection's text from its exact current cached frame,
    /// keyed by exact terminal identity and clipped to the pane's rendered
    /// copy grid, so extraction always matches the rendered highlight: cells
    /// outside the rendered grid (a larger frame clipped by the pane, or a
    /// mid-gesture resize/relayout) are never copied.
    fn projected_selection_visible_text(
        &self,
        selection: &crate::selection::ProjectedSelection,
    ) -> Option<String> {
        let hit = self
            .state
            .view
            .remote_projection_hit_areas
            .iter()
            .find(|hit| {
                hit.terminal_id.as_deref() == Some(selection.key.terminal_id.as_str())
                    && hit.host == selection.key.host
                    && hit.session == selection.key.session
            })?;
        let frame = self
            .state
            .remote_projection_frames
            .get(&selection.key)?
            .frame
            .as_ref()?;
        let (_, _, copy_width, copy_height) =
            crate::selection::projected_visible_grid(hit.rect, frame.width, frame.height);
        selection.extract_visible(frame, copy_width, copy_height)
    }

    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) {
        if self.handle_confirm_remote_attach_mouse(mouse) {
            return;
        }

        if self.handle_overlay_mouse(mouse) {
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
                    MouseAction::FocusRemoteAttachPane {
                        ws_idx,
                        pane_id,
                        selected_remote_agent,
                    } => {
                        self.focus_pane_internal_via_api(ws_idx, pane_id);
                        self.state.selected_remote_space = None;
                        self.state.selected_remote_agent = Some(selected_remote_agent);
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
        self.drain_remote_attach_request();
        self.drain_remote_attach_in_new_split_request();
        self.drain_remote_detach_view_request();
        self.drain_remote_workspace_create_request();
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
            let Some(url) = self.state.remote_projection_url_at(mouse.column, mouse.row) else {
                return false;
            };
            self.last_pane_click = None;
            if let Err(err) = crate::platform::open_url(&url) {
                tracing::warn!(err = %err, url = %url, "failed to open projected remote URL");
            }
            return true;
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
    fn projected_mouse_translation_is_pane_relative_clipped_and_excludes_border() {
        let hit = crate::app::state::RemoteProjectionHitArea {
            rect: ratatui::layout::Rect::new(10, 5, 8, 6),
            host: "remote-a".into(),
            session: "default".into(),
            pane_id: Some("pane".into()),
            terminal_id: Some("term".into()),
            label: "pane".into(),
            focused: true,
            live: true,
        };
        let event = translate_remote_projection_mouse(
            &hit,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 16,
                row: 9,
                modifiers: KeyModifiers::CONTROL,
            },
        )
        .expect("interior mouse event");
        assert_eq!(
            event,
            crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Down(
                    crate::protocol::ClientMouseButton::Left
                ),
                column: 5,
                row: 3,
                modifiers: KeyModifiers::CONTROL.bits(),
            }
        );
        assert!(translate_remote_projection_mouse(
            &hit,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 10,
                row: 5,
                modifiers: KeyModifiers::empty(),
            },
        )
        .is_none());
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

    // ------------------------------------------------------------------
    // Projected remote-frame selection gesture routing
    // ------------------------------------------------------------------

    /// The projected test pane is a 22x8 bordered box at (30, 2), so its
    /// 20x6 frame interior starts at screen (31, 3).
    const PROJECTED_TEST_RECT: ratatui::layout::Rect = ratatui::layout::Rect::new(30, 2, 22, 8);

    fn projected_screen(frame_row: u16, frame_col: u16) -> (u16, u16) {
        (31 + frame_col, 3 + frame_row)
    }

    fn projected_frame(lines: &[&str]) -> crate::protocol::FrameData {
        let height = lines.len() as u16;
        let width = lines
            .iter()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0) as u16;
        let mut cells = Vec::with_capacity(usize::from(width) * usize::from(height));
        for line in lines {
            let mut col = 0usize;
            for ch in line.chars() {
                cells.push(crate::protocol::CellData {
                    symbol: ch.to_string(),
                    fg: 0,
                    bg: 0,
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                });
                col += 1;
            }
            while col < usize::from(width) {
                cells.push(crate::protocol::CellData {
                    symbol: " ".into(),
                    fg: 0,
                    bg: 0,
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                });
                col += 1;
            }
        }
        crate::protocol::FrameData {
            cells,
            width,
            height,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        }
    }

    /// A 20x6 frame whose first two rows carry copyable text.
    fn projected_text_frame() -> crate::protocol::FrameData {
        projected_frame(&[
            "first line content  ",
            " hello world        ",
            "                    ",
            "                    ",
            "                    ",
            "                    ",
        ])
    }

    /// Build an app with one selected projected remote space, one terminal
    /// (`term-1`) seeded at the given stream status/frame, and one matching
    /// hit area. No stream handle is inserted; unix tests that exercise the
    /// writable-control path add one explicitly.
    fn projected_selection_app(
        status: crate::remote_source::RemoteProjectionStreamStatus,
        frame: Option<crate::protocol::FrameData>,
        focused: bool,
        live: bool,
    ) -> App {
        let mut app = test_app();
        app.state.mode = Mode::Terminal;
        app.state.mouse_capture = true;
        let selected = crate::remote_source::RemoteSpaceKey {
            host: "remote-a".into(),
            session: "default".into(),
            workspace_id: "ws-a".into(),
        };
        app.state.selected_remote_space = Some(selected.clone());
        let generation = app
            .state
            .begin_remote_projection_generation(Some(&selected), false);
        let key = crate::remote_source::RemoteProjectionTerminalKey {
            host: "remote-a".into(),
            session: "default".into(),
            workspace_id: "ws-a".into(),
            terminal_id: "term-1".into(),
        };
        app.state.seed_remote_projection_streams(
            generation,
            [(
                key.clone(),
                crate::remote_source::RemoteProjectionStreamRole::Control,
                crate::remote_source::RemoteProjectionStreamStatus::Connecting,
                None,
            )],
        );
        app.state.apply_remote_projection_stream_event(
            key,
            generation,
            crate::remote_source::RemoteProjectionStreamRole::Control,
            status,
            frame,
            None,
        );
        app.state.view.remote_projection_hit_areas =
            vec![crate::app::state::RemoteProjectionHitArea {
                rect: PROJECTED_TEST_RECT,
                host: "remote-a".into(),
                session: "default".into(),
                pane_id: Some("pane-1".into()),
                terminal_id: Some("term-1".into()),
                label: "pane".into(),
                focused,
                live,
            }];
        app
    }

    /// Pop the next event, requiring exactly one clipboard write with the
    /// expected bytes and nothing else queued.
    fn assert_single_clipboard_write(app: &mut App, expected: &[u8]) {
        match app.event_rx.try_recv() {
            Ok(crate::events::AppEvent::ClipboardWrite { content }) => {
                assert_eq!(content, expected);
            }
            other => panic!("expected one ClipboardWrite event, got {other:?}"),
        }
        assert!(
            app.event_rx.try_recv().is_err(),
            "clipboard write must be queued exactly once"
        );
        assert!(
            app.state.request_clipboard_write.is_none(),
            "the queued request must be drained so it cannot double queue"
        );
    }

    fn assert_no_clipboard_write(app: &mut App) {
        assert!(
            app.state.projected_selection.is_none(),
            "selection must be cleared"
        );
        assert!(app.state.request_clipboard_write.is_none());
        while let Ok(event) = app.event_rx.try_recv() {
            assert!(
                !matches!(event, crate::events::AppEvent::ClipboardWrite { .. }),
                "no ClipboardWrite event may be queued"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn projected_selection_copies_dragged_text_exactly_once_without_remote_forwarding() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::LiveController,
            Some(projected_text_frame()),
            true,
            true,
        );
        app.remote_projection_runtime
            .test_insert_writable_stream_handle("term-1", Some(false));

        let (down_col, down_row) = projected_screen(0, 0);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));
        let selection = app
            .state
            .projected_selection
            .as_ref()
            .expect("mouse-capture-disabled control stream starts a local selection");
        assert_eq!(selection.key.terminal_id, "term-1");
        assert!(selection.is_in_progress());

        let (drag_col, drag_row) = projected_screen(1, 11);
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            drag_col,
            drag_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            drag_col,
            drag_row,
        ));

        assert_single_clipboard_write(&mut app, b"first line content\n hello world");
        assert!(app.state.projected_selection.is_none());
        assert!(
            !app.remote_projection_runtime
                .test_stream_input_sent("term-1"),
            "a locally selected gesture must never reach the remote stream"
        );
    }

    #[cfg(unix)]
    #[test]
    fn projected_selection_mouse_capture_enabled_forwards_without_selecting() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::LiveController,
            Some(projected_text_frame()),
            true,
            true,
        );
        app.remote_projection_runtime
            .test_insert_writable_stream_handle("term-1", Some(true));

        let (down_col, down_row) = projected_screen(0, 0);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));

        assert!(app.state.projected_selection.is_none());
        assert!(
            app.remote_projection_runtime
                .test_stream_input_sent("term-1"),
            "enabled application mouse tracking keeps structured remote forwarding"
        );
        assert!(app.state.request_clipboard_write.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn projected_selection_mouse_capture_unknown_forwards_without_selecting() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::LiveController,
            Some(projected_text_frame()),
            true,
            true,
        );
        app.remote_projection_runtime
            .test_insert_writable_stream_handle("term-1", None);

        let (down_col, down_row) = projected_screen(0, 0);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));

        assert!(app.state.projected_selection.is_none());
        assert!(
            app.remote_projection_runtime
                .test_stream_input_sent("term-1"),
            "not-yet-known mouse tracking keeps structured remote forwarding"
        );
        assert!(app.state.request_clipboard_write.is_none());
    }

    #[test]
    fn projected_selection_owned_read_only_copies_without_remote_input() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::OwnedReadOnly,
            Some(projected_text_frame()),
            true,
            true,
        );

        let (down_col, down_row) = projected_screen(1, 1);
        let (up_col, up_row) = projected_screen(1, 11);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));
        assert!(app.state.projected_selection.is_some());
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            up_col,
            up_row,
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), up_col, up_row));

        assert_single_clipboard_write(&mut app, b"hello world");
    }

    #[test]
    fn projected_selection_stale_cached_frame_copies_without_remote_input() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::StaleLastKnown,
            Some(projected_text_frame()),
            true,
            false,
        );

        let (down_col, down_row) = projected_screen(0, 0);
        let (up_col, up_row) = projected_screen(1, 5);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));
        assert!(app.state.projected_selection.is_some());
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            up_col,
            up_row,
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), up_col, up_row));

        assert_single_clipboard_write(&mut app, b"first line content\n hello");
    }

    #[test]
    fn projected_selection_non_focused_down_keeps_remote_focus_behavior() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::LiveController,
            Some(projected_text_frame()),
            false,
            true,
        );

        let (down_col, down_row) = projected_screen(0, 0);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));

        assert!(
            app.state.projected_selection.is_none(),
            "a non-focused projected pane keeps the remote focus mutation route"
        );
        assert!(app.state.request_clipboard_write.is_none());
        assert!(app.state.selected_remote_space.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn projected_selection_requires_mouse_capture_config() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::LiveController,
            Some(projected_text_frame()),
            true,
            true,
        );
        app.remote_projection_runtime
            .test_insert_writable_stream_handle("term-1", Some(false));
        app.state.mouse_capture = false;

        let (down_col, down_row) = projected_screen(0, 0);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));

        assert!(
            app.state.projected_selection.is_none(),
            "ui.mouse_capture = false keeps Herdr's own selection UI off"
        );
        assert!(
            app.remote_projection_runtime
                .test_stream_input_sent("term-1"),
            "the gesture keeps its existing remote forwarding"
        );
        assert!(app.state.request_clipboard_write.is_none());

        // A stale cached frame is likewise not captured for local selection.
        let mut stale = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::StaleLastKnown,
            Some(projected_text_frame()),
            true,
            false,
        );
        stale.state.mouse_capture = false;
        stale.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));
        assert!(stale.state.projected_selection.is_none());
        assert!(stale.state.request_clipboard_write.is_none());
    }

    #[test]
    fn projected_selection_plain_click_clears_without_copy() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::OwnedReadOnly,
            Some(projected_text_frame()),
            true,
            true,
        );

        let (col, row) = projected_screen(1, 3);
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
        assert!(app.state.projected_selection.is_some());
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), col, row));

        assert_no_clipboard_write(&mut app);
    }

    #[test]
    fn projected_selection_empty_frame_copies_nothing() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::OwnedReadOnly,
            Some(projected_frame(&["          ", "          "])),
            true,
            true,
        );

        let (down_col, down_row) = projected_screen(0, 0);
        let (up_col, up_row) = projected_screen(1, 9);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            up_col,
            up_row,
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), up_col, up_row));

        assert_no_clipboard_write(&mut app);
    }

    #[test]
    fn projected_selection_malformed_frame_fails_closed() {
        let mut malformed = projected_text_frame();
        malformed.cells.truncate(7);
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::OwnedReadOnly,
            Some(malformed),
            true,
            true,
        );

        let (down_col, down_row) = projected_screen(0, 0);
        let (up_col, up_row) = projected_screen(1, 5);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            up_col,
            up_row,
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), up_col, up_row));

        assert_no_clipboard_write(&mut app);
    }

    #[test]
    fn projected_selection_mismatched_key_copies_nothing() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::OwnedReadOnly,
            Some(projected_text_frame()),
            true,
            true,
        );
        // A selection keyed to a terminal that has no matching hit area or
        // cached frame must fail closed instead of copying unrelated data.
        // The synthetic overlay seeds the gesture marker directly so its
        // mouse-up routes through the gesture release path.
        let mut selection = crate::selection::ProjectedSelection::anchor(
            crate::remote_source::RemoteProjectionTerminalKey {
                host: "remote-a".into(),
                session: "default".into(),
                workspace_id: "ws-a".into(),
                terminal_id: "term-other".into(),
            },
            0,
            0,
        );
        selection.drag(1, 5, 20, 6);
        app.state.projected_selection = Some(selection);
        app.state.projected_selection_gesture_active = true;

        let (up_col, up_row) = projected_screen(1, 5);
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), up_col, up_row));

        assert_no_clipboard_write(&mut app);
        assert!(
            !app.state.projected_selection_gesture_active,
            "mouse-up always releases local gesture ownership"
        );
    }

    #[cfg(unix)]
    #[test]
    fn projected_selection_stays_local_when_mouse_mode_changes_mid_gesture() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::LiveController,
            Some(projected_text_frame()),
            true,
            true,
        );
        app.remote_projection_runtime
            .test_insert_writable_stream_handle("term-1", Some(false));

        let (down_col, down_row) = projected_screen(0, 0);
        let (drag_col, drag_row) = projected_screen(1, 4);
        let (up_col, up_row) = projected_screen(1, 11);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            drag_col,
            drag_row,
        ));
        assert!(app.state.projected_selection.is_some());

        // The authoritative runtime enables application mouse tracking
        // mid-gesture; the in-progress selection still owns drag/up locally.
        app.remote_projection_runtime
            .test_set_stream_mouse_capture("term-1", Some(true));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            up_col,
            up_row,
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), up_col, up_row));

        assert_single_clipboard_write(&mut app, b"first line content\n hello world");
        assert!(
            !app.remote_projection_runtime
                .test_stream_input_sent("term-1"),
            "a mid-gesture mode change must not reroute the selection remotely"
        );
    }

    #[test]
    fn projected_selection_new_down_reanchors_to_exact_focused_key() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::OwnedReadOnly,
            Some(projected_text_frame()),
            true,
            true,
        );
        // Seed a second projected terminal with distinct content.
        let selected = app.state.selected_remote_space.clone().expect("selected");
        let key2 = crate::remote_source::RemoteProjectionTerminalKey {
            host: "remote-a".into(),
            session: "default".into(),
            workspace_id: "ws-a".into(),
            terminal_id: "term-2".into(),
        };
        let generation = app.state.remote_projection_generation;
        app.state.seed_remote_projection_streams(
            generation,
            [(
                key2.clone(),
                crate::remote_source::RemoteProjectionStreamRole::Observe,
                crate::remote_source::RemoteProjectionStreamStatus::Connecting,
                None,
            )],
        );
        app.state.apply_remote_projection_stream_event(
            key2,
            generation,
            crate::remote_source::RemoteProjectionStreamRole::Observe,
            crate::remote_source::RemoteProjectionStreamStatus::LiveObserver,
            Some(projected_frame(&[
                "second pane text    ",
                "                    ",
            ])),
            None,
        );
        app.state.view.remote_projection_hit_areas = vec![
            crate::app::state::RemoteProjectionHitArea {
                rect: PROJECTED_TEST_RECT,
                host: "remote-a".into(),
                session: "default".into(),
                pane_id: Some("pane-1".into()),
                terminal_id: Some("term-1".into()),
                label: "pane-1".into(),
                focused: true,
                live: true,
            },
            crate::app::state::RemoteProjectionHitArea {
                rect: ratatui::layout::Rect::new(52, 2, 22, 8),
                host: "remote-a".into(),
                session: "default".into(),
                pane_id: Some("pane-2".into()),
                terminal_id: Some("term-2".into()),
                label: "pane-2".into(),
                focused: false,
                live: true,
            },
        ];

        // Anchor on term-1, then the focus flips to term-2 and a new press
        // replaces the in-progress selection with the exact new key.
        let (first_col, first_row) = projected_screen(0, 0);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            first_col,
            first_row,
        ));
        assert_eq!(
            app.state
                .projected_selection
                .as_ref()
                .map(|selection| selection.key.terminal_id.as_str()),
            Some("term-1")
        );
        for hit in &mut app.state.view.remote_projection_hit_areas {
            hit.focused = hit.terminal_id.as_deref() == Some("term-2");
        }
        let (second_col, second_row) = (53, 3);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            second_col,
            second_row,
        ));
        assert_eq!(
            app.state
                .projected_selection
                .as_ref()
                .map(|selection| selection.key.terminal_id.as_str()),
            Some("term-2")
        );
        let (up_col, up_row) = (53 + 16, 3);
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            up_col,
            up_row,
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), up_col, up_row));

        assert_single_clipboard_write(&mut app, b"second pane text");
        drop(selected);
    }

    #[test]
    fn projected_selection_generation_advance_clears_in_progress_selection() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::OwnedReadOnly,
            Some(projected_text_frame()),
            true,
            true,
        );

        let (down_col, down_row) = projected_screen(0, 0);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));
        assert!(app.state.projected_selection.is_some());

        let selected = app.state.selected_remote_space.clone().expect("selected");
        app.state
            .begin_remote_projection_generation(Some(&selected), true);
        assert!(app.state.projected_selection.is_none());

        let (up_col, up_row) = projected_screen(1, 5);
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), up_col, up_row));
        assert_no_clipboard_write(&mut app);
    }

    #[test]
    fn projected_selection_start_rejects_blank_interior_outside_smaller_frame() {
        // An 8x3 frame inside the 20x6 pane interior: a down in the blank
        // pane interior right of or below the rendered frame must not clamp
        // onto an unseen edge cell and must not start a selection.
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::OwnedReadOnly,
            Some(projected_frame(&["12345678", "abcdefgh", "ABCDEFGH"])),
            true,
            true,
        );

        // Right of the rendered frame but inside the pane interior.
        let (right_col, right_row) = projected_screen(0, 15);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            right_col,
            right_row,
        ));
        assert!(app.state.projected_selection.is_none());
        assert!(!app.state.projected_selection_gesture_active);

        // Below the rendered frame but inside the pane interior.
        let (below_col, below_row) = projected_screen(4, 2);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            below_col,
            below_row,
        ));
        assert!(app.state.projected_selection.is_none());
        assert!(!app.state.projected_selection_gesture_active);

        // A following drag/up highlights and copies nothing.
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            below_col,
            below_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            below_col,
            below_row,
        ));
        assert_no_clipboard_write(&mut app);

        // The rendered frame cells themselves remain selectable.
        let (down_col, down_row) = projected_screen(1, 1);
        let (up_col, up_row) = projected_screen(1, 6);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));
        assert!(app.state.projected_selection.is_some());
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            up_col,
            up_row,
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), up_col, up_row));
        assert_single_clipboard_write(&mut app, b"bcdefg");
    }

    #[test]
    fn projected_selection_larger_frame_never_selects_clipped_cells() {
        // A 30x10 frame clipped by the 20x6 pane interior: only the rendered
        // 20x6 region can ever be highlighted, extracted, or copied.
        let rows: Vec<String> = (0..10u16)
            .map(|r| {
                (0..30u16)
                    .map(|c| char::from(b'a' + ((r * 30 + c) % 26) as u8))
                    .collect()
            })
            .collect();
        let row_refs: Vec<&str> = rows.iter().map(String::as_str).collect();
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::OwnedReadOnly,
            Some(projected_frame(&row_refs)),
            true,
            true,
        );

        let (down_col, down_row) = projected_screen(0, 0);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));
        // Drag far past the pane's bottom-right corner: the cursor clamps to
        // the last rendered cell, never into clipped frame rows/columns.
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 55, 15));
        {
            let selection = app.state.projected_selection.as_ref().expect("selection");
            assert!(selection.contains(5, 19));
            assert!(
                !selection.contains(5, 20),
                "a clipped column must not be selectable"
            );
            assert!(
                !selection.contains(6, 0),
                "a clipped row must not be selectable"
            );
        }
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 55, 15));

        let expected = rows[..6]
            .iter()
            .map(|row| row[..20].trim_end())
            .collect::<Vec<_>>()
            .join("\n");
        assert_single_clipboard_write(&mut app, expected.as_bytes());
    }

    #[test]
    fn projected_selection_out_of_bounds_drag_clamps_to_visible_grid() {
        let rows = [
            "alpha beta gamma    ",
            "delta epsilon zeta  ",
            "eta theta iota      ",
            "kappa lambda mu     ",
            "nu xi omicron pi    ",
            "rho sigma tau upsil ",
        ];
        for row in rows {
            assert_eq!(row.chars().count(), 20);
        }

        let run = |anchor_screen: (u16, u16), drag_screen: (u16, u16), expected: &str| {
            let mut app = projected_selection_app(
                crate::remote_source::RemoteProjectionStreamStatus::OwnedReadOnly,
                Some(projected_frame(&rows)),
                true,
                true,
            );
            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                anchor_screen.0,
                anchor_screen.1,
            ));
            app.handle_mouse(mouse(
                MouseEventKind::Drag(MouseButton::Left),
                drag_screen.0,
                drag_screen.1,
            ));
            app.handle_mouse(mouse(
                MouseEventKind::Up(MouseButton::Left),
                drag_screen.0,
                drag_screen.1,
            ));
            assert_single_clipboard_write(&mut app, expected.as_bytes());
        };

        // Anchor at frame cell (2, 8) = screen (39, 5); each direction clamps
        // to the visible grid edge and copies only rendered cells.
        run(
            (39, 5),
            (39, 0),
            &format!(
                "{}\n{}\n{}",
                rows[0][8..].trim_end(),
                rows[1].trim_end(),
                rows[2][..=8].trim_end()
            ),
        );
        run((39, 5), (10, 5), rows[2][..=8].trim_end());
        run((39, 5), (70, 5), rows[2][8..].trim_end());
        run(
            (39, 5),
            (39, 30),
            &format!(
                "{}\n{}\n{}\n{}",
                rows[2][8..].trim_end(),
                rows[3].trim_end(),
                rows[4].trim_end(),
                rows[5][..=8].trim_end()
            ),
        );

        // Top/left out-of-bounds is not collapsed into a plain click: the
        // pointer left the anchor cell, clamping pinned the cursor back onto
        // it, and the gesture still counts as a drag that copies the anchor
        // cell — symmetric with right/bottom force-dragging.
        run((31, 3), (10, 0), rows[0][..=0].trim_end());
    }

    #[cfg(unix)]
    #[test]
    fn projected_selection_generation_replacement_keeps_gesture_local() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::LiveController,
            Some(projected_text_frame()),
            true,
            true,
        );
        app.remote_projection_runtime
            .test_insert_writable_stream_handle("term-1", Some(false));

        let (down_col, down_row) = projected_screen(0, 0);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));
        assert!(app.state.projected_selection.is_some());
        assert!(app.state.projected_selection_gesture_active);

        // A generation advance clears the selection overlay/key mid-gesture...
        let selected = app.state.selected_remote_space.clone().expect("selected");
        app.state
            .begin_remote_projection_generation(Some(&selected), true);
        assert!(app.state.projected_selection.is_none());
        assert!(
            app.state.projected_selection_gesture_active,
            "clearing the overlay must not release local gesture ownership"
        );

        // ...and a replacement control stream goes live with application
        // mouse tracking enabled: without gesture ownership, the remaining
        // drag/up would be forwarded to the remote runtime.
        let key = crate::remote_source::RemoteProjectionTerminalKey {
            host: "remote-a".into(),
            session: "default".into(),
            workspace_id: "ws-a".into(),
            terminal_id: "term-1".into(),
        };
        let generation = app.state.remote_projection_generation;
        app.state.seed_remote_projection_streams(
            generation,
            [(
                key.clone(),
                crate::remote_source::RemoteProjectionStreamRole::Control,
                crate::remote_source::RemoteProjectionStreamStatus::Connecting,
                None,
            )],
        );
        app.state.apply_remote_projection_stream_event(
            key,
            generation,
            crate::remote_source::RemoteProjectionStreamRole::Control,
            crate::remote_source::RemoteProjectionStreamStatus::LiveController,
            Some(projected_text_frame()),
            None,
        );
        app.remote_projection_runtime
            .test_insert_writable_stream_handle("term-1", Some(true));

        let (drag_col, drag_row) = projected_screen(1, 5);
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            drag_col,
            drag_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            drag_col,
            drag_row,
        ));

        assert!(
            !app.remote_projection_runtime
                .test_stream_input_sent("term-1"),
            "the tail of a locally-started gesture must never reach a replacement stream"
        );
        assert_no_clipboard_write(&mut app);
        assert!(
            !app.state.projected_selection_gesture_active,
            "mouse-up always releases local gesture ownership"
        );
    }

    #[test]
    fn projected_selection_source_switch_mid_gesture_stays_local() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::OwnedReadOnly,
            Some(projected_text_frame()),
            true,
            true,
        );

        let (down_col, down_row) = projected_screen(0, 0);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));
        assert!(app.state.projected_selection.is_some());

        // Switching back to the local source mid-gesture clears the selected
        // space and the overlay; the gesture's remaining drag/up are still
        // consumed locally and never leak into a local pane selection.
        app.state.sidebar_source = crate::app::state::SidebarSource::Remote(
            crate::remote_source::RemoteHostKey::new("remote-a", "default"),
        );
        app.state
            .select_sidebar_source(crate::app::state::SidebarSource::Local);
        assert!(app.state.selected_remote_space.is_none());
        assert!(app.state.projected_selection.is_none());
        assert!(app.state.projected_selection_gesture_active);

        let (drag_col, drag_row) = projected_screen(1, 5);
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            drag_col,
            drag_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            drag_col,
            drag_row,
        ));

        assert!(
            app.state.selection.is_none(),
            "no local pane selection may start from a projected gesture"
        );
        assert_no_clipboard_write(&mut app);
        assert!(!app.state.projected_selection_gesture_active);
    }

    #[test]
    fn projected_selection_target_cleared_mid_gesture_releases_without_copy() {
        let mut app = projected_selection_app(
            crate::remote_source::RemoteProjectionStreamStatus::OwnedReadOnly,
            Some(projected_text_frame()),
            true,
            true,
        );

        let (down_col, down_row) = projected_screen(0, 0);
        let (drag_col, drag_row) = projected_screen(1, 5);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            down_col,
            down_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            drag_col,
            drag_row,
        ));
        assert!(app.state.projected_selection.is_some());

        // The exact target frame is replaced/cleared mid-gesture: mouse-up
        // copies nothing, sends nothing remote, and still releases ownership.
        app.state.remote_projection_frames.clear();
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            drag_col,
            drag_row,
        ));

        assert_no_clipboard_write(&mut app);
        assert!(
            !app.state.projected_selection_gesture_active,
            "mouse-up always releases local gesture ownership"
        );
    }
}
