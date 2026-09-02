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
    render_confirm_close_overlay, render_new_linked_worktree_overlay,
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
use self::sidebar::{render_sidebar, render_sidebar_collapsed, render_sidebar_glass_yielded};
use self::status::{
    copy_feedback_rect, render_config_diagnostic, render_copy_feedback, render_toast_notification,
    toast_notification_rect,
};
use self::tabs::render_tab_bar;
pub(crate) use self::{
    dialogs::{
        confirm_close_button_rects, confirm_close_popup_rect, new_linked_worktree_button_rects,
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
use crate::protocol::ViewContext;
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
    compute_view_with_context(
        app,
        terminal_runtimes,
        area,
        true,
        crate::kitty_graphics::HostCellSize::default(),
        ViewContext::Standalone,
    );
}

pub fn compute_view_with_cell_size(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    compute_view_with_context(
        app,
        terminal_runtimes,
        area,
        true,
        cell_size,
        ViewContext::Standalone,
    );
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
    compute_view_with_context(
        app,
        terminal_runtimes,
        area,
        false,
        crate::kitty_graphics::HostCellSize::default(),
        ViewContext::Standalone,
    );
}

pub(crate) fn compute_view_with_context(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
    view_context: ViewContext,
) {
    let cell_size = if resize_panes {
        cell_size
    } else {
        crate::kitty_graphics::HostCellSize::default()
    };
    compute_view_internal(
        app,
        terminal_runtimes,
        area,
        resize_panes,
        cell_size,
        view_context,
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
    view_context: ViewContext,
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
    let host_rail_visually_suppressed =
        !app.sidebar_collapsed && view_context == ViewContext::Embedded;
    let rail_w = if app.sidebar_collapsed || host_rail_visually_suppressed {
        0
    } else {
        host_rail_width()
    };

    // Glass-sidebar-yield: when the host glass is active and the sidebar is
    // expanded, the local Spaces/Agents panel is hidden so the remote's
    // streamed sidebar serves as "the sidebar for that host". The rail always
    // stays visible as the un-trappable escape hatch. We detect this before
    // computing sidebar_w so the geometry is correct from the start.
    //
    // `host_glass_surface_active()` reads `view.layout` / `host_rail_rect`
    // which haven't been written yet, so we replicate its preconditions
    // directly: glass enabled, sidebar expanded, embedded context not active
    // (rail not suppressed), and a remote source is selected.
    let glass_sidebar_yielded = !app.sidebar_collapsed
        && !host_rail_visually_suppressed
        && app.host_glass_enabled
        && rail_w > 0
        && matches!(
            app.sidebar_source,
            crate::app::state::SidebarSource::Remote(_)
        );

    let sidebar_w = if app.sidebar_collapsed {
        match app.sidebar_collapsed_mode {
            crate::config::SidebarCollapsedModeConfig::Compact => COLLAPSED_WIDTH,
            crate::config::SidebarCollapsedModeConfig::Hidden => 0,
        }
    } else if glass_sidebar_yielded {
        // Sidebar shrinks to just the rail; the panel disappears and the
        // terminal/glass area gains the freed columns.
        rail_w
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
    } else if glass_sidebar_yielded && rail_w > 0 && sidebar_area.width >= rail_w {
        // The entire sidebar IS the rail when yielded.
        Rect::new(sidebar_area.x, sidebar_area.y, rail_w, sidebar_area.height)
    } else if rail_w > 0 && sidebar_area.width > rail_w {
        Rect::new(sidebar_area.x, sidebar_area.y, rail_w, sidebar_area.height)
    } else {
        Rect::default()
    };
    let sidebar_panel_rect = if app.sidebar_collapsed || glass_sidebar_yielded {
        // Collapsed or yielded: no panel.
        Rect::default()
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
    app.view.host_rail_visually_suppressed = host_rail_visually_suppressed;
    app.view.glass_sidebar_yielded = glass_sidebar_yielded;
    app.view.sidebar_panel_rect = sidebar_panel_rect;

    let (tab_bar_rect, terminal_area) = app
        .active
        .and_then(|i| app.workspaces.get(i))
        .map(|ws| desktop_tab_bar_and_terminal_area(app, ws, main_area))
        .unwrap_or((Rect::default(), main_area));

    if !app.sidebar_collapsed {
        app.workspace_scroll =
            normalized_workspace_scroll(app, sidebar_panel_rect, app.workspace_scroll);
        if !host_rail_visually_suppressed {
            app.host_list_scroll =
                crate::ui::sidebar::normalized_host_list_scroll(app, app.host_list_scroll);
        }
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

    // Full-App glass owns the main area. Local tab/pane geometry, hit targets,
    // split borders, and background pane resizing are suppressed so no local
    // terminal runtime can be touched through mouse/keyboard while glass is
    // active.
    if app.host_glass_surface_active() {
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
            host_rail_visually_suppressed,
            glass_sidebar_yielded,
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
        host_rail_visually_suppressed,
        glass_sidebar_yielded,
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
        host_rail_visually_suppressed: false,
        glass_sidebar_yielded: false,
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
        } else if app.view.glass_sidebar_yielded {
            // Glass is active: local sidebar yields its Spaces/Agents panel;
            // only the host rail (the un-trappable escape hatch) remains.
            render_sidebar_glass_yielded(app, frame, sidebar_area);
        } else {
            render_sidebar(app, terminal_runtimes, frame, sidebar_area);
        }
    }
    if app.view.layout != ViewLayout::Mobile && !app.host_glass_surface_active() {
        render_tab_bar(app, frame, tab_bar_area);
    }
    if app.host_glass_surface_active() {
        render_host_glass(app, glass_surfaces, frame, terminal_area);
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

    #[tokio::test]
    async fn desktop_view_with_host_glass_hides_local_hit_targets_and_skips_resize() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let root = ws.tabs[0].root_pane;
        let _ = ws.test_split(ratatui::layout::Direction::Horizontal);
        ws.insert_test_runtime(
            root,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"local"),
        );
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.host_glass_enabled = true;
        app.select_sidebar_source(crate::app::state::SidebarSource::Remote(
            crate::remote_source::RemoteHostKey::new("jafar", "default"),
        ));
        let size_before = app.workspaces[0].test_runtimes[&root].current_size();

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        assert!(app.host_glass_surface_active());
        assert_eq!(app.view.tab_bar_rect, Rect::default());
        assert!(app.view.tab_hit_areas.is_empty());
        assert_eq!(app.view.tab_scroll_left_hit_area, Rect::default());
        assert_eq!(app.view.tab_scroll_right_hit_area, Rect::default());
        assert_eq!(app.view.new_tab_hit_area, Rect::default());
        assert!(app.view.pane_infos.is_empty());
        assert!(app.view.split_borders.is_empty());
        assert_eq!(
            app.workspaces[0].test_runtimes[&root].current_size(),
            size_before,
            "glass ownership must skip local pane resizing",
        );
        assert!(app.view.terminal_area.width > 0);
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
        // Row 0 is the ` hosts` header, row 1 is its breathing buffer, and the
        // first host row is row 2.
        assert_eq!(
            host_target_at(&app, 0, 0),
            None,
            "header row is not a host target"
        );
        assert_eq!(
            host_target_at(&app, 0, 1),
            None,
            "breathing row is not a host target"
        );
        assert_eq!(
            host_target_at(&app, 0, 2),
            Some(crate::app::state::SidebarSource::Local)
        );
        assert_eq!(
            host_target_at(&app, 0, 3),
            Some(crate::app::state::SidebarSource::Remote(default_host))
        );
    }

    #[test]
    fn embedded_view_omits_host_rail_and_reflows_without_losing_remote_authority() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.host_glass_enabled = true;
        let host = crate::remote_source::RemoteHostKey::new(
            "remote-a",
            crate::session::DEFAULT_SESSION_NAME,
        );
        app.remote_sources.mark_status(
            &host,
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        app.select_sidebar_source(crate::app::state::SidebarSource::Remote(host.clone()));

        // Glass sidebar yield is active in the normal standalone context
        // (glass enabled + remote selected). Take a glass-OFF snapshot for the
        // geometry reference so the embedded-vs-standalone reflow assertions
        // can compare rail-plus-panel standalone geometry against the
        // rail-suppressed embedded geometry — the invariant the test was
        // written to verify.
        let area = Rect::new(0, 0, 100, 20);
        app.host_glass_enabled = false;
        compute_view(&mut app, area);
        let standalone_sidebar = app.view.sidebar_rect;
        let standalone_panel = app.view.sidebar_panel_rect;
        let standalone_main = app.view.terminal_area;
        let standalone_rail = app.view.host_rail_rect;
        app.host_glass_enabled = true;
        let mut standalone_terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("standalone terminal");
        standalone_terminal
            .draw(|frame| render(&app, frame))
            .expect("render standalone host rail");
        let standalone_buffer = standalone_terminal.backend().buffer().clone();

        let terminal_runtimes = TerminalRuntimeRegistry::new();
        compute_view_with_context(
            &mut app,
            &terminal_runtimes,
            area,
            true,
            crate::kitty_graphics::HostCellSize::default(),
            ViewContext::Embedded,
        );

        assert_eq!(app.view.layout, ViewLayout::Desktop);
        assert_eq!(app.view.host_rail_rect, Rect::default());
        assert!(app.view.host_rail_visually_suppressed);
        assert_eq!(app.view.sidebar_panel_rect.x, standalone_sidebar.x);
        assert_eq!(app.view.sidebar_panel_rect.width, standalone_panel.width);
        assert_eq!(
            app.view.sidebar_rect.width + host_rail_width(),
            standalone_sidebar.width
        );
        assert_eq!(
            app.view.terminal_area.x + host_rail_width(),
            standalone_main.x
        );
        assert_eq!(
            app.view.terminal_area.width,
            standalone_main.width + host_rail_width()
        );
        assert_eq!(app.host_list_scroll, 0);
        assert_eq!(host_target_at(&app, standalone_rail.x, 2), None);
        assert_eq!(
            app.effective_sidebar_source(),
            crate::app::state::SidebarSource::Remote(host)
        );
        assert!(app.host_glass_surface_active());

        let mut embedded_terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("embedded terminal");
        embedded_terminal
            .draw(|frame| render(&app, frame))
            .expect("render embedded sidebar");
        let embedded_buffer = embedded_terminal.backend().buffer();
        let standalone_divider_x = standalone_rail.x + standalone_rail.width - 1;
        let embedded_outer_divider_x =
            app.view.sidebar_rect.x + app.view.sidebar_rect.width.saturating_sub(1);
        assert!(
            (0..area.height).all(|y| standalone_buffer[(standalone_divider_x, y)].symbol() == "│")
        );
        assert!(
            (0..area.height).any(|y| embedded_buffer[(standalone_divider_x, y)].symbol() != "│")
        );
        assert!((0..area.height)
            .all(|y| embedded_buffer[(embedded_outer_divider_x, y)].symbol() == "│"));
        assert!(buffer_row_text(
            embedded_buffer,
            app.view.sidebar_panel_rect,
            app.view.sidebar_panel_rect.y
        )
        .starts_with(" spaces"));
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
        // The remote selection is still effective (no local fallback).
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
            host_target_at(&app, 0, 2),
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

        // Match the production order: compute the remote takeover geometry,
        // then reconcile glass before rendering the frame.
        app.state
            .select_sidebar_source(crate::app::state::SidebarSource::Remote(host.clone()));
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
                view_context: crate::protocol::ViewContext::Embedded,
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

    #[tokio::test]
    async fn glass_disabled_remote_source_keeps_local_panes_visible() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("local");
        let root = workspace.tabs[0].root_pane;
        workspace.insert_test_runtime(
            root,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"LOCAL-SURFACE"),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        let host =
            crate::remote_source::RemoteHostKey::new("jafar", crate::session::DEFAULT_SESSION_NAME);
        app.sidebar_source = crate::app::state::SidebarSource::Remote(host.clone());
        compute_view(&mut app, Rect::new(0, 0, 140, 20));

        assert!(!app.host_glass_surface_active());
        assert!(!app.view.pane_infos.is_empty());
        assert_ne!(app.view.tab_bar_rect, Rect::default());
        let mut terminal = Terminal::new(TestBackend::new(140, 20)).expect("test terminal");
        terminal
            .draw(|frame| render(&app, frame))
            .expect("render local panes");
        assert!(buffer_row_text(
            terminal.backend().buffer(),
            app.view.terminal_area,
            app.view.terminal_area.y,
        )
        .contains("LOCAL-SURFACE"));
    }

    fn make_glass_app() -> (
        crate::app::state::AppState,
        crate::remote_source::RemoteHostKey,
    ) {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.host_glass_enabled = true;
        let host = crate::remote_source::RemoteHostKey::new(
            "remote-host",
            crate::session::DEFAULT_SESSION_NAME,
        );
        app.remote_sources.mark_status(
            &host,
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        app.select_sidebar_source(crate::app::state::SidebarSource::Remote(host.clone()));
        (app, host)
    }

    #[test]
    fn glass_active_sidebar_yields_panel_and_widens_terminal_area() {
        let (mut app, _host) = make_glass_app();
        let area = Rect::new(0, 0, 100, 20);

        // Baseline: glass disabled — full sidebar (rail + panel).
        app.host_glass_enabled = false;
        compute_view(&mut app, area);
        let baseline_terminal_x = app.view.terminal_area.x;
        let baseline_terminal_w = app.view.terminal_area.width;
        let baseline_sidebar_w = app.view.sidebar_rect.width;

        // Glass on: sidebar should shrink to just the rail.
        app.host_glass_enabled = true;
        compute_view(&mut app, area);

        assert!(
            app.view.glass_sidebar_yielded,
            "glass_sidebar_yielded must be true when glass is active"
        );
        assert_eq!(
            app.view.sidebar_rect.width,
            host_rail_width(),
            "sidebar collapses to rail width when yielded"
        );
        assert_eq!(
            app.view.host_rail_rect,
            Rect::new(0, 0, host_rail_width(), area.height),
            "host rail occupies the entire (shrunken) sidebar"
        );
        assert_eq!(
            app.view.sidebar_panel_rect,
            Rect::default(),
            "panel disappears when sidebar is yielded"
        );
        assert!(
            app.view.terminal_area.x < baseline_terminal_x,
            "terminal area starts earlier when panel is hidden"
        );
        assert!(
            app.view.terminal_area.width > baseline_terminal_w,
            "terminal area is wider when panel is hidden"
        );
        // Combined: sidebar_w + terminal_w + tab/no-tab bar should equal area
        // width. Both must add up correctly.
        assert_eq!(
            app.view.sidebar_rect.width + app.view.terminal_area.width,
            baseline_sidebar_w + baseline_terminal_w,
            "total width is conserved across yield"
        );
    }

    #[test]
    fn glass_inactive_sidebar_unchanged() {
        // Local source selected — glass is never active regardless of
        // host_glass_enabled; sidebar should be the full rail + panel.
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.host_glass_enabled = true; // enabled but local source
        let host = crate::remote_source::RemoteHostKey::new(
            "remote-host",
            crate::session::DEFAULT_SESSION_NAME,
        );
        app.remote_sources.mark_status(
            &host,
            crate::remote_source::RemoteConnectionStatus::Connected,
        );
        // Keep local selected (don't call select_sidebar_source(Remote(...))).
        let area = Rect::new(0, 0, 100, 20);
        compute_view(&mut app, area);

        assert!(
            !app.view.glass_sidebar_yielded,
            "glass_sidebar_yielded must be false when local is selected"
        );
        assert!(
            app.view.sidebar_panel_rect != Rect::default(),
            "panel present when glass is inactive"
        );
        assert_eq!(
            app.view.host_rail_rect.width,
            host_rail_width(),
            "host rail present when glass is inactive"
        );
        // sidebar = rail + panel + separator
        assert!(
            app.view.sidebar_rect.width > host_rail_width(),
            "sidebar wider than just the rail when glass is inactive"
        );
    }

    #[test]
    fn glass_sidebar_yield_renders_rail_hides_spaces_agents() {
        let (mut app, _host) = make_glass_app();
        let area = Rect::new(0, 0, 100, 20);
        compute_view(&mut app, area);
        assert!(app.view.glass_sidebar_yielded);

        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buf = terminal.backend().buffer().clone();

        // The host rail header " hosts" must appear in the sidebar area.
        let rail_rect = app.view.host_rail_rect;
        let header_row = buffer_row_text(&buf, rail_rect, rail_rect.y);
        assert!(
            header_row.contains("hosts"),
            "host rail header must be present in yielded sidebar: {header_row:?}"
        );

        // No " spaces" or " agents" header should appear in the (yielded) sidebar.
        let sidebar_rect = app.view.sidebar_rect;
        for y in sidebar_rect.y..sidebar_rect.y + sidebar_rect.height {
            let row = buffer_row_text(&buf, sidebar_rect, y);
            assert!(
                !row.contains("spaces"),
                "spaces header must not appear in yielded sidebar (row {y}): {row:?}"
            );
            assert!(
                !row.contains("agents"),
                "agents header must not appear in yielded sidebar (row {y}): {row:?}"
            );
        }
    }

    #[test]
    fn glass_sidebar_yield_rail_click_targets_preserved() {
        // host_target_at uses app.view.host_rail_rect, which is computed by
        // compute_view. Even in the yielded state, the rail is present and
        // its row areas must be populated so clicks route correctly.
        let (mut app, host) = make_glass_app();
        let area = Rect::new(0, 0, 100, 20);
        compute_view(&mut app, area);
        assert!(app.view.glass_sidebar_yielded);

        // Row 0 is the ` hosts` header, row 1 is its breathing buffer.
        assert_eq!(
            host_target_at(&app, 0, 0),
            None,
            "header row is not a host target in yielded sidebar"
        );
        assert_eq!(
            host_target_at(&app, 0, 1),
            None,
            "breathing row is not a host target in yielded sidebar"
        );
        // Row 2 is local, row 3 is the remote host.
        assert_eq!(
            host_target_at(&app, 0, 2),
            Some(crate::app::state::SidebarSource::Local),
            "local row clickable in yielded sidebar"
        );
        assert_eq!(
            host_target_at(&app, 0, 3),
            Some(crate::app::state::SidebarSource::Remote(host)),
            "remote host row clickable in yielded sidebar"
        );
    }

    #[test]
    fn glass_sidebar_yield_collapsed_sidebar_not_yielded() {
        // When the sidebar is collapsed, glass_sidebar_yielded must be false:
        // collapsed + glass = still the compact collapsed variant, not yielded.
        let (mut app, _host) = make_glass_app();
        app.sidebar_collapsed = true;
        let area = Rect::new(0, 0, 100, 20);
        compute_view(&mut app, area);

        assert!(
            !app.view.glass_sidebar_yielded,
            "collapsed sidebar must not trigger glass_sidebar_yielded"
        );
        // Collapsed sidebar drops the rail entirely per existing policy.
        assert_eq!(
            app.view.host_rail_rect,
            Rect::default(),
            "collapsed sidebar has no host rail"
        );
    }
}
