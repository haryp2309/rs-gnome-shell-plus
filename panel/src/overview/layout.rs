//! Geometry: where everything in the overview goes, and GNOME Shell's
//! window-picker layout (js/ui/workspace.js, UnalignedLayoutStrategy and
//! WorkspaceLayout._createBestLayout).

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn lerp(self, to: Rect, t: f64) -> Rect {
        Rect::new(
            lerp(self.x, to.x, t),
            lerp(self.y, to.y, t),
            lerp(self.w, to.w, t),
            lerp(self.h, to.h, t),
        )
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn inset(&self, dx: f64, dy: f64) -> Rect {
        Rect::new(self.x + dx, self.y + dy, (self.w - 2.0 * dx).max(0.0), (self.h - 2.0 * dy).max(0.0))
    }

    /// Scales around the center.
    pub fn scale_centered(&self, s: f64) -> Rect {
        let (cx, cy) = self.center();
        Rect::new(cx - self.w * s / 2.0, cy - self.h * s / 2.0, self.w * s, self.h * s)
    }

    pub fn translate(&self, dx: f64, dy: f64) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Maps `self`, given in the coordinate space `from`, into `to`.
    pub fn map(&self, from: &Rect, to: &Rect) -> Rect {
        let sx = to.w / from.w.max(1.0);
        let sy = to.h / from.h.max(1.0);
        Rect::new(
            to.x + (self.x - from.x) * sx,
            to.y + (self.y - from.y) * sy,
            self.w * sx,
            self.h * sy,
        )
    }

    pub fn graphene(&self) -> gtk::graphene::Rect {
        gtk::graphene::Rect::new(self.x as f32, self.y as f32, self.w as f32, self.h as f32)
    }
}

pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

// --- Overview chrome ------------------------------------------------------

/// Height of the top bar, which stays over the overview.
pub const PANEL_HEIGHT: f64 = 32.0;
/// Vertical gap between the overview's rows (search, thumbnails, view, dash).
pub const SPACING: f64 = 12.0;
pub const SEARCH_WIDTH: f64 = 320.0;
pub const SEARCH_HEIGHT: f64 = 36.0;
/// workspaceThumbnail.js MAX_THUMBNAIL_SCALE.
pub const THUMBNAIL_SCALE: f64 = 0.05;
pub const THUMBNAIL_SPACING: f64 = 8.0;
/// Gap between workspace cards in the window picker.
pub const WORKSPACE_SPACING: f64 = 32.0;
/// workspacesView.js WORKSPACE_INACTIVE_SCALE.
pub const INACTIVE_SCALE: f64 = 0.94;
/// overviewControls.js SMALL_WORKSPACE_RATIO (app grid).
pub const SMALL_WORKSPACE_RATIO: f64 = 0.15;
pub const DASH_BOTTOM_MARGIN: f64 = 8.0;
/// Rounded corners of a workspace card in the window picker.
pub const CARD_RADIUS: f64 = 20.0;

/// The fixed boxes of the overview for a monitor of `w` x `h` with a dash
/// of `dash` size (natural width, height).
#[derive(Clone, Copy, Debug)]
pub struct Boxes {
    pub screen: Rect,
    pub search: Rect,
    /// The thumbnails strip (its height; thumbnails are centered in it).
    pub thumbnails: Rect,
    /// Where the workspace card goes in the window picker.
    pub picker: Rect,
    /// The small workspace strip above the app grid.
    pub strip: Rect,
    /// App grid / search results.
    pub grid: Rect,
    pub results: Rect,
    pub dash: Rect,
}

impl Boxes {
    pub fn compute(w: f64, h: f64, dash: (f64, f64), n_workspaces: usize) -> Self {
        let screen = Rect::new(0.0, 0.0, w, h);
        let search = Rect::new(
            ((w - SEARCH_WIDTH) / 2.0).round(),
            PANEL_HEIGHT + SPACING,
            SEARCH_WIDTH,
            SEARCH_HEIGHT,
        );
        let below_search = search.y + search.h + SPACING;

        let dash_w = dash.0.min(w - 2.0 * SPACING);
        let dash_rect = Rect::new(
            ((w - dash_w) / 2.0).round(),
            h - dash.1 - DASH_BOTTOM_MARGIN,
            dash_w,
            dash.1,
        );
        let above_dash = dash_rect.y - SPACING;

        // GNOME hides the strip with a single workspace.
        let thumb_h = if n_workspaces > 1 { (h * THUMBNAIL_SCALE).round() } else { 0.0 };
        let thumbnails = Rect::new(0.0, below_search, w, thumb_h);
        let view_top = below_search + if thumb_h > 0.0 { thumb_h + SPACING } else { 0.0 };
        let picker = fit_aspect(&Rect::new(0.0, view_top, w, (above_dash - view_top).max(1.0)), w / h);

        let strip_h = ((above_dash - below_search) * SMALL_WORKSPACE_RATIO).round();
        let strip = fit_aspect(&Rect::new(0.0, below_search, w, strip_h), w / h);
        let grid_top = strip.y + strip.h + SPACING;
        let grid = Rect::new(0.0, grid_top, w, (above_dash - grid_top).max(1.0));
        let results = Rect::new(0.0, below_search, w, (above_dash - below_search).max(1.0));

        Self {
            screen,
            search,
            thumbnails,
            picker,
            strip,
            grid,
            results,
            dash: dash_rect,
        }
    }

    /// The thumbnail of workspace `i` of `n`.
    pub fn thumbnail(&self, i: usize, n: usize) -> Rect {
        let th = self.thumbnails.h;
        let tw = (th * self.screen.w / self.screen.h).round();
        let total = n as f64 * tw + (n.saturating_sub(1)) as f64 * THUMBNAIL_SPACING;
        let x0 = ((self.screen.w - total) / 2.0).round();
        Rect::new(x0 + i as f64 * (tw + THUMBNAIL_SPACING), self.thumbnails.y, tw, th)
    }
}

/// The largest rect with the given aspect ratio centered in `area`.
pub fn fit_aspect(area: &Rect, aspect: f64) -> Rect {
    let (mut w, mut h) = (area.w, area.w / aspect);
    if h > area.h {
        h = area.h;
        w = h * aspect;
    }
    Rect::new(
        (area.x + (area.w - w) / 2.0).round(),
        (area.y + (area.h - h) / 2.0).round(),
        w.round(),
        h.round(),
    )
}

// --- Window picker layout -------------------------------------------------

/// Space around and between window previews, leaving room for each
/// preview's chrome (close button above, icon and title below).
pub const PICKER_PADDING_X: f64 = 24.0;
pub const PICKER_PADDING_TOP: f64 = 24.0;
pub const PICKER_PADDING_BOTTOM: f64 = 32.0;
pub const COLUMN_SPACING: f64 = 36.0;
pub const ROW_SPACING: f64 = 46.0;
/// workspace.js WINDOW_PREVIEW_MAXIMUM_SCALE.
const MAX_SCALE: f64 = 0.95;
const LAYOUT_SCALE_WEIGHT: f64 = 1.0;
const LAYOUT_SPACE_WEIGHT: f64 = 0.1;

struct Row {
    windows: Vec<usize>,
    full_width: f64,
    full_height: f64,
    width: f64,
    height: f64,
    x: f64,
    y: f64,
    additional_scale: f64,
}

struct Layout {
    rows: Vec<Row>,
    max_columns: usize,
    grid_width: f64,
    grid_height: f64,
    scale: f64,
}

/// Bigger previews for small windows (UnalignedLayoutStrategy._computeWindowScale).
fn window_scale(rect: &Rect, monitor_h: f64) -> f64 {
    let ratio = rect.h / monitor_h;
    lerp(1.5, 1.0, ratio)
}

fn compute_layout(windows: &[Rect], num_rows: usize, monitor_h: f64) -> Layout {
    let total_width: f64 = windows.iter().map(|r| r.w * window_scale(r, monitor_h)).sum();
    let ideal_row_width = total_width / num_rows as f64;

    let mut sorted: Vec<usize> = (0..windows.len()).collect();
    sorted.sort_by(|&a, &b| windows[a].center().1.total_cmp(&windows[b].center().1));

    let mut rows = Vec::new();
    let mut index = 0;
    for r in 0..num_rows {
        let mut row = Row {
            windows: Vec::new(),
            full_width: 0.0,
            full_height: 0.0,
            width: 0.0,
            height: 0.0,
            x: 0.0,
            y: 0.0,
            additional_scale: 1.0,
        };
        while index < sorted.len() {
            let rect = &windows[sorted[index]];
            let s = window_scale(rect, monitor_h);
            let (width, height) = (rect.w * s, rect.h * s);
            row.full_height = row.full_height.max(height);

            // Either the new window fits, or adding it brings the row
            // closer to the ideal width (_keepSameRow); the last row takes
            // the rest.
            let keep = row.full_width + width <= ideal_row_width || {
                let old = row.full_width / ideal_row_width;
                let new = (row.full_width + width) / ideal_row_width;
                (1.0 - new).abs() < (1.0 - old).abs()
            };
            if keep || r == num_rows - 1 {
                row.windows.push(sorted[index]);
                row.full_width += width;
                index += 1;
            } else {
                break;
            }
        }
        rows.push(row);
    }

    let mut grid_width: f64 = 0.0;
    let mut grid_height = 0.0;
    let mut max_columns = 0;
    for row in &mut rows {
        row.windows.sort_by(|&a, &b| windows[a].center().0.total_cmp(&windows[b].center().0));
        max_columns = max_columns.max(row.windows.len());
        grid_width = grid_width.max(row.full_width);
        grid_height += row.full_height;
    }
    Layout {
        rows,
        max_columns,
        grid_width,
        grid_height,
        scale: 1.0,
    }
}

fn scale_and_space(layout: &mut Layout, area: &Rect) -> (f64, f64) {
    let hspacing = layout.max_columns.saturating_sub(1) as f64 * COLUMN_SPACING;
    let vspacing = layout.rows.len().saturating_sub(1) as f64 * ROW_SPACING;
    let spaced_w = area.w - hspacing;
    let spaced_h = area.h - vspacing;
    let scale = (spaced_w / layout.grid_width)
        .min(spaced_h / layout.grid_height)
        .min(MAX_SCALE)
        .max(0.01);
    let scaled_w = layout.grid_width * scale + hspacing;
    let scaled_h = layout.grid_height * scale + vspacing;
    let space = (scaled_w * scaled_h) / (area.w * area.h);
    layout.scale = scale;
    (scale, space)
}

fn is_better(old_scale: f64, old_space: f64, scale: f64, space: f64) -> bool {
    let space_power = (space - old_space) * LAYOUT_SPACE_WEIGHT;
    let scale_power = (scale - old_scale) * LAYOUT_SCALE_WEIGHT;
    if scale > old_scale && space > old_space {
        true
    } else if scale < old_scale && space < old_space {
        false
    } else {
        scale_power + space_power > 0.0
    }
}

fn window_slots(layout: &mut Layout, windows: &[Rect], area: &Rect, monitor_h: f64) -> Vec<Rect> {
    for row in &mut layout.rows {
        row.width = row.full_width * layout.scale + row.windows.len().saturating_sub(1) as f64 * COLUMN_SPACING;
        row.height = row.full_height * layout.scale;
    }
    let height_without_spacing: f64 = layout.rows.iter().map(|r| r.height).sum();
    let vertical_spacing = layout.rows.len().saturating_sub(1) as f64 * ROW_SPACING;
    let additional_vscale = ((area.h - vertical_spacing) / height_without_spacing).min(1.0);

    let mut compensation = 0.0;
    let mut y = 0.0;
    for row in &mut layout.rows {
        let hspacing = row.windows.len().saturating_sub(1) as f64 * COLUMN_SPACING;
        let width_without_spacing = row.width - hspacing;
        let additional_hscale = ((area.w - hspacing) / width_without_spacing).min(1.0);
        if additional_hscale < additional_vscale {
            row.additional_scale = additional_hscale;
            compensation += (additional_vscale - additional_hscale) * row.height;
        } else {
            row.additional_scale = additional_vscale;
        }
        row.x = area.x
            + ((area.w - (width_without_spacing * row.additional_scale + hspacing)).max(0.0) / 2.0);
        row.y = area.y + ((area.h - (height_without_spacing + vertical_spacing)).max(0.0) / 2.0) + y;
        y += row.height * row.additional_scale + ROW_SPACING;
    }
    compensation /= 2.0;

    let mut slots = vec![Rect::default(); windows.len()];
    for row in &layout.rows {
        let row_y = row.y + compensation;
        let row_height = row.height * row.additional_scale;
        let mut x = row.x;
        for &i in &row.windows {
            let rect = &windows[i];
            let mut s = window_scale(rect, monitor_h) * layout.scale * row.additional_scale;
            let cell_w = rect.w * s;
            let cell_h = rect.h * s;
            s = s.min(MAX_SCALE);
            let clone_w = rect.w * s;
            let clone_h = rect.h * s;
            let clone_x = x + (cell_w - clone_w) / 2.0;
            let clone_y = row_y + row_height - cell_h;
            slots[i] = Rect::new(clone_x.round(), clone_y.round(), clone_w.round(), clone_h.round());
            x += cell_w + COLUMN_SPACING;
        }
    }
    slots
}

/// Slots for `windows` (their real frame rects) inside `area`, in the same
/// order; `monitor_h` is the monitor's height.
pub fn window_picker_slots(windows: &[Rect], area: &Rect, monitor_h: f64) -> Vec<Rect> {
    if windows.is_empty() {
        return Vec::new();
    }
    let mut best: Option<Layout> = None;
    let (mut last_scale, mut last_space) = (0.0, 0.0);
    let mut last_columns = usize::MAX;
    for num_rows in 1..=windows.len() {
        let columns = windows.len().div_ceil(num_rows);
        if columns == last_columns {
            break;
        }
        let mut layout = compute_layout(windows, num_rows, monitor_h);
        let (scale, space) = scale_and_space(&mut layout, area);
        if best.is_some() && !is_better(last_scale, last_space, scale, space) {
            break;
        }
        best = Some(layout);
        last_columns = columns;
        last_scale = scale;
        last_space = space;
    }
    let mut layout = best.expect("at least one layout");
    window_slots(&mut layout, windows, area, monitor_h)
}

/// The area inside a workspace card that the windows are laid out in.
pub fn picker_area(card: &Rect) -> Rect {
    Rect::new(
        card.x + PICKER_PADDING_X,
        card.y + PICKER_PADDING_TOP,
        (card.w - 2.0 * PICKER_PADDING_X).max(1.0),
        (card.h - PICKER_PADDING_TOP - PICKER_PADDING_BOTTOM).max(1.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_window_is_centered_and_fits() {
        let area = Rect::new(100.0, 100.0, 800.0, 500.0);
        let slots = window_picker_slots(&[Rect::new(0.0, 32.0, 1280.0, 768.0)], &area, 800.0);
        let s = slots[0];
        assert!(s.x >= area.x && s.x + s.w <= area.x + area.w + 1.0);
        assert!(s.y >= area.y && s.y + s.h <= area.y + area.h + 1.0);
        assert!((s.center().0 - area.center().0).abs() <= 1.0);
        assert!((s.w / s.h - 1280.0 / 768.0).abs() < 0.02);
    }

    #[test]
    fn several_windows_do_not_overlap() {
        let area = Rect::new(0.0, 0.0, 900.0, 560.0);
        let windows = [
            Rect::new(50.0, 80.0, 560.0, 380.0),
            Rect::new(400.0, 200.0, 560.0, 380.0),
            Rect::new(700.0, 300.0, 400.0, 300.0),
            Rect::new(0.0, 32.0, 1280.0, 768.0),
        ];
        let slots = window_picker_slots(&windows, &area, 800.0);
        for (i, a) in slots.iter().enumerate() {
            assert!(a.w > 10.0 && a.h > 10.0);
            for b in &slots[i + 1..] {
                assert!(!a.inset(1.0, 1.0).intersects(b), "{a:?} overlaps {b:?}");
            }
        }
    }
}
