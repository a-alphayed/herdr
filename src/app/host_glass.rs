use std::collections::BTreeMap;
use std::io;
use std::time::Instant;

use ratatui::layout::Rect;
use ratatui::Frame;

use crate::pane::{PtyFreeTerminalSurface, TerminalCursorState};
use crate::remote_source::RemoteHostKey;

const GLASS_SCROLLBACK_LIMIT_BYTES: usize = 0;
const DEFAULT_CELL_WIDTH_PX: u32 = 1;
const DEFAULT_CELL_HEIGHT_PX: u32 = 1;

/// Truthful lifecycle state for one host's glass surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // S1 defines all truthful states; stream transitions begin in S2.
pub(crate) enum GlassStatus {
    Connecting,
    Live,
    Stale { since: Instant },
}

/// Pure AppState metadata for one host glass generation. Presence in the
/// AppState map is the existence marker; runtime objects stay on `App`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HostGlassState {
    pub(crate) generation: u64,
    pub(crate) status: GlassStatus,
}

/// A local, PTY-free VT surface fed only by TerminalAnsi bytes.
///
/// This type deliberately has no pane/terminal id and is never inserted into
/// PaneRuntime or TerminalRuntimeRegistry, keeping it outside agent detection.
#[allow(dead_code)] // S1 foundation; stream ownership and UI rendering arrive in later slices.
pub(crate) struct GlassSurfaceCore {
    generation: u64,
    area: Rect,
    terminal: PtyFreeTerminalSurface,
}

#[allow(dead_code)] // S1 foundation; stream ownership and UI rendering arrive in later slices.
impl GlassSurfaceCore {
    pub(crate) fn new(area: Rect, generation: u64) -> io::Result<Self> {
        let terminal = PtyFreeTerminalSurface::new(
            area.width.max(1),
            area.height.max(1),
            GLASS_SCROLLBACK_LIMIT_BYTES,
        )?;
        Ok(Self {
            generation,
            area,
            terminal,
        })
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn area(&self) -> Rect {
        self.area
    }

    pub(crate) fn feed(&self, bytes: &[u8]) {
        self.terminal.feed(bytes);
    }

    pub(crate) fn resize(&mut self, area: Rect) -> io::Result<()> {
        self.terminal.resize(
            area.height.max(1),
            area.width.max(1),
            DEFAULT_CELL_WIDTH_PX,
            DEFAULT_CELL_HEIGHT_PX,
        )?;
        self.area = area;
        Ok(())
    }

    pub(crate) fn visible_text(&self) -> String {
        self.terminal.visible_text()
    }

    pub(crate) fn cursor_state(&self) -> Option<TerminalCursorState> {
        self.terminal.cursor_state()
    }

    /// Render through the same grid-to-ratatui seam as local pane terminals.
    /// No IO or state reconciliation occurs here.
    pub(crate) fn render(&self, frame: &mut Frame<'_>, show_cursor: bool) {
        self.terminal.render(frame, self.area, show_cursor);
    }
}

/// App-owned VT registry. The pure existence/status mirror lives separately
/// on AppState; this collection contains only local runtime surfaces.
#[derive(Default)]
#[allow(dead_code)] // S1 foundation; reconciliation starts in the stream slice.
pub(crate) struct HostGlassSurfaceRegistry {
    surfaces: BTreeMap<RemoteHostKey, GlassSurfaceCore>,
}

#[allow(dead_code)] // S1 foundation; reconciliation starts in the stream slice.
impl HostGlassSurfaceRegistry {
    pub(crate) fn insert(&mut self, host: RemoteHostKey, surface: GlassSurfaceCore) {
        self.surfaces.insert(host, surface);
    }

    pub(crate) fn get(&self, host: &RemoteHostKey) -> Option<&GlassSurfaceCore> {
        self.surfaces.get(host)
    }

    pub(crate) fn get_mut(&mut self, host: &RemoteHostKey) -> Option<&mut GlassSurfaceCore> {
        self.surfaces.get_mut(host)
    }

    pub(crate) fn remove(&mut self, host: &RemoteHostKey) -> Option<GlassSurfaceCore> {
        self.surfaces.remove(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{CellData, CursorState, FrameData};
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    fn cell(symbol: &str, fg: u32) -> CellData {
        CellData {
            symbol: symbol.to_owned(),
            fg,
            bg: 0,
            modifier: 0,
            skip: false,
            hyperlink: None,
        }
    }

    #[test]
    fn handwritten_ansi_updates_grid_cursor_style_and_resize() {
        let mut glass = GlassSurfaceCore::new(Rect::new(1, 1, 12, 4), 7)
            .expect("PTY-free glass terminal should initialize");

        glass.feed(b"\x1b[2J\x1b[H\x1b[31mRED\x1b[0m\x1b[2;1Hna\xc3\xafve \xe6\x97\xa5\xe6\x9c\xac\x1b[3;5H\x1b[?25l");

        let text = glass.visible_text();
        assert!(text
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("RED")));
        assert!(text
            .lines()
            .nth(1)
            .is_some_and(|line| line.starts_with("naïve 日本")));
        assert_eq!(
            glass.cursor_state(),
            Some(TerminalCursorState {
                x: 4,
                y: 2,
                visible: false,
                shape: 0,
            })
        );

        glass
            .resize(Rect::new(2, 1, 6, 2))
            .expect("PTY-free glass terminal should resize");
        glass.feed(b"\x1b[2J\x1b[HABC\x1b[2;1Hxy\x1b[?25h");

        assert_eq!(glass.generation(), 7);
        assert_eq!(glass.area(), Rect::new(2, 1, 6, 2));
        let resized = glass.visible_text();
        assert_eq!(resized.lines().next(), Some("ABC"));
        assert_eq!(resized.lines().nth(1), Some("xy"));
        assert_eq!(
            glass.cursor_state(),
            Some(TerminalCursorState {
                x: 2,
                y: 1,
                visible: true,
                shape: 0,
            })
        );

        let mut terminal = Terminal::new(TestBackend::new(10, 4)).expect("test terminal");
        terminal
            .draw(|frame| glass.render(frame, true))
            .expect("glass render should draw");
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(2, 1)].symbol(), "A");
        assert_eq!(buffer[(2, 2)].symbol(), "x");
        terminal.backend_mut().assert_cursor_position((4, 2));

        // Verify the earlier SGR was interpreted by the shared render seam on
        // a fresh surface, rather than merely retained as literal bytes.
        let styled = GlassSurfaceCore::new(Rect::new(0, 0, 3, 1), 8)
            .expect("styled glass terminal should initialize");
        styled.feed(b"\x1b[31mR\x1b[0m");
        let mut terminal = Terminal::new(TestBackend::new(3, 1)).expect("test terminal");
        terminal
            .draw(|frame| styled.render(frame, false))
            .expect("styled glass render should draw");
        assert_eq!(
            terminal.backend().buffer()[(0, 0)].style().fg,
            Some(Color::Indexed(1))
        );
    }

    #[test]
    fn captured_terminal_ansi_blit_dialect_round_trips_through_glass() {
        // Captured from BlitEncoder for the exact 2x1 frame below. Keeping the
        // bytes literal makes this a parser compatibility fixture rather than
        // merely feeding hand-authored ANSI through both sides of one test.
        const CAPTURED_TERMINAL_ANSI: &[u8] = b"\x1b[?25l\x1b[?2026h\x1b]8;;\x1b\\\x1b[2J\x1b[H\x1b[1;1H\x1b[0;31;49mA\x1b[1;2H\x1b[0;39;49mB\x1b[0m\x1b[1;2H\x1b[5 q\x1b[?25h\x1b[?2026l\x1b[1;2H\x1b[?25h";
        let frame = FrameData {
            cells: vec![cell("A", 2), cell("B", 0)],
            width: 2,
            height: 1,
            cursor: Some(CursorState {
                x: 1,
                y: 0,
                visible: true,
                shape: 5,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let encoded = crate::protocol::render_ansi::BlitEncoder::new().encode(&frame, false);
        assert!(encoded.full);
        assert_eq!(encoded.bytes, CAPTURED_TERMINAL_ANSI);

        let glass = GlassSurfaceCore::new(Rect::new(0, 0, 2, 1), 11)
            .expect("captured-blit glass terminal should initialize");
        glass.feed(CAPTURED_TERMINAL_ANSI);

        let text = glass.visible_text();
        assert_eq!(text.lines().next(), Some("AB"));
        assert_eq!(
            glass.cursor_state(),
            Some(TerminalCursorState {
                x: 1,
                y: 0,
                visible: true,
                shape: 5,
            })
        );
    }
}
