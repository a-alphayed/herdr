//! Text selection and clipboard support.
//!
//! Selection lifecycle:
//!
//!   MouseDown in pane → Anchor recorded (no visual yet)
//!   MouseDrag         → Selection becomes active, cells highlighted
//!   MouseUp           → Text extracted, copied via OSC 52, highlight stays
//!   Next click / key  → Selection cleared
//!
//! Double-click copy also briefly highlights the selected word.
//!
//! Rows are stored in screen-buffer coordinates instead of viewport-relative
//! coordinates. That keeps selection stable while the pane scrolls.

use ratatui::layout::Rect;
use std::{ffi::OsStr, io::Write};

use crate::{layout::PaneId, pane::ScrollMetrics};

/// Current phase of a selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Mouse is down but hasn't moved yet. If released without
    /// moving, this was just a click — no selection created.
    Anchored,
    /// Mouse has moved from the anchor point. Cells are being highlighted.
    Dragging,
    /// Mouse released after dragging. Selection is visible and text
    /// has been copied to clipboard. Cleared on next interaction.
    Done,
}

/// A text selection within a terminal pane.
#[derive(Debug, Clone)]
pub struct Selection {
    /// Which pane the selection belongs to.
    pub pane_id: PaneId,
    /// Anchor position in screen-buffer coordinates (row, col).
    anchor: (u32, u16),
    /// Current/final position in screen-buffer coordinates (row, col).
    cursor: (u32, u16),
    /// Selection phase.
    phase: Phase,
}

impl Selection {
    /// Start a potential selection. This records the anchor but doesn't
    /// make anything visible yet — the user might just be clicking.
    pub fn anchor(
        pane_id: PaneId,
        viewport_row: u16,
        col: u16,
        metrics: Option<ScrollMetrics>,
    ) -> Self {
        let anchor = (absolute_row_for_viewport_row(viewport_row, metrics), col);
        Self {
            pane_id,
            anchor,
            cursor: anchor,
            phase: Phase::Anchored,
        }
    }

    /// Create an active selection from an explicit viewport-row range.
    pub(crate) fn range(
        pane_id: PaneId,
        viewport_row: u16,
        start_col: u16,
        end_col: u16,
        metrics: Option<ScrollMetrics>,
    ) -> Self {
        let row = absolute_row_for_viewport_row(viewport_row, metrics);
        Self {
            pane_id,
            anchor: (row, start_col),
            cursor: (row, end_col),
            phase: Phase::Dragging,
        }
    }

    pub(crate) fn line_range(
        pane_id: PaneId,
        anchor_row: u32,
        cursor_row: u32,
        end_col: u16,
    ) -> Self {
        let (anchor_col, cursor_col) = if anchor_row <= cursor_row {
            (0, end_col)
        } else {
            (end_col, 0)
        };
        Self {
            pane_id,
            anchor: (anchor_row, anchor_col),
            cursor: (cursor_row, cursor_col),
            phase: Phase::Dragging,
        }
    }

    pub(crate) fn absolute_row_for_viewport(
        viewport_row: u16,
        metrics: Option<ScrollMetrics>,
    ) -> u32 {
        absolute_row_for_viewport_row(viewport_row, metrics)
    }

    /// Convert the anchor's absolute row and pane-relative column back to
    /// screen coordinates. Adds the pane origin before clamping so the
    /// returned (screen_row, screen_col) can be compared directly against
    /// mouse screen positions.
    pub fn anchor_screen_pos(
        &self,
        pane_inner: Rect,
        metrics: Option<ScrollMetrics>,
    ) -> (u16, u16) {
        let viewport_row = viewport_row_for_absolute_row(self.anchor.0, metrics);
        // Convert pane-relative to screen coordinates, then clamp.
        let row = (viewport_row.saturating_add(pane_inner.y)).clamp(
            pane_inner.y,
            pane_inner.y + pane_inner.height.saturating_sub(1),
        );
        let col = (self.anchor.1.saturating_add(pane_inner.x)).clamp(
            pane_inner.x,
            pane_inner.x + pane_inner.width.saturating_sub(1),
        );
        (row, col)
    }

    /// Extend the selection as the mouse drags. Activates highlighting
    /// once the cursor moves to a different cell than the anchor.
    /// Screen coordinates are clamped to the pane boundary.
    pub fn drag(
        &mut self,
        screen_col: u16,
        screen_row: u16,
        pane_inner: Rect,
        metrics: Option<ScrollMetrics>,
    ) {
        let (viewport_row, col) = clamp_to_pane(screen_col, screen_row, pane_inner);
        self.cursor = (absolute_row_for_viewport_row(viewport_row, metrics), col);
        if self.cursor != self.anchor {
            self.phase = Phase::Dragging;
        }
    }

    /// Finalize the selection. Returns the selected range if the user
    /// actually dragged (not just clicked). Returns None for plain clicks.
    pub fn finish(&mut self) -> bool {
        if self.phase == Phase::Dragging {
            self.phase = Phase::Done;
            true
        } else {
            false
        }
    }

    /// Whether this selection should be rendered (highlight visible).
    pub fn is_visible(&self) -> bool {
        self.phase == Phase::Dragging || self.phase == Phase::Done
    }

    /// Whether this selection was already finalized and copied.
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// Whether the user just clicked without dragging (not a selection).
    pub fn was_just_click(&self) -> bool {
        self.phase == Phase::Anchored
    }

    /// Whether the user just clicked without dragging (not a selection).
    pub fn is_just_click(&self) -> bool {
        self.phase == Phase::Anchored
    }

    /// Force the selection into Dragging phase, used when the mouse
    /// has moved off the anchor cell but drag() couldn't transition
    /// because the cursor was clamped to the same cell as the anchor.
    pub fn force_dragging(&mut self) {
        if self.phase == Phase::Anchored {
            self.phase = Phase::Dragging;
        }
    }

    /// Whether the pointer is still down and the selection can keep extending.
    pub fn is_in_progress(&self) -> bool {
        matches!(self.phase, Phase::Anchored | Phase::Dragging)
    }

    /// Whether the user is actively dragging (cursor moved from anchor).
    pub fn is_dragging(&self) -> bool {
        self.phase == Phase::Dragging
    }

    /// Returns (start, end) in reading order (top-left to bottom-right).
    fn ordered(&self) -> ((u32, u16), (u32, u16)) {
        let (ar, ac) = self.anchor;
        let (cr, cc) = self.cursor;
        if ar < cr || (ar == cr && ac <= cc) {
            ((ar, ac), (cr, cc))
        } else {
            ((cr, cc), (ar, ac))
        }
    }

    pub(crate) fn ordered_cells(&self) -> ((u32, u16), (u32, u16)) {
        self.ordered()
    }

    /// Check whether a pane-relative cell (row, col) is inside the selection.
    pub fn contains(&self, viewport_row: u16, col: u16, metrics: Option<ScrollMetrics>) -> bool {
        if !self.is_visible() {
            return false;
        }
        let row = absolute_row_for_viewport_row(viewport_row, metrics);
        let ((sr, sc), (er, ec)) = self.ordered();
        if row < sr || row > er {
            return false;
        }
        if sr == er {
            col >= sc && col <= ec
        } else if row == sr {
            col >= sc
        } else if row == er {
            col <= ec
        } else {
            true
        }
    }
}

// ---------------------------------------------------------------------------
// Projected remote-frame selection
// ---------------------------------------------------------------------------

/// A text selection over one projected remote terminal frame.
///
/// Unlike [`Selection`], coordinates are plain frame-grid (row, col) pairs
/// inside the bounded cached semantic frame the controller already renders:
/// there is no projected scrollback, viewport offset, or autoscroll. The
/// exact terminal key pins the selection to one authority
/// (host/session/workspace/terminal) so a highlight or extracted text can
/// never cross a source, space, or authority-generation boundary.
///
/// This type owns no runtime, socket, PTY, or remote state; it is pure
/// controller overlay state over the visible projected frame grid.
#[derive(Debug, Clone)]
pub(crate) struct ProjectedSelection {
    /// Exact projected terminal identity this selection belongs to.
    pub(crate) key: crate::remote_source::RemoteProjectionTerminalKey,
    /// Anchor position in frame-grid coordinates (row, col).
    anchor: (u16, u16),
    /// Current/final position in frame-grid coordinates (row, col).
    cursor: (u16, u16),
    /// Selection phase.
    phase: Phase,
}

impl ProjectedSelection {
    /// Start a potential selection at a frame-grid cell. Like
    /// [`Selection::anchor`], nothing is visible until the cursor moves to a
    /// different cell — the gesture might be a plain click.
    pub(crate) fn anchor(
        key: crate::remote_source::RemoteProjectionTerminalKey,
        row: u16,
        col: u16,
    ) -> Self {
        Self {
            key,
            anchor: (row, col),
            cursor: (row, col),
            phase: Phase::Anchored,
        }
    }

    /// Extend the selection as the mouse drags, clamped to the frame grid.
    /// Activates highlighting once the cursor leaves the anchor cell.
    pub(crate) fn drag(&mut self, row: u16, col: u16, width: u16, height: u16) {
        let clamped_row = row.min(height.saturating_sub(1));
        let clamped_col = col.min(width.saturating_sub(1));
        self.cursor = (clamped_row, clamped_col);
        if self.cursor != self.anchor {
            self.phase = Phase::Dragging;
        }
    }

    /// Finalize the selection. Returns true only when the user actually
    /// dragged (not a plain click).
    pub(crate) fn finish(&mut self) -> bool {
        if self.phase == Phase::Dragging {
            self.phase = Phase::Done;
            true
        } else {
            false
        }
    }

    /// Whether this selection should be rendered (highlight visible).
    pub(crate) fn is_visible(&self) -> bool {
        matches!(self.phase, Phase::Dragging | Phase::Done)
    }

    /// Whether this selection was finalized after a drag.
    // Test-only today: production finishes through `finish` and
    // `extract_visible`; exercised by in-module lifecycle tests.
    #[allow(dead_code)]
    pub(crate) fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// Whether the pointer went down without dragging (plain click).
    pub(crate) fn was_just_click(&self) -> bool {
        self.phase == Phase::Anchored
    }

    /// Force the selection into the dragging phase, used when the pointer
    /// left the anchor cell but clamping kept the cursor on the anchor.
    pub(crate) fn force_dragging(&mut self) {
        if self.phase == Phase::Anchored {
            self.phase = Phase::Dragging;
        }
    }

    /// Whether the pointer is still down and the selection can keep extending.
    // Test-only today: the input layer tracks gesture activity separately;
    // exercised by in-module lifecycle tests.
    #[allow(dead_code)]
    pub(crate) fn is_in_progress(&self) -> bool {
        matches!(self.phase, Phase::Anchored | Phase::Dragging)
    }

    /// Whether the user is actively dragging (cursor moved from anchor).
    // Test-only today: exercised by in-module lifecycle tests.
    #[allow(dead_code)]
    pub(crate) fn is_dragging(&self) -> bool {
        self.phase == Phase::Dragging
    }

    /// Returns (start, end) in reading order (top-left to bottom-right).
    // Only called by the test-only `contains` helper below.
    #[allow(dead_code)]
    fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        let (ar, ac) = self.anchor;
        let (cr, cc) = self.cursor;
        if ar < cr || (ar == cr && ac <= cc) {
            ((ar, ac), (cr, cc))
        } else {
            ((cr, cc), (ar, ac))
        }
    }

    /// Check whether a frame-grid cell (row, col) is inside the selection.
    // Test-only today: projected hit-testing lives in the input layer;
    // exercised by in-module containment tests.
    #[allow(dead_code)]
    pub(crate) fn contains(&self, row: u16, col: u16) -> bool {
        if !self.is_visible() {
            return false;
        }
        let ((sr, sc), (er, ec)) = self.ordered();
        if row < sr || row > er {
            return false;
        }
        if sr == er {
            col >= sc && col <= ec
        } else if row == sr {
            col >= sc
        } else if row == er {
            col <= ec
        } else {
            true
        }
    }

    /// Extract the selected visible text from the matching current frame.
    ///
    /// Returns None for a non-visible selection, a malformed frame (zero or
    /// inconsistent dimensions), out-of-bounds selection coordinates,
    /// invalid `skip` topology, or an empty result. Interior spaces and
    /// Unicode/grapheme symbols are preserved, terminal padding at selected
    /// visual-row ends is trimmed, and selected visual rows join with `\n`.
    // Test-only full-frame variant: production extraction goes through
    // `extract_visible`, clipped to the exact rendered copy grid.
    #[allow(dead_code)]
    pub(crate) fn extract(&self, frame: &crate::protocol::FrameData) -> Option<String> {
        self.extract_clipped(frame, None)
    }

    /// Extract like [`Self::extract`], but validated against the exact
    /// visible copy grid `(copy_width, copy_height)` the render loop drew, so
    /// extraction always matches the rendered highlight: the whole selection
    /// must fit inside the grid (a mid-gesture resize that moved any endpoint
    /// or row outside it fails closed), intermediate rows end at the visible
    /// edge, and a visible boundary cutting a selected wide grapheme from its
    /// required continuation fails closed — a partial wide grapheme is never
    /// copied. A selection with no visible text fails closed as empty.
    pub(crate) fn extract_visible(
        &self,
        frame: &crate::protocol::FrameData,
        copy_width: u16,
        copy_height: u16,
    ) -> Option<String> {
        self.extract_clipped(frame, Some((copy_width, copy_height)))
    }

    fn extract_clipped(
        &self,
        frame: &crate::protocol::FrameData,
        visible: Option<(u16, u16)>,
    ) -> Option<String> {
        let width = usize::from(frame.width);
        let ranges = self.validated_row_ranges(frame, visible)?;
        let mut lines = Vec::with_capacity(ranges.len());
        for (row, start, end) in ranges {
            let row_start = usize::from(row) * width;
            let row_cells = &frame.cells[row_start..row_start + width];
            let mut line = String::new();
            for col in start..=end {
                let cell = &row_cells[usize::from(col)];
                // Validated wide-character continuation: the lead cell already
                // emitted the full grapheme, so duplicates are omitted.
                if cell.skip {
                    continue;
                }
                line.push_str(&cell.symbol);
            }
            lines.push(line.trim_end().to_string());
        }
        let text = lines.join("\n");
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// Per-row highlight ranges `(row, start_col, end_col)` for the render
    /// overlay, validated against the exact frame being drawn and clipped to
    /// the exact visible copy grid `(copy_width, copy_height)` the render
    /// loop drew. Wide-grapheme endpoints are normalized onto the full
    /// displayed grapheme so valid wide tails receive the whole highlight. A
    /// None result (non-visible selection, malformed frame, invalid `skip`
    /// topology, a selection exceeding the visible grid, or a visible edge
    /// cutting a wide grapheme from its required continuation) means no
    /// highlight is painted at all — the overlay fails closed on the exact
    /// same predicate as text extraction.
    pub(crate) fn highlighted_row_ranges(
        &self,
        frame: &crate::protocol::FrameData,
        copy_width: u16,
        copy_height: u16,
    ) -> Option<Vec<(u16, u16, u16)>> {
        self.validated_row_ranges(frame, Some((copy_width, copy_height)))
    }

    /// Validated per-row selected ranges `(row, start_col, end_col)` shared
    /// by text extraction and the render highlight overlay so both readers
    /// fail closed on the exact same predicate. Returns None for a
    /// non-visible selection, a malformed frame (zero or inconsistent
    /// dimensions), out-of-bounds selection coordinates, or invalid `skip`
    /// topology. When `visible` carries the rendered copy grid
    /// `(copy_width, copy_height)`, the whole selection must fit inside it:
    /// any endpoint or row outside the grid (a mid-gesture resize) fails
    /// closed, intermediate rows end at the visible edge instead of the full
    /// frame width, and wide-grapheme normalization that would cross the
    /// visible edge fails closed rather than truncating the grapheme, so a
    /// partial wide grapheme is never highlighted or extracted. Topology is
    /// validated against the full frame before clipping, keeping both
    /// readers' fail-closed predicate identical.
    fn validated_row_ranges(
        &self,
        frame: &crate::protocol::FrameData,
        visible: Option<(u16, u16)>,
    ) -> Option<Vec<(u16, u16, u16)>> {
        if !self.is_visible() {
            return None;
        }
        let width = usize::from(frame.width);
        let height = usize::from(frame.height);
        if width == 0 || height == 0 || frame.cells.len() != width * height {
            return None;
        }
        let ((sr, sc), (er, ec)) = self.ordered();
        if usize::from(sr) >= height
            || usize::from(er) >= height
            || usize::from(sc) >= width
            || usize::from(ec) >= width
        {
            return None;
        }
        if let Some((copy_width, copy_height)) = visible {
            // Visible-clipped readers match the rendered grid exactly: the
            // whole selection, endpoints included, must fit inside the
            // current visible copy grid. A mid-gesture resize that moved any
            // endpoint or row outside it fails the whole copy/highlight
            // closed instead of silently copying a partial subset.
            if copy_width == 0
                || copy_height == 0
                || sr >= copy_height
                || er >= copy_height
                || sc >= copy_width
                || ec >= copy_width
            {
                return None;
            }
        }

        let mut ranges = Vec::with_capacity(usize::from(er - sr) + 1);
        for row in sr..=er {
            let row_start = usize::from(row) * width;
            let row_cells = &frame.cells[row_start..row_start + width];
            let raw_start = if row == sr { sc } else { 0 };
            let raw_end = if row == er {
                ec
            } else if let Some((copy_width, _)) = visible {
                // Intermediate rows end at the visible grid edge, not the
                // full frame width, so clipping can never hide a selected
                // wide continuation the render loop never drew.
                copy_width - 1
            } else {
                frame.width - 1
            };
            let (start, end) = Self::normalize_row_endpoints(row_cells, raw_start, raw_end)?;
            if let Some((copy_width, _)) = visible {
                if end >= copy_width {
                    // Normalization pulled a selected wide grapheme's
                    // required continuation past the visible edge: the exact
                    // boundary cuts the grapheme, so fail closed instead of
                    // highlighting or extracting a partial grapheme.
                    return None;
                }
            }
            ranges.push((row, start, end));
        }
        Some(ranges)
    }

    /// Validate the `skip` topology of one selected row range and normalize
    /// endpoints landing on a valid wide-character continuation back onto the
    /// full displayed grapheme: a start endpoint snaps back to the lead cell
    /// and an end endpoint snaps forward to the last continuation cell, so
    /// the whole grapheme is highlighted and extracted exactly once. An end
    /// endpoint landing on a true-wide lead cell (one whose immediate
    /// successor is marked `skip`) likewise extends forward to its required
    /// continuation. A width>1 cell followed by a `skip` continuation is
    /// validated: its exact display-width-matching run must exist and lie
    /// wholly inside the selected range. A width>1 cell with NO skipped
    /// successor is the valid single-grid-cell Ghostty metadata-disagreement
    /// shape (`CellWide::Narrow` carrying a width-two grapheme such as `⌨️`,
    /// `⚠️`, or `💳`) and is accepted as one atomic cell — the producer marks
    /// only `SpacerTail` cells as skipped, so those width-two cells carry no
    /// continuation. Returns None for orphaned, impossible, arbitrary,
    /// overlong, or row-end-cut continuation runs.
    fn normalize_row_endpoints(
        row_cells: &[crate::protocol::CellData],
        start: u16,
        end: u16,
    ) -> Option<(u16, u16)> {
        let mut start = usize::from(start);
        let mut end = usize::from(end);
        if row_cells.get(start)?.skip {
            let lead = Self::skip_run_lead(row_cells, start)?;
            Self::validate_skip_run(row_cells, lead)?;
            start = lead;
        }
        if row_cells.get(end)?.skip {
            let lead = Self::skip_run_lead(row_cells, end)?;
            end = Self::validate_skip_run(row_cells, lead)?;
        } else if let Some(run_end) = Self::validate_wide_lead(row_cells, end)? {
            // The range ends on a valid wide lead: extend to its required
            // continuation so the whole grapheme is included exactly once.
            end = run_end;
        }
        let mut col = start;
        while col <= end {
            if row_cells[col].skip {
                let lead = Self::skip_run_lead(row_cells, col)?;
                let run_end = Self::validate_skip_run(row_cells, lead)?;
                if lead < start || run_end > end {
                    return None;
                }
                col = run_end + 1;
            } else if let Some(run_end) = Self::validate_wide_lead(row_cells, col)? {
                // A wide lead without a selected continuation is only valid
                // when its full required run is normalized into the range.
                if run_end > end {
                    return None;
                }
                col = run_end + 1;
            } else {
                col += 1;
            }
        }
        Some((start as u16, end as u16))
    }

    /// Validate a non-skip cell as a potential wide-grapheme lead. Narrow or
    /// zero-width cells need no continuation (`Some(None)`).
    ///
    /// A grapheme whose Unicode display width is two or greater is a
    /// continuation-bearing true-wide lead ONLY when its immediate grid
    /// successor is marked `skip`: the producer (`ghostty_buffer_symbol_into` /
    /// the render cells loop in `src/pane/terminal.rs`) serializes a true-wide
    /// `CellWide::Wide` CJK grapheme as one lead cell plus its exact
    /// display-width-matching `SpacerTail` skip run, and that run is validated
    /// exactly as before (`Some(Some(run_end))`; a malformed, overlong, or
    /// row-end-cut run fails closed as `None`).
    ///
    /// A width-two grapheme with NO skipped successor is instead the valid
    /// Ghostty metadata-disagreement shape: `CellWide::Narrow` carrying a
    /// width-two grapheme (e.g. `⌨️`, `⚠️`, `💳`), which the producer preserves
    /// as a single non-`skip` grid cell. It occupies exactly one column and has
    /// no continuation to validate, so it is accepted as one atomic semantic
    /// grid cell (`Some(None)`) — the only representation available for that
    /// disagreement. Treating it as a missing continuation would reject every
    /// supported single-grid-cell emoji, so a width-two/no-skip cell is valid,
    /// not malformed. Row end and an ordinary next cell are both valid
    /// no-continuation successors.
    fn validate_wide_lead(
        row_cells: &[crate::protocol::CellData],
        lead: usize,
    ) -> Option<Option<usize>> {
        use unicode_width::UnicodeWidthStr;
        if row_cells.get(lead)?.symbol.width() < 2 {
            return Some(None);
        }
        // A true-wide lead carries its exact skip continuation only when the
        // immediate successor is actually marked `skip`. Any other successor —
        // row end or an ordinary cell — is the valid Ghostty
        // metadata-disagreement shape (CellWide::Narrow + width two): accept it
        // as one atomic grid cell and validate nothing further.
        match row_cells.get(lead + 1) {
            Some(next) if next.skip => Self::validate_skip_run(row_cells, lead).map(Some),
            _ => Some(None),
        }
    }

    /// Find the lead (display-width-bearing) cell of the skip run containing
    /// `col`. A run reaching column 0 without a lead cell is orphaned.
    fn skip_run_lead(row_cells: &[crate::protocol::CellData], col: usize) -> Option<usize> {
        let mut lead = col;
        while lead > 0 && row_cells[lead].skip {
            lead -= 1;
        }
        if row_cells[lead].skip {
            return None;
        }
        Some(lead)
    }

    /// Validate that the skip run after `lead` matches the lead grapheme's
    /// display width exactly, returning the run's last continuation column.
    fn validate_skip_run(row_cells: &[crate::protocol::CellData], lead: usize) -> Option<usize> {
        use unicode_width::UnicodeWidthStr;
        let display_width = row_cells[lead].symbol.width();
        if display_width < 2 {
            // A continuation cell after a narrow or zero-width grapheme is
            // impossible topology.
            return None;
        }
        let run_end = lead.checked_add(display_width - 1)?;
        if run_end >= row_cells.len() {
            // The grapheme's continuation run is cut off by the row end.
            return None;
        }
        if row_cells[lead + 1..=run_end].iter().any(|cell| !cell.skip) {
            return None;
        }
        if row_cells.get(run_end + 1).is_some_and(|cell| cell.skip) {
            // The run is longer than the grapheme's display width allows.
            return None;
        }
        Some(run_end)
    }
}

/// The exact visible copy grid of one projected frame inside its bordered
/// pane rect: the pane-interior origin plus the `min(interior, frame)`
/// copied dimensions the render loop draws. Shared by the render copy/clip
/// path and the projected-selection input bounds so highlighting, gestures,
/// and extraction always agree on exactly which frame cells are visible.
pub(crate) fn projected_visible_grid(
    rect: Rect,
    frame_width: u16,
    frame_height: u16,
) -> (u16, u16, u16, u16) {
    let inner_x = rect.x.saturating_add(1);
    let inner_y = rect.y.saturating_add(1);
    let inner_width = rect.width.saturating_sub(2);
    let inner_height = rect.height.saturating_sub(2);
    (
        inner_x,
        inner_y,
        inner_width.min(frame_width),
        inner_height.min(frame_height),
    )
}

fn viewport_top_row(metrics: Option<ScrollMetrics>) -> u32 {
    metrics
        .map(|metrics| {
            metrics
                .max_offset_from_bottom
                .saturating_sub(metrics.offset_from_bottom)
        })
        .unwrap_or(0) as u32
}

fn absolute_row_for_viewport_row(viewport_row: u16, metrics: Option<ScrollMetrics>) -> u32 {
    viewport_top_row(metrics) + u32::from(viewport_row)
}

fn viewport_row_for_absolute_row(absolute_row: u32, metrics: Option<ScrollMetrics>) -> u16 {
    absolute_row
        .saturating_sub(viewport_top_row(metrics))
        .try_into()
        .unwrap_or(0)
}

fn clamp_to_pane(screen_col: u16, screen_row: u16, pane_inner: Rect) -> (u16, u16) {
    let clamped_col = screen_col.clamp(
        pane_inner.x,
        pane_inner.x + pane_inner.width.saturating_sub(1),
    );
    let clamped_row = screen_row.clamp(
        pane_inner.y,
        pane_inner.y + pane_inner.height.saturating_sub(1),
    );
    (clamped_row - pane_inner.y, clamped_col - pane_inner.x)
}

fn osc52_sequence(bytes: &[u8]) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("\x1b]52;c;{encoded}\x07")
}

fn contains_wsl_marker(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("microsoft") || lower.contains("wsl2") || lower.contains("-wsl")
}

fn is_wsl_for_env(
    os_release: Option<&str>,
    proc_version: Option<&str>,
    wsl_distro_name: Option<&OsStr>,
    wsl_interop: Option<&OsStr>,
    runtime_marker_exists: bool,
) -> bool {
    wsl_distro_name.is_some()
        || wsl_interop.is_some()
        || os_release.is_some_and(contains_wsl_marker)
        || proc_version.is_some_and(contains_wsl_marker)
        || runtime_marker_exists
}

fn is_wsl() -> bool {
    let os_release = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok();
    let proc_version = std::fs::read_to_string("/proc/version").ok();
    is_wsl_for_env(
        os_release.as_deref(),
        proc_version.as_deref(),
        std::env::var_os("WSL_DISTRO_NAME").as_deref(),
        std::env::var_os("WSL_INTEROP").as_deref(),
        std::path::Path::new("/run/WSL").exists()
            || std::path::Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists(),
    )
}

fn should_prefer_osc52_for_env(
    ssh_connection: Option<&OsStr>,
    ssh_tty: Option<&OsStr>,
    wsl: bool,
    herdr_env: Option<&OsStr>,
) -> bool {
    ssh_connection.is_some()
        || ssh_tty.is_some()
        || wsl
        || herdr_env == Some(OsStr::new(crate::HERDR_ENV_VALUE))
}

fn should_prefer_osc52() -> bool {
    should_prefer_osc52_for_env(
        std::env::var_os("SSH_CONNECTION").as_deref(),
        std::env::var_os("SSH_TTY").as_deref(),
        is_wsl(),
        std::env::var_os(crate::HERDR_ENV_VAR).as_deref(),
    )
}

/// Write clipboard bytes to the system clipboard via native platform tools or OSC 52.
///
/// OSC 52 format: `ESC ] 52 ; c ; <base64> BEL`
///
/// Some terminals still only honor BEL-terminated OSC 52 writes, so herdr
/// emits BEL here even though ST works in newer emulators.
pub fn write_osc52_bytes(bytes: &[u8]) {
    if !should_prefer_osc52() && crate::platform::write_clipboard(bytes) {
        return;
    }

    let sequence = osc52_sequence(bytes);
    let _ = std::io::stdout().write_all(sequence.as_bytes());
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sel(sr: u32, sc: u16, er: u32, ec: u16) -> Selection {
        let mut sel = Selection::anchor(PaneId::from_raw(0), sr as u16, sc, None);
        sel.anchor = (sr, sc);
        sel.cursor = (er, ec);
        sel.phase = Phase::Dragging;
        sel
    }

    #[test]
    fn osc52_sequence_uses_bel_terminator() {
        assert_eq!(osc52_sequence(b"hello"), "\x1b]52;c;aGVsbG8=\x07");
    }

    #[test]
    fn ssh_sessions_prefer_osc52() {
        assert!(should_prefer_osc52_for_env(
            Some(OsStr::new("1 2 3 4")),
            None,
            false,
            None
        ));
        assert!(should_prefer_osc52_for_env(
            None,
            Some(OsStr::new("/dev/ttys001")),
            false,
            None
        ));
        assert!(!should_prefer_osc52_for_env(None, None, false, None));
    }

    #[test]
    fn wsl_sessions_prefer_osc52() {
        assert!(should_prefer_osc52_for_env(None, None, true, None));
    }

    #[test]
    fn nested_herdr_sessions_prefer_osc52() {
        assert!(should_prefer_osc52_for_env(
            None,
            None,
            false,
            Some(OsStr::new(crate::HERDR_ENV_VALUE))
        ));
        assert!(!should_prefer_osc52_for_env(
            None,
            None,
            false,
            Some(OsStr::new("0"))
        ));
    }

    #[test]
    fn wsl_detection_uses_env_vars() {
        assert!(is_wsl_for_env(
            None,
            None,
            Some(OsStr::new("Ubuntu")),
            None,
            false
        ));
        assert!(is_wsl_for_env(
            None,
            None,
            None,
            Some(OsStr::new("/run/WSL/123_interop")),
            false
        ));
    }

    #[test]
    fn wsl_detection_uses_kernel_markers() {
        assert!(is_wsl_for_env(
            Some("5.15.167.4-microsoft-standard-WSL2"),
            None,
            None,
            None,
            false
        ));
        assert!(is_wsl_for_env(
            None,
            Some("Linux version 5.15.167.4-microsoft-standard-WSL2"),
            None,
            None,
            false
        ));
    }

    #[test]
    fn wsl_detection_ignores_non_wsl_kernel_strings() {
        assert!(!contains_wsl_marker("notwsl-kernel"));
        assert!(!is_wsl_for_env(
            Some("6.8.0-31-generic"),
            Some("Linux version 6.8.0-31-generic"),
            None,
            None,
            false
        ));
    }

    #[test]
    fn wsl_detection_uses_wsl_runtime_markers() {
        assert!(is_wsl_for_env(None, None, None, None, true));
        assert!(!is_wsl_for_env(None, None, None, None, false));
    }

    #[test]
    fn ordering_forward() {
        let sel = make_sel(2, 5, 4, 10);
        assert_eq!(sel.ordered(), ((2, 5), (4, 10)));
    }

    #[test]
    fn ordering_backward() {
        let sel = make_sel(4, 10, 2, 5);
        assert_eq!(sel.ordered(), ((2, 5), (4, 10)));
    }

    #[test]
    fn single_line_contains() {
        let sel = make_sel(2, 5, 2, 15);
        assert!(!sel.contains(2, 4, None));
        assert!(sel.contains(2, 5, None));
        assert!(sel.contains(2, 10, None));
        assert!(sel.contains(2, 15, None));
        assert!(!sel.contains(2, 16, None));
        assert!(!sel.contains(1, 10, None));
        assert!(!sel.contains(3, 10, None));
    }

    #[test]
    fn multi_line_contains() {
        let sel = make_sel(2, 5, 4, 10);
        assert!(!sel.contains(2, 4, None));
        assert!(sel.contains(2, 5, None));
        assert!(sel.contains(2, 79, None));
        assert!(sel.contains(3, 0, None));
        assert!(sel.contains(3, 79, None));
        assert!(sel.contains(4, 0, None));
        assert!(sel.contains(4, 10, None));
        assert!(!sel.contains(4, 11, None));
    }

    #[test]
    fn anchored_not_visible() {
        let sel = Selection::anchor(PaneId::from_raw(0), 5, 10, None);
        assert!(!sel.is_visible());
        assert!(!sel.contains(5, 10, None));
    }

    #[test]
    fn click_without_drag() {
        let mut sel = Selection::anchor(PaneId::from_raw(0), 5, 10, None);
        assert!(sel.was_just_click());
        let copied = sel.finish();
        assert!(!copied);
    }

    #[test]
    fn drag_then_finish() {
        let mut sel = Selection::anchor(PaneId::from_raw(0), 5, 10, None);
        sel.drag(20, 7, Rect::new(10, 5, 80, 24), None);
        assert!(sel.is_visible());
        assert!(!sel.was_just_click());
        let copied = sel.finish();
        assert!(copied);
    }

    #[test]
    fn drag_uses_buffer_rows_when_scrolled() {
        let mut sel = Selection::anchor(
            PaneId::from_raw(0),
            0,
            10,
            Some(ScrollMetrics {
                offset_from_bottom: 1,
                max_offset_from_bottom: 10,
                viewport_rows: 4,
            }),
        );

        sel.drag(
            10,
            5,
            Rect::new(10, 5, 80, 4),
            Some(ScrollMetrics {
                offset_from_bottom: 2,
                max_offset_from_bottom: 10,
                viewport_rows: 4,
            }),
        );

        assert_eq!(sel.ordered_cells(), ((8, 0), (9, 10)));
    }

    #[test]
    fn contains_tracks_current_viewport_after_scroll() {
        let sel = make_sel(8, 2, 10, 4);
        let metrics = Some(ScrollMetrics {
            offset_from_bottom: 2,
            max_offset_from_bottom: 10,
            viewport_rows: 4,
        });

        assert!(sel.contains(0, 2, metrics));
        assert!(sel.contains(1, 40, metrics));
        assert!(sel.contains(2, 4, metrics));
        assert!(!sel.contains(3, 4, metrics));
    }

    #[test]
    fn clamp_to_pane_bounds() {
        let (row, col) = clamp_to_pane(200, 100, Rect::new(10, 5, 80, 24));
        assert_eq!(row, 23);
        assert_eq!(col, 79);

        let (row, col) = clamp_to_pane(0, 0, Rect::new(10, 5, 80, 24));
        assert_eq!(row, 0);
        assert_eq!(col, 0);
    }

    #[test]
    fn anchor_screen_pos_adds_pane_origin() {
        // Pane offset by sidebar (x=10) and tab bar (y=5).
        // Anchor at viewport_row=3, col=5 (pane-relative).
        let sel = Selection::anchor(PaneId::from_raw(0), 3, 5, None);
        let pane_inner = Rect::new(10, 5, 80, 24);
        let (row, col) = sel.anchor_screen_pos(pane_inner, None);
        // Screen row = 3 + 5 = 8, screen col = 5 + 10 = 15
        assert_eq!(row, 8);
        assert_eq!(col, 15);
    }

    #[test]
    fn anchor_screen_pos_same_cell_as_mouse_with_offset() {
        // When the pane has a non-zero origin, anchor and mouse on the same
        // screen cell must compare equal — no false drag detection.
        let pane_inner = Rect::new(10, 5, 80, 24);
        // Mouse clicked at screen (15, 8) → anchor stored as (viewport_row=3, col=5)
        let sel = Selection::anchor(PaneId::from_raw(0), 3, 5, None);
        let (ar, ac) = sel.anchor_screen_pos(pane_inner, None);
        // Screen position of the anchor must match the mouse position
        assert_eq!((ar, ac), (8, 15));
    }

    // -----------------------------------------------------------------------
    // Projected remote-frame selection
    // -----------------------------------------------------------------------

    fn projected_key() -> crate::remote_source::RemoteProjectionTerminalKey {
        crate::remote_source::RemoteProjectionTerminalKey {
            host: "remote-a".into(),
            session: "default".into(),
            workspace_id: "ws-a".into(),
            terminal_id: "term-a".into(),
        }
    }

    fn make_projected(sr: u16, sc: u16, er: u16, ec: u16) -> ProjectedSelection {
        let mut sel = ProjectedSelection::anchor(projected_key(), sr, sc);
        sel.cursor = (er, ec);
        sel.phase = Phase::Dragging;
        sel
    }

    fn projected_cell(symbol: &str, skip: bool) -> crate::protocol::CellData {
        crate::protocol::CellData {
            symbol: symbol.into(),
            fg: 0,
            bg: 0,
            modifier: 0,
            skip,
            hyperlink: None,
        }
    }

    fn projected_frame(
        width: u16,
        height: u16,
        cells: Vec<crate::protocol::CellData>,
    ) -> crate::protocol::FrameData {
        crate::protocol::FrameData {
            cells,
            width,
            height,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        }
    }

    /// Build a frame from exact-width ASCII rows (padding included in the
    /// literals so interior/trailing spaces stay visible in the test).
    fn projected_text_frame(width: u16, rows: &[&str]) -> crate::protocol::FrameData {
        let mut cells = Vec::new();
        for row in rows {
            assert_eq!(row.chars().count(), usize::from(width));
            for ch in row.chars() {
                cells.push(projected_cell(&ch.to_string(), false));
            }
        }
        projected_frame(width, rows.len() as u16, cells)
    }

    #[test]
    fn projected_selection_key_records_exact_terminal_identity() {
        let sel = ProjectedSelection::anchor(projected_key(), 0, 0);
        assert_eq!(sel.key, projected_key());
        let mut other = projected_key();
        other.terminal_id = "term-b".into();
        assert_ne!(sel.key, other);
    }

    #[test]
    fn projected_selection_contains_single_row_forward_and_backward() {
        let forward = make_projected(2, 5, 2, 15);
        assert!(!forward.contains(2, 4));
        assert!(forward.contains(2, 5));
        assert!(forward.contains(2, 15));
        assert!(!forward.contains(2, 16));
        assert!(!forward.contains(1, 10));

        let backward = make_projected(2, 15, 2, 5);
        assert!(!backward.contains(2, 4));
        assert!(backward.contains(2, 5));
        assert!(backward.contains(2, 15));
        assert!(!backward.contains(2, 16));
    }

    #[test]
    fn projected_selection_contains_multi_row() {
        let sel = make_projected(2, 5, 4, 10);
        assert!(!sel.contains(2, 4));
        assert!(sel.contains(2, 5));
        assert!(sel.contains(2, 79));
        assert!(sel.contains(3, 0));
        assert!(sel.contains(3, 79));
        assert!(sel.contains(4, 0));
        assert!(sel.contains(4, 10));
        assert!(!sel.contains(4, 11));

        let backward = make_projected(4, 10, 2, 5);
        assert!(backward.contains(2, 5));
        assert!(backward.contains(3, 40));
        assert!(backward.contains(4, 10));
        assert!(!backward.contains(4, 11));
    }

    #[test]
    fn projected_selection_anchored_is_not_visible_and_extracts_nothing() {
        let sel = ProjectedSelection::anchor(projected_key(), 1, 1);
        assert!(!sel.is_visible());
        assert!(sel.was_just_click());
        assert!(!sel.contains(1, 1));
        let frame = projected_text_frame(5, &["hello"]);
        assert_eq!(sel.extract(&frame), None);
    }

    #[test]
    fn projected_selection_lifecycle_click_drag_finish() {
        let mut sel = ProjectedSelection::anchor(projected_key(), 1, 1);
        assert!(sel.is_in_progress());
        assert!(!sel.finish(), "plain click must not finalize");

        sel.drag(3, 4, 80, 24);
        assert!(sel.is_dragging());
        assert!(sel.is_visible());
        assert!(sel.finish());
        assert!(sel.is_done());
        assert!(!sel.is_in_progress());
    }

    #[test]
    fn projected_selection_force_dragging_activates_clamped_anchor() {
        let mut sel = ProjectedSelection::anchor(projected_key(), 0, 0);
        // Pointer moved off-screen; clamping keeps the cursor on the anchor.
        sel.drag(0, 0, 80, 24);
        assert!(sel.was_just_click());
        sel.force_dragging();
        assert!(sel.is_dragging());
        assert!(sel.is_visible());
    }

    #[test]
    fn projected_selection_drag_clamps_to_frame_grid() {
        let mut sel = ProjectedSelection::anchor(projected_key(), 0, 0);
        sel.drag(100, 200, 80, 24);
        assert_eq!(sel.cursor, (23, 79));
        assert!(sel.contains(23, 79));
        assert!(!sel.is_done());
    }

    #[test]
    fn projected_selection_extract_single_row_forward_and_backward() {
        let frame = projected_text_frame(5, &["hello"]);
        let forward = make_projected(0, 1, 0, 3);
        assert_eq!(forward.extract(&frame).as_deref(), Some("ell"));
        let backward = make_projected(0, 3, 0, 1);
        assert_eq!(backward.extract(&frame).as_deref(), Some("ell"));
    }

    #[test]
    fn projected_selection_extract_preserves_interior_and_leading_spaces() {
        let frame = projected_text_frame(8, &[" a b  c "]);
        let sel = make_projected(0, 0, 0, 7);
        // Trailing terminal padding is trimmed; leading/interior spaces stay.
        assert_eq!(sel.extract(&frame).as_deref(), Some(" a b  c"));
    }

    #[test]
    fn projected_selection_extract_multi_row_joins_and_trims_rows() {
        let frame = projected_text_frame(5, &["ab   ", "cd e ", "ef   "]);
        let full = make_projected(0, 0, 2, 4);
        assert_eq!(full.extract(&frame).as_deref(), Some("ab\ncd e\nef"));

        // Partial first/last rows with full middle row.
        let partial = make_projected(0, 1, 2, 0);
        assert_eq!(partial.extract(&frame).as_deref(), Some("b\ncd e\ne"));
    }

    #[test]
    fn projected_selection_extract_empty_selection_returns_none() {
        let frame = projected_text_frame(5, &["     ", "     "]);
        let sel = make_projected(0, 0, 1, 4);
        assert_eq!(sel.extract(&frame), None);
    }

    #[test]
    fn projected_selection_extract_preserves_unicode_graphemes() {
        let frame = projected_text_frame(6, &["héllo☺"]);
        let sel = make_projected(0, 0, 0, 5);
        assert_eq!(sel.extract(&frame).as_deref(), Some("héllo☺"));
    }

    #[test]
    fn projected_selection_extract_omits_wide_continuation_duplicates() {
        // [a][好][skip][b]: the wide grapheme must appear exactly once.
        let frame = projected_frame(
            4,
            1,
            vec![
                projected_cell("a", false),
                projected_cell("好", false),
                projected_cell("", true),
                projected_cell("b", false),
            ],
        );
        let sel = make_projected(0, 0, 0, 3);
        assert_eq!(sel.extract(&frame).as_deref(), Some("a好b"));
    }

    #[test]
    fn projected_selection_extract_normalizes_start_on_valid_wide_tail() {
        let frame = projected_frame(
            4,
            1,
            vec![
                projected_cell("a", false),
                projected_cell("好", false),
                projected_cell("", true),
                projected_cell("b", false),
            ],
        );
        // Start lands on the continuation cell: snap back to the lead so the
        // whole displayed grapheme is included.
        let sel = make_projected(0, 2, 0, 3);
        assert_eq!(sel.extract(&frame).as_deref(), Some("好b"));
    }

    #[test]
    fn projected_selection_extract_normalizes_end_on_valid_wide_tail() {
        let frame = projected_frame(
            4,
            1,
            vec![
                projected_cell("a", false),
                projected_cell("好", false),
                projected_cell("", true),
                projected_cell("b", false),
            ],
        );
        // End lands on the continuation cell: the grapheme is extracted once,
        // never duplicated from its continuation.
        let sel = make_projected(0, 0, 0, 2);
        assert_eq!(sel.extract(&frame).as_deref(), Some("a好"));
    }

    #[test]
    fn projected_selection_extract_rejects_orphaned_skip_run() {
        // Skip at column 0 has no preceding display-width-bearing grapheme.
        let frame = projected_frame(
            2,
            1,
            vec![projected_cell("", true), projected_cell("a", false)],
        );
        let sel = make_projected(0, 0, 0, 1);
        assert_eq!(sel.extract(&frame), None);
    }

    #[test]
    fn projected_selection_extract_rejects_skip_after_narrow_grapheme() {
        // A continuation cell after a width-1 grapheme is impossible topology.
        let frame = projected_frame(
            2,
            1,
            vec![projected_cell("a", false), projected_cell("", true)],
        );
        let sel = make_projected(0, 0, 0, 1);
        assert_eq!(sel.extract(&frame), None);
    }

    #[test]
    fn projected_selection_extract_rejects_overlong_skip_run() {
        // "好" is width 2, so exactly one continuation cell is valid.
        let frame = projected_frame(
            3,
            1,
            vec![
                projected_cell("好", false),
                projected_cell("", true),
                projected_cell("", true),
            ],
        );
        let sel = make_projected(0, 0, 0, 2);
        assert_eq!(sel.extract(&frame), None);
    }

    #[test]
    fn projected_selection_extract_rejects_skip_run_cut_by_row_end() {
        // Width-3 grapheme needs two continuations; the row has room for one.
        let frame = projected_frame(
            2,
            1,
            vec![projected_cell("好a", false), projected_cell("", true)],
        );
        let sel = make_projected(0, 0, 0, 1);
        assert_eq!(sel.extract(&frame), None);
    }

    #[test]
    fn projected_selection_extract_rejects_non_skip_cell_inside_run() {
        let frame = projected_frame(
            3,
            1,
            vec![
                projected_cell("好", false),
                projected_cell("x", false),
                projected_cell("", true),
            ],
        );
        let sel = make_projected(0, 0, 0, 2);
        assert_eq!(sel.extract(&frame), None);
    }

    #[test]
    fn projected_selection_extract_accepts_valid_wide_run_inside_selection() {
        // Full-width selection crossing a valid wide run in the middle.
        let frame = projected_frame(
            5,
            1,
            vec![
                projected_cell("a", false),
                projected_cell("b", false),
                projected_cell("好", false),
                projected_cell("", true),
                projected_cell("c", false),
            ],
        );
        let sel = make_projected(0, 0, 0, 4);
        assert_eq!(sel.extract(&frame).as_deref(), Some("ab好c"));
    }

    #[test]
    fn projected_selection_extract_accepts_width_two_no_skip_lead() {
        // A width-two symbol with NO skipped successor is the valid Ghostty
        // metadata-disagreement shape (CellWide::Narrow carrying a width-two
        // grapheme): it occupies one grid cell and needs no continuation, so it
        // is accepted and extracted as one atomic cell, not rejected as
        // malformed. (The producer marks only SpacerTail cells as skipped, so
        // these cells legitimately carry no skip; see src/pane/terminal.rs.)
        let frame = projected_frame(
            3,
            1,
            vec![
                projected_cell("a", false),
                projected_cell("好", false),
                projected_cell("b", false),
            ],
        );
        let sel = make_projected(0, 0, 0, 2);
        assert_eq!(sel.extract(&frame).as_deref(), Some("a好b"));

        // The width-two/no-skip cell as the very first selected cell is
        // accepted the same way.
        let lead_first = projected_frame(
            2,
            1,
            vec![projected_cell("好", false), projected_cell("b", false)],
        );
        let sel = make_projected(0, 0, 0, 1);
        assert_eq!(sel.extract(&lead_first).as_deref(), Some("好b"));
    }

    #[test]
    fn projected_selection_extract_accepts_width_two_no_skip_at_row_end() {
        // A width-two/no-skip symbol in the last column is a valid atomic
        // cell at row end — its single grid cell is not cut off by the edge
        // because there is no continuation to truncate.
        let frame = projected_frame(
            2,
            1,
            vec![projected_cell("a", false), projected_cell("好", false)],
        );
        let sel = make_projected(0, 0, 0, 1);
        assert_eq!(sel.extract(&frame).as_deref(), Some("a好"));
        assert_eq!(
            sel.highlighted_row_ranges(&frame, 2, 1),
            Some(vec![(0, 0, 1)]),
            "the width-two/no-skip cell occupies exactly one grid column"
        );
    }

    #[test]
    fn projected_selection_extract_end_on_wide_lead_includes_tail_exactly_once() {
        let frame = projected_frame(
            4,
            1,
            vec![
                projected_cell("a", false),
                projected_cell("好", false),
                projected_cell("", true),
                projected_cell("b", false),
            ],
        );
        // The range ends on the wide lead itself: normalize forward to its
        // required continuation so the whole grapheme is highlighted and
        // extracted exactly once, never left half-selected.
        let sel = make_projected(0, 0, 0, 1);
        assert_eq!(sel.extract(&frame).as_deref(), Some("a好"));
        assert_eq!(
            sel.highlighted_row_ranges(&frame, 4, 1),
            Some(vec![(0, 0, 2)]),
            "the highlight must cover the lead and its continuation"
        );
    }

    #[test]
    fn projected_selection_extract_accepts_width_two_no_skip_inside_selection() {
        // A width-two/no-skip cell in the interior of a wider selection is a
        // valid atomic cell: the whole range is extracted once, never failed
        // closed as malformed.
        let frame = projected_frame(
            5,
            1,
            vec![
                projected_cell("a", false),
                projected_cell("b", false),
                projected_cell("好", false),
                projected_cell("c", false),
                projected_cell("d", false),
            ],
        );
        let sel = make_projected(0, 0, 0, 4);
        assert_eq!(sel.extract(&frame).as_deref(), Some("ab好cd"));
        assert_eq!(
            sel.highlighted_row_ranges(&frame, 5, 1),
            Some(vec![(0, 0, 4)]),
            "the width-two/no-skip cell occupies exactly one grid column"
        );
    }

    #[test]
    fn projected_selection_extracts_metadata_disagreement_emoji_as_one_cell() {
        // Ghostty classifies ⌨️/⚠️/💳 as CellWide::Narrow while their Unicode
        // width is two. The producer (src/pane/terminal.rs::
        // ghostty_buffer_symbol_into) preserves each as a single non-`skip`
        // grid cell occupying one column — the same shape its
        // ghostty_normalize_buffer_symbol contract keeps for Narrow+width2.
        // Selection must treat each as exactly one atomic grid cell,
        // highlighted and extracted once. (This fixture states the producer's
        // emitted cell shape directly; it does not assume any particular
        // libghostty/platform/version classifies a given emoji as Narrow.)
        //
        // [a][⌨️][b][⚠️][💳] at width 5: every emoji is one grid column.
        let frame = projected_frame(
            5,
            1,
            vec![
                projected_cell("a", false),
                projected_cell("⌨️", false),
                projected_cell("b", false),
                projected_cell("⚠️", false),
                projected_cell("💳", false),
            ],
        );
        let sel = make_projected(0, 0, 0, 4);
        assert_eq!(sel.extract(&frame).as_deref(), Some("a⌨️b⚠️💳"));
        // Each width-two/no-skip emoji occupies exactly one grid column: the
        // highlight covers columns 0..=4 and is never extended by a
        // continuation (none exists).
        assert_eq!(
            sel.highlighted_row_ranges(&frame, 5, 1),
            Some(vec![(0, 0, 4)])
        );

        // Ending a selection on a width-two/no-skip emoji extracts it once
        // and is NOT extended forward.
        let end_on_emoji = make_projected(0, 0, 0, 1);
        assert_eq!(end_on_emoji.extract(&frame).as_deref(), Some("a⌨️"));
        assert_eq!(
            end_on_emoji.highlighted_row_ranges(&frame, 5, 1),
            Some(vec![(0, 0, 1)])
        );

        // Starting a selection on a width-two/no-skip emoji keeps it once.
        let start_on_emoji = make_projected(0, 1, 0, 2);
        assert_eq!(start_on_emoji.extract(&frame).as_deref(), Some("⌨️b"));
        assert_eq!(
            start_on_emoji.highlighted_row_ranges(&frame, 5, 1),
            Some(vec![(0, 1, 2)])
        );
    }

    #[test]
    fn projected_selection_extracts_metadata_disagreement_emoji_at_row_end_and_preserves_spaces() {
        // A width-two/no-skip emoji at the last column is one valid atomic
        // cell (not cut off by the row end), and interior spaces and ordinary
        // neighbors are preserved around it.
        //
        // [ ][x][⚠️][ ][💳] at width 5: the space before ⚠️ and between ⚠️ and
        // 💳 are interior and preserved; trailing padding is trimmed.
        let frame = projected_frame(
            5,
            1,
            vec![
                projected_cell(" ", false),
                projected_cell("x", false),
                projected_cell("⚠️", false),
                projected_cell(" ", false),
                projected_cell("💳", false),
            ],
        );
        let sel = make_projected(0, 0, 0, 4);
        assert_eq!(sel.extract(&frame).as_deref(), Some(" x⚠️ 💳"));
        assert_eq!(
            sel.highlighted_row_ranges(&frame, 5, 1),
            Some(vec![(0, 0, 4)])
        );

        // Same emoji shape ending the row (last column, no successor): still
        // one atomic cell, highlighted and extracted once.
        let row_end = projected_frame(
            3,
            1,
            vec![
                projected_cell("a", false),
                projected_cell("b", false),
                projected_cell("⌨️", false),
            ],
        );
        let end_sel = make_projected(0, 0, 0, 2);
        assert_eq!(end_sel.extract(&row_end).as_deref(), Some("ab⌨️"));
        assert_eq!(
            end_sel.highlighted_row_ranges(&row_end, 3, 1),
            Some(vec![(0, 0, 2)])
        );
    }

    #[test]
    fn projected_visible_grid_matches_rendered_copy_bounds() {
        let rect = Rect::new(30, 2, 22, 8);
        // Smaller frame than the 20x6 pane interior: only the frame's cells.
        assert_eq!(projected_visible_grid(rect, 8, 3), (31, 3, 8, 3));
        // Larger frame clipped by the pane interior: only the interior.
        assert_eq!(projected_visible_grid(rect, 30, 10), (31, 3, 20, 6));
        // Exact fit.
        assert_eq!(projected_visible_grid(rect, 20, 6), (31, 3, 20, 6));
        // Degenerate rects and frames copy nothing.
        assert_eq!(
            projected_visible_grid(Rect::new(0, 0, 1, 1), 8, 3),
            (1, 1, 0, 0)
        );
        assert_eq!(projected_visible_grid(rect, 0, 0), (31, 3, 0, 0));
    }

    #[test]
    fn projected_selection_extract_rejects_malformed_frame_dimensions() {
        let sel = make_projected(0, 0, 0, 1);
        // Cell count does not match width * height.
        let malformed = projected_frame(3, 2, vec![projected_cell("a", false)]);
        assert_eq!(sel.extract(&malformed), None);
        // Zero dimensions carry no selectable text.
        let empty = projected_frame(0, 0, Vec::new());
        assert_eq!(sel.extract(&empty), None);
    }

    #[test]
    fn projected_selection_extract_rejects_out_of_bounds_coordinates() {
        let frame = projected_text_frame(5, &["hello", "world"]);
        let row_oob = make_projected(0, 0, 5, 2);
        assert_eq!(row_oob.extract(&frame), None);
        let col_oob = make_projected(0, 0, 0, 9);
        assert_eq!(col_oob.extract(&frame), None);
    }

    #[test]
    fn projected_selection_visible_boundary_cutting_wide_grapheme_fails_closed() {
        // [a][好][skip]: the wide grapheme needs columns 1..=2.
        let frame = projected_frame(
            3,
            1,
            vec![
                projected_cell("a", false),
                projected_cell("好", false),
                projected_cell("", true),
            ],
        );
        let sel = make_projected(0, 0, 0, 1);
        // A two-cell visible grid cuts the required continuation at column
        // 2: no extraction and no highlight — never a partial wide grapheme.
        assert_eq!(sel.extract_visible(&frame, 2, 1), None);
        assert_eq!(sel.highlighted_row_ranges(&frame, 2, 1), None);
        // A three-cell visible grid covers the whole grapheme: accepted and
        // extracted exactly once, highlighted across lead + continuation.
        assert_eq!(sel.extract_visible(&frame, 3, 1).as_deref(), Some("a好"));
        assert_eq!(
            sel.highlighted_row_ranges(&frame, 3, 1),
            Some(vec![(0, 0, 2)])
        );
    }

    #[test]
    fn projected_selection_visible_intermediate_row_wide_cut_fails_closed() {
        // Row 1 is [a][好][skip][b]; a two-column visible grid cuts the wide
        // grapheme's required continuation on an intermediate selected row.
        let frame = projected_frame(
            4,
            2,
            vec![
                projected_cell("a", false),
                projected_cell("b", false),
                projected_cell("c", false),
                projected_cell("d", false),
                projected_cell("a", false),
                projected_cell("好", false),
                projected_cell("", true),
                projected_cell("b", false),
            ],
        );
        let sel = make_projected(0, 0, 1, 1);
        assert_eq!(sel.extract_visible(&frame, 2, 2), None);
        assert_eq!(sel.highlighted_row_ranges(&frame, 2, 2), None);
        // The full-width grid keeps the whole grapheme, extracted once.
        assert_eq!(
            sel.extract_visible(&frame, 4, 2).as_deref(),
            Some("abcd\na好")
        );
        assert_eq!(
            sel.highlighted_row_ranges(&frame, 4, 2),
            Some(vec![(0, 0, 3), (1, 0, 2)])
        );
    }

    #[test]
    fn projected_selection_visible_endpoint_outside_grid_fails_closed() {
        let frame = projected_text_frame(4, &["ab  ", "cd  "]);
        let sel = make_projected(0, 0, 1, 3);
        // A mid-gesture shrink that moves any selected endpoint or row
        // outside the current visible grid fails the whole copy — never a
        // silently clipped partial subset.
        assert_eq!(sel.extract_visible(&frame, 2, 2), None);
        assert_eq!(sel.extract_visible(&frame, 4, 1), None);
        assert_eq!(sel.highlighted_row_ranges(&frame, 2, 2), None);
        assert_eq!(sel.highlighted_row_ranges(&frame, 4, 1), None);
        // Empty visible grids carry no selectable text either.
        assert_eq!(sel.extract_visible(&frame, 0, 2), None);
        assert_eq!(sel.extract_visible(&frame, 2, 0), None);
        // The same selection fully inside the grid still extracts.
        assert_eq!(sel.extract_visible(&frame, 4, 2).as_deref(), Some("ab\ncd"));
        assert_eq!(
            sel.highlighted_row_ranges(&frame, 4, 2),
            Some(vec![(0, 0, 3), (1, 0, 3)])
        );
    }

    #[test]
    fn projected_selection_visible_intermediate_rows_stop_at_grid_edge() {
        // Intermediate rows end at the visible grid edge, never the full
        // frame width: column 3 of row 0 is neither copied nor highlighted.
        let frame = projected_text_frame(4, &["abcd", "efgh"]);
        let sel = make_projected(0, 0, 1, 1);
        assert_eq!(
            sel.extract_visible(&frame, 3, 2).as_deref(),
            Some("abc\nef")
        );
        assert_eq!(
            sel.highlighted_row_ranges(&frame, 3, 2),
            Some(vec![(0, 0, 2), (1, 0, 1)])
        );
    }
}
