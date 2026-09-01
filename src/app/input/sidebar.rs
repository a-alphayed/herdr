use ratatui::layout::Rect;

use crate::app::state::{AppState, Mode, ViewLayout};

use super::ScrollbarClickTarget;

impl AppState {
    pub(super) fn workspace_list_rect(&self) -> Rect {
        let sidebar = self.view.sidebar_panel_rect;
        if self.sidebar_collapsed || sidebar.width <= 1 || sidebar.height == 0 {
            return Rect::default();
        }
        crate::ui::workspace_list_rect(sidebar, self.sidebar_section_split)
    }

    pub(super) fn agent_panel_rect(&self) -> Rect {
        let sidebar = self.view.sidebar_panel_rect;
        if self.sidebar_collapsed || sidebar.width <= 1 || sidebar.height == 0 {
            return Rect::default();
        }
        let (_, detail_area) =
            crate::ui::expanded_sidebar_sections(sidebar, self.sidebar_section_split);
        detail_area
    }

    pub(super) fn workspace_list_scrollbar_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<ScrollbarClickTarget> {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        let track = crate::ui::workspace_list_scrollbar_rect(self, area)?;
        if col < track.x
            || col >= track.x + track.width
            || row < track.y
            || row >= track.y + track.height
        {
            return None;
        }
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some(ScrollbarClickTarget::Thumb { grab_row_offset })
        } else {
            Some(ScrollbarClickTarget::Track {
                offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
            })
        }
    }

    pub(super) fn workspace_list_offset_for_drag_row(
        &self,
        row: u16,
        grab_row_offset: u16,
    ) -> Option<usize> {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        let track = crate::ui::workspace_list_scrollbar_rect(self, area)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    pub(super) fn set_workspace_list_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        self.workspace_scroll = metrics
            .max_offset_from_bottom
            .saturating_sub(offset_from_bottom);
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_panel_rect,
            self.workspace_scroll,
        );
    }

    pub(super) fn scroll_workspace_list(&mut self, delta: i16) {
        if delta.is_negative() {
            self.workspace_scroll = self
                .workspace_scroll
                .saturating_sub(delta.unsigned_abs() as usize);
            self.workspace_scroll = crate::ui::normalized_workspace_scroll(
                self,
                self.view.sidebar_panel_rect,
                self.workspace_scroll,
            );
            return;
        }

        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        self.workspace_scroll = self
            .workspace_scroll
            .saturating_add(delta as usize)
            .min(metrics.max_offset_from_bottom);
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_panel_rect,
            self.workspace_scroll,
        );
    }

    pub(super) fn agent_panel_scrollbar_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<ScrollbarClickTarget> {
        let area = self.agent_panel_rect();
        let metrics = crate::ui::agent_panel_scroll_metrics(self, area);
        let track = crate::ui::agent_panel_scrollbar_rect(self, area)?;
        if col < track.x
            || col >= track.x + track.width
            || row < track.y
            || row >= track.y + track.height
        {
            return None;
        }
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some(ScrollbarClickTarget::Thumb { grab_row_offset })
        } else {
            Some(ScrollbarClickTarget::Track {
                offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
            })
        }
    }

    pub(super) fn agent_panel_offset_for_drag_row(
        &self,
        row: u16,
        grab_row_offset: u16,
    ) -> Option<usize> {
        let area = self.agent_panel_rect();
        let metrics = crate::ui::agent_panel_scroll_metrics(self, area);
        let track = crate::ui::agent_panel_scrollbar_rect(self, area)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    pub(super) fn set_agent_panel_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let area = self.agent_panel_rect();
        let metrics = crate::ui::agent_panel_scroll_metrics(self, area);
        self.agent_panel_scroll = metrics
            .max_offset_from_bottom
            .saturating_sub(offset_from_bottom);
    }

    pub(super) fn scroll_agent_panel(&mut self, delta: i16) {
        let area = self.agent_panel_rect();
        let max_scroll = crate::ui::agent_panel_scroll_metrics(self, area).max_offset_from_bottom;
        if delta.is_negative() {
            self.agent_panel_scroll = self
                .agent_panel_scroll
                .saturating_sub(delta.unsigned_abs() as usize);
        } else {
            self.agent_panel_scroll = self
                .agent_panel_scroll
                .saturating_add(delta as usize)
                .min(max_scroll);
        }
    }

    pub(crate) fn sidebar_footer_rect(&self) -> Rect {
        crate::ui::workspace_list_footer_rect(self.workspace_list_rect())
    }

    pub(crate) fn sidebar_local_actions_rect(&self) -> Rect {
        crate::ui::workspace_list_local_actions_rect(self, self.view.sidebar_panel_rect)
    }

    pub(crate) fn sidebar_new_button_rect(&self) -> Rect {
        crate::ui::workspace_list_new_button_rect(self.sidebar_local_actions_rect())
    }

    pub(crate) fn global_launcher_rect(&self) -> Rect {
        if self.view.layout == ViewLayout::Mobile {
            return self.view.mobile_menu_hit_area;
        }

        crate::ui::workspace_list_menu_button_rect(
            self.sidebar_local_actions_rect(),
            self.global_menu_attention_badge_visible(),
        )
    }

    pub(crate) fn global_menu_labels(&self) -> Vec<&'static str> {
        let mut labels = vec!["settings", "keybinds", "reload config"];
        if self.update_available.is_some() {
            labels.push("update ready");
        } else if self.latest_release_notes_available {
            labels.push("what's new");
        }
        labels.push("detach");
        labels
    }

    pub(crate) fn global_menu_rect(&self) -> Rect {
        let screen = self.screen_rect();
        let launcher = self.global_launcher_rect();
        let labels = self.global_menu_labels();
        let content_width = labels
            .iter()
            .map(|label| {
                let badge_width = if self.global_menu_item_has_badge(label) {
                    2
                } else {
                    0
                };
                label.chars().count() as u16 + badge_width
            })
            .max()
            .unwrap_or(8)
            .saturating_add(2);
        let menu_w = content_width.saturating_add(2).min(screen.width.max(1));
        let menu_h = (labels.len() as u16 + 2).min(screen.height.max(1));
        let max_x = screen.x + screen.width.saturating_sub(menu_w);
        let desired_x = launcher.x + launcher.width.saturating_sub(menu_w);
        let x = desired_x.min(max_x);
        let max_y = screen.y + screen.height.saturating_sub(menu_h);
        let y = if launcher.y >= menu_h {
            launcher.y - menu_h
        } else {
            launcher.y.saturating_add(launcher.height).min(max_y)
        };
        Rect::new(x, y, menu_w, menu_h)
    }

    pub(super) fn on_sidebar_divider(&self, col: u16, row: u16) -> bool {
        if self.sidebar_collapsed {
            return false;
        }
        let sidebar = self.view.sidebar_rect;
        let toggle = crate::ui::expanded_sidebar_toggle_rect(sidebar);
        let on_toggle = toggle.width > 0
            && col >= toggle.x
            && col < toggle.x + toggle.width
            && row >= toggle.y
            && row < toggle.y + toggle.height;
        sidebar.width > 0
            && !on_toggle
            && col == sidebar.x + sidebar.width.saturating_sub(1)
            && row >= sidebar.y
            && row < sidebar.y + sidebar.height
    }

    pub(super) fn on_sidebar_toggle(&self, col: u16, row: u16) -> bool {
        let rect = if self.sidebar_collapsed {
            crate::ui::collapsed_sidebar_toggle_rect(self.view.sidebar_rect)
        } else {
            crate::ui::expanded_sidebar_toggle_rect(self.view.sidebar_rect)
        };
        rect.width > 0
            && col >= rect.x
            && col < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height
    }

    pub(super) fn set_manual_sidebar_width(&mut self, divider_col: u16) {
        let panel = self.view.sidebar_panel_rect;
        let width = divider_col.saturating_sub(panel.x).saturating_add(1);
        self.sidebar_width = width.clamp(self.sidebar_min_width, self.sidebar_max_width);
        self.sidebar_width_source = crate::app::state::SidebarWidthSource::Manual;
        self.mark_session_dirty();
    }

    pub(super) fn on_sidebar_section_divider(&self, col: u16, row: u16) -> bool {
        if self.sidebar_collapsed {
            return false;
        }
        let rect = crate::ui::sidebar_section_divider_rect(
            self.view.sidebar_panel_rect,
            self.sidebar_section_split,
        );
        rect.width > 0
            && col >= rect.x
            && col < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height
    }

    pub(super) fn set_sidebar_section_split(&mut self, row: u16) {
        let sidebar = self.view.sidebar_panel_rect;
        let content_height = sidebar.height;
        if content_height < 6 {
            return;
        }
        let relative_y = row.saturating_sub(sidebar.y);
        let ratio = (relative_y as f32) / (content_height as f32);
        self.sidebar_section_split = ratio.clamp(0.1, 0.9);
        self.mark_session_dirty();
    }

    pub(super) fn workspace_at_row(&self, row: u16) -> Option<usize> {
        let footer = self.sidebar_footer_rect();
        if footer == Rect::default() {
            return None;
        }

        let cards = if self.view.workspace_card_areas.is_empty() {
            crate::ui::compute_workspace_card_areas(self, self.view.sidebar_panel_rect)
        } else {
            self.view.workspace_card_areas.clone()
        };

        cards.iter().find_map(|card| {
            (row >= card.rect.y && row < card.rect.y + card.rect.height).then_some(card.ws_idx)
        })
    }

    pub(super) fn host_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<crate::app::state::SidebarSource> {
        if self.sidebar_collapsed {
            return None;
        }

        crate::ui::host_target_at(self, col, row)
    }

    pub(super) fn on_host_rail(&self, col: u16, row: u16) -> bool {
        if self.sidebar_collapsed {
            return false;
        }
        let rect = self.view.host_rail_rect;
        rect.width > 0
            && rect.height > 0
            && col >= rect.x
            && col < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height
    }

    pub(super) fn host_rail_rect(&self) -> Rect {
        if self.sidebar_collapsed {
            return Rect::default();
        }
        self.view.host_rail_rect
    }

    pub(super) fn host_list_scrollbar_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<ScrollbarClickTarget> {
        let area = self.host_rail_rect();
        let metrics = crate::ui::host_list_scroll_metrics(self, area);
        let track = crate::ui::host_list_scrollbar_rect(self, area)?;
        if col < track.x
            || col >= track.x + track.width
            || row < track.y
            || row >= track.y + track.height
        {
            return None;
        }
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some(ScrollbarClickTarget::Thumb { grab_row_offset })
        } else {
            Some(ScrollbarClickTarget::Track {
                offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
            })
        }
    }

    pub(super) fn host_list_offset_for_drag_row(
        &self,
        row: u16,
        grab_row_offset: u16,
    ) -> Option<usize> {
        let area = self.host_rail_rect();
        let metrics = crate::ui::host_list_scroll_metrics(self, area);
        let track = crate::ui::host_list_scrollbar_rect(self, area)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    pub(super) fn set_host_list_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let area = self.view.host_rail_rect;
        let metrics = crate::ui::host_list_scroll_metrics(self, area);
        self.host_list_scroll = metrics
            .max_offset_from_bottom
            .saturating_sub(offset_from_bottom);
        self.host_list_scroll = crate::ui::normalized_host_list_scroll(self, self.host_list_scroll);
    }

    pub(super) fn scroll_host_list(&mut self, delta: i16) {
        let area = self.view.host_rail_rect;
        let metrics = crate::ui::host_list_scroll_metrics(self, area);
        let max_offset = metrics.max_offset_from_bottom;
        if delta.is_negative() {
            self.host_list_scroll = self
                .host_list_scroll
                .saturating_sub(delta.unsigned_abs() as usize);
        } else {
            self.host_list_scroll = self
                .host_list_scroll
                .saturating_add(delta as usize)
                .min(max_offset);
        }
        self.host_list_scroll = crate::ui::normalized_host_list_scroll(self, self.host_list_scroll);
    }

    pub(super) fn collapsed_workspace_at_row(&self, row: u16) -> Option<usize> {
        if !self.sidebar_collapsed {
            return None;
        }

        let (ws_area, _, _) = crate::ui::collapsed_sidebar_sections(self.view.sidebar_rect);
        if ws_area == Rect::default() || row < ws_area.y || row >= ws_area.y + ws_area.height {
            return None;
        }

        let idx = (row - ws_area.y) as usize;
        (idx < self.workspaces.len()).then_some(idx)
    }

    fn collapsed_detail_workspace_idx(&self) -> Option<usize> {
        if matches!(
            self.mode,
            Mode::Navigate
                | Mode::RenameWorkspace
                | Mode::Resize
                | Mode::ConfirmClose
                | Mode::ConfirmRemoteProjectedPaneClose
                | Mode::ConfirmRemoteProjectedTabClose
                | Mode::ContextMenu
                | Mode::Settings
                | Mode::GlobalMenu
                | Mode::KeybindHelp
        ) {
            Some(self.selected)
        } else {
            self.active
        }
    }

    pub(super) fn collapsed_agent_detail_target_at(
        &self,
        row: u16,
    ) -> Option<(usize, usize, crate::layout::PaneId)> {
        if !self.sidebar_collapsed {
            return None;
        }

        let (_, _, detail_area) = crate::ui::collapsed_sidebar_sections(self.view.sidebar_rect);
        let detail_content_area = Rect::new(
            detail_area.x,
            detail_area.y,
            detail_area.width,
            detail_area.height.saturating_sub(1),
        );
        if detail_content_area == Rect::default()
            || row < detail_content_area.y
            || row >= detail_content_area.y + detail_content_area.height
        {
            return None;
        }

        let ws_idx = self.collapsed_detail_workspace_idx()?;
        let ws = self.workspaces.get(ws_idx)?;
        let detail_idx = (row - detail_content_area.y) as usize;
        let details = ws.pane_details(&self.terminals);
        let detail = details.get(detail_idx)?;
        Some((ws_idx, detail.tab_idx, detail.pane_id))
    }

    pub(super) fn workspace_drop_index_at_row(&self, row: u16) -> Option<usize> {
        let area = self.workspace_list_rect();
        let footer = self.sidebar_footer_rect();
        if area == Rect::default() || row < area.y || row >= footer.y {
            return None;
        }

        let cards = if self.view.workspace_card_areas.is_empty() {
            crate::ui::compute_workspace_card_areas(self, self.view.sidebar_panel_rect)
        } else {
            self.view.workspace_card_areas.clone()
        };
        if cards.is_empty() {
            return Some(0);
        }

        let mut insert_indices = Vec::with_capacity(cards.len() + 1);
        for (idx, card) in cards.iter().enumerate() {
            let card_group = self
                .workspaces
                .get(card.ws_idx)
                .and_then(|ws| ws.worktree_space())
                .map(|space| space.key.as_str());
            let previous_group = idx.checked_sub(1).and_then(|prev_idx| {
                self.workspaces
                    .get(cards[prev_idx].ws_idx)
                    .and_then(|ws| ws.worktree_space())
                    .map(|space| space.key.as_str())
            });
            let inside_group_gap = card_group.is_some() && card_group == previous_group;
            if !inside_group_gap {
                insert_indices.push(card.ws_idx);
            }
        }
        insert_indices.push(cards.last().map(|card| card.ws_idx + 1).unwrap_or(0));

        let mut best: Option<(usize, u16)> = None;
        for insert_idx in insert_indices {
            let Some(slot_row) = crate::ui::workspace_drop_indicator_row(&cards, area, insert_idx)
            else {
                continue;
            };
            let distance = row.abs_diff(slot_row);
            match best {
                Some((best_idx, best_distance))
                    if distance > best_distance
                        || (distance == best_distance && insert_idx < best_idx) => {}
                _ => best = Some((insert_idx, distance)),
            }
        }

        best.map(|(insert_idx, _)| insert_idx)
    }

    pub(super) fn on_agent_panel_sort_toggle(&self, col: u16, row: u16) -> bool {
        if self.sidebar_collapsed {
            return false;
        }
        let (_, detail_area) = crate::ui::expanded_sidebar_sections(
            self.view.sidebar_rect,
            self.sidebar_section_split,
        );
        let rect = crate::ui::agent_panel_toggle_rect(detail_area, self.agent_panel_sort);
        rect.width > 0
            && col >= rect.x
            && col < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height
    }

    pub(super) fn agent_detail_entry_at(&self, row: u16) -> Option<crate::ui::AgentPanelEntry> {
        if self.sidebar_collapsed {
            return None;
        }

        let detail_area = self.agent_panel_rect();
        let metrics = crate::ui::agent_panel_scroll_metrics(self, detail_area);
        let body = crate::ui::agent_panel_body_rect(
            detail_area,
            crate::ui::should_show_scrollbar(metrics),
        );
        if body.height == 0 || row < body.y || row >= body.y + body.height {
            return None;
        }

        let mut row_y = body.y;
        let body_bottom = body.y + body.height;
        for detail in crate::ui::agent_panel_entries(self)
            .into_iter()
            .skip(self.agent_panel_scroll)
        {
            let content_height = crate::ui::agent_panel_entry_content_height(&detail);
            if row_y.saturating_add(content_height) > body_bottom {
                break;
            }
            if row >= row_y && row < row_y.saturating_add(content_height) {
                return Some(detail);
            }
            row_y = row_y.saturating_add(content_height);
            let gap = crate::ui::agent_panel_entry_gap_after(&detail);
            if gap > 0 && row_y < body_bottom {
                row_y = row_y.saturating_add(gap);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crossterm::event::{MouseButton, MouseEventKind};
    use ratatui::layout::Rect;

    use super::super::{app_for_mouse_test, capture_snapshot, mouse, unique_temp_path};
    use crate::{
        app::state::{AgentPanelSort, ContextMenuKind, DragTarget, Mode},
        config::SidebarCollapsedModeConfig,
        detect::Agent,
        remote_source::{RemoteHostKey, RemoteSourceCapabilities},
        workspace::Workspace,
    };

    fn remote_space_key() -> crate::remote_source::RemoteSpaceKey {
        crate::remote_source::RemoteSpaceKey {
            host: "jafar".into(),
            session: crate::session::DEFAULT_SESSION_NAME.into(),
            workspace_id: "remote-ws".into(),
        }
    }

    fn compute_desktop_view(app: &mut crate::app::App) {
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 32));
    }

    #[test]
    fn clicking_launcher_opens_global_menu() {
        let mut app = app_for_mouse_test();
        let rect = app.state.global_launcher_rect();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + rect.width.saturating_sub(1),
            rect.y,
        ));

        assert_eq!(app.state.mode, Mode::GlobalMenu);
    }

    #[test]
    fn hovering_global_menu_updates_highlight() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(MouseEventKind::Moved, menu.x + 2, menu.y + 2));

        assert_eq!(app.state.global_menu.highlighted, 1);
    }

    #[test]
    fn clicking_keybinds_menu_item_opens_help() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 2,
        ));

        assert_eq!(app.state.mode, Mode::KeybindHelp);
    }

    #[test]
    fn clicking_settings_menu_item_opens_settings() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 1,
        ));

        assert_eq!(app.state.mode, Mode::Settings);
    }

    #[test]
    fn clicking_reload_config_menu_item_requests_reload() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 3,
        ));

        assert!(app.state.request_reload_config);
        assert_eq!(app.state.mode, Mode::Navigate);
    }

    #[test]
    fn update_pending_menu_surfaces_update_ready_entry() {
        let mut app = app_for_mouse_test();
        app.state.update_available = Some("0.3.2".into());
        app.state.latest_release_notes_available = true;

        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        assert_eq!(
            app.state.global_menu_labels(),
            vec![
                "settings",
                "keybinds",
                "reload config",
                "update ready",
                "detach"
            ]
        );
        assert!(!app.state.should_quit);
    }

    #[test]
    fn persistence_mode_menu_surfaces_detach_action() {
        let mut app = app_for_mouse_test();
        app.state.detach_exits = false;

        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        assert_eq!(
            app.state.global_menu_labels(),
            vec!["settings", "keybinds", "reload config", "detach"]
        );

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 4,
        ));

        assert!(app.state.detach_requested);
        assert!(!app.state.should_quit);
        assert_ne!(app.state.mode, Mode::GlobalMenu);
    }

    #[test]
    fn whats_new_remains_in_menu_for_latest_installed_release_notes() {
        let mut app = app_for_mouse_test();
        app.state.latest_release_notes_available = true;

        assert_eq!(
            app.state.global_menu_labels(),
            vec![
                "settings",
                "keybinds",
                "reload config",
                "what's new",
                "detach"
            ]
        );
    }

    #[test]
    fn clicking_agent_detail_row_switches_to_correct_tab_and_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("main".into());
        let first_pane = ws.tabs[0].root_pane;
        let first_tab = ws.test_add_tab(Some("logs"));
        let second_pane = ws.tabs[first_tab].root_pane;
        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[0].tabs[first_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 16));

        assert_eq!(app.state.workspaces[0].active_tab, 1);
        assert_eq!(
            app.state.workspaces[0].tabs[1].layout.focused(),
            second_pane
        );
        assert_eq!(app.state.mode, Mode::Terminal);
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.workspaces[0].active_tab, first_tab);
        assert_eq!(
            snapshot.workspaces[0].tabs[first_tab].focused,
            Some(second_pane.raw())
        );
    }

    #[test]
    fn clicking_host_row_switches_projection_without_touching_local_focus_or_dirty_state() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.state.ensure_test_terminals();
        app.state.active = Some(1);
        app.state.selected = 1;
        app.state.mode = Mode::Terminal;
        let focused_before = app.state.workspaces[1].focused_pane_id();
        let first = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        let second = RemoteHostKey::new("work", crate::session::DEFAULT_SESSION_NAME);
        app.state.remote_sources.mark_status(
            &first,
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        app.state.remote_sources.mark_status(
            &second,
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        app.state.selected_remote_space = Some(remote_space_key());
        app.state.workspace_scroll = 2;
        app.state.agent_panel_scroll = 1;
        app.state.session_dirty = false;
        compute_desktop_view(&mut app);
        // Preserve-exactly contract: source selection never mutates remote
        // state/authority (the cached `remote_sources` registry). Capture
        // both hosts' cached status up front and reassert it is bit-for-bit
        // unchanged after every switch below.
        let remote_cache_before = format!("{:?}", app.state.remote_sources);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 2));

        assert_eq!(
            app.state.sidebar_source,
            crate::app::state::SidebarSource::Remote(first.clone())
        );
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].focused_pane_id(), focused_before);
        assert!(app.state.selected_remote_space.is_none());
        assert_eq!(app.state.workspace_scroll, 0);
        assert_eq!(app.state.agent_panel_scroll, 0);
        assert!(!app.state.session_dirty);
        assert!(app.state.terminal_runtime_shutdowns.is_empty());
        assert_eq!(
            format!("{:?}", app.state.remote_sources),
            remote_cache_before
        );

        app.state.selected_remote_space = Some(remote_space_key());
        app.state.workspace_scroll = 3;
        app.state.agent_panel_scroll = 2;
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 3));

        assert_eq!(
            app.state.sidebar_source,
            crate::app::state::SidebarSource::Remote(second)
        );
        assert!(app.state.selected_remote_space.is_none());
        assert_eq!(app.state.workspace_scroll, 0);
        assert_eq!(app.state.agent_panel_scroll, 0);
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].focused_pane_id(), focused_before);
        assert!(!app.state.session_dirty);
        assert_eq!(
            format!("{:?}", app.state.remote_sources),
            remote_cache_before
        );
    }

    #[test]
    fn blank_host_rail_left_click_and_drag_are_consumed() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
        ];
        app.state.ensure_test_terminals();
        app.state.active = Some(1);
        app.state.selected = 1;
        app.state.mode = Mode::Terminal;
        app.state.remote_sources.mark_status(
            &RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME),
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        app.state.selected_remote_space = Some(remote_space_key());
        compute_desktop_view(&mut app);
        let focused_before = app.state.workspaces[1].focused_pane_id();
        let names_before: Vec<_> = app
            .state
            .workspaces
            .iter()
            .map(|workspace| workspace.display_name())
            .collect();
        // The host rail's header row is inside the rail but is not a host
        // target, so it is the canonical "blank" host-rail click area.
        let blank_col = app.state.view.host_rail_rect.x;
        let blank_row = app.state.view.host_rail_rect.y;
        assert!(app.state.on_host_rail(blank_col, blank_row));
        assert!(app.state.host_target_at(blank_col, blank_row).is_none());

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            blank_col,
            blank_row,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(app.state.workspaces[1].focused_pane_id(), focused_before);
        assert_eq!(app.state.selected_remote_space, Some(remote_space_key()));
        assert!(!app.state.request_new_workspace);
        assert!(app.state.context_menu.is_none());
        assert!(app.state.workspace_press.is_none());
        assert!(app.state.drag.is_none());

        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            blank_col,
            blank_row.saturating_add(6),
        ));
        assert!(!matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::WorkspaceReorder { .. })
        ));

        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            blank_col,
            blank_row.saturating_add(6),
        ));
        let names_after: Vec<_> = app
            .state
            .workspaces
            .iter()
            .map(|workspace| workspace.display_name())
            .collect();
        assert_eq!(names_after, names_before);
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert!(app.state.workspace_press.is_none());
        assert!(app.state.drag.is_none());
    }

    #[test]
    fn blank_host_rail_right_click_is_consumed() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("local")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.remote_sources.mark_status(
            &RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME),
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        compute_desktop_view(&mut app);

        // The host rail's header row is inside the rail but is not a host
        // target. Right-clicking it must stay consumed (no remote menu, no
        // source switch), mirroring local/blank host-rail consumption.
        let blank_col = app.state.view.host_rail_rect.x;
        let blank_row = app.state.view.host_rail_rect.y;
        assert!(app.state.on_host_rail(blank_col, blank_row));
        assert!(app.state.host_target_at(blank_col, blank_row).is_none());

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            blank_col,
            blank_row,
        ));

        assert!(app.state.context_menu.is_none());
        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(
            app.state.sidebar_source,
            crate::app::state::SidebarSource::Local
        );
    }

    #[test]
    fn left_click_local_footer_new_with_remote_source_cached_requests_only_local_workspace() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("local")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.state
            .remote_sources
            .replace_connected_snapshot(host.clone(), Vec::new());
        app.state
            .remote_sources
            .replace_workspace_snapshot(host.clone(), Vec::new());
        app.state.remote_sources.set_capabilities(
            &host,
            RemoteSourceCapabilities {
                workspace_list_local: true,
                workspace_create: true,
                tab_list: true,
                layout_export: true,
                ..Default::default()
            },
        );
        compute_desktop_view(&mut app);
        let local_new = app.state.sidebar_new_button_rect();

        let action = app.state.handle_mouse(
            &mut app.terminal_runtimes,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                local_new.x.saturating_add(1),
                local_new.y,
            ),
        );

        assert!(action.is_none());
        assert!(app.state.request_new_workspace);
        assert!(app.state.context_menu.is_none());
    }

    #[test]
    fn scrolled_local_workspace_list_keeps_footer_new_and_menu_hit_targets() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = (0..10)
            .map(|idx| Workspace::test_new(&format!("local-{idx}")))
            .collect();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        compute_desktop_view(&mut app);
        app.state.workspace_scroll = app.state.workspaces.len();

        let new_button = app.state.sidebar_new_button_rect();
        let launcher = app.state.global_launcher_rect();
        let footer = app.state.sidebar_footer_rect();
        assert_ne!(new_button, Rect::default());
        assert_ne!(launcher, Rect::default());
        assert_eq!(new_button.y, footer.y);
        assert_eq!(launcher.y, footer.y);

        let list = app.state.workspace_list_rect();
        let old_body_row = list.y.saturating_add(2);
        let action = app.state.handle_mouse(
            &mut app.terminal_runtimes,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                list.x.saturating_add(1),
                old_body_row,
            ),
        );

        assert!(action.is_none());
        assert!(!app.state.request_new_workspace);
        assert_eq!(app.state.mode, Mode::Terminal);

        let action = app.state.handle_mouse(
            &mut app.terminal_runtimes,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                new_button.x.saturating_add(1),
                new_button.y,
            ),
        );

        assert!(action.is_none());
        assert!(app.state.request_new_workspace);

        app.state.handle_mouse(
            &mut app.terminal_runtimes,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                launcher.x + launcher.width.saturating_sub(1),
                launcher.y,
            ),
        );

        assert_eq!(app.state.mode, Mode::GlobalMenu);
    }

    #[test]
    fn desktop_remote_source_keeps_local_footer_new_and_menu_hit_targets() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("local")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        let host = RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.state.remote_sources.mark_status(
            &host,
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        compute_desktop_view(&mut app);
        app.state
            .select_sidebar_source(crate::app::state::SidebarSource::Remote(host));
        compute_desktop_view(&mut app);

        assert_ne!(app.state.sidebar_new_button_rect(), Rect::default());
        assert_ne!(app.state.global_launcher_rect(), Rect::default());

        let footer = app.state.sidebar_footer_rect();
        let action = app.state.handle_mouse(
            &mut app.terminal_runtimes,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                footer.x.saturating_add(1),
                footer.y,
            ),
        );

        assert!(action.is_none());
        assert!(app.state.request_new_workspace);
        assert_eq!(app.state.mode, Mode::Terminal);

        app.state.handle_mouse(
            &mut app.terminal_runtimes,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                footer.x + footer.width.saturating_sub(1),
                footer.y,
            ),
        );

        assert_eq!(app.state.mode, Mode::GlobalMenu);
        assert!(app.state.context_menu.is_none());
    }

    #[test]
    fn global_menu_rect_opens_from_fixed_local_footer_row() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = Vec::new();
        app.state.active = None;
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 140, 32));

        let empty_launcher = app.state.global_launcher_rect();
        let empty_menu = app.state.global_menu_rect();
        let empty_render_footer =
            crate::ui::workspace_list_footer_rect(app.state.workspace_list_rect());
        assert_ne!(empty_launcher, Rect::default());
        assert_eq!(empty_render_footer.y, app.state.sidebar_footer_rect().y);
        assert_eq!(empty_launcher.y, app.state.sidebar_footer_rect().y);
        assert!(empty_launcher.y >= empty_menu.height);
        assert_eq!(empty_menu.y + empty_menu.height, empty_launcher.y);

        app.state.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
        ];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.workspace_scroll = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 140, 32));

        let populated_launcher = app.state.global_launcher_rect();
        let populated_menu = app.state.global_menu_rect();
        let populated_render_footer =
            crate::ui::workspace_list_footer_rect(app.state.workspace_list_rect());
        assert_ne!(populated_launcher, Rect::default());
        assert_eq!(populated_render_footer.y, app.state.sidebar_footer_rect().y);
        assert_eq!(populated_launcher.y, app.state.sidebar_footer_rect().y);
        assert!(populated_launcher.y >= populated_menu.height);
        assert_eq!(
            populated_menu.y + populated_menu.height,
            populated_launcher.y
        );
    }

    #[test]
    fn right_click_remote_host_row_opens_remote_source_menu_without_switching_source() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("local")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.remote_sources.mark_status(
            &RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME),
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        compute_desktop_view(&mut app);
        // Stay on the local projection so we can prove right-click does not
        // switch the active projected source.
        app.state
            .select_sidebar_source(crate::app::state::SidebarSource::Local);
        compute_desktop_view(&mut app);

        let section = app.state.view.host_rail_rect;
        let host_row = (section.y..section.y + section.height)
            .find(|row| {
                matches!(
                    app.state.host_target_at(section.x, *row),
                    Some(crate::app::state::SidebarSource::Remote(ref host))
                        if host.host == "jafar"
                )
            })
            .expect("remote host row for jafar");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            section.x,
            host_row,
        ));

        let menu = app
            .state
            .context_menu
            .as_ref()
            .expect("remote source context menu opens");
        assert!(matches!(
            menu.kind,
            ContextMenuKind::RemoteSource { ref host }
                if host == &RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME)
        ));
        assert_eq!(
            menu.items(),
            &[
                "Copy remote diagnostics command",
                "Copy full remote command"
            ]
        );
        // Right-click must not switch the active projected source.
        assert_eq!(
            app.state.sidebar_source,
            crate::app::state::SidebarSource::Local
        );
        assert_eq!(app.state.mode, Mode::ContextMenu);
    }

    #[test]
    fn right_click_local_host_row_stays_consumed_without_remote_menu() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("local")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.remote_sources.mark_status(
            &RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME),
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        compute_desktop_view(&mut app);

        let section = app.state.view.host_rail_rect;
        let local_row = (section.y..section.y + section.height)
            .find(|row| {
                matches!(
                    app.state.host_target_at(section.x, *row),
                    Some(crate::app::state::SidebarSource::Local)
                )
            })
            .expect("local host row");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            section.x,
            local_row,
        ));

        // Local rail rows must not get remote actions; right-click stays
        // consumed (no context menu, mode unchanged).
        assert!(app.state.context_menu.is_none());
        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(
            app.state.sidebar_source,
            crate::app::state::SidebarSource::Local
        );
    }

    #[test]
    fn clicking_agent_panel_toggle_switches_sort() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.agent_panel_scroll = 3;

        let (_, detail_area) = crate::ui::expanded_sidebar_sections(
            app.state.view.sidebar_rect,
            app.state.sidebar_section_split,
        );
        let toggle = crate::ui::agent_panel_toggle_rect(detail_area, app.state.agent_panel_sort);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            toggle.x,
            toggle.y,
        ));

        assert_eq!(app.state.agent_panel_sort, AgentPanelSort::Priority);
        assert_eq!(app.state.agent_panel_scroll, 0);
    }

    #[test]
    fn clicking_all_workspaces_agent_row_switches_to_correct_workspace() {
        let mut app = app_for_mouse_test();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;

        let second = Workspace::test_new("two");
        let second_pane = second.tabs[0].root_pane;

        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[1].tabs[0].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        let (_, detail_area) = crate::ui::expanded_sidebar_sections(
            app.state.view.sidebar_rect,
            app.state.sidebar_section_split,
        );
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            detail_area.x + 2,
            detail_area.y + 6,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert_eq!(app.state.workspaces[1].active_tab, 0);
        assert_eq!(
            app.state.workspaces[1].tabs[0].layout.focused(),
            second_pane
        );
    }

    #[test]
    fn scrolling_agent_panel_with_wheel_updates_agent_panel_scroll() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;

        let mut tabs = Vec::new();
        for (tab_name, agent) in [
            ("logs", Agent::Claude),
            ("review", Agent::Codex),
            ("ops", Agent::Gemini),
        ] {
            let tab_idx = ws.test_add_tab(Some(tab_name));
            let pane_id = ws.tabs[tab_idx].root_pane;
            tabs.push((tab_idx, pane_id, agent));
        }

        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        for (tab_idx, pane_id, agent) in tabs {
            let terminal_id = app.state.workspaces[0].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .detected_agent = Some(agent);
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        let detail_area = app.state.agent_panel_rect();
        assert!(crate::ui::should_show_scrollbar(
            crate::ui::agent_panel_scroll_metrics(&app.state, detail_area)
        ));

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            detail_area.x + 1,
            detail_area.y + 4,
        ));

        assert_eq!(app.state.agent_panel_scroll, 1);
        assert_eq!(app.state.selected, 0);
    }

    #[test]
    fn clicking_scrolled_agent_detail_row_switches_to_correct_tab_and_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_tab = ws.test_add_tab(Some("logs"));
        let second_pane = ws.tabs[second_tab].root_pane;
        let mut extra_tabs = Vec::new();
        for (tab_name, agent) in [("review", Agent::Codex), ("ops", Agent::Gemini)] {
            let tab_idx = ws.test_add_tab(Some(tab_name));
            let pane_id = ws.tabs[tab_idx].root_pane;
            extra_tabs.push((tab_idx, pane_id, agent));
        }

        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[0].tabs[second_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        for (tab_idx, pane_id, agent) in extra_tabs {
            let terminal_id = app.state.workspaces[0].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .detected_agent = Some(agent);
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.agent_panel_scroll = 1;

        let detail_area = app.state.agent_panel_rect();
        let body = crate::ui::agent_panel_body_rect(detail_area, true);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            body.x + 1,
            body.y,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, second_tab);
        assert_eq!(
            app.state.workspaces[0].tabs[second_tab].layout.focused(),
            second_pane
        );
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn clicking_collapsed_agent_row_switches_to_correct_tab_and_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_tab = ws.test_add_tab(Some("logs"));
        let second_pane = ws.tabs[second_tab].root_pane;
        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[0].tabs[second_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.sidebar_collapsed = true;
        app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
        app.state.view.terminal_area = Rect::new(4, 0, 80, 20);

        let (_, _, detail_area) =
            crate::ui::collapsed_sidebar_sections(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            detail_area.x,
            detail_area.y + 1,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, 1);
        assert_eq!(
            app.state.workspaces[0].tabs[1].layout.focused(),
            second_pane
        );
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn clicking_collapsed_sidebar_toggle_expands_sidebar() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_collapsed = true;
        app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
        app.state.view.terminal_area = Rect::new(4, 0, 80, 20);

        let toggle = crate::ui::collapsed_sidebar_toggle_rect(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            toggle.x,
            toggle.y,
        ));

        assert!(!app.state.sidebar_collapsed);
    }

    #[test]
    fn hidden_collapsed_sidebar_has_no_mouse_expand_hotspot() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_collapsed = true;
        app.state.sidebar_collapsed_mode = SidebarCollapsedModeConfig::Hidden;
        app.state.view.sidebar_rect = Rect::new(0, 0, 0, 20);
        app.state.view.terminal_area = Rect::new(0, 0, 80, 20);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 19));

        assert!(app.state.sidebar_collapsed);
    }

    #[test]
    fn clicking_expanded_sidebar_toggle_collapses_sidebar() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_collapsed = false;
        app.state.view.sidebar_rect = Rect::new(0, 0, 26, 20);
        app.state.view.terminal_area = Rect::new(26, 0, 80, 20);

        let toggle = crate::ui::expanded_sidebar_toggle_rect(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            toggle.x,
            toggle.y,
        ));

        assert!(app.state.sidebar_collapsed);
        assert!(app.state.drag.is_none());
    }

    #[test]
    fn clicking_workspace_switches_on_mouse_up() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let target_row = app.state.view.workspace_card_areas[1].rect.y;
        // Click within the panel content, not the host rail that now occupies
        // the leading columns of the sidebar.
        let target_col = app.state.view.workspace_card_areas[1].rect.x + 2;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target_col,
            target_row,
        ));
        assert_eq!(app.state.active, Some(0));
        assert!(app.state.workspace_press.is_some());

        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            target_col,
            target_row,
        ));
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert!(app.state.workspace_press.is_none());
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.active, Some(1));
        assert_eq!(snapshot.selected, 1);
    }

    #[test]
    fn clicking_worktree_parent_row_focuses_workspace_without_toggling() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("main"), Workspace::test_new("issue")];
        for (idx, checkout_path) in ["/repo/herdr", "/repo/herdr-issue"].into_iter().enumerate() {
            app.state.workspaces[idx].worktree_space =
                Some(crate::workspace::WorktreeSpaceMembership {
                    key: "repo-key".into(),
                    label: "herdr".into(),
                    repo_root: "/repo/herdr".into(),
                    checkout_path: checkout_path.into(),
                    is_linked_worktree: idx > 0,
                });
        }
        app.state.active = None;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let parent = app.state.view.workspace_card_areas[0].rect;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            parent.x + 2,
            parent.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            parent.x + 2,
            parent.y,
        ));

        assert_eq!(app.state.active, Some(0));
        assert!(!app.state.collapsed_space_keys.contains("repo-key"));
    }

    #[test]
    fn clicking_worktree_parent_chevron_toggles_group_only() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("main"), Workspace::test_new("issue")];
        for (idx, checkout_path) in ["/repo/herdr", "/repo/herdr-issue"].into_iter().enumerate() {
            app.state.workspaces[idx].worktree_space =
                Some(crate::workspace::WorktreeSpaceMembership {
                    key: "repo-key".into(),
                    label: "herdr".into(),
                    repo_root: "/repo/herdr".into(),
                    checkout_path: checkout_path.into(),
                    is_linked_worktree: idx > 0,
                });
        }
        app.state.active = None;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let parent = app.state.view.workspace_card_areas[0].rect;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            parent.x,
            parent.y,
        ));

        assert_eq!(app.state.active, None);
        assert!(app.state.workspace_press.is_none());
        assert!(app.state.collapsed_space_keys.contains("repo-key"));

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            parent.x,
            parent.y,
        ));

        assert!(!app.state.collapsed_space_keys.contains("repo-key"));
    }

    #[test]
    fn wheel_workspace_selection_follows_grouped_visual_order_without_scrollbar() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("main"),
            Workspace::test_new("normal"),
            Workspace::test_new("issue"),
        ];
        for (idx, checkout_path) in [(0, "/repo/herdr"), (2, "/repo/herdr-issue")] {
            app.state.workspaces[idx].worktree_space =
                Some(crate::workspace::WorktreeSpaceMembership {
                    key: "repo-key".into(),
                    label: "herdr".into(),
                    repo_root: "/repo/herdr".into(),
                    checkout_path: checkout_path.into(),
                    is_linked_worktree: idx != 0,
                });
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 30));
        let list = app.state.workspace_list_rect();
        assert!(!crate::ui::should_show_scrollbar(
            crate::ui::workspace_list_scroll_metrics(&app.state, list)
        ));

        app.handle_mouse(mouse(MouseEventKind::ScrollDown, list.x + 1, list.y + 1));

        assert_eq!(app.state.selected, 2);
    }

    #[test]
    fn dragging_workspace_reorders_without_changing_identity() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("a"),
            Workspace::test_new("b"),
            Workspace::test_new("c"),
        ];
        let active_id = app.state.workspaces[1].id.clone();
        let selected_id = app.state.workspaces[2].id.clone();
        app.state.active = Some(1);
        app.state.selected = 2;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let source_row = app.state.view.workspace_card_areas[1].rect.y;
        // Click within the panel content, not the host rail that now occupies
        // the leading columns of the sidebar.
        let source_col = app.state.view.workspace_card_areas[1].rect.x + 2;
        let target_row = crate::ui::workspace_drop_indicator_row(
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
            0,
        )
        .unwrap();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            source_col,
            source_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            source_col,
            target_row,
        ));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::WorkspaceReorder {
                source_ws_idx: 1,
                insert_idx: Some(0),
            })
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            source_col,
            target_row,
        ));

        let names: Vec<_> = app
            .state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect();
        assert_eq!(names, vec!["b", "a", "c"]);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.selected, 2);
        assert_eq!(app.state.workspaces[0].id, active_id);
        assert_eq!(app.state.workspaces[2].id, selected_id);
        let snapshot = capture_snapshot(&app.state);
        let captured_names: Vec<_> = snapshot
            .workspaces
            .iter()
            .map(|ws| ws.custom_name.clone().unwrap())
            .collect();
        assert_eq!(captured_names, vec!["b", "a", "c"]);
    }

    #[test]
    fn clicking_tab_scroll_button_reveals_hidden_tabs_without_renaming() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(Some("logs"));
        ws.test_add_tab(Some("review"));
        ws.test_add_tab(Some("ops"));
        ws.test_add_tab(Some("notes"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 65, 20));

        let right = app.state.view.tab_scroll_right_hit_area;
        assert!(right.width > 0);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            right.x + 1,
            right.y,
        ));

        assert_eq!(app.state.tab_scroll, 1);
        assert!(!app.state.tab_scroll_follow_active);
        assert_eq!(app.state.workspaces[0].active_tab, 0);
        assert_eq!(app.state.view.tab_hit_areas[0].width, 0);
        assert!(app.state.workspaces[0].tabs[0].custom_name.is_none());
        assert_eq!(
            app.state.workspaces[0].tabs[1].custom_name.as_deref(),
            Some("logs")
        );
    }

    #[test]
    fn clicking_last_visible_tab_at_right_edge_does_not_overscroll() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        for name in [
            "one", "two", "three", "four", "five", "six", "seven", "eight",
        ] {
            ws.test_add_tab(Some(name));
        }
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.tab_scroll = usize::MAX;
        app.state.tab_scroll_follow_active = false;
        // Widen by the host rail's fixed width so the tab bar/main area gets
        // the same usable width this test was originally calibrated against.
        crate::ui::compute_view(
            &mut app.state,
            Rect::new(0, 0, 65 + crate::ui::host_rail_width(), 20),
        );

        let last_idx = app.state.workspaces[0].tabs.len() - 1;
        let target = app.state.view.tab_hit_areas[last_idx];
        let clamped_scroll = app.state.tab_scroll;
        assert!(target.width > 0, "last tab should already be visible");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.x + 1,
            target.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            target.x + 1,
            target.y,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, last_idx);
        assert_eq!(app.state.tab_scroll, clamped_scroll);
        assert!(app.state.view.tab_hit_areas[last_idx].width > 0);
    }

    #[test]
    fn dragging_tab_reorders_auto_and_custom_names_without_materializing_numbers() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(Some("foo"));
        ws.test_add_tab(None);
        let moved_root = ws.tabs[0].root_pane;
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let source = app.state.view.tab_hit_areas[0];
        let last = app.state.view.tab_hit_areas[2];
        let drop_col = last.x + last.width;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            source.x + 1,
            source.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            drop_col,
            source.y,
        ));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::TabReorder {
                ws_idx: 0,
                source_tab_idx: 0,
                insert_idx: Some(3),
            })
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            drop_col,
            source.y,
        ));

        let labels: Vec<_> = app.state.workspaces[0]
            .tabs
            .iter()
            .enumerate()
            .map(|(tab_idx, _)| app.state.workspaces[0].tab_display_name(tab_idx).unwrap())
            .collect();
        assert_eq!(labels, vec!["foo", "2", "3"]);
        assert_eq!(
            app.state.workspaces[0].tabs[0].custom_name.as_deref(),
            Some("foo")
        );
        assert!(app.state.workspaces[0].tabs[1].custom_name.is_none());
        assert!(app.state.workspaces[0].tabs[2].custom_name.is_none());
        assert_eq!(app.state.workspaces[0].tabs[0].number, 2);
        assert_eq!(app.state.workspaces[0].tabs[1].number, 3);
        assert_eq!(app.state.workspaces[0].tabs[2].number, 1);
        assert_eq!(app.state.workspaces[0].tabs[2].root_pane, moved_root);
        assert_eq!(app.state.workspaces[0].active_tab, 2);
    }

    fn temp_git_repo(branch: &str) -> std::path::PathBuf {
        let repo = unique_temp_path("sidebar-drop-slot-repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(
            repo.join(".git/HEAD"),
            format!("ref: refs/heads/{branch}\n"),
        )
        .unwrap();
        repo
    }

    fn workspace_with_space(name: &str, key: &str) -> Workspace {
        let mut ws = Workspace::test_new(name);
        ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: key.into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: format!("/repo/{name}").into(),
            is_linked_worktree: name != "main",
        });
        ws
    }

    #[test]
    fn top_drop_slot_is_distinct_from_gap_below_first_workspace() {
        let mut app = app_for_mouse_test();
        let first_repo = temp_git_repo("main");
        let second_repo = temp_git_repo("main");

        let mut first = Workspace::test_new("a");
        let first_root = first.tabs[0].root_pane;
        first.identity_cwd = first_repo.clone();
        first.refresh_git_ahead_behind();

        let mut second = Workspace::test_new("b");
        let second_root = second.tabs[0].root_pane;
        second.identity_cwd = second_repo.clone();
        second.refresh_git_ahead_behind();

        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_root]
            .attached_terminal_id
            .clone();
        app.state.terminals.get_mut(&first_terminal_id).unwrap().cwd = first_repo.clone();
        let second_terminal_id = app.state.workspaces[1].tabs[0].panes[&second_root]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .cwd = second_repo.clone();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        // The host rail sits beside the sidebar panel, not above it, so the
        // Spaces/Agents panel (and its workspace drop slots) starts at the
        // top of the sidebar with no vertical offset.
        assert_eq!(app.state.workspace_drop_index_at_row(0), Some(0));
        assert_eq!(app.state.workspace_drop_index_at_row(1), Some(0));
        assert_eq!(app.state.workspace_drop_index_at_row(2), Some(0));
        assert_eq!(app.state.workspace_drop_index_at_row(3), Some(1));

        let _ = fs::remove_dir_all(first_repo);
        let _ = fs::remove_dir_all(second_repo);
    }

    #[test]
    fn bottom_drop_slot_stays_below_last_workspace_not_footer() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("a"),
            Workspace::test_new("b"),
            Workspace::test_new("c"),
        ];
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let cards = &app.state.view.workspace_card_areas;
        let bottom_slot = crate::ui::workspace_drop_indicator_row(
            cards,
            app.state.workspace_list_rect(),
            cards.len(),
        )
        .unwrap();

        let last = cards.last().unwrap().rect;
        assert_eq!(bottom_slot, last.y + last.height);
        assert!(bottom_slot < app.state.sidebar_footer_rect().y.saturating_sub(1));
    }

    #[test]
    fn grouped_sidebar_drop_slots_do_not_land_inside_compact_group() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("main", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("issue", "repo-key"),
        ];
        app.state.active = Some(1);
        app.state.selected = 1;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));

        let cards = &app.state.view.workspace_card_areas;
        let order = cards.iter().map(|card| card.ws_idx).collect::<Vec<_>>();
        assert_eq!(order, vec![0, 2, 1]);
        let issue = cards.iter().find(|card| card.ws_idx == 2).unwrap();
        let normal = cards.iter().find(|card| card.ws_idx == 1).unwrap();

        assert_eq!(app.state.workspace_drop_index_at_row(issue.rect.y), Some(1));
        assert_eq!(
            crate::ui::workspace_drop_indicator_row(cards, app.state.workspace_list_rect(), 2),
            Some(normal.rect.y + normal.rect.height)
        );
    }

    #[test]
    fn dragging_worktree_space_member_does_not_reorder_workspaces() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("main", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("issue", "repo-key"),
        ];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));

        let source = app
            .state
            .view
            .workspace_card_areas
            .iter()
            .find(|card| card.ws_idx == 2)
            .unwrap()
            .rect;
        let target_row = crate::ui::workspace_drop_indicator_row(
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
            0,
        )
        .unwrap();

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, source.y));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            2,
            target_row,
        ));
        assert!(app.state.drag.is_none());
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));

        let names = app
            .state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["main", "normal", "issue"]);
    }

    #[test]
    fn dragging_sidebar_divider_sets_manual_width() {
        let mut app = app_for_mouse_test();

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 30, 5));

        assert_eq!(app.state.sidebar_width, 31);
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.sidebar_width, Some(31));
    }

    #[test]
    fn dragging_sidebar_bottom_divider_still_sets_manual_width() {
        let mut app = app_for_mouse_test();
        let divider_col = app.state.view.sidebar_rect.x + app.state.view.sidebar_rect.width - 1;
        let bottom_row = app.state.view.sidebar_rect.y + app.state.view.sidebar_rect.height - 1;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            divider_col,
            bottom_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            divider_col + 5,
            bottom_row,
        ));

        assert_eq!(app.state.sidebar_width, 31);
    }

    #[test]
    fn dragging_past_max_clamps_to_configured_max() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_max_width = 30;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 50, 5));

        assert_eq!(app.state.sidebar_width, 30);
    }

    #[test]
    fn dragging_below_min_clamps_to_configured_min() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_min_width = 22;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 5, 5));

        assert_eq!(app.state.sidebar_width, 22);
    }

    #[test]
    fn dragging_sidebar_section_divider_sets_split_ratio() {
        let mut app = app_for_mouse_test();
        let divider = crate::ui::sidebar_section_divider_rect(
            app.state.view.sidebar_rect,
            app.state.sidebar_section_split,
        );

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            divider.x + 1,
            divider.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            divider.x + 1,
            divider.y + 4,
        ));

        assert!(app.state.sidebar_section_split > 0.5);
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(
            snapshot.sidebar_section_split,
            Some(app.state.sidebar_section_split)
        );
    }

    #[test]
    fn double_clicking_sidebar_divider_resets_default_width() {
        let mut app = app_for_mouse_test();
        app.state.default_sidebar_width = 26;
        app.state.sidebar_width = 30;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));

        assert_eq!(app.state.sidebar_width, 26);
        assert!(app.state.drag.is_none());
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.sidebar_width, Some(26));
    }

    #[test]
    fn wheel_over_host_rail_moves_host_when_no_scrollbar() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.remote_sources.mark_status(
            &RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME),
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        compute_desktop_view(&mut app);

        // local + jafar fit the host rail viewport, so no scrollbar is needed:
        // wheel moves/clamps the selected host (local -> jafar), mirroring
        // Spaces.
        let section = app.state.view.host_rail_rect;
        let col = section.x;
        let local_row = section.y + 1;
        app.handle_mouse(mouse(MouseEventKind::ScrollDown, col, local_row));
        assert_eq!(
            app.state.sidebar_source,
            crate::app::state::SidebarSource::Remote(RemoteHostKey::new(
                "jafar",
                crate::session::DEFAULT_SESSION_NAME
            ))
        );
        // Wheel over the host rail must not leak into workspace list scrolling.
        assert_eq!(app.state.workspace_scroll, 0);
    }

    #[test]
    fn wheel_over_host_rail_scrolls_list_when_scrollbar_present() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        for i in 0..40 {
            app.state.remote_sources.mark_status(
                &RemoteHostKey::new(format!("host{i:02}"), crate::session::DEFAULT_SESSION_NAME),
                crate::remote_source::RemoteConnectionStatus::Connected,
            );
        }
        compute_desktop_view(&mut app);

        let section = app.state.view.host_rail_rect;
        assert!(section.height > 0);
        let col = section.x;
        let row = section.y + 1;

        assert_eq!(app.state.host_list_scroll, 0);
        app.handle_mouse(mouse(MouseEventKind::ScrollDown, col, row));
        // Overflowing list -> a scrollbar is needed -> wheel scrolls the host
        // list instead of moving the selected host.
        assert!(app.state.host_list_scroll > 0);
    }

    #[test]
    fn clicking_visible_host_in_scrolled_list_keeps_selection_visible() {
        // Reviewer B regression: left-clicking a visible host in an
        // overflow-scrolled host rail list must not jump the viewport back to
        // the top (which would hide the just-selected host). The host list
        // viewport is independent of the selected source, so selecting a host
        // preserves the current scroll.
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        // 40 hosts + local = 41 entries; the full-height rail viewport is
        // narrower than the list, so it overflows and scrolls.
        for i in 0..40 {
            app.state.remote_sources.mark_status(
                &RemoteHostKey::new(format!("host{i:02}"), crate::session::DEFAULT_SESSION_NAME),
                crate::remote_source::RemoteConnectionStatus::Connected,
            );
        }
        compute_desktop_view(&mut app);
        assert!(
            crate::ui::host_list_scroll_metrics(&app.state, app.state.view.host_rail_rect)
                .max_offset_from_bottom
                > 0,
            "host list must overflow the rail viewport for this regression to be meaningful"
        );

        // Sanity: host34 is beyond the first viewport (no visible hit target
        // at scroll 0).
        let target = RemoteHostKey::new("host34", crate::session::DEFAULT_SESSION_NAME);
        assert!(crate::ui::host_list_row_areas(&app.state)
            .into_iter()
            .all(|area| area.source != crate::app::state::SidebarSource::Remote(target.clone())));

        // Scroll down so host34 becomes a visible row.
        app.state.host_list_scroll = 5;
        let row = crate::ui::host_list_row_areas(&app.state)
            .into_iter()
            .find(|area| area.source == crate::app::state::SidebarSource::Remote(target.clone()))
            .expect("host34 is visible after scrolling")
            .rect;
        let scroll_before = app.state.host_list_scroll;
        assert_eq!(scroll_before, 5);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), row.x, row.y));

        assert_eq!(
            app.state.sidebar_source,
            crate::app::state::SidebarSource::Remote(target.clone())
        );
        // Regression guard: selecting a host must NOT reset the host list
        // viewport to the top, which would hide the just-clicked host.
        assert_eq!(
            app.state.host_list_scroll, scroll_before,
            "host list scroll must be preserved across host selection"
        );
        // The just-selected host must still be a visible hit target.
        assert_eq!(
            app.state.host_target_at(row.x, row.y),
            Some(crate::app::state::SidebarSource::Remote(target.clone()))
        );
        // And it survives the next render, which normalizes (not resets) scroll.
        compute_desktop_view(&mut app);
        assert!(app.state.host_list_scroll > 0);
        assert_eq!(
            app.state.host_target_at(row.x, row.y),
            Some(crate::app::state::SidebarSource::Remote(target))
        );
    }
}
