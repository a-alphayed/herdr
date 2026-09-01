use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::Span,
    Frame,
};

mod dialogs;
mod keybind_help;
mod menus;
mod mobile;
mod navigator;
mod onboarding;
mod panes;
mod release_notes;
mod scrollbar;
mod settings;
mod sidebar;
mod status;
mod tabs;
mod text;
mod widgets;

use self::dialogs::{
    render_confirm_close_overlay, render_confirm_remote_projected_pane_close_overlay,
    render_confirm_remote_projected_tab_close_overlay, render_new_linked_worktree_overlay,
    render_open_existing_worktree_overlay, render_remove_worktree_overlay, render_rename_overlay,
};
use self::keybind_help::render_keybind_help_overlay;
use self::menus::{
    render_context_menu, render_copy_mode_overlay, render_global_launcher_menu,
    render_navigate_overlay, render_prefix_overlay, render_resize_overlay,
};
use self::mobile::{
    compute_mobile_header_hit_areas, is_mobile_width, mobile_switcher_max_scroll_for_height,
    mobile_toast_banner_rect, render_mobile_header, render_mobile_panel,
    render_mobile_toast_banner,
};
use self::navigator::render_navigator_overlay;
pub(crate) use self::onboarding::onboarding_welcome_continue_rect;
use self::onboarding::render_onboarding_overlay;
use self::panes::{compute_pane_infos, render_panes, resize_tab_panes};
pub(crate) use self::release_notes::{
    product_announcement_display_lines, release_notes_close_button_rect,
    release_notes_display_lines, release_notes_wrapped_line_count, PRODUCT_ANNOUNCEMENT_MODAL_SIZE,
    RELEASE_NOTES_MODAL_SIZE,
};
use self::release_notes::{render_product_announcement_overlay, render_release_notes_overlay};
pub(crate) use self::scrollbar::{
    pane_scrollbar_rect, release_notes_scrollbar_rect, scrollbar_offset_from_drag_row,
    scrollbar_offset_from_row, scrollbar_thumb_grab_offset, should_show_scrollbar,
};
use self::settings::render_settings_overlay;
/// `host_list_entries` is consumed by host selection/navigation helpers in
/// `actions.rs`/`navigate.rs` (and by `render_host_rail` internally), so it is
/// re-exported for non-test cross-module use.
pub(crate) use self::sidebar::host_list_entries;
#[cfg(test)]
pub(crate) use self::sidebar::host_list_row_areas;
use self::sidebar::{render_sidebar, render_sidebar_collapsed};
use self::status::{
    copy_feedback_rect, render_config_diagnostic, render_copy_feedback, render_toast_notification,
    toast_notification_rect,
};
use self::tabs::render_tab_bar;
pub(crate) use self::{
    dialogs::{
        confirm_close_button_rects, confirm_close_popup_rect,
        confirm_remote_projected_pane_close_button_rects,
        confirm_remote_projected_pane_close_inner_rect,
        confirm_remote_projected_tab_close_button_rects,
        confirm_remote_projected_tab_close_inner_rect, new_linked_worktree_button_rects,
        new_linked_worktree_inner_rect, open_existing_worktree_button_rects,
        open_existing_worktree_inner_rect, open_existing_worktree_max_visible_rows,
        open_existing_worktree_visible_start, remove_worktree_button_rects,
        remove_worktree_popup_rect, rename_button_rects,
    },
    settings::{
        settings_button_rects, settings_popup_height, settings_show_primary_action,
        SETTINGS_POPUP_WIDTH,
    },
    sidebar::{
        agent_panel_body_rect, agent_panel_entries, agent_panel_entry_content_height,
        agent_panel_entry_gap_after, agent_panel_scroll_metrics, agent_panel_scrollbar_rect,
        agent_panel_toggle_rect, collapsed_sidebar_sections, collapsed_sidebar_toggle_rect,
        compute_workspace_card_areas, expanded_sidebar_sections, expanded_sidebar_toggle_rect,
        host_list_scroll_metrics, host_list_scrollbar_rect, host_rail_width, host_target_at,
        normalized_host_list_scroll, normalized_workspace_scroll, sidebar_section_divider_rect,
        workspace_drop_indicator_row, workspace_list_entries, workspace_list_entries_expanded,
        workspace_list_footer_rect, workspace_list_local_actions_rect,
        workspace_list_menu_button_rect, workspace_list_new_button_rect, workspace_list_rect,
        workspace_list_scroll_metrics, workspace_list_scrollbar_rect, workspace_parent_group_state,
        AgentPanelEntry, WorkspaceListEntry,
    },
};
pub(crate) use self::{
    keybind_help::keybind_help_lines,
    mobile::{
        mobile_switcher_areas, mobile_switcher_max_scroll, mobile_switcher_target_at,
        mobile_switcher_workspace_doc_range, MobileSwitcherTarget,
    },
    panes::{apply_pane_chrome, pane_inner_rect, pane_is_scrolled_back},
    tabs::compute_tab_bar_view,
    widgets::{centered_popup_rect, modal_stack_areas},
};
use crate::app::state::ViewLayout;
use crate::app::{AppState, Mode};
use crate::terminal::TerminalRuntimeRegistry;

const COLLAPSED_WIDTH: u16 = 4; // num + space + dot + separator

// Braille spinner frames — smooth rotation
const SPINNERS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Map spinner_tick (incremented every frame at ~60fps) to a spinner frame.
/// We want ~8 updates/sec so divide by 8.
pub(super) fn spinner_frame(tick: u32) -> &'static str {
    SPINNERS[(tick as usize / 8) % SPINNERS.len()]
}

/// Compute view geometry and reconcile pane sizes.
/// Called before render to separate mutation from drawing.
#[cfg_attr(not(test), allow(dead_code))]
pub fn compute_view(app: &mut AppState, area: Rect) {
    let terminal_runtimes = TerminalRuntimeRegistry::new();
    compute_view_with_runtime_registry(app, &terminal_runtimes, area);
}

pub fn compute_view_with_runtime_registry(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
) {
    compute_view_internal(
        app,
        terminal_runtimes,
        area,
        true,
        crate::kitty_graphics::HostCellSize::default(),
    );
}

pub fn compute_view_with_cell_size(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    compute_view_internal(app, terminal_runtimes, area, true, cell_size);
}

/// Compute view geometry for a client-sized render without resizing pane runtimes.
///
/// This is used by the headless server when a non-foreground client needs its
/// own frame size while the shared pane runtimes stay pinned to the foreground
/// client.
pub(crate) fn compute_view_without_resizing_panes(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
) {
    compute_view_internal(
        app,
        terminal_runtimes,
        area,
        false,
        crate::kitty_graphics::HostCellSize::default(),
    );
}

fn resize_background_tab_panes_to_area(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    terminal_area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        for (tab_idx, tab) in ws.tabs.iter().enumerate() {
            if app.active == Some(ws_idx) && tab_idx == ws.active_tab_index() {
                continue;
            }
            resize_tab_panes(app, terminal_runtimes, tab, terminal_area, cell_size);
        }
    }
}

fn resize_background_tab_panes_for_desktop(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    main_area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        let (_, terminal_area) = desktop_tab_bar_and_terminal_area(app, ws, main_area);
        for (tab_idx, tab) in ws.tabs.iter().enumerate() {
            if app.active == Some(ws_idx) && tab_idx == ws.active_tab_index() {
                continue;
            }
            resize_tab_panes(app, terminal_runtimes, tab, terminal_area, cell_size);
        }
    }
}

fn desktop_tab_bar_and_terminal_area(
    app: &AppState,
    ws: &crate::workspace::Workspace,
    main_area: Rect,
) -> (Rect, Rect) {
    let hide_single_tab_bar = app.hide_tab_bar_when_single_tab && ws.tabs.len() == 1;
    if !hide_single_tab_bar && main_area.height > 1 {
        let [tab_bar_rect, terminal_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(main_area);
        (tab_bar_rect, terminal_area)
    } else {
        (Rect::default(), main_area)
    }
}

fn compute_view_internal(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    if is_mobile_width(area, app.mobile_width_threshold) {
        compute_mobile_view(app, terminal_runtimes, area, resize_panes, cell_size);
        return;
    }

    let desired_panel_w = app
        .sidebar_width
        .clamp(app.sidebar_min_width, app.sidebar_max_width);
    // Ahmed's 2026-07-20 correction restores a dedicated, fixed-width
    // full-height host-selection rail beside the Spaces/Agents panel (the
    // pre-existing `SOURCE_RAIL_WIDTH = 10` rail pattern), replacing the
    // full-width Hosts section that briefly lived above the panel. Unlike the
    // old compact rail, the new rail is never gated by remote-cache state or
    // by width: it is always present on expanded desktop (only collapsed
    // sidebar and mobile layout drop it), so ordinary narrow expanded-desktop
    // widths and local-only setups both keep it visible.
    let rail_w = if app.sidebar_collapsed {
        0
    } else {
        host_rail_width()
    };
    let sidebar_w = if app.sidebar_collapsed {
        match app.sidebar_collapsed_mode {
            crate::config::SidebarCollapsedModeConfig::Compact => COLLAPSED_WIDTH,
            crate::config::SidebarCollapsedModeConfig::Hidden => 0,
        }
    } else {
        let max_panel_w = area.width.saturating_sub(rail_w).saturating_sub(1);
        let panel_w = if max_panel_w == 0 {
            desired_panel_w
        } else {
            desired_panel_w
                .min(max_panel_w)
                .max(app.sidebar_min_width.min(max_panel_w))
        };
        rail_w.saturating_add(panel_w)
    };

    let [sidebar_area, main_area] =
        Layout::horizontal([Constraint::Length(sidebar_w), Constraint::Min(1)]).areas(area);
    let host_rail_rect = if app.sidebar_collapsed {
        Rect::default()
    } else if rail_w > 0 && sidebar_area.width > rail_w {
        Rect::new(sidebar_area.x, sidebar_area.y, rail_w, sidebar_area.height)
    } else {
        Rect::default()
    };
    let sidebar_panel_rect = if app.sidebar_collapsed {
        sidebar_area
    } else if host_rail_rect != Rect::default() {
        Rect::new(
            sidebar_area.x + host_rail_rect.width,
            sidebar_area.y,
            sidebar_area.width.saturating_sub(host_rail_rect.width),
            sidebar_area.height,
        )
    } else {
        sidebar_area
    };

    app.view.layout = ViewLayout::Desktop;
    app.view.sidebar_rect = sidebar_area;
    app.view.host_rail_rect = host_rail_rect;
    app.view.sidebar_panel_rect = sidebar_panel_rect;

    let (tab_bar_rect, terminal_area) = app
        .active
        .and_then(|i| app.workspaces.get(i))
        .map(|ws| desktop_tab_bar_and_terminal_area(app, ws, main_area))
        .unwrap_or((Rect::default(), main_area));

    if !app.sidebar_collapsed {
        app.workspace_scroll =
            normalized_workspace_scroll(app, sidebar_panel_rect, app.workspace_scroll);
        app.host_list_scroll =
            crate::ui::sidebar::normalized_host_list_scroll(app, app.host_list_scroll);
        let (_, detail_area) =
            expanded_sidebar_sections(sidebar_panel_rect, app.sidebar_section_split);
        let max_agent_scroll = agent_panel_scroll_metrics(app, detail_area).max_offset_from_bottom;
        app.agent_panel_scroll = app.agent_panel_scroll.min(max_agent_scroll);
    } else {
        app.workspace_scroll = app
            .workspace_scroll
            .min(app.workspaces.len().saturating_sub(1));
        app.agent_panel_scroll = 0;
        app.host_list_scroll = 0;
    }

    let workspace_card_areas = if app.sidebar_collapsed {
        Vec::new()
    } else {
        compute_workspace_card_areas(app, sidebar_panel_rect)
    };

    // A selected remote source reserves the main area as an authority-routing
    // boundary, even while its first authoritative workspace snapshot is still
    // pending. Local tab/pane geometry, hit targets, split borders, and
    // background pane resizing are all suppressed so no local terminal runtime
    // can be touched through mouse/keyboard while the remote surface is active.
    if app.remote_projection_surface_active() {
        let toast_hit_area = app
            .toast
            .as_ref()
            .map(|toast| {
                toast_notification_rect(
                    area,
                    toast,
                    app.config_diagnostic.is_some(),
                    toast.position.unwrap_or(app.toast_config.herdr.position),
                )
            })
            .unwrap_or_default();
        let (remote_projection_tab_hit_areas, remote_projection_hit_areas) =
            if app.host_glass_surface_active() {
                // Glass is a single full-App surface, not a locally
                // reconstructed pane layout. The host rail remains clickable,
                // while every local/projection content hit target is absent.
                (Vec::new(), Vec::new())
            } else {
                (
                    compute_remote_projection_tab_hit_areas(app, main_area),
                    compute_remote_projection_hit_areas(app, main_area),
                )
            };
        app.view = crate::app::ViewState {
            layout: ViewLayout::Desktop,
            sidebar_rect: sidebar_area,
            host_rail_rect,
            sidebar_panel_rect,
            workspace_card_areas,
            tab_bar_rect: Rect::default(),
            tab_hit_areas: Vec::new(),
            tab_scroll_left_hit_area: Rect::default(),
            tab_scroll_right_hit_area: Rect::default(),
            new_tab_hit_area: Rect::default(),
            terminal_area: main_area,
            mobile_header_rect: Rect::default(),
            mobile_menu_hit_area: Rect::default(),
            toast_hit_area,
            pane_infos: Vec::new(),
            split_borders: Vec::new(),
            remote_projection_tab_hit_areas,
            remote_projection_hit_areas,
        };
        return;
    }

    let tab_bar_view = app
        .active
        .and_then(|ws_idx| app.workspaces.get(ws_idx))
        .map(|ws| {
            compute_tab_bar_view(
                ws,
                tab_bar_rect,
                app.tab_scroll,
                app.tab_scroll_follow_active,
                app.mouse_capture,
            )
        })
        .unwrap_or_default();
    app.tab_scroll = tab_bar_view.scroll;

    let split_borders = app
        .active
        .and_then(|i| app.workspaces.get(i))
        .map(|ws| {
            if ws.zoomed {
                Vec::new()
            } else {
                ws.layout.splits(terminal_area)
            }
        })
        .unwrap_or_default();

    let pane_infos = compute_pane_infos(
        app,
        terminal_runtimes,
        terminal_area,
        resize_panes,
        cell_size,
    );
    if resize_panes {
        resize_background_tab_panes_for_desktop(app, terminal_runtimes, main_area, cell_size);
    }

    let toast_hit_area = app
        .toast
        .as_ref()
        .map(|toast| {
            toast_notification_rect(
                area,
                toast,
                app.config_diagnostic.is_some(),
                toast.position.unwrap_or(app.toast_config.herdr.position),
            )
        })
        .unwrap_or_default();

    app.view = crate::app::ViewState {
        layout: ViewLayout::Desktop,
        sidebar_rect: sidebar_area,
        host_rail_rect,
        sidebar_panel_rect,
        workspace_card_areas,
        tab_bar_rect,
        tab_hit_areas: tab_bar_view.tab_hit_areas,
        tab_scroll_left_hit_area: tab_bar_view.scroll_left_hit_area,
        tab_scroll_right_hit_area: tab_bar_view.scroll_right_hit_area,
        new_tab_hit_area: tab_bar_view.new_tab_hit_area,
        terminal_area,
        mobile_header_rect: Rect::default(),
        mobile_menu_hit_area: Rect::default(),
        toast_hit_area,
        pane_infos,
        split_borders,
        remote_projection_tab_hit_areas: Vec::new(),
        remote_projection_hit_areas: Vec::new(),
    };
}

fn compute_mobile_view(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    let header_h = area.height.min(2);
    let (header_rect, terminal_area) = if area.height > header_h {
        let [header_rect, terminal_area] =
            Layout::vertical([Constraint::Length(header_h), Constraint::Min(1)]).areas(area);
        (header_rect, terminal_area)
    } else {
        (area, Rect::default())
    };

    if app.mode == Mode::Navigate {
        let switcher_viewport_h = area.height.saturating_sub(header_h + 1);
        let max_scroll = mobile_switcher_max_scroll_for_height(app, switcher_viewport_h);
        app.mobile_switcher_scroll = app.mobile_switcher_scroll.min(max_scroll);
    }

    // A projected remote space renders read-only on mobile too: no local split
    // borders, pane infos, or background pane resizing.
    if app.selected_remote_space.is_some() {
        let toast_hit_area = app
            .toast
            .as_ref()
            .map(|_| mobile_toast_banner_rect(area, app.config_diagnostic.is_some()))
            .unwrap_or_default();
        let remote_projection_tab_hit_areas =
            compute_remote_projection_tab_hit_areas(app, terminal_area);
        let remote_projection_hit_areas = compute_remote_projection_hit_areas(app, terminal_area);
        app.view = crate::app::ViewState {
            layout: ViewLayout::Mobile,
            sidebar_rect: Rect::default(),
            host_rail_rect: Rect::default(),
            sidebar_panel_rect: Rect::default(),
            workspace_card_areas: Vec::new(),
            tab_bar_rect: Rect::default(),
            tab_hit_areas: Vec::new(),
            tab_scroll_left_hit_area: Rect::default(),
            tab_scroll_right_hit_area: Rect::default(),
            new_tab_hit_area: Rect::default(),
            terminal_area,
            mobile_header_rect: header_rect,
            mobile_menu_hit_area: Rect::default(),
            toast_hit_area,
            pane_infos: Vec::new(),
            split_borders: Vec::new(),
            remote_projection_tab_hit_areas,
            remote_projection_hit_areas,
        };
        return;
    }

    let split_borders = app
        .active
        .and_then(|i| app.workspaces.get(i))
        .map(|ws| {
            if ws.zoomed {
                Vec::new()
            } else {
                ws.layout.splits(terminal_area)
            }
        })
        .unwrap_or_default();

    let pane_infos = compute_pane_infos(
        app,
        terminal_runtimes,
        terminal_area,
        resize_panes,
        cell_size,
    );
    if resize_panes {
        resize_background_tab_panes_to_area(app, terminal_runtimes, terminal_area, cell_size);
    }
    let header_hits = compute_mobile_header_hit_areas(app, header_rect);

    let toast_hit_area = app
        .toast
        .as_ref()
        .map(|_| mobile_toast_banner_rect(area, app.config_diagnostic.is_some()))
        .unwrap_or_default();

    app.view = crate::app::ViewState {
        layout: ViewLayout::Mobile,
        sidebar_rect: Rect::default(),
        host_rail_rect: Rect::default(),
        sidebar_panel_rect: Rect::default(),
        workspace_card_areas: Vec::new(),
        tab_bar_rect: Rect::default(),
        tab_hit_areas: Vec::new(),
        tab_scroll_left_hit_area: Rect::default(),
        tab_scroll_right_hit_area: Rect::default(),
        new_tab_hit_area: Rect::default(),
        terminal_area,
        mobile_header_rect: header_rect,
        mobile_menu_hit_area: header_hits.menu,
        toast_hit_area,
        pane_infos,
        split_borders,
        remote_projection_tab_hit_areas: Vec::new(),
        remote_projection_hit_areas: Vec::new(),
    };
}

/// Render the UI — reads AppState but does not mutate it.
#[cfg_attr(not(test), allow(dead_code))]
pub fn render(app: &AppState, frame: &mut Frame) {
    let terminal_runtimes = TerminalRuntimeRegistry::new();
    render_with_runtime_registry(app, &terminal_runtimes, frame);
}

pub fn render_with_runtime_registry(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
) {
    render_with_runtime_registry_and_glass(app, terminal_runtimes, None, frame);
}

pub(crate) fn render_with_runtime_registry_and_glass(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    glass_surfaces: Option<&crate::app::host_glass::HostGlassSurfaceRegistry>,
    frame: &mut Frame,
) {
    let sidebar_area = app.view.sidebar_rect;
    let tab_bar_area = app.view.tab_bar_rect;
    let terminal_area = app.view.terminal_area;

    if app.view.layout == ViewLayout::Mobile {
        render_mobile_header(app, terminal_runtimes, frame, app.view.mobile_header_rect);
    } else if sidebar_area.width > 0 {
        if app.sidebar_collapsed {
            render_sidebar_collapsed(app, frame, sidebar_area);
        } else {
            render_sidebar(app, terminal_runtimes, frame, sidebar_area);
        }
    }
    let remote_projection_active = app.remote_projection_surface_active();
    if app.view.layout != ViewLayout::Mobile && !remote_projection_active {
        render_tab_bar(app, frame, tab_bar_area);
    }
    if app.host_glass_surface_active() {
        render_host_glass(app, glass_surfaces, frame, terminal_area);
    } else if remote_projection_active {
        render_remote_projection(app, frame, terminal_area);
    } else {
        render_panes(app, terminal_runtimes, frame, terminal_area);
    }

    // Ambient notifications sit above panes, but below interactive overlays.
    render_notifications(app, frame, terminal_area);

    match app.mode {
        Mode::Onboarding => render_onboarding_overlay(app, frame, frame.area()),
        Mode::ReleaseNotes => render_release_notes_overlay(app, frame, frame.area()),
        Mode::ProductAnnouncement => render_product_announcement_overlay(app, frame, frame.area()),
        Mode::Navigate if app.view.layout == ViewLayout::Mobile => {
            render_mobile_panel(app, terminal_runtimes, frame, frame.area())
        }
        Mode::Navigate => render_navigate_overlay(app, frame, terminal_area),
        Mode::Prefix => render_prefix_overlay(app, frame, terminal_area),
        Mode::Copy => render_copy_mode_overlay(app, frame, terminal_area),
        Mode::Resize => render_resize_overlay(app, frame, terminal_area),
        Mode::ConfirmClose => render_confirm_close_overlay(app, frame, terminal_area),
        Mode::ConfirmRemoteProjectedPaneClose => {
            render_confirm_remote_projected_pane_close_overlay(app, frame, terminal_area)
        }
        Mode::ConfirmRemoteProjectedTabClose => {
            render_confirm_remote_projected_tab_close_overlay(app, frame, terminal_area)
        }
        Mode::ContextMenu => {
            render_context_menu(app, frame);
        }
        Mode::Settings => render_settings_overlay(app, frame, frame.area()),
        Mode::RenameWorkspace | Mode::RenameTab | Mode::RenamePane => {
            render_rename_overlay(app, frame, frame.area())
        }
        Mode::NewLinkedWorktree => render_new_linked_worktree_overlay(app, frame, frame.area()),
        Mode::OpenExistingWorktree => {
            render_open_existing_worktree_overlay(app, frame, frame.area())
        }
        Mode::ConfirmRemoveWorktree => render_remove_worktree_overlay(app, frame, frame.area()),
        Mode::GlobalMenu => render_global_launcher_menu(app, frame),
        Mode::KeybindHelp => render_keybind_help_overlay(app, frame),
        Mode::Navigator => render_navigator_overlay(app, terminal_runtimes, frame),
        Mode::Terminal => {}
    }
}

/// The first main-area row is local, persistent identity/status chrome; only
/// the remaining body is advertised to and painted by the remote full-App
/// stream.
pub(crate) fn host_glass_body_area(area: Rect) -> Rect {
    if area.width == 0 || area.height <= 1 {
        return Rect::default();
    }
    Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1),
    )
}

fn render_host_glass(
    app: &AppState,
    glass_surfaces: Option<&crate::app::host_glass::HostGlassSurfaceRegistry>,
    frame: &mut Frame,
    area: Rect,
) {
    use ratatui::layout::Alignment;
    use ratatui::text::Line;
    use ratatui::widgets::{Clear, Paragraph};

    let crate::app::state::SidebarSource::Remote(host) = app.effective_sidebar_source() else {
        return;
    };
    if area == Rect::default() {
        return;
    }

    let metadata = app
        .selected_host_glass_mode()
        .map(|(_selected, glass)| glass);
    let status = metadata
        .map(|glass| glass.status)
        .unwrap_or(crate::app::host_glass::GlassStatus::Connecting);
    let (status_label, status_color) = match status {
        crate::app::host_glass::GlassStatus::Connecting => ("Connecting", app.palette.yellow),
        crate::app::host_glass::GlassStatus::Live => ("Live", app.palette.green),
        crate::app::host_glass::GlassStatus::Stale { .. } => ("Stale", app.palette.peach),
    };
    let host_label = if host.session == crate::session::DEFAULT_SESSION_NAME {
        host.host.clone()
    } else {
        format!("{} / {}", host.host, host.session)
    };
    let indicator = Line::from(vec![
        Span::styled(" glass  ", Style::default().fg(app.palette.accent)),
        Span::styled(
            host_label.clone(),
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(
            status_label,
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    let indicator_area = Rect::new(area.x, area.y, area.width, 1);
    frame.render_widget(
        Paragraph::new(indicator)
            .style(Style::default().bg(app.palette.panel_bg))
            .alignment(Alignment::Left),
        indicator_area,
    );

    let body = host_glass_body_area(area);
    if body == Rect::default() {
        return;
    }
    let surface = glass_surfaces.and_then(|surfaces| surfaces.get(&host));
    let frame_is_renderable =
        surface.is_some_and(|surface| surface.has_frame() && surface.area() == body);

    if frame_is_renderable {
        if let Some(surface) = surface {
            surface.render(
                frame,
                matches!(status, crate::app::host_glass::GlassStatus::Live),
            );
        }
    } else {
        frame.render_widget(Clear, body);
    }

    match status {
        crate::app::host_glass::GlassStatus::Live if frame_is_renderable => {
            if let Some(reason) = metadata.and_then(|glass| glass.input_drop_cue) {
                let banner_area = Rect::new(
                    body.x,
                    body.y.saturating_add(body.height / 2),
                    body.width,
                    1,
                );
                frame.render_widget(
                    Paragraph::new(format!(
                        " INPUT DROPPED · {} · not queued ",
                        reason.cue_text()
                    ))
                    .style(
                        Style::default()
                            .fg(app.palette.text)
                            .bg(app.palette.surface0)
                            .add_modifier(Modifier::BOLD),
                    )
                    .alignment(Alignment::Center),
                    banner_area,
                );
            }
        }
        crate::app::host_glass::GlassStatus::Stale { .. } if frame_is_renderable => {
            dim_background(frame, body);
            render_host_glass_stale_banner(
                app,
                frame,
                body,
                &host_label,
                metadata.and_then(|glass| glass.last_live_frame_age_secs),
                metadata.and_then(|glass| glass.input_drop_cue),
            );
        }
        crate::app::host_glass::GlassStatus::Stale { .. } => {
            render_host_glass_stale_banner(
                app,
                frame,
                body,
                &host_label,
                metadata.and_then(|glass| glass.last_live_frame_age_secs),
                metadata.and_then(|glass| glass.input_drop_cue),
            );
        }
        _ => {
            let message = if let Some(reason) = metadata.and_then(|glass| glass.input_drop_cue) {
                format!("INPUT DROPPED · {} · not queued", reason.cue_text())
            } else {
                metadata
                    .and_then(|glass| glass.message.as_deref())
                    .unwrap_or("opening host glass stream")
                    .to_string()
            };
            let placeholder_area = Rect::new(
                body.x,
                body.y.saturating_add(body.height / 2),
                body.width,
                1,
            );
            frame.render_widget(
                Paragraph::new(format!("{status_label} · {message}"))
                    .style(Style::default().fg(app.palette.overlay0))
                    .alignment(Alignment::Center),
                placeholder_area,
            );
        }
    }
}

fn render_host_glass_stale_banner(
    app: &AppState,
    frame: &mut Frame<'_>,
    body: Rect,
    host_label: &str,
    last_live_frame_age_secs: Option<u64>,
    input_drop: Option<crate::app::host_glass::GlassInputDropReason>,
) {
    use ratatui::layout::Alignment;
    use ratatui::text::Line;
    use ratatui::widgets::Paragraph;

    let lines = host_glass_stale_banner_lines(host_label, last_live_frame_age_secs, input_drop);
    let height = lines.len() as u16;
    let center_row = body.y.saturating_add(body.height / 2);
    let banner_area = Rect::new(
        body.x,
        center_row.saturating_sub(height.saturating_sub(1)),
        body.width,
        height,
    );
    frame.render_widget(
        Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>())
            .style(
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.surface0)
                    .add_modifier(Modifier::BOLD),
            )
            .alignment(Alignment::Center),
        banner_area,
    );
}

fn host_glass_stale_banner_lines(
    host_label: &str,
    last_live_frame_age_secs: Option<u64>,
    input_drop: Option<crate::app::host_glass::GlassInputDropReason>,
) -> [String; 2] {
    let frame_age = last_live_frame_age_secs.map_or_else(
        || "no live frame received".to_string(),
        |age_secs| format!("last live frame {age_secs}s ago"),
    );
    let detail = input_drop.map_or(frame_age.clone(), |_| {
        format!("{frame_age} · INPUT DROPPED · not queued")
    });
    [format!(" STALE · {host_label} "), format!(" {detail} ")]
}

// ---------------------------------------------------------------------------
// Projection geometry — shared between compute_view and render
// ---------------------------------------------------------------------------

/// Recursively decompose a layout tree into `(pane, rect, is_focused)` tuples
/// using the same split math as the renderer. The `focused_pane_id` argument
/// marks which leaf is the remote-focused pane.
pub(crate) fn project_layout_rects<'a>(
    node: &'a crate::api::schema::LayoutNode,
    area: Rect,
    focused_pane_id: &str,
) -> Vec<(&'a crate::api::schema::LayoutPane, Rect, bool)> {
    use crate::api::schema::{LayoutNode, SplitDirection};
    match node {
        LayoutNode::Pane { pane } => {
            let is_focused = pane
                .pane_id
                .as_deref()
                .is_some_and(|id| id == focused_pane_id);
            vec![(pane, area, is_focused)]
        }
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            if area.width < 2 || area.height < 2 {
                return Vec::new();
            }
            let weight = ((*ratio).clamp(0.05, 0.95) * 1000.0).round() as u32;
            let other = 1000u32.saturating_sub(weight);
            let constraints = [
                Constraint::Ratio(weight, 1000),
                Constraint::Ratio(other.max(1), 1000),
            ];
            let [first_area, second_area] = match direction {
                SplitDirection::Right => Layout::horizontal(constraints).areas(area),
                SplitDirection::Down => Layout::vertical(constraints).areas(area),
            };
            let mut out = project_layout_rects(first, first_area, focused_pane_id);
            out.extend(project_layout_rects(second, second_area, focused_pane_id));
            out
        }
    }
}

fn remote_projection_tab_strip(
    app: &crate::app::AppState,
) -> Option<(
    crate::remote_source::RemoteSpaceKey,
    crate::remote_source::RemoteTabSnapshotEntry,
    crate::remote_source::RemoteSourceCapabilities,
)> {
    use crate::remote_source::{RemoteHostKey, RemoteProjectionStatus};

    let selected = app.selected_remote_space.as_ref()?;
    let projection = app.remote_sources.projection_for_space(selected)?;
    if projection.status != RemoteProjectionStatus::Available {
        return None;
    }
    let host = RemoteHostKey::new(selected.host.clone(), selected.session.clone());
    if !app
        .remote_sources
        .host_status(&host)
        .is_some_and(|status| status.is_connected())
    {
        return None;
    }
    let tabs = app.remote_sources.tab_snapshot_for_space(selected)?;
    if tabs.status != RemoteProjectionStatus::Available {
        return None;
    }
    let capabilities = app.remote_sources.host_capabilities(&host);
    Some((selected.clone(), tabs, capabilities))
}

fn remote_projection_chrome_rows(app: &crate::app::AppState) -> u16 {
    if remote_projection_tab_strip(app).is_some() {
        2
    } else {
        1
    }
}

fn remote_projection_body_area(app: &crate::app::AppState, area: Rect) -> Option<Rect> {
    let header_rows = remote_projection_chrome_rows(app);
    if area.height <= header_rows {
        return None;
    }
    let [_, body] =
        Layout::vertical([Constraint::Length(header_rows), Constraint::Min(1)]).areas(area);
    Some(body)
}

fn compute_remote_projection_tab_hit_areas(
    app: &crate::app::AppState,
    area: Rect,
) -> Vec<crate::app::state::RemoteProjectionTabHitArea> {
    use crate::app::state::{RemoteProjectionTabAction, RemoteProjectionTabHitArea};

    let Some((selected, tabs, capabilities)) = remote_projection_tab_strip(app) else {
        return Vec::new();
    };
    if area.height <= remote_projection_chrome_rows(app) || area.width == 0 {
        return Vec::new();
    }

    let mut hits = Vec::new();
    let mut x = area
        .x
        .saturating_add(5)
        .min(area.x.saturating_add(area.width));
    let row = area.y.saturating_add(1);
    let right = area.x.saturating_add(area.width);
    for tab in tabs.tabs {
        if x >= right {
            break;
        }
        let label = if tab.focused {
            format!("[{}]", tab.label)
        } else {
            format!(" {} ", tab.label)
        };
        let width = (label.chars().count() as u16)
            .max(1)
            .min(right.saturating_sub(x));
        if capabilities.tab_focus && width > 0 {
            hits.push(RemoteProjectionTabHitArea {
                rect: Rect::new(x, row, width, 1),
                host: selected.host.clone(),
                session: selected.session.clone(),
                workspace_id: selected.workspace_id.clone(),
                tab_id: Some(tab.tab_id.clone()),
                label: tab.label.clone(),
                action: RemoteProjectionTabAction::Focus,
                live: true,
            });
        }
        x = x.saturating_add(width);
        if capabilities.tab_close && x < right {
            let close_width = 3u16.min(right.saturating_sub(x));
            hits.push(RemoteProjectionTabHitArea {
                rect: Rect::new(x, row, close_width, 1),
                host: selected.host.clone(),
                session: selected.session.clone(),
                workspace_id: selected.workspace_id.clone(),
                tab_id: Some(tab.tab_id.clone()),
                label: tab.label.clone(),
                action: RemoteProjectionTabAction::Close,
                live: true,
            });
            x = x.saturating_add(close_width);
        }
        if x < right {
            x = x.saturating_add(1);
        }
    }
    if capabilities.tab_create && x < right {
        let width = 5u16.min(right.saturating_sub(x));
        hits.push(RemoteProjectionTabHitArea {
            rect: Rect::new(x, row, width, 1),
            host: selected.host.clone(),
            session: selected.session.clone(),
            workspace_id: selected.workspace_id.clone(),
            tab_id: None,
            label: "new tab".to_string(),
            action: RemoteProjectionTabAction::New,
            live: true,
        });
    }
    hits
}

/// Build the projection hit-area list for `compute_view`. Called only when
/// `selected_remote_space` is Some.
fn compute_remote_projection_hit_areas(
    app: &crate::app::AppState,
    body_area: Rect,
) -> Vec<crate::app::state::RemoteProjectionHitArea> {
    use crate::remote_source::RemoteProjectionStatus;
    let Some(selected) = app.selected_remote_space.as_ref() else {
        return Vec::new();
    };
    let Some(projection) = app.remote_sources.projection_for_space(selected) else {
        return Vec::new();
    };
    let live = projection.status == RemoteProjectionStatus::Available;
    let Some(layout) = &projection.layout else {
        return Vec::new();
    };

    let Some(actual_body) = remote_projection_body_area(app, body_area) else {
        return Vec::new();
    };

    project_layout_rects(&layout.root, actual_body, &layout.focused_pane_id)
        .into_iter()
        .map(
            |(pane, rect, focused)| crate::app::state::RemoteProjectionHitArea {
                rect,
                host: selected.host.clone(),
                session: selected.session.clone(),
                pane_id: pane.pane_id.clone(),
                terminal_id: pane.terminal_id.clone(),
                label: pane
                    .label
                    .clone()
                    .or_else(|| pane.pane_id.clone())
                    .unwrap_or_else(|| "pane".to_string()),
                focused,
                live: live && pane.terminal_id.is_some(),
            },
        )
        .collect()
}

/// Render a read-only projected remote workspace in the main area.
///
/// This is intentionally non-interactive: it draws the host/session/workspace/
/// tab identity, a live/stale/unavailable status, and (when available) the remote
/// layout as plain bordered boxes. It never forwards input or mutates any local
/// or remote terminal runtime.
fn render_remote_projection(app: &AppState, frame: &mut Frame, terminal_area: Rect) {
    use ratatui::layout::Alignment;
    use ratatui::text::Line;
    use ratatui::widgets::Paragraph;

    use crate::remote_source::RemoteProjectionStatus;

    let Some(selected) = app.selected_remote_space.as_ref() else {
        let host = match app.effective_sidebar_source() {
            crate::app::state::SidebarSource::Remote(host) => host,
            crate::app::state::SidebarSource::Local => return,
        };
        let host_label = if host.session == crate::session::DEFAULT_SESSION_NAME {
            host.host
        } else {
            format!("{} / {}", host.host, host.session)
        };
        let header = Line::from(vec![
            Span::raw("remote  "),
            Span::styled(host_label, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("   workspace <waiting>   "),
            Span::styled("no local pane active", Style::default()),
        ]);
        if terminal_area.height <= 1 {
            frame.render_widget(
                Paragraph::new(vec![header]).alignment(Alignment::Left),
                terminal_area,
            );
            return;
        }
        let [header_area, body_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(terminal_area);
        frame.render_widget(
            Paragraph::new(vec![header]).alignment(Alignment::Left),
            header_area,
        );
        frame.render_widget(
            Paragraph::new(vec![Line::from(vec![Span::raw(
                "waiting for authoritative remote workspace snapshot",
            )])])
            .alignment(Alignment::Center),
            body_area,
        );
        return;
    };
    let projection = app.remote_sources.projection_for_space(selected);
    let status = projection.as_ref().map(|projection| projection.status);
    let workspace_label = projection
        .as_ref()
        .map(|projection| projection.workspace_id.clone())
        .unwrap_or_else(|| selected.workspace_id.clone());
    let tab_label = projection
        .as_ref()
        .and_then(|projection| {
            projection
                .tab_label
                .clone()
                .or_else(|| projection.tab_id.clone())
        })
        .unwrap_or_else(|| "<no active tab>".to_string());
    let layout = projection
        .as_ref()
        .and_then(|projection| projection.layout.as_ref())
        .cloned();
    let layout_root = layout.as_ref().map(|l| l.root.clone());
    let focused_pane_id = layout
        .as_ref()
        .map(|l| l.focused_pane_id.as_str())
        .unwrap_or("");
    let live = matches!(status, Some(RemoteProjectionStatus::Available));

    let (status_label, status_style) = match status {
        Some(RemoteProjectionStatus::Available) => {
            ("live", Style::default().add_modifier(Modifier::BOLD))
        }
        Some(RemoteProjectionStatus::StaleLastKnown) => ("stale (last known)", Style::default()),
        Some(RemoteProjectionStatus::Unavailable) | None => ("unavailable", Style::default()),
    };

    let header = Line::from(vec![
        Span::raw("remote  "),
        Span::styled(
            format!("{} / {}", selected.host, selected.session),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("   workspace "),
        Span::raw(workspace_label),
        Span::raw("   tab "),
        Span::raw(tab_label),
        Span::raw("   "),
        Span::styled(status_label, status_style),
    ]);

    let chrome_rows = remote_projection_chrome_rows(app);
    if terminal_area.height <= chrome_rows {
        frame.render_widget(
            Paragraph::new(vec![header]).alignment(Alignment::Left),
            terminal_area,
        );
        return;
    }

    let [chrome_area, body_area] =
        Layout::vertical([Constraint::Length(chrome_rows), Constraint::Min(1)])
            .areas(terminal_area);
    let [header_area, tab_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(chrome_area);
    frame.render_widget(
        Paragraph::new(vec![header]).alignment(Alignment::Left),
        header_area,
    );
    if chrome_rows > 1 {
        render_remote_projection_tab_strip(app, frame, tab_area);
    }

    match status {
        Some(RemoteProjectionStatus::Available) | Some(RemoteProjectionStatus::StaleLastKnown)
            if layout_root.is_some() =>
        {
            render_projection_layout(
                app,
                frame,
                &layout_root.expect("layout root"),
                body_area,
                focused_pane_id,
                live,
            );
        }
        _ => frame.render_widget(
            Paragraph::new(vec![Line::from(vec![Span::raw(format!(
                "remote projection {status_label}"
            ))])])
            .alignment(Alignment::Center),
            body_area,
        ),
    }
}

fn render_remote_projection_tab_strip(app: &AppState, frame: &mut Frame, area: Rect) {
    use ratatui::text::Line;
    use ratatui::widgets::Paragraph;

    let Some((_selected, tabs, capabilities)) = remote_projection_tab_strip(app) else {
        return;
    };
    let mut spans = vec![Span::raw("tabs ")];
    for tab in tabs.tabs {
        let label_style = if tab.focused {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        spans.push(Span::styled(
            if tab.focused {
                format!("[{}]", tab.label)
            } else {
                format!(" {} ", tab.label)
            },
            label_style,
        ));
        if capabilities.tab_close {
            spans.push(Span::raw(" × "));
        }
        spans.push(Span::raw(" "));
    }
    if capabilities.tab_create {
        spans.push(Span::styled(
            " + ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Draw projected remote layout panes using the shared geometry helper.
/// Live panes (attachable) and the focused pane get distinct visual treatment.
fn render_projection_layout(
    app: &AppState,
    frame: &mut Frame,
    node: &crate::api::schema::LayoutNode,
    area: Rect,
    focused_pane_id: &str,
    live: bool,
) {
    use ratatui::style::Color;
    use ratatui::widgets::{Block, Borders, Paragraph};

    let Some(selected) = app.selected_remote_space.as_ref() else {
        return;
    };
    for (pane, rect, is_focused) in project_layout_rects(node, area, focused_pane_id) {
        let title = pane
            .label
            .clone()
            .or_else(|| pane.pane_id.clone())
            .unwrap_or_else(|| "pane".to_string());
        let stream = pane
            .terminal_id
            .as_deref()
            .and_then(|terminal_id| app.remote_projection_frame(selected, terminal_id));
        let status = stream.map(|entry| entry.status).unwrap_or_else(|| {
            if live && pane.terminal_id.is_some() {
                crate::remote_source::RemoteProjectionStreamStatus::Connecting
            } else {
                crate::remote_source::RemoteProjectionStreamStatus::StaleLastKnown
            }
        });
        let (color, bold) = match status {
            crate::remote_source::RemoteProjectionStreamStatus::LiveController => {
                (Color::Green, true)
            }
            crate::remote_source::RemoteProjectionStreamStatus::LiveObserver => {
                (Color::DarkGray, false)
            }
            crate::remote_source::RemoteProjectionStreamStatus::OwnedReadOnly => {
                (Color::Yellow, true)
            }
            crate::remote_source::RemoteProjectionStreamStatus::Connecting => (Color::Cyan, false),
            crate::remote_source::RemoteProjectionStreamStatus::StaleLastKnown
            | crate::remote_source::RemoteProjectionStreamStatus::Disconnected => {
                (Color::DarkGray, false)
            }
            crate::remote_source::RemoteProjectionStreamStatus::Unsupported
            | crate::remote_source::RemoteProjectionStreamStatus::NeedsAttention => {
                (Color::Red, false)
            }
        };
        let mut title_style = Style::default().fg(color);
        if bold || (is_focused && status.accepts_input()) {
            title_style = title_style.add_modifier(Modifier::BOLD);
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color))
            .title(Span::styled(
                format!(" {title} · {} ", status.concise_label()),
                title_style,
            ));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);

        let Some(entry) = stream else {
            if inner.width > 0 && inner.height > 0 {
                frame.render_widget(
                    Paragraph::new(status.concise_label()).style(Style::default().fg(color)),
                    inner,
                );
            }
            continue;
        };
        let Some(remote_frame) = entry.frame.as_ref() else {
            if inner.width > 0 && inner.height > 0 {
                frame.render_widget(
                    Paragraph::new(
                        entry
                            .message
                            .as_deref()
                            .unwrap_or_else(|| status.concise_label()),
                    )
                    .style(Style::default().fg(color)),
                    inner,
                );
            }
            continue;
        };

        // Copy exact semantic cells/styles into this projected pane interior,
        // clipped in both dimensions. No parser PTY/local pane exists. Kitty
        // graphics bytes are deliberately ignored in this unit; text cells,
        // styles and cursor remain live. The copied dimensions come from the
        // shared visible-grid helper so the render copy/clip path and the
        // projected-selection input bounds agree on exactly which frame
        // cells are visible.
        let (_, _, copy_width, copy_height) =
            crate::selection::projected_visible_grid(rect, remote_frame.width, remote_frame.height);
        {
            let buffer = frame.buffer_mut();
            for row in 0..copy_height {
                for col in 0..copy_width {
                    let index = row as usize * remote_frame.width as usize + col as usize;
                    let Some(source) = remote_frame.cells.get(index) else {
                        continue;
                    };
                    let destination = &mut buffer[(inner.x + col, inner.y + row)];
                    destination.set_symbol(&source.symbol);
                    destination.fg = crate::protocol::u32_to_color(source.fg);
                    destination.bg = crate::protocol::u32_to_color(source.bg);
                    destination.modifier = crate::protocol::u16_to_modifier(source.modifier);
                    destination.skip = source.skip;
                }
            }
        }
        // Projected selection overlay: repaint the uniform automatic-selection
        // style only for cells inside a visible projected selection whose
        // exact (host, session, workspace_id, terminal_id) key matches this
        // pane. The validated row ranges are clipped to the exact copied
        // frame interior and fail closed — no highlight at all — on a
        // malformed frame, impossible `skip` topology, a selection exceeding
        // the visible grid (a mid-gesture shrink), or a visible edge cutting
        // a wide grapheme from its required continuation, exactly matching
        // clipboard extraction; valid wide-grapheme tails are included so
        // the whole displayed grapheme is highlighted. Local panes and
        // non-matching projected panes are never touched.
        if let Some(selection) = app.projected_selection.as_ref().filter(|selection| {
            selection.is_visible()
                && selection.key.host == selected.host
                && selection.key.session == selected.session
                && selection.key.workspace_id == selected.workspace_id
                && Some(selection.key.terminal_id.as_str()) == pane.terminal_id.as_deref()
        }) {
            if let Some(ranges) =
                selection.highlighted_row_ranges(remote_frame, copy_width, copy_height)
            {
                let style = panes::automatic_selection_style(&app.palette, app.host_terminal_theme);
                let buffer = frame.buffer_mut();
                for (row, start, end) in ranges {
                    for col in start..=end {
                        buffer[(inner.x + col, inner.y + row)].set_style(style);
                    }
                }
            }
        }
        if is_focused && status.accepts_input() {
            if let Some(cursor) = remote_frame.cursor.as_ref().filter(|cursor| cursor.visible) {
                if cursor.x < copy_width && cursor.y < copy_height {
                    frame.set_cursor_position((inner.x + cursor.x, inner.y + cursor.y));
                }
            }
        }
    }
}

fn render_notifications(app: &AppState, frame: &mut Frame, terminal_area: Rect) {
    let has_config_diagnostic = app.config_diagnostic.is_some();
    if let Some(message) = &app.config_diagnostic {
        render_config_diagnostic(frame, terminal_area, message, &app.palette);
    }
    let mut copy_feedback_offset = u16::from(has_config_diagnostic);
    let mut toast_rect = None;
    if let Some(toast) = &app.toast {
        if app.view.layout == ViewLayout::Mobile {
            render_mobile_toast_banner(
                frame,
                frame.area(),
                toast,
                has_config_diagnostic,
                &app.palette,
            );
        } else {
            render_toast_notification(
                frame,
                frame.area(),
                toast,
                has_config_diagnostic,
                toast.position.unwrap_or(app.toast_config.herdr.position),
                &app.palette,
            );
            toast_rect = Some(toast_notification_rect(
                frame.area(),
                toast,
                has_config_diagnostic,
                toast.position.unwrap_or(app.toast_config.herdr.position),
            ));
        }
        if app.view.layout == ViewLayout::Mobile {
            toast_rect = Some(mobile_toast_banner_rect(
                frame.area(),
                has_config_diagnostic,
            ));
        }
    }
    if let Some(feedback) = &app.copy_feedback {
        let area = if app.view.layout == ViewLayout::Mobile {
            frame.area()
        } else {
            terminal_area
        };
        if let Some(toast_rect) = toast_rect {
            copy_feedback_offset = copy_feedback_offset_for_toast(
                area,
                feedback,
                copy_feedback_offset,
                app.toast_config.clipboard.position,
                toast_rect,
            );
        }
        render_copy_feedback(
            frame,
            area,
            feedback,
            copy_feedback_offset,
            app.toast_config.clipboard.position,
            &app.palette,
        );
    }
}

fn copy_feedback_offset_for_toast(
    area: Rect,
    feedback: &crate::app::state::CopyFeedback,
    base_offset: u16,
    position: crate::config::ToastClipboardPosition,
    toast_rect: Rect,
) -> u16 {
    let feedback_rect = copy_feedback_rect(area, feedback, base_offset, position);
    if rects_overlap(feedback_rect, toast_rect) {
        base_offset.saturating_add(toast_rect.height)
    } else {
        base_offset
    }
}

fn rects_overlap(a: Rect, b: Rect) -> bool {
    a.x < b.x.saturating_add(b.width)
        && b.x < a.x.saturating_add(a.width)
        && a.y < b.y.saturating_add(b.height)
        && b.y < a.y.saturating_add(a.height)
}

fn dim_background(frame: &mut Frame, area: Rect) {
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            let cell = &mut buf[(x, y)];
            cell.set_style(cell.style().add_modifier(Modifier::DIM));
        }
    }
}

/// Floating overlay for navigate mode — appears at bottom of terminal area.
fn _build_hints(items: &[(&str, &str)], key_style: Style, dim_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    spans.push(Span::raw(" "));
    for (i, (k, desc)) in items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", dim_style));
        }
        spans.push(Span::styled(k.to_string(), key_style));
        spans.push(Span::styled(format!(" {desc}"), dim_style));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::keybind_help::keybind_help_groups;
    use super::scrollbar::scrollbar_thumb;
    use super::*;
    use crate::{app::state::ViewLayout, layout::PaneInfo, workspace::Workspace};
    use ratatui::style::Color;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn projected_live_and_stale_frames_render_remote_cells_without_local_fallthrough() {
        let mut app = crate::app::state::AppState::test_new();
        let selected = crate::remote_source::RemoteSpaceKey {
            host: "remote-a".into(),
            session: "default".into(),
            workspace_id: "ws-a".into(),
        };
        app.selected_remote_space = Some(selected.clone());
        let generation = app.begin_remote_projection_generation(Some(&selected), false);
        let key = crate::remote_source::RemoteProjectionTerminalKey {
            host: "remote-a".into(),
            session: "default".into(),
            workspace_id: "ws-a".into(),
            terminal_id: "term-a".into(),
        };
        app.seed_remote_projection_streams(
            generation,
            [(
                key.clone(),
                crate::remote_source::RemoteProjectionStreamRole::Control,
                crate::remote_source::RemoteProjectionStreamStatus::Connecting,
                None,
            )],
        );
        let frame_data = crate::protocol::FrameData {
            cells: vec![crate::protocol::CellData {
                symbol: "R".into(),
                fg: crate::protocol::color_to_u32(Color::LightGreen),
                bg: crate::protocol::color_to_u32(Color::Black),
                modifier: crate::protocol::modifier_to_u16(Modifier::BOLD),
                skip: false,
                hyperlink: Some(0),
            }],
            width: 1,
            height: 1,
            cursor: Some(crate::protocol::CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 2,
            }),
            hyperlinks: vec!["https://example.com/remote".into()],
            graphics: Vec::new(),
        };
        app.apply_remote_projection_stream_event(
            key.clone(),
            generation,
            crate::remote_source::RemoteProjectionStreamRole::Control,
            crate::remote_source::RemoteProjectionStreamStatus::LiveController,
            Some(frame_data),
            None,
        );
        app.view.remote_projection_hit_areas = vec![crate::app::state::RemoteProjectionHitArea {
            rect: Rect::new(0, 0, 30, 5),
            host: "remote-a".into(),
            session: "default".into(),
            pane_id: Some("pane-a".into()),
            terminal_id: Some("term-a".into()),
            label: "remote".into(),
            focused: true,
            live: true,
        }];
        assert_eq!(
            app.remote_projection_url_at(1, 1).as_deref(),
            Some("https://example.com/remote")
        );
        let node = crate::api::schema::LayoutNode::Pane {
            pane: crate::api::schema::LayoutPane {
                pane_id: Some("pane-a".into()),
                terminal_id: Some("term-a".into()),
                label: Some("remote".into()),
                ..Default::default()
            },
        };
        let backend = TestBackend::new(30, 5);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                render_projection_layout(
                    &app,
                    frame,
                    &node,
                    Rect::new(0, 0, 30, 5),
                    "pane-a",
                    true,
                );
            })
            .expect("render live frame");
        let live = terminal.backend().buffer();
        assert_eq!(live[(1, 1)].symbol(), "R");
        assert_eq!(live[(1, 1)].fg, Color::LightGreen);
        assert!(live[(1, 1)].modifier.contains(Modifier::BOLD));

        app.apply_remote_projection_stream_event(
            key,
            generation,
            crate::remote_source::RemoteProjectionStreamRole::Control,
            crate::remote_source::RemoteProjectionStreamStatus::StaleLastKnown,
            None,
            Some("disconnected".into()),
        );
        terminal
            .draw(|frame| {
                render_projection_layout(
                    &app,
                    frame,
                    &node,
                    Rect::new(0, 0, 30, 5),
                    "pane-a",
                    false,
                );
            })
            .expect("render stale frame");
        assert_eq!(terminal.backend().buffer()[(1, 1)].symbol(), "R");
    }

    fn projected_overlay_app(
        frame_data: crate::protocol::FrameData,
    ) -> (
        AppState,
        crate::remote_source::RemoteProjectionTerminalKey,
        u64,
    ) {
        let mut app = crate::app::state::AppState::test_new();
        let selected = crate::remote_source::RemoteSpaceKey {
            host: "remote-a".into(),
            session: "default".into(),
            workspace_id: "ws-a".into(),
        };
        app.selected_remote_space = Some(selected.clone());
        let generation = app.begin_remote_projection_generation(Some(&selected), false);
        let key = crate::remote_source::RemoteProjectionTerminalKey {
            host: "remote-a".into(),
            session: "default".into(),
            workspace_id: "ws-a".into(),
            terminal_id: "term-a".into(),
        };
        app.seed_remote_projection_streams(
            generation,
            [(
                key.clone(),
                crate::remote_source::RemoteProjectionStreamRole::Control,
                crate::remote_source::RemoteProjectionStreamStatus::Connecting,
                None,
            )],
        );
        app.apply_remote_projection_stream_event(
            key.clone(),
            generation,
            crate::remote_source::RemoteProjectionStreamRole::Control,
            crate::remote_source::RemoteProjectionStreamStatus::LiveController,
            Some(frame_data),
            None,
        );
        (app, key, generation)
    }

    /// Build a projected frame from exact-width ASCII rows in a fixed color.
    fn projected_overlay_frame(width: u16, rows: &[&str]) -> crate::protocol::FrameData {
        let mut cells = Vec::new();
        for row in rows {
            assert_eq!(row.chars().count(), usize::from(width));
            for ch in row.chars() {
                cells.push(crate::protocol::CellData {
                    symbol: ch.to_string(),
                    fg: crate::protocol::color_to_u32(Color::LightGreen),
                    bg: crate::protocol::color_to_u32(Color::Black),
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                });
            }
        }
        crate::protocol::FrameData {
            cells,
            width,
            height: rows.len() as u16,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        }
    }

    fn projected_overlay_node(terminal_id: &str) -> crate::api::schema::LayoutNode {
        crate::api::schema::LayoutNode::Pane {
            pane: crate::api::schema::LayoutPane {
                pane_id: Some("pane-a".into()),
                terminal_id: Some(terminal_id.into()),
                label: Some("remote".into()),
                ..Default::default()
            },
        }
    }

    /// A finalized (mouse-released) visible projected selection, the state the
    /// render overlay draws.
    fn visible_projected_selection(
        key: crate::remote_source::RemoteProjectionTerminalKey,
        anchor: (u16, u16),
        cursor: (u16, u16),
        width: u16,
        height: u16,
    ) -> crate::selection::ProjectedSelection {
        let mut selection = crate::selection::ProjectedSelection::anchor(key, anchor.0, anchor.1);
        selection.drag(cursor.0, cursor.1, width, height);
        assert!(selection.finish());
        selection
    }

    fn render_projected_node(app: &AppState, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let node = projected_overlay_node("term-a");
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                render_projection_layout(
                    app,
                    frame,
                    &node,
                    Rect::new(0, 0, width, height),
                    "pane-a",
                    true,
                );
            })
            .expect("render projection");
        terminal.backend().buffer().clone()
    }

    #[test]
    fn projected_selection_overlay_styles_only_matching_key_with_shared_style() {
        let (mut app, key, _generation) =
            projected_overlay_app(projected_overlay_frame(6, &["abcdef", "ghijkl"]));
        app.projected_selection = Some(visible_projected_selection(
            key.clone(),
            (0, 1),
            (0, 3),
            6,
            2,
        ));
        let expected = panes::automatic_selection_style(&app.palette, app.host_terminal_theme);

        let buffer = render_projected_node(&app, 10, 5);

        // Bordered interior is (1,1,8,3); the 6x2 frame copies to x 1..=6, y 1..=2.
        for col in 1..=3u16 {
            let style = buffer[(1 + col, 1)].style();
            assert_eq!(style.fg, expected.fg, "selected col {col} fg");
            assert_eq!(style.bg, expected.bg, "selected col {col} bg");
        }
        // Cells outside the selection keep the copied frame style: no bleed.
        for (x, y) in [(1u16, 1u16), (5, 1), (6, 1), (2, 2), (4, 2)] {
            let cell = &buffer[(x, y)];
            assert_eq!(cell.fg, Color::LightGreen, "unselected cell ({x},{y}) fg");
            assert_eq!(cell.bg, Color::Black, "unselected cell ({x},{y}) bg");
        }

        // A selection keyed to a different terminal id must paint nowhere in
        // this pane (a local pane never enters this render path at all).
        let mut mismatched = key.clone();
        mismatched.terminal_id = "term-b".into();
        app.projected_selection = Some(visible_projected_selection(
            mismatched,
            (0, 1),
            (0, 3),
            6,
            2,
        ));
        let buffer = render_projected_node(&app, 10, 5);
        for y in 0..5u16 {
            for x in 0..10u16 {
                let style = buffer[(x, y)].style();
                assert_ne!(style.bg, expected.bg, "mismatched key styled ({x},{y})");
            }
        }
    }

    #[test]
    fn projected_selection_overlay_fails_closed_when_selection_exceeds_visible_grid() {
        // Frame (10x4) is larger than the bordered interior (6x2 of an 8x4
        // area). A selection reaching outside the copied interior can only
        // come from a mid-gesture shrink; it must fail closed with no
        // highlight at all, exactly like clipboard extraction — never a
        // silently clipped partial subset.
        let (mut app, key, _generation) = projected_overlay_app(projected_overlay_frame(
            10,
            &["aaaaaaaaaa", "bbbbbbbbbb", "cccccccccc", "dddddddddd"],
        ));
        app.projected_selection = Some(visible_projected_selection(
            key.clone(),
            (0, 0),
            (3, 9),
            10,
            4,
        ));
        let expected = panes::automatic_selection_style(&app.palette, app.host_terminal_theme);

        let buffer = render_projected_node(&app, 8, 4);

        for y in 0..4u16 {
            for x in 0..8u16 {
                let style = buffer[(x, y)].style();
                assert_ne!(style.bg, expected.bg, "fail-closed styled ({x},{y})");
            }
        }

        // A selection fully inside the copied 6x2 interior still highlights.
        app.projected_selection = Some(visible_projected_selection(
            key.clone(),
            (0, 0),
            (1, 5),
            10,
            4,
        ));
        let buffer = render_projected_node(&app, 8, 4);
        for y in [1u16, 2] {
            for x in 1..=6u16 {
                let style = buffer[(x, y)].style();
                assert_eq!(style.bg, expected.bg, "in-grid cell ({x},{y}) highlighted");
            }
        }
        // Right/bottom border cells stay untouched.
        for (x, y) in [(7u16, 1u16), (7, 2), (1, 3), (6, 3)] {
            let style = buffer[(x, y)].style();
            assert_ne!(style.bg, expected.bg, "outside copied interior ({x},{y})");
        }
    }

    #[test]
    fn projected_selection_overlay_never_paints_a_partial_wide_grapheme() {
        // Frame row [a][好][skip]: the wide grapheme needs columns 1..=2.
        let cell = |symbol: &str, skip: bool| crate::protocol::CellData {
            symbol: symbol.into(),
            fg: crate::protocol::color_to_u32(Color::LightGreen),
            bg: crate::protocol::color_to_u32(Color::Black),
            modifier: 0,
            skip,
            hyperlink: None,
        };
        let (mut app, key, _generation) = projected_overlay_app(crate::protocol::FrameData {
            cells: vec![cell("a", false), cell("好", false), cell("", true)],
            width: 3,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        });
        app.projected_selection = Some(visible_projected_selection(
            key.clone(),
            (0, 0),
            (0, 1),
            3,
            1,
        ));
        let expected = panes::automatic_selection_style(&app.palette, app.host_terminal_theme);

        // A 4x3 area leaves a 2x1 bordered interior: the visible edge cuts
        // the wide grapheme's required continuation, so nothing is painted.
        let buffer = render_projected_node(&app, 4, 3);
        for y in 0..3u16 {
            for x in 0..4u16 {
                let style = buffer[(x, y)].style();
                assert_ne!(style.bg, expected.bg, "partial-wide styled ({x},{y})");
            }
        }

        // A 5x3 area leaves a 3x1 interior covering the whole grapheme: lead
        // and continuation are both highlighted. The continuation cell is
        // marked `skip`, so ratatui's backend diff never flushes it — assert
        // against the frame buffer the overlay actually painted.
        let node = projected_overlay_node("term-a");
        let mut terminal = Terminal::new(TestBackend::new(5, 3)).expect("terminal");
        let completed = terminal
            .draw(|frame| {
                render_projection_layout(&app, frame, &node, Rect::new(0, 0, 5, 3), "pane-a", true);
            })
            .expect("render projection");
        for x in 1..=3u16 {
            let cell = &completed.buffer[(x, 1)];
            assert_eq!(
                Some(cell.bg),
                expected.bg,
                "full wide grapheme col {x} highlighted"
            );
        }
        assert!(
            completed.buffer[(3, 1)].skip,
            "the highlighted range must cover the continuation cell, not just the lead"
        );
    }

    #[test]
    fn projected_selection_overlay_highlights_cached_read_only_and_stale_frames() {
        let (mut app, key, generation) =
            projected_overlay_app(projected_overlay_frame(6, &["abcdef", "ghijkl"]));
        let expected = panes::automatic_selection_style(&app.palette, app.host_terminal_theme);

        for status in [
            crate::remote_source::RemoteProjectionStreamStatus::OwnedReadOnly,
            crate::remote_source::RemoteProjectionStreamStatus::StaleLastKnown,
        ] {
            // Keep the cached frame (None frame) while the stream degrades.
            app.apply_remote_projection_stream_event(
                key.clone(),
                generation,
                crate::remote_source::RemoteProjectionStreamRole::Control,
                status,
                None,
                Some("remote stream unavailable".into()),
            );
            app.projected_selection = Some(visible_projected_selection(
                key.clone(),
                (0, 1),
                (0, 3),
                6,
                2,
            ));

            let buffer = render_projected_node(&app, 10, 5);

            for col in 1..=3u16 {
                let style = buffer[(1 + col, 1)].style();
                assert_eq!(
                    style.bg, expected.bg,
                    "{status:?} cached frame col {col} stays highlightable"
                );
            }
            app.projected_selection = None;
        }
    }

    #[test]
    fn projected_selection_overlay_fails_closed_on_malformed_frame_or_skip_topology() {
        let (mut app, key, generation) =
            projected_overlay_app(projected_overlay_frame(6, &["abcdef", "ghijkl"]));
        let expected = panes::automatic_selection_style(&app.palette, app.host_terminal_theme);
        app.projected_selection = Some(visible_projected_selection(
            key.clone(),
            (0, 0),
            (0, 5),
            6,
            2,
        ));

        let assert_no_highlight = |app: &AppState| {
            let buffer = render_projected_node(app, 10, 5);
            for y in 0..5u16 {
                for x in 0..10u16 {
                    let style = buffer[(x, y)].style();
                    assert_ne!(style.bg, expected.bg, "fail-closed styled ({x},{y})");
                }
            }
        };

        // Malformed frame: cell count does not match width * height.
        app.apply_remote_projection_stream_event(
            key.clone(),
            generation,
            crate::remote_source::RemoteProjectionStreamRole::Control,
            crate::remote_source::RemoteProjectionStreamStatus::LiveController,
            Some(crate::protocol::FrameData {
                cells: vec![crate::protocol::CellData {
                    symbol: "z".into(),
                    fg: crate::protocol::color_to_u32(Color::LightGreen),
                    bg: crate::protocol::color_to_u32(Color::Black),
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                }],
                width: 3,
                height: 2,
                cursor: None,
                hyperlinks: Vec::new(),
                graphics: Vec::new(),
            }),
            None,
        );
        assert_no_highlight(&app);

        // Impossible skip topology inside the selection: a continuation cell
        // after a width-1 grapheme.
        app.apply_remote_projection_stream_event(
            key.clone(),
            generation,
            crate::remote_source::RemoteProjectionStreamRole::Control,
            crate::remote_source::RemoteProjectionStreamStatus::LiveController,
            Some(crate::protocol::FrameData {
                cells: vec![
                    crate::protocol::CellData {
                        symbol: "a".into(),
                        fg: crate::protocol::color_to_u32(Color::LightGreen),
                        bg: crate::protocol::color_to_u32(Color::Black),
                        modifier: 0,
                        skip: false,
                        hyperlink: None,
                    },
                    crate::protocol::CellData {
                        symbol: String::new(),
                        fg: 0,
                        bg: 0,
                        modifier: 0,
                        skip: true,
                        hyperlink: None,
                    },
                ],
                width: 2,
                height: 1,
                cursor: None,
                hyperlinks: Vec::new(),
                graphics: Vec::new(),
            }),
            None,
        );
        app.projected_selection = Some(visible_projected_selection(
            key.clone(),
            (0, 0),
            (0, 1),
            2,
            1,
        ));
        assert_no_highlight(&app);
    }

    #[tokio::test]
    async fn desktop_view_with_selected_remote_space_hides_local_hit_targets_and_skips_resize() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let _ = ws.test_split(ratatui::layout::Direction::Horizontal);
        ws.insert_test_runtime(
            ws.tabs[0].root_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"local"),
        );
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.selected_remote_space = Some(crate::remote_source::RemoteSpaceKey {
            host: "jafar".to_string(),
            session: "default".to_string(),
            workspace_id: "ws-remote".to_string(),
        });

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        // No local tab bar or tab hit targets while a projected remote space is
        // selected.
        assert_eq!(app.view.tab_bar_rect, Rect::default());
        assert!(app.view.tab_hit_areas.is_empty());
        assert_eq!(app.view.tab_scroll_left_hit_area, Rect::default());
        assert_eq!(app.view.tab_scroll_right_hit_area, Rect::default());
        assert_eq!(app.view.new_tab_hit_area, Rect::default());
        // No local pane hit targets or split-border resize handles: because the
        // projection branch skips compute_pane_infos and the background/desktop
        // pane resize entirely, no local pane is resized and no mouse click/drag
        // can resolve a local pane.
        assert!(app.view.pane_infos.is_empty());
        assert!(app.view.split_borders.is_empty());
        // The projection renders across the whole main area.
        assert!(app.view.terminal_area.width > 0);
    }

    #[test]
    fn project_layout_rects_partitions_area_and_identifies_focused_pane() {
        use crate::api::schema::{LayoutNode, LayoutPane, SplitDirection};

        let left_id = "left-pane";
        let right_id = "right-pane";
        let area = Rect::new(0, 0, 80, 24);

        let node = LayoutNode::Split {
            direction: SplitDirection::Right,
            ratio: 0.5,
            first: Box::new(LayoutNode::Pane {
                pane: LayoutPane {
                    pane_id: Some(left_id.to_string()),
                    terminal_id: Some("term-left".to_string()),
                    ..Default::default()
                },
            }),
            second: Box::new(LayoutNode::Pane {
                pane: LayoutPane {
                    pane_id: Some(right_id.to_string()),
                    terminal_id: Some("term-right".to_string()),
                    ..Default::default()
                },
            }),
        };

        let rects = project_layout_rects(&node, area, right_id);

        assert_eq!(rects.len(), 2, "two leaf panes");

        // Non-overlapping: each rect must not contain any point of the other.
        let (_, r0, _) = rects[0];
        let (_, r1, _) = rects[1];
        let overlap_x = r0.x < r1.x + r1.width && r1.x < r0.x + r0.width;
        let overlap_y = r0.y < r1.y + r1.height && r1.y < r0.y + r0.height;
        assert!(!overlap_x || !overlap_y, "rects must not overlap");

        // Together they must cover the full area width (horizontal split).
        assert_eq!(r0.width + r1.width, area.width);
        assert_eq!(r0.height, area.height);
        assert_eq!(r1.height, area.height);

        // Focused flag is only on the right pane.
        let focused_ids: Vec<_> = rects
            .iter()
            .filter(|(_, _, focused)| *focused)
            .map(|(p, _, _)| p.pane_id.as_deref())
            .collect();
        assert_eq!(focused_ids, vec![Some(right_id)]);
    }

    #[test]
    fn project_layout_rects_minimum_size_guard_returns_empty_for_tiny_area() {
        use crate::api::schema::{LayoutNode, LayoutPane, SplitDirection};

        let area = Rect::new(0, 0, 1, 1);
        let node = LayoutNode::Split {
            direction: SplitDirection::Right,
            ratio: 0.5,
            first: Box::new(LayoutNode::Pane {
                pane: LayoutPane {
                    ..Default::default()
                },
            }),
            second: Box::new(LayoutNode::Pane {
                pane: LayoutPane {
                    ..Default::default()
                },
            }),
        };

        let rects = project_layout_rects(&node, area, "");
        assert!(rects.is_empty(), "below minimum size must yield no rects");
    }

    #[test]
    fn copy_feedback_offset_only_increases_when_toast_rect_overlaps() {
        let area = Rect::new(0, 0, 80, 24);
        let feedback = crate::app::state::CopyFeedback {
            message: "copied to clipboard".into(),
        };
        let toast = crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "pi finished".into(),
            context: "workspace · 1".into(),
            position: None,
            target: None,
        };

        let bottom_right_toast = toast_notification_rect(
            area,
            &toast,
            false,
            crate::config::ToastHerdrPosition::BottomRight,
        );
        assert_eq!(
            copy_feedback_offset_for_toast(
                area,
                &feedback,
                0,
                crate::config::ToastClipboardPosition::TopCenter,
                bottom_right_toast,
            ),
            0
        );

        let bottom_center_toast = Rect::new(28, 21, 24, 3);
        assert_eq!(
            copy_feedback_offset_for_toast(
                area,
                &feedback,
                0,
                crate::config::ToastClipboardPosition::BottomCenter,
                bottom_center_toast,
            ),
            bottom_center_toast.height
        );
    }

    #[tokio::test]
    async fn focused_pane_cursor_wins_during_terminal_render() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_pane = ws.test_split(ratatui::layout::Direction::Horizontal);

        ws.insert_test_runtime(
            first_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left"),
        );
        ws.insert_test_runtime(
            second_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"r\r\nb"),
        );
        ws.tabs[0].layout.focus_pane(first_pane);

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        let focused = app
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == first_pane)
            .expect("focused pane info");

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();

        terminal
            .backend_mut()
            .assert_cursor_position((focused.inner_rect.x + 4, focused.inner_rect.y));
    }

    #[test]
    fn mobile_width_uses_header_and_full_width_terminal() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 44, 20));

        assert_eq!(app.view.layout, ViewLayout::Mobile);
        assert_eq!(app.view.sidebar_rect, Rect::default());
        assert_eq!(app.view.tab_bar_rect, Rect::default());
        assert_eq!(app.view.mobile_header_rect, Rect::new(0, 0, 44, 2));
        assert_eq!(app.view.terminal_area, Rect::new(0, 2, 44, 18));
        assert_eq!(app.view.mobile_menu_hit_area.height, 2);
        assert_eq!(
            app.view.mobile_menu_hit_area.x + app.view.mobile_menu_hit_area.width,
            44
        );
    }

    #[test]
    fn desktop_toast_hit_area_uses_full_frame_not_terminal_area() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.toast_config.herdr.position = crate::config::ToastHerdrPosition::TopLeft;
        app.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "pi finished".into(),
            context: "one".into(),
            position: None,
            target: None,
        });

        compute_view(&mut app, Rect::new(0, 0, 100, 20));

        assert_eq!(app.view.layout, ViewLayout::Desktop);
        assert!(app.view.terminal_area.x > 0);
        assert_eq!(app.view.toast_hit_area.x, 0);
        assert_eq!(app.view.toast_hit_area.y, 0);
    }

    #[test]
    fn desktop_toast_hit_area_still_offsets_for_config_diagnostic() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.config_diagnostic = Some("config warning".into());
        app.toast_config.herdr.position = crate::config::ToastHerdrPosition::TopLeft;
        app.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "pi finished".into(),
            context: "one".into(),
            position: None,
            target: None,
        });

        compute_view(&mut app, Rect::new(0, 0, 100, 20));

        assert_eq!(app.view.toast_hit_area.x, 0);
        assert_eq!(app.view.toast_hit_area.y, 1);
    }

    #[test]
    fn configured_mobile_width_threshold_controls_layout_switch() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        assert_eq!(app.view.layout, ViewLayout::Desktop);

        app.mobile_width_threshold = 90;
        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        assert_eq!(app.view.layout, ViewLayout::Mobile);
        assert_eq!(app.view.mobile_header_rect, Rect::new(0, 0, 80, 2));
        assert_eq!(app.view.terminal_area, Rect::new(0, 2, 80, 18));
    }

    #[test]
    fn hide_tab_bar_when_single_tab_toggles_geometry_with_tab_count() {
        let mut app = crate::app::state::AppState::test_new();
        app.hide_tab_bar_when_single_tab = true;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        let single_tab_terminal_area = app.view.terminal_area;
        assert_eq!(app.view.tab_bar_rect, Rect::default());
        // Main area starts right after the rail-plus-panel sidebar
        // (`host_rail_width()` wider than the panel alone).
        assert_eq!(single_tab_terminal_area, Rect::new(36, 0, 44, 20));
        assert!(app.view.tab_hit_areas.is_empty());
        assert_eq!(app.view.new_tab_hit_area, Rect::default());

        app.workspaces[0].test_add_tab(Some("logs"));
        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        assert_eq!(app.view.tab_bar_rect, Rect::new(36, 0, 44, 1));
        assert_eq!(app.view.terminal_area, Rect::new(36, 1, 44, 19));
        assert_eq!(app.view.tab_hit_areas.len(), 2);
        assert!(app.view.tab_hit_areas.iter().all(|rect| rect.width > 0));
        assert!(app.view.new_tab_hit_area.width > 0);

        assert!(app.workspaces[0].close_tab(1));
        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        assert_eq!(app.view.terminal_area, single_tab_terminal_area);
        assert_eq!(app.view.tab_bar_rect, Rect::default());
        assert!(app.view.tab_hit_areas.is_empty());
        assert_eq!(app.view.new_tab_hit_area, Rect::default());
    }

    #[tokio::test]
    async fn hide_tab_bar_when_single_tab_resizes_background_tabs_per_workspace() {
        let mut app = crate::app::state::AppState::test_new();
        app.hide_tab_bar_when_single_tab = true;

        let mut one_tab_workspace = Workspace::test_new("one");
        let one_tab_pane = one_tab_workspace.tabs[0].root_pane;
        let one_tab_runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(10, 5, b"");
        one_tab_workspace.tabs[0]
            .runtimes
            .insert(one_tab_pane, one_tab_runtime);

        let mut two_tab_workspace = Workspace::test_new("two");
        let background_tab = two_tab_workspace.test_add_tab(Some("logs"));
        let two_tab_pane = two_tab_workspace.tabs[background_tab].root_pane;
        let two_tab_runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(10, 5, b"");
        two_tab_workspace.tabs[background_tab]
            .runtimes
            .insert(two_tab_pane, two_tab_runtime);

        app.workspaces = vec![one_tab_workspace, two_tab_workspace];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let one_tab_size = app.workspaces[0].tabs[0].runtimes[&one_tab_pane].current_size();
        let two_tab_size =
            app.workspaces[1].tabs[background_tab].runtimes[&two_tab_pane].current_size();
        // Column width shrinks by the rail's fixed width versus the
        // pre-rail expectation; row counts are unaffected (the rail only
        // consumes columns).
        assert_eq!(one_tab_size, (20, 43));
        assert_eq!(two_tab_size, (19, 43));
    }

    #[tokio::test]
    async fn mobile_background_tabs_use_mobile_terminal_area() {
        let mut app = crate::app::state::AppState::test_new();

        let mut workspace = Workspace::test_new("mobile");
        let background_tab = workspace.test_add_tab(Some("logs"));
        let background_pane = workspace.tabs[background_tab].root_pane;
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(10, 5, b"");
        workspace.tabs[background_tab]
            .runtimes
            .insert(background_pane, runtime);

        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 44, 20));

        assert_eq!(app.view.layout, ViewLayout::Mobile);
        assert_eq!(app.view.terminal_area, Rect::new(0, 2, 44, 18));
        assert_eq!(
            app.workspaces[0].tabs[background_tab].runtimes[&background_pane].current_size(),
            (18, 43)
        );
    }

    #[test]
    fn product_announcement_renders_above_config_diagnostic() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::ProductAnnouncement;
        app.product_announcement = Some(crate::app::state::ProductAnnouncementState {
            version: "0.6.0".into(),
            id: "keybinding-v2".into(),
            title: "Keybinding syntax changed".into(),
            body: "### Update\n- Body".into(),
            scroll: 0,
            preview: false,
        });
        app.config_diagnostic = Some(
            "unsafe direct keybinding: keys.new_workspace = \"n\"\nunsafe direct keybinding: keys.new_tab = \"c\""
                .into(),
        );

        let area = Rect::new(0, 0, 44, 20);
        compute_view(&mut app, area);

        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let popup = centered_popup_rect(
            area,
            PRODUCT_ANNOUNCEMENT_MODAL_SIZE.0,
            PRODUCT_ANNOUNCEMENT_MODAL_SIZE.1,
        )
        .expect("announcement popup");
        let title_row = popup.y + 1;
        let row = buffer_row_text(buffer, Rect::new(0, title_row, area.width, 1), title_row);

        assert!(row.contains("Keybinding syntax changed"));
        assert!(!row.contains("config warning"));
    }

    #[test]
    fn compute_view_clamps_sidebar_width_to_configured_max() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.sidebar_max_width = 30;
        app.sidebar_width = 999;

        compute_view(&mut app, Rect::new(0, 0, 100, 20));

        // Total sidebar is the fixed-width host rail plus the clamped
        // Spaces/Agents panel: the panel itself still clamps to the
        // configured max, but the rail adds its own fixed width on top.
        assert_eq!(app.view.sidebar_panel_rect.width, 30);
        assert_eq!(app.view.host_rail_rect.width, host_rail_width());
        assert_eq!(app.view.sidebar_rect.width, host_rail_width() + 30);
    }

    #[test]
    fn compute_view_clamps_sidebar_width_to_configured_min() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.sidebar_min_width = 22;
        app.sidebar_width = 5;

        compute_view(&mut app, Rect::new(0, 0, 100, 20));

        // Same rail-plus-panel total as the max-width case above, clamped to
        // the configured min on the panel side.
        assert_eq!(app.view.sidebar_panel_rect.width, 22);
        assert_eq!(app.view.host_rail_rect.width, host_rail_width());
        assert_eq!(app.view.sidebar_rect.width, host_rail_width() + 22);
    }

    #[test]
    fn compute_view_allocates_host_rail_and_panel_rects_for_remote_sources() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        let default_host =
            crate::remote_source::RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        let session_host = crate::remote_source::RemoteHostKey::new("jafar", "agents");
        app.remote_sources.mark_status(
            &default_host,
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        app.remote_sources.mark_status(
            &session_host,
            crate::remote_source::RemoteConnectionStatus::Disconnected,
        );

        compute_view(&mut app, Rect::new(0, 0, 100, 20));

        // The dedicated 10-column host rail sits beside the Spaces/Agents
        // panel at full sidebar height (never a full-width section above it).
        assert_eq!(app.view.host_rail_rect, Rect::new(0, 0, 10, 20));
        assert_eq!(app.view.sidebar_panel_rect, Rect::new(10, 0, 26, 20));
        assert_eq!(app.view.sidebar_rect, Rect::new(0, 0, 36, 20));
        let labels = host_list_entries(&app)
            .into_iter()
            .map(|entry| entry.label)
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["local", "jafar", "jafar/agents"]);
        // Row 0 is the ` hosts` header, so the first host row is row 1.
        assert_eq!(
            host_target_at(&app, 0, 0),
            None,
            "header row is not a host target"
        );
        assert_eq!(
            host_target_at(&app, 0, 1),
            Some(crate::app::state::SidebarSource::Local)
        );
        assert_eq!(
            host_target_at(&app, 0, 2),
            Some(crate::app::state::SidebarSource::Remote(default_host))
        );
    }

    #[test]
    fn compute_view_keeps_host_rail_on_narrow_expanded_desktop() {
        // Ahmed's 2026-07-20 correction: an ordinary narrow EXPANDED-DESKTOP
        // width (just above the mobile threshold) must still show the fixed
        // 10-column host rail beside the panel. The rail is never suppressed
        // for width alone on desktop (only the mobile layout and the
        // collapsed sidebar drop it).
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        let host =
            crate::remote_source::RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources.mark_status(
            &host,
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        app.sidebar_source = crate::app::state::SidebarSource::Remote(host.clone());

        // Width 70 is above the mobile threshold (64) but still a narrow
        // desktop sidebar.
        compute_view(&mut app, Rect::new(0, 0, 70, 20));

        assert_eq!(app.view.layout, ViewLayout::Desktop);
        // The rail is present (not Rect::default()), fixed at 10 columns, and
        // full sidebar height; the panel sits directly beside it (never below).
        assert_ne!(app.view.host_rail_rect, Rect::default());
        assert_eq!(app.view.host_rail_rect.width, 10);
        assert_eq!(app.view.host_rail_rect.height, app.view.sidebar_rect.height);
        assert!(app.view.sidebar_panel_rect.height > 0);
        assert_eq!(
            app.view.sidebar_panel_rect.x,
            app.view.host_rail_rect.x + app.view.host_rail_rect.width
        );
        assert_eq!(app.view.sidebar_panel_rect.y, app.view.host_rail_rect.y);
        // The projected remote selection is still effective (no local fallback).
        assert_eq!(
            app.effective_sidebar_source(),
            crate::app::state::SidebarSource::Remote(host)
        );
    }

    #[test]
    fn compute_view_keeps_host_rail_present_with_local_only() {
        // The dedicated host rail is never suppressed just because no remote
        // host is configured or cached — it always shows the ` hosts` header
        // with `local` as a selectable row, matching the ROADMAP host-rail
        // contract (not the old cache-only visibility gate).
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 100, 20));

        assert_eq!(app.view.host_rail_rect, Rect::new(0, 0, 10, 20));
        assert_eq!(
            app.effective_sidebar_source(),
            crate::app::state::SidebarSource::Local
        );
        assert_eq!(
            host_target_at(&app, 0, 1),
            Some(crate::app::state::SidebarSource::Local)
        );
    }

    #[test]
    fn hidden_collapsed_sidebar_uses_full_width_terminal_area() {
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_collapsed = true;
        app.sidebar_collapsed_mode = crate::config::SidebarCollapsedModeConfig::Hidden;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        assert_eq!(app.view.sidebar_rect, Rect::new(0, 0, 0, 20));
        assert_eq!(app.view.tab_bar_rect, Rect::new(0, 0, 80, 1));
        assert_eq!(app.view.terminal_area, Rect::new(0, 1, 80, 19));
        assert!(app.view.workspace_card_areas.is_empty());

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
    }

    #[test]
    fn collapsed_sidebar_keeps_active_workspace_highlight_in_terminal_mode() {
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_collapsed = true;
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.active = Some(1);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let (ws_area, _, _) = collapsed_sidebar_sections(app.view.sidebar_rect);
        let active_row = ws_area.y + 1;
        let active_style = buffer[(ws_area.x, active_row)].style();

        assert_eq!(active_style.bg, Some(app.palette.surface_dim));
    }

    #[test]
    fn expanded_sidebar_workspace_rows_show_state_before_name_without_numbers() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("one");
        let repo = temp_git_repo("main");
        ws.identity_cwd = repo.clone();
        let root_pane = ws.tabs[0].root_pane;
        ws.refresh_git_ahead_behind();

        app.workspaces = vec![ws];
        app.ensure_test_terminals();
        let root_terminal_id = app.workspaces[0].tabs[0].panes[&root_pane]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&root_terminal_id).unwrap().cwd = repo.clone();
        app.selected = 0;
        app.mode = Mode::Navigate;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let card = app.view.workspace_card_areas[0].rect;
        let line1 = buffer_row_text(buffer, card, card.y);
        let line2 = buffer_row_text(buffer, card, card.y + 1);

        assert!(line1.starts_with(" · one"));
        assert!(!line1.contains("1 one"));
        assert_eq!(line2, "   main");

        std::fs::remove_dir_all(repo).ok();
    }

    #[test]
    fn tab_bar_dims_auto_named_tabs_and_emphasizes_custom_tabs() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let custom_tab = ws.test_add_tab(Some("logs"));
        ws.switch_tab(custom_tab);

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let auto_rect = app.view.tab_hit_areas[0];
        let custom_rect = app.view.tab_hit_areas[1];
        let auto_style = buffer[(auto_rect.x + 1, auto_rect.y)].style();
        let custom_style = buffer[(custom_rect.x + 1, custom_rect.y)].style();

        assert_eq!(auto_style.fg, Some(app.palette.overlay0));
        assert!(auto_style.add_modifier.contains(Modifier::DIM));
        assert_eq!(custom_style.fg, Some(app.palette.panel_bg));
        assert!(custom_style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn tab_bar_uses_surface_dim_when_panel_background_resets() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let custom_tab = ws.test_add_tab(Some("logs"));
        ws.switch_tab(custom_tab);

        app.palette.panel_bg = Color::Reset;
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let custom_rect = app.view.tab_hit_areas[1];
        let custom_style = buffer[(custom_rect.x + 1, custom_rect.y)].style();

        assert_eq!(custom_style.bg, Some(app.palette.accent));
        assert_eq!(custom_style.fg, Some(app.palette.surface_dim));
        assert!(custom_style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn new_tab_button_tracks_rightmost_tab_when_tabs_fit() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(Some("logs"));

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let last_visible = app
            .view
            .tab_hit_areas
            .iter()
            .rev()
            .find(|rect| rect.width > 0)
            .copied()
            .expect("last visible tab");

        assert_eq!(
            app.view.new_tab_hit_area.x,
            last_visible.x + last_visible.width
        );
    }

    #[test]
    fn tab_bar_shows_scroll_controls_when_tabs_overflow() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        for name in ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta"] {
            ws.test_add_tab(Some(name));
        }

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.tab_scroll_follow_active = false;
        app.tab_scroll = 2;

        compute_view(&mut app, Rect::new(0, 0, 65, 20));

        assert!(app.view.tab_scroll_left_hit_area.width > 0);
        assert!(app.view.tab_scroll_right_hit_area.width > 0);
        assert_eq!(app.view.tab_hit_areas[0].width, 0);
        assert_eq!(app.view.tab_hit_areas[1].width, 0);
        assert!(app.view.tab_hit_areas[2].width > 0);
        assert!(app.view.new_tab_hit_area.width > 0);

        let last_visible = app
            .view
            .tab_hit_areas
            .iter()
            .rev()
            .find(|rect| rect.width > 0)
            .copied()
            .expect("last visible tab");

        assert_eq!(
            app.view.tab_scroll_right_hit_area.x,
            last_visible.x + last_visible.width
        );
        assert_eq!(
            app.view.new_tab_hit_area.x,
            app.view.tab_scroll_right_hit_area.x + app.view.tab_scroll_right_hit_area.width
        );
    }

    #[test]
    fn tab_bar_clamps_manual_scroll_at_last_visible_tab() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        for name in [
            "one", "two", "three", "four", "five", "six", "seven", "eight",
        ] {
            ws.test_add_tab(Some(name));
        }

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.tab_scroll_follow_active = false;
        app.tab_scroll = usize::MAX;

        compute_view(&mut app, Rect::new(0, 0, 65, 20));

        let last_idx = app.workspaces[0].tabs.len() - 1;
        assert!(app.view.tab_hit_areas[last_idx].width > 0);
        let clamped_scroll = app.tab_scroll;

        app.scroll_tabs_right();

        assert_eq!(app.tab_scroll, clamped_scroll);
        assert!(app.view.tab_hit_areas[last_idx].width > 0);
    }

    #[test]
    fn pane_scrollbar_rect_uses_reserved_rightmost_column() {
        let info = PaneInfo {
            id: crate::layout::PaneId::from_raw(1),
            rect: Rect::new(0, 0, 12, 8),
            inner_rect: Rect::new(1, 1, 9, 6),
            scrollbar_rect: Some(Rect::new(10, 1, 1, 6)),
            borders: ratatui::widgets::Borders::ALL,
            is_focused: true,
        };

        assert_eq!(pane_scrollbar_rect(&info), Some(Rect::new(10, 1, 1, 6)));
    }

    #[tokio::test]
    async fn compute_view_reserves_terminal_column_when_pane_scrollbar_is_visible() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                12,
                4,
                4096,
                b"000000000000\r\n111111111111\r\n222222222222\r\n333333333333\r\n444444444444\r\n",
            ),
        );

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;

        compute_view(&mut app, Rect::new(0, 0, 40, 12));

        let info = app.view.pane_infos.first().expect("pane info");
        assert_eq!(info.inner_rect.width + 1, app.view.terminal_area.width);
        assert_eq!(
            info.scrollbar_rect,
            Some(Rect::new(
                info.inner_rect.x + info.inner_rect.width,
                info.inner_rect.y,
                1,
                info.inner_rect.height,
            ))
        );
    }

    #[test]
    fn scrollbar_stays_hidden_without_scrollback() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 0,
            viewport_rows: 5,
        };

        assert!(!should_show_scrollbar(metrics));
    }

    #[test]
    fn scrollbar_shows_with_scrollback() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };

        assert!(should_show_scrollbar(metrics));
    }

    #[test]
    fn scrollbar_thumb_reaches_bottom_when_scrolled_to_bottom() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };
        let track = Rect::new(9, 4, 1, 5);

        let thumb = scrollbar_thumb(metrics, track).expect("thumb");
        assert_eq!(thumb.top + thumb.len, track.y + track.height);
    }

    #[test]
    fn scrollbar_offset_mapping_hits_top_middle_and_bottom() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };
        let track = Rect::new(9, 4, 1, 5);

        assert_eq!(scrollbar_offset_from_row(metrics, track, 4), 20);
        assert_eq!(scrollbar_offset_from_row(metrics, track, 6), 10);
        assert_eq!(scrollbar_offset_from_row(metrics, track, 8), 0);
    }

    #[test]
    fn dragging_from_current_thumb_row_preserves_offset() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 7,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };
        let track = Rect::new(9, 4, 1, 8);
        let thumb = scrollbar_thumb(metrics, track).expect("thumb");
        let row = thumb.top + thumb.len / 2;
        let grab = scrollbar_thumb_grab_offset(metrics, track, row).expect("grab");

        assert_eq!(scrollbar_offset_from_drag_row(metrics, track, row, grab), 7);
    }

    fn buffer_row_text(buffer: &ratatui::buffer::Buffer, area: Rect, row: u16) -> String {
        (area.x..area.x + area.width)
            .map(|x| buffer[(x, row)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn host_glass_takes_over_content_and_renders_identity_live_and_cached_stale_states() {
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("local")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.host_glass_enabled = true;
        let host =
            crate::remote_source::RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.state
            .select_sidebar_source(crate::app::state::SidebarSource::Remote(host.clone()));

        compute_view_with_runtime_registry(
            &mut app.state,
            &app.terminal_runtimes,
            Rect::new(0, 0, 100, 20),
        );
        assert!(app.state.host_glass_surface_active());
        assert!(app.state.view.host_rail_rect.width > 0);
        assert_eq!(app.state.view.tab_bar_rect, Rect::default());
        assert!(app.state.view.tab_hit_areas.is_empty());
        assert!(app.state.view.pane_infos.is_empty());
        assert!(app.state.view.split_borders.is_empty());
        assert!(app.state.view.remote_projection_tab_hit_areas.is_empty());
        assert!(app.state.view.remote_projection_hit_areas.is_empty());

        let generation = app.state.begin_host_glass_generation(host.clone());
        let body = host_glass_body_area(app.state.view.terminal_area);
        let surface = crate::app::host_glass::GlassSurfaceCore::new(body, generation)
            .expect("PTY-free host glass surface");
        surface.feed(b"\x1b[2J\x1b[HREMOTE-APP");
        app.host_glass_surfaces.insert(host.clone(), surface);
        assert!(app.state.set_host_glass_status(
            &host,
            generation,
            crate::app::host_glass::GlassStatus::Live,
            None,
        ));

        let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("test terminal");
        terminal
            .draw(|frame| {
                render_with_runtime_registry_and_glass(
                    &app.state,
                    &app.terminal_runtimes,
                    Some(&app.host_glass_surfaces),
                    frame,
                )
            })
            .expect("render live glass");
        let buffer = terminal.backend().buffer();
        let indicator = buffer_row_text(
            buffer,
            app.state.view.terminal_area,
            app.state.view.terminal_area.y,
        );
        assert!(indicator.contains("glass"));
        assert!(indicator.contains("jafar"));
        assert!(indicator.contains("Live"));
        assert!(buffer_row_text(buffer, body, body.y).starts_with("REMOTE-APP"));

        app.state
            .host_glass_states
            .get_mut(&host)
            .expect("selected glass metadata")
            .input_drop_cue = Some(crate::app::host_glass::GlassInputDropReason::QueueFull);
        terminal
            .draw(|frame| {
                render_with_runtime_registry_and_glass(
                    &app.state,
                    &app.terminal_runtimes,
                    Some(&app.host_glass_surfaces),
                    frame,
                )
            })
            .expect("render live queue-full cue");
        let live_cue_row = body.y + body.height / 2;
        let live_cue = buffer_row_text(terminal.backend().buffer(), body, live_cue_row);
        assert!(live_cue.contains("INPUT DROPPED"));
        assert!(live_cue.contains("input queue full"));
        assert!(live_cue.contains("not queued"));
        app.state
            .host_glass_states
            .get_mut(&host)
            .expect("selected glass metadata")
            .input_drop_cue = None;

        let stale_now = std::time::Instant::now();
        app.state
            .host_glass_states
            .get_mut(&host)
            .expect("selected glass metadata")
            .last_frame_at = Some(stale_now - std::time::Duration::from_secs(42));
        assert!(app.state.set_host_glass_status(
            &host,
            generation,
            crate::app::host_glass::GlassStatus::Stale { since: stale_now },
            Some("cached frame".into()),
        ));
        assert!(app.tick_host_glass_status(stale_now));
        assert_eq!(
            app.state
                .host_glass_states
                .get(&host)
                .and_then(|glass| glass.last_live_frame_age_secs),
            Some(42)
        );
        terminal
            .draw(|frame| {
                render_with_runtime_registry_and_glass(
                    &app.state,
                    &app.terminal_runtimes,
                    Some(&app.host_glass_surfaces),
                    frame,
                )
            })
            .expect("render stale glass");
        let buffer = terminal.backend().buffer();
        let indicator = buffer_row_text(
            buffer,
            app.state.view.terminal_area,
            app.state.view.terminal_area.y,
        );
        assert!(indicator.contains("Stale"));
        let detail_row = body.y + body.height / 2;
        let identity_row = detail_row - 1;
        assert_eq!(
            buffer_row_text(buffer, body, identity_row).trim(),
            "STALE · jafar"
        );
        assert_eq!(
            buffer_row_text(buffer, body, detail_row).trim(),
            "last live frame 42s ago"
        );
        assert!(buffer[(body.x, body.y)]
            .style()
            .add_modifier
            .contains(Modifier::DIM));

        app.state
            .host_glass_states
            .get_mut(&host)
            .expect("selected glass metadata")
            .input_drop_cue = Some(crate::app::host_glass::GlassInputDropReason::Stale);
        terminal
            .draw(|frame| {
                render_with_runtime_registry_and_glass(
                    &app.state,
                    &app.terminal_runtimes,
                    Some(&app.host_glass_surfaces),
                    frame,
                )
            })
            .expect("render stale input-drop cue");
        assert_eq!(
            buffer_row_text(terminal.backend().buffer(), body, identity_row).trim(),
            "STALE · jafar"
        );
        assert_eq!(
            buffer_row_text(terminal.backend().buffer(), body, detail_row).trim(),
            "last live frame 42s ago · INPUT DROPPED · not queued"
        );
    }

    #[test]
    fn host_glass_stale_banner_names_host_and_reports_exact_last_frame_age() {
        let banner = host_glass_stale_banner_lines("jafar", Some(42), None);
        assert_eq!(banner, [" STALE · jafar ", " last live frame 42s ago "]);

        let dropped = host_glass_stale_banner_lines(
            "jafar / work",
            Some(42),
            Some(crate::app::host_glass::GlassInputDropReason::Stale),
        );
        assert_eq!(dropped[0], " STALE · jafar / work ");
        assert_eq!(
            dropped[1],
            " last live frame 42s ago · INPUT DROPPED · not queued "
        );

        assert_eq!(
            host_glass_stale_banner_lines("jafar", None, None),
            [" STALE · jafar ", " no live frame received "]
        );
    }

    #[test]
    fn interactive_cached_host_reselect_uses_current_view_and_cell_pixels_without_kitty() {
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("local")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.host_glass_enabled = true;
        assert!(!app.state.kitty_graphics_enabled);

        let host = crate::remote_source::RemoteHostKey::new(
            "remote-a",
            crate::session::DEFAULT_SESSION_NAME,
        );
        assert!(app
            .state
            .remote_sources
            .connected_bridge_state(&host)
            .is_none());
        let area = Rect::new(0, 0, 100, 20);
        let cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 9,
            height_px: 18,
        };

        // Populate one real retained surface through the same post-compute
        // reconciliation seam used by the interactive loop.
        app.state
            .select_sidebar_source(crate::app::state::SidebarSource::Remote(host.clone()));
        compute_view_with_runtime_registry(&mut app.state, &app.terminal_runtimes, area);
        let first_geometry = app.reconcile_remote_content_surfaces(cell_size);
        app.host_glass_surfaces
            .get(&host)
            .expect("first attach creates a surface without a prepared bridge")
            .feed(b"\x1b[2J\x1b[HCACHED-EXACT");

        // The local view's terminal area excludes its tab row. Detaching must
        // not resize the cached glass from that unrelated geometry.
        app.state
            .select_sidebar_source(crate::app::state::SidebarSource::Local);
        compute_view_with_runtime_registry(&mut app.state, &app.terminal_runtimes, area);
        assert_ne!(
            host_glass_body_area(app.state.view.terminal_area),
            first_geometry.area
        );
        app.reconcile_remote_content_surfaces(cell_size);

        // Match the production order: projection retires before compute, then
        // glass reconciles from the newly computed remote takeover geometry
        // before the frame is rendered.
        app.state
            .select_sidebar_source(crate::app::state::SidebarSource::Remote(host.clone()));
        app.remote_projection_runtime.reconcile(
            &mut app.state,
            &app.remote_hosts,
            &app.event_tx,
            &mut app.selected_host_bridge_runtime,
        );
        compute_view_with_runtime_registry(&mut app.state, &app.terminal_runtimes, area);
        let expected_body = host_glass_body_area(app.state.view.terminal_area);
        let geometry = app.reconcile_remote_content_surfaces(cell_size);

        assert_eq!(geometry.area, expected_body);
        assert_eq!(
            geometry.hello(),
            crate::protocol::ClientMessage::Hello {
                version: crate::protocol::PROTOCOL_VERSION,
                cols: expected_body.width,
                rows: expected_body.height,
                cell_width_px: 9,
                cell_height_px: 18,
                requested_encoding: crate::protocol::RenderEncoding::TerminalAnsi,
                keybindings: crate::protocol::ClientKeybindings::Server,
                launch_mode: crate::protocol::ClientLaunchMode::App,
            }
        );
        let retained = app
            .host_glass_surfaces
            .get(&host)
            .expect("cached surface remains available on reselect");
        assert_eq!(retained.area(), expected_body);
        assert!(matches!(
            app.state
                .host_glass_states
                .get(&host)
                .map(|glass| glass.status),
            Some(crate::app::host_glass::GlassStatus::Stale { .. })
        ));

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("cached reselect terminal");
        terminal
            .draw(|frame| {
                render_with_runtime_registry_and_glass(
                    &app.state,
                    &app.terminal_runtimes,
                    Some(&app.host_glass_surfaces),
                    frame,
                )
            })
            .expect("render cached reselect in the same frame");
        assert_eq!(
            buffer_row_text(terminal.backend().buffer(), expected_body, expected_body.y),
            "CACHED-EXACT"
        );
    }

    #[test]
    fn first_host_glass_attach_renders_connecting_without_a_surface() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("local")];
        app.active = Some(0);
        app.selected = 0;
        app.host_glass_enabled = true;
        let host = crate::remote_source::RemoteHostKey::new(
            "remote-a",
            crate::session::DEFAULT_SESSION_NAME,
        );
        app.select_sidebar_source(crate::app::state::SidebarSource::Remote(host));
        compute_view(&mut app, Rect::new(0, 0, 100, 20));

        let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("test terminal");
        terminal
            .draw(|frame| render(&app, frame))
            .expect("render connecting glass");
        let buffer = terminal.backend().buffer();
        let indicator = buffer_row_text(buffer, app.view.terminal_area, app.view.terminal_area.y);
        assert!(indicator.contains("remote-a"));
        assert!(indicator.contains("Connecting"));
        let body = host_glass_body_area(app.view.terminal_area);
        assert!(buffer_row_text(buffer, body, body.y + body.height / 2).contains("Connecting"));

        let host = match app.effective_sidebar_source() {
            crate::app::state::SidebarSource::Remote(host) => host,
            crate::app::state::SidebarSource::Local => panic!("remote glass remains selected"),
        };
        let generation = app.begin_host_glass_generation(host.clone());
        assert!(app.set_host_glass_status(
            &host,
            generation,
            crate::app::host_glass::GlassStatus::Stale {
                since: std::time::Instant::now(),
            },
            Some("link down".into()),
        ));
        assert!(app.note_selected_host_glass_input_dropped(
            crate::app::host_glass::GlassInputDropReason::Stale,
        ));
        terminal
            .draw(|frame| render(&app, frame))
            .expect("render stale no-frame input cue");
        let detail_row = body.y + body.height / 2;
        assert_eq!(
            buffer_row_text(terminal.backend().buffer(), body, detail_row - 1).trim(),
            "STALE · remote-a"
        );
        assert_eq!(
            buffer_row_text(terminal.backend().buffer(), body, detail_row).trim(),
            "no live frame received · INPUT DROPPED · not queued"
        );
    }

    #[test]
    fn host_glass_flag_round_trip_restores_projection_view_hits_and_render_exactly() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("local")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        let host = crate::remote_source::RemoteHostKey::new(
            "remote-a",
            crate::session::DEFAULT_SESSION_NAME,
        );
        app.remote_sources.replace_workspace_snapshot(
            host.clone(),
            vec![crate::api::schema::WorkspaceInfo {
                workspace_id: "ws-a".into(),
                number: 1,
                label: "remote space".into(),
                focused: true,
                pane_count: 1,
                tab_count: 1,
                active_tab_id: "tab-a".into(),
                agent_status: crate::api::schema::AgentStatus::Unknown,
                worktree: None,
            }],
        );
        app.remote_sources.replace_tab_snapshot(
            &host,
            "ws-a",
            vec![crate::api::schema::TabInfo {
                tab_id: "tab-a".into(),
                workspace_id: "ws-a".into(),
                number: 1,
                label: "remote tab".into(),
                focused: true,
                pane_count: 1,
                agent_status: crate::api::schema::AgentStatus::Unknown,
            }],
        );
        app.remote_sources.set_capabilities(
            &host,
            crate::remote_source::RemoteSourceCapabilities {
                layout_export: true,
                tab_focus: true,
                tab_close: true,
                tab_create: true,
                ..Default::default()
            },
        );
        app.remote_sources.apply_projection_snapshot(
            &host,
            vec![crate::remote_source::RemoteProjectionSnapshot {
                workspace_id: "ws-a".into(),
                tab_id: Some("tab-a".into()),
                tab_label: Some("remote tab".into()),
                status: crate::remote_source::RemoteProjectionStatus::Available,
                layout: Some(crate::api::schema::LayoutDescription {
                    workspace_id: "ws-a".into(),
                    tab_id: "tab-a".into(),
                    zoomed: false,
                    focused_pane_id: "pane-a".into(),
                    root: crate::api::schema::LayoutNode::Pane {
                        pane: crate::api::schema::LayoutPane {
                            pane_id: Some("pane-a".into()),
                            terminal_id: Some("term-a".into()),
                            label: Some("remote shell".into()),
                            ..Default::default()
                        },
                    },
                }),
            }],
        );
        app.select_sidebar_source(crate::app::state::SidebarSource::Remote(host));

        let area = Rect::new(0, 0, 100, 20);
        compute_view(&mut app, area);
        assert!(!app.host_glass_surface_active());
        assert!(!app.view.remote_projection_tab_hit_areas.is_empty());
        assert!(!app.view.remote_projection_hit_areas.is_empty());
        let baseline_help = keybind_help_groups(&app);
        assert!(baseline_help.iter().all(|(_, entries)| entries
            .iter()
            .all(|(_, label)| label.as_ref() != "exit host glass")));

        let baseline_layout = app.view.layout;
        let baseline_sidebar_rect = app.view.sidebar_rect;
        let baseline_host_rail_rect = app.view.host_rail_rect;
        let baseline_sidebar_panel_rect = app.view.sidebar_panel_rect;
        let baseline_terminal_area = app.view.terminal_area;
        let baseline_tab_bar_rect = app.view.tab_bar_rect;
        let baseline_tab_hit_areas = app.view.tab_hit_areas.clone();
        let baseline_remote_tab_hits = app.view.remote_projection_tab_hit_areas.clone();
        let baseline_remote_pane_hits = app.view.remote_projection_hit_areas.clone();
        let mut baseline_terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("baseline terminal");
        baseline_terminal
            .draw(|frame| render(&app, frame))
            .expect("render baseline projection");
        let baseline_buffer = baseline_terminal.backend().buffer().clone();

        app.host_glass_enabled = true;
        compute_view(&mut app, area);
        assert!(app.host_glass_surface_active());
        assert!(app.view.remote_projection_tab_hit_areas.is_empty());
        assert!(app.view.remote_projection_hit_areas.is_empty());
        assert!(keybind_help_groups(&app).iter().any(|(_, entries)| entries
            .iter()
            .any(|(_, label)| label.as_ref() == "exit host glass")));
        let mut glass_terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("glass terminal");
        glass_terminal
            .draw(|frame| render(&app, frame))
            .expect("render glass takeover");
        assert_ne!(glass_terminal.backend().buffer(), &baseline_buffer);

        app.host_glass_enabled = false;
        compute_view(&mut app, area);
        assert_eq!(app.view.layout, baseline_layout);
        assert_eq!(app.view.sidebar_rect, baseline_sidebar_rect);
        assert_eq!(app.view.host_rail_rect, baseline_host_rail_rect);
        assert_eq!(app.view.sidebar_panel_rect, baseline_sidebar_panel_rect);
        assert_eq!(app.view.terminal_area, baseline_terminal_area);
        assert_eq!(app.view.tab_bar_rect, baseline_tab_bar_rect);
        assert_eq!(app.view.tab_hit_areas, baseline_tab_hit_areas);
        assert_eq!(
            app.view.remote_projection_tab_hit_areas,
            baseline_remote_tab_hits
        );
        assert_eq!(
            app.view.remote_projection_hit_areas,
            baseline_remote_pane_hits
        );
        assert_eq!(keybind_help_groups(&app), baseline_help);

        let mut restored_terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("restored terminal");
        restored_terminal
            .draw(|frame| render(&app, frame))
            .expect("render restored projection");
        assert_eq!(restored_terminal.backend().buffer(), &baseline_buffer);
    }

    fn temp_git_repo(branch: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("herdr-ui-test-{unique}"));
        std::fs::create_dir_all(root.join(".git")).expect("create .git dir");
        std::fs::write(
            root.join(".git/HEAD"),
            format!("ref: refs/heads/{branch}\n"),
        )
        .expect("write HEAD");
        root
    }

    #[test]
    fn prefix_mode_renders_prefix_indicator() {
        let mut app = crate::app::state::AppState::test_new();
        app.mode = Mode::Prefix;
        app.view.terminal_area = ratatui::layout::Rect::new(0, 0, 60, 4);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 4))
            .expect("test terminal");

        terminal
            .draw(|frame| render_prefix_overlay(&app, frame, app.view.terminal_area))
            .expect("draw prefix overlay");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("PREFIX"));
    }

    #[test]
    fn keybind_help_shows_unset_for_optional_actions() {
        let app = crate::app::state::AppState::test_new();
        let groups = keybind_help_groups(&app);

        let workspace_tab = groups
            .iter()
            .find(|(name, _)| *name == "workspaces / tabs")
            .expect("workspace tab group")
            .1
            .clone();
        let panes = groups
            .iter()
            .find(|(name, _)| *name == "panes")
            .expect("panes group")
            .1
            .clone();

        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "previous workspace"));
        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "next workspace"));
        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "previous agent"));
        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "next agent"));
        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "focus agent 1-9"));
        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "switch workspace 1-9"));
        assert!(panes
            .iter()
            .any(|(key, label)| key == "prefix+h" && label.as_ref() == "focus pane left"));
        assert!(panes
            .iter()
            .any(|(key, label)| key == "prefix+j" && label.as_ref() == "focus pane down"));
        assert!(panes
            .iter()
            .any(|(key, label)| key == "prefix+k" && label.as_ref() == "focus pane up"));
        assert!(panes
            .iter()
            .any(|(key, label)| key == "prefix+l" && label.as_ref() == "focus pane right"));
    }

    #[test]
    fn keybind_help_shows_custom_command_descriptions() {
        let mut app = crate::app::state::AppState::test_new();
        app.keybinds.custom_commands = vec![
            crate::config::CustomCommandKeybind {
                bindings: crate::config::ActionKeybinds::prefix("alt+g"),
                label: "prefix+alt+g".to_string(),
                command: "lazygit".to_string(),
                action: crate::config::CustomCommandAction::Pane,
                description: Some("open lazygit".to_string()),
            },
            crate::config::CustomCommandKeybind {
                bindings: crate::config::ActionKeybinds::prefix("alt+h"),
                label: "prefix+alt+h".to_string(),
                command: "echo hello".to_string(),
                action: crate::config::CustomCommandAction::Shell,
                description: None,
            },
        ];

        let groups = keybind_help_groups(&app);
        let custom = groups
            .iter()
            .find(|(name, _)| *name == "custom")
            .expect("custom group")
            .1
            .clone();
        assert!(custom
            .iter()
            .any(|(key, label)| key == "prefix+alt+g" && label.as_ref() == "open lazygit"));
        assert!(custom
            .iter()
            .any(|(key, label)| key == "prefix+alt+h" && label.as_ref() == "custom command"));

        let rendered_help = keybind_help_lines(&app)
            .into_iter()
            .flat_map(|(_, line)| line.spans)
            .map(|span| span.content.into_owned())
            .collect::<Vec<_>>()
            .join("");
        assert!(rendered_help.contains("open lazygit"));
        assert!(rendered_help.contains("custom command"));
    }

    #[test]
    fn keybind_help_compacts_multiple_indexed_ranges() {
        let config: crate::config::Config = toml::from_str(
            r#"
[keys]
switch_tab = ["prefix+1..9", "alt+1..9"]
switch_workspace = "ctrl+1..9"
"#,
        )
        .expect("config parses");

        let mut app = crate::app::state::AppState::test_new();
        app.keybinds = config.keybinds();

        let workspace_tab = keybind_help_groups(&app)
            .into_iter()
            .find(|(name, _)| *name == "workspaces / tabs")
            .expect("workspace tab group")
            .1;

        let switch_tab_key = workspace_tab
            .iter()
            .find(|(_, label)| label.as_ref() == "switch tab 1-9")
            .map(|(key, _)| key.as_str())
            .expect("switch tab help entry");
        let switch_workspace_key = workspace_tab
            .iter()
            .find(|(_, label)| label.as_ref() == "switch workspace 1-9")
            .map(|(key, _)| key.as_str())
            .expect("switch workspace help entry");

        assert_eq!(switch_tab_key, "prefix+1..9 / alt+1..9");
        assert_eq!(switch_workspace_key, "ctrl+1..9");
    }

    #[test]
    fn remote_source_without_workspace_snapshot_hides_local_surface() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        let host =
            crate::remote_source::RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources.mark_status(
            &host,
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        app.sidebar_source = crate::app::state::SidebarSource::Remote(host);
        app.selected_remote_space = None;

        compute_view(&mut app, Rect::new(0, 0, 140, 20));

        assert!(app.remote_projection_surface_active());
        assert!(app.view.pane_infos.is_empty());
        assert!(app.view.split_borders.is_empty());
        assert_eq!(app.view.tab_bar_rect, Rect::default());

        let backend = TestBackend::new(140, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let top_row = buffer_row_text(buffer, app.view.terminal_area, app.view.terminal_area.y);
        assert!(
            top_row.contains("jafar"),
            "remote source header not rendered: {top_row:?}"
        );
        assert!(
            top_row.contains("no local pane active"),
            "remote source header must not leave local active: {top_row:?}"
        );
        let body_row = buffer_row_text(
            buffer,
            app.view.terminal_area,
            app.view.terminal_area.y.saturating_add(1),
        );
        assert!(
            body_row.contains("waiting for authoritative remote workspace snapshot"),
            "waiting state missing: {body_row:?}"
        );
    }

    #[test]
    fn remote_source_with_projected_space_does_not_show_waiting_state() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        let host =
            crate::remote_source::RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.remote_sources.mark_status(
            &host,
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        app.sidebar_source = crate::app::state::SidebarSource::Remote(host.clone());
        app.selected_remote_space = Some(crate::remote_source::RemoteSpaceKey {
            host: host.host.clone(),
            session: host.session.clone(),
            workspace_id: "remote-ws".to_string(),
        });

        // Wide enough that the fixed 10-col host rail + 26-col sidebar panel
        // still leave room for the full projected header line (host/session,
        // workspace, tab, status) — mirrors the neighboring waiting-state test
        // above, which needs the same rail+panel headroom for its own header.
        compute_view(&mut app, Rect::new(0, 0, 140, 20));

        let backend = TestBackend::new(140, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let top_row = buffer_row_text(buffer, app.view.terminal_area, app.view.terminal_area.y);
        assert!(
            top_row.contains("workspace remote-ws"),
            "selected remote workspace header missing: {top_row:?}"
        );
        assert!(
            !top_row.contains("no local pane active"),
            "waiting header should not replace a selected remote workspace: {top_row:?}"
        );
    }
}
