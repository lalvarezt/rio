#[cfg(test)]
mod compute_tests;

use crate::context::Context;
use crate::mouse::Mouse;
use rio_backend::config::layout::Margin;
use rio_backend::crosswords::grid::Dimensions;
use rio_backend::event::EventListener;
use rio_backend::sugarloaf::{layout::TextDimensions, Sugarloaf};
use rustc_hash::FxHashMap;

use taffy::{
    geometry, style_helpers::length, AvailableSpace, Display, NodeId, Style, TaffyError,
    TaffyTree, TraversePartialTree,
};

const MIN_COLS: usize = 2;
const MIN_LINES: usize = 1;
const MIN_RESIZE_SIZE: f32 = 50.0;

/// Direction of a draggable panel border
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderDirection {
    /// Border between left/right panels (drag horizontally)
    Vertical,
    /// Border between top/bottom panels (drag vertically)
    Horizontal,
}

/// Describes a draggable border between two panels
#[derive(Debug, Clone, Copy)]
pub struct PanelBorder {
    pub direction: BorderDirection,
    pub left_or_top: NodeId,
    pub right_or_bottom: NodeId,
}

/// What sugarloaf keeps per panel: image overlays by rich text id and
/// the images a route drew. The layout and the context manager release
/// them through this, so both run in tests without a GPU context.
pub trait PanelImages {
    fn clear_image_overlays_for(&mut self, rich_text_id: usize);
    fn remove_route_images(&mut self, route_id: usize);
}

impl PanelImages for Sugarloaf<'_> {
    fn clear_image_overlays_for(&mut self, rich_text_id: usize) {
        Sugarloaf::clear_image_overlays_for(self, rich_text_id);
    }

    fn remove_route_images(&mut self, route_id: usize) {
        Sugarloaf::remove_route_images(self, route_id);
    }
}

/// Cell metrics for a font size, line height and scale: the font
/// library's answer, which only sugarloaf has outside of tests.
pub trait CellMetricsSource {
    fn cell_metrics(
        &self,
        font_size: f32,
        line_height: f32,
        scale: f32,
    ) -> (TextDimensions, rio_backend::sugarloaf::layout::CellMetrics);
}

impl CellMetricsSource for Sugarloaf<'_> {
    fn cell_metrics(
        &self,
        font_size: f32,
        line_height: f32,
        scale: f32,
    ) -> (TextDimensions, rio_backend::sugarloaf::layout::CellMetrics) {
        self.compute_cell_metrics(font_size, line_height, scale)
    }
}

/// What a removed panel held in sugarloaf, for the caller to release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemovedPanel {
    pub rich_text_id: usize,
    pub route_id: usize,
}

impl RemovedPanel {
    /// Drop the image overlays and route images the panel owned;
    /// sugarloaf keeps no other per-panel state.
    pub fn release(self, images: &mut impl PanelImages) {
        images.clear_image_overlays_for(self.rich_text_id);
        images.remove_route_images(self.route_id);
    }
}

/// A grid's name: the route id of the pane it was created with. Route
/// ids are minted once and never reused, so the name holds after that
/// pane is gone, and a grid can be found again after the tabs were
/// renumbered. Its own type, so a route id cannot be passed for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridId(pub usize);

/// Active resize drag state
#[derive(Debug, Clone, Copy)]
pub struct ResizeState {
    pub border: PanelBorder,
    /// Mouse position at drag start (physical pixels)
    pub start_pos: f32,
    /// Original sizes of the two panels at drag start
    pub original_sizes: (f32, f32),
}

fn compute(
    width: f32,
    height: f32,
    cell: rio_backend::sugarloaf::layout::CellMetrics,
    margin: Margin,
    scale: f32,
) -> (usize, usize) {
    // Ensure we have positive dimensions
    if width <= 0.0
        || height <= 0.0
        || scale <= 0.0
        || cell.cell_width == 0
        || cell.cell_height == 0
    {
        return (MIN_COLS, MIN_LINES);
    }

    // Calculate available space accounting for margins (scale margins to physical pixels)
    let available_width = width - (margin.left * scale) - (margin.right * scale);
    let available_height = height - (margin.top * scale) - (margin.bottom * scale);

    // Ensure we have positive available space
    if available_width <= 0.0 || available_height <= 0.0 {
        return (MIN_COLS, MIN_LINES);
    }

    // Cols/rows divide by the canonical integer cell stride
    // (`Metrics.cell_width / cell_height`). Same value the grid
    // shader uses for `cell_size` and the mouse-hit-test divides by;
    // single source of truth so the right/bottom edges of the grid
    // stay aligned with the painted cells.
    let cell_w = cell.cell_width as f32;
    let cell_h = cell.cell_height as f32;
    let visible_columns = std::cmp::max((available_width / cell_w) as usize, MIN_COLS);
    let visible_lines =
        std::cmp::max((available_height / cell_h).floor() as usize, MIN_LINES);

    (visible_columns, visible_lines)
}

/// Whether a flex direction lays children out left to right, the axis
/// a vertical divider sits across.
fn is_row_direction(direction: taffy::FlexDirection) -> bool {
    matches!(
        direction,
        taffy::FlexDirection::Row | taffy::FlexDirection::RowReverse
    )
}

/// `basis` moved by `delta` when it is a length; auto and percent
/// bases are left alone.
fn basis_plus(basis: taffy::Dimension, delta: f32) -> taffy::Dimension {
    match basis.into_option() {
        Some(value) => length(value + delta),
        None => basis,
    }
}

pub struct ContextGrid<T: EventListener> {
    /// Tab identity remains stable when panes are focused, added, or removed.
    pub tab_id: usize,
    pub width: f32,
    pub height: f32,
    pub current: NodeId,
    pub scaled_margin: Margin,
    // custom_title has priority over the active panel's computed title.
    pub custom_title: Option<String>,
    // custom_color is the tab's background override (tab color picker).
    pub custom_color: Option<[f32; 4]>,
    // bell is set when a pane in this tab rang while the tab was in the
    // background; cleared once the tab is focused again.
    pub bell: bool,
    scale: f32,
    inner: FxHashMap<NodeId, ContextGridItem<T>>,
    pub root: Option<NodeId>,
    panel_config: rio_backend::config::layout::Panel,
    divider_width: f32,
    tree: TaffyTree<()>,
    root_node: NodeId,
    dividers: Vec<Divider>,
    id: GridId,
    shape: u64,
}

/// Resolved window coordinates in physical pixels. Rendering and input consume
/// these boxes directly; only layout knows about root margins or Taffy offsets.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PaneGeometry {
    pub frame: [f32; 4],
    pub content: [f32; 4],
    pub radius: f32,
    pub border_width: f32,
}

impl PaneGeometry {
    fn resolve(
        slot: [f32; 4],
        root: [f32; 4],
        panel: &rio_backend::config::layout::Panel,
        scale: f32,
        framed: bool,
    ) -> Self {
        // Half of each gap belongs to either side of its divider center. Outer
        // edges have only the configured margin, regardless of split depth.
        let gap_x = panel.column_gap * scale / 2.0;
        let gap_y = panel.row_gap * scale / 2.0;
        let frame = Self::inset(
            slot,
            Margin::new(
                panel.margin.top * scale + if slot[1] > root[1] { gap_y } else { 0.0 },
                panel.margin.right * scale
                    + if slot[0] + slot[2] < root[0] + root[2] {
                        gap_x
                    } else {
                        0.0
                    },
                panel.margin.bottom * scale
                    + if slot[1] + slot[3] < root[1] + root[3] {
                        gap_y
                    } else {
                        0.0
                    },
                panel.margin.left * scale + if slot[0] > root[0] { gap_x } else { 0.0 },
            ),
        );
        let border_width = if framed {
            panel.border_width.min(frame[2].min(frame[3]) / 2.0)
        } else {
            0.0
        };
        let content = Self::inset(
            frame,
            Margin::new(
                border_width + panel.padding.top * scale,
                border_width + panel.padding.right * scale,
                border_width + panel.padding.bottom * scale,
                border_width + panel.padding.left * scale,
            ),
        );
        Self {
            frame,
            content,
            radius: (panel.border_radius * scale).min(frame[2].min(frame[3]) / 2.0),
            border_width,
        }
    }

    fn inset(rect: [f32; 4], margin: Margin) -> [f32; 4] {
        // When a window is smaller than its chrome, reduce opposing insets
        // proportionally. Content stays inside its pane with nonnegative size.
        let fit = |available: f32, total: f32| {
            if total > available {
                available / total
            } else {
                1.0
            }
        };
        let horizontal = fit(rect[2], margin.left + margin.right);
        let vertical = fit(rect[3], margin.top + margin.bottom);
        let left = (rect[0] + margin.left * horizontal).round();
        let top = (rect[1] + margin.top * vertical).round();
        let right = (rect[0] + rect[2] - margin.right * horizontal).round();
        let bottom = (rect[1] + rect[3] - margin.bottom * vertical).round();
        [left, top, (right - left).max(0.0), (bottom - top).max(0.0)]
    }
}

#[derive(Debug, Clone, Copy)]
struct Divider {
    border: PanelBorder,
    center: f32,
    span: [f32; 2],
}

pub struct ContextGridItem<T: EventListener> {
    pub val: Context<T>,
    pub geometry: PaneGeometry,
    /// The rect the panel was last laid out at, so a relayout only
    /// touches the panels it moved.
    applied_rect: [f32; 4],
    /// The grid last pushed into the terminal: columns, lines and cell
    /// size, the only things its reflow depends on.
    applied_grid: Option<GridKey>,
    /// A pixel-only winsize held back during a divider drag.
    winsize_pending: bool,
}

/// What a terminal's reflow depends on.
type GridKey = (usize, usize, u32, u32);

fn grid_key(dimension: &ContextDimension) -> GridKey {
    (
        dimension.columns(),
        dimension.screen_lines(),
        dimension.square_width().to_bits(),
        dimension.square_height().to_bits(),
    )
}

impl<T: rio_backend::event::EventListener> ContextGridItem<T> {
    pub fn new(context: Context<T>) -> Self {
        // The terminal and its PTY were created at this dimension, so
        // the first relayout only pushes what differs from it.
        let dimension = context.dimension;
        Self {
            applied_rect: [0.0, 0.0, dimension.width, dimension.height],
            applied_grid: Some(grid_key(&dimension)),
            winsize_pending: false,
            val: context,
            geometry: PaneGeometry::default(),
        }
    }

    /// Push the panel's layout into its terminal and PTY.
    ///
    /// A panel whose rect did not move is left alone. One whose grid
    /// changed is reflowed and told its new winsize. One whose size
    /// changed only in pixels keeps its grid and gets the winsize now,
    /// or, `mid_drag`, once the pointer lets go: the pixel fields reach
    /// the child as a SIGWINCH, and a drag would otherwise raise one
    /// per pixel in both panels beside the divider.
    fn push_layout(&mut self, mid_drag: bool) {
        let moved = self.applied_rect != self.geometry.content;
        let resized = self.applied_rect[2..] != self.geometry.content[2..];
        self.applied_rect = self.geometry.content;
        let key = grid_key(&self.val.dimension);
        let reflowed = self.applied_grid != Some(key);
        if moved || reflowed {
            // The present gate reads `pending_update.is_dirty()` before
            // it looks at the terminal, so a panel that moved or was
            // reflowed in place (a font size change keeps the rect) has
            // to be marked or an idle terminal would not present.
            self.val.renderable_content.pending_update.set_dirty();
        }

        if reflowed {
            self.applied_grid = Some(key);
            // The terminal ends a synchronized update itself when its
            // grid or cell size changes.
            self.val
                .terminal
                .lock()
                .resize::<ContextDimension>(self.val.dimension);
            self.send_winsize();
        } else if resized || self.winsize_pending {
            if mid_drag {
                self.winsize_pending = true;
            } else {
                self.send_winsize();
            }
        }
    }

    fn send_winsize(&mut self) {
        let winsize = crate::renderer::utils::terminal_dimensions(&self.val.dimension);
        let _ = self.val.messenger.send_resize(winsize);
        self.winsize_pending = false;
    }

    /// Send a winsize held back during a drag.
    fn flush_winsize(&mut self) {
        if self.winsize_pending {
            self.send_winsize();
        }
    }

    #[inline]
    pub fn context(&self) -> &Context<T> {
        &self.val
    }

    #[inline]
    pub fn context_mut(&mut self) -> &mut Context<T> {
        &mut self.val
    }
}

impl<T: rio_backend::event::EventListener> ContextGrid<T> {
    pub fn new(
        context: Context<T>,
        scaled_margin: Margin,
        panel_config: rio_backend::config::layout::Panel,
        divider_width: f32,
    ) -> Self {
        let id = context.route_id;
        let width = context.dimension.width;
        let height = context.dimension.height;
        let scale = context.dimension.dimension.scale;
        let tab_id = context.route_id;

        let mut tree: TaffyTree<()> = TaffyTree::new();

        // Calculate available size after window margin (already scaled)
        let available_width =
            ((width - scaled_margin.right).round() - scaled_margin.left.round()).max(0.0);
        let available_height = ((height - scaled_margin.bottom).round()
            - scaled_margin.top.round())
        .max(0.0);

        // Create root container (window margin handled separately via position offset)
        let root_style = Style {
            display: Display::Flex,
            size: geometry::Size {
                width: length(available_width),
                height: length(available_height),
            },
            ..Default::default()
        };

        let root_node = tree
            .new_leaf(root_style)
            .expect("Failed to create root node");

        let panel_style = Self::panel_style();

        let panel_node = tree
            .new_leaf(panel_style)
            .expect("Failed to create panel node");
        tree.add_child(root_node, panel_node)
            .expect("Failed to add child");

        // Use NodeId as the key
        let mut inner = FxHashMap::default();
        inner.insert(panel_node, ContextGridItem::new(context));

        let mut grid = Self {
            tab_id,
            inner,
            current: panel_node,
            scaled_margin,
            custom_title: None,
            custom_color: None,
            bell: false,
            scale,
            width,
            height,
            root: Some(panel_node),
            panel_config,
            divider_width,
            tree,
            root_node,
            dividers: Vec::new(),
            id: GridId(id),
            shape: 0,
        };
        grid.calculate_positions();
        grid.update_context_dimensions(false);
        grid
    }

    /// This grid's name, unique for the life of the process.
    #[inline]
    pub fn id(&self) -> GridId {
        self.id
    }

    /// A number that changes whenever a panel is added or removed.
    #[inline]
    pub fn shape(&self) -> u64 {
        self.shape
    }

    #[inline]
    #[allow(dead_code)]
    pub fn get_mut(&mut self, key: NodeId) -> Option<&mut ContextGridItem<T>> {
        self.inner.get_mut(&key)
    }

    /// Get item by route_id (used for event routing)
    #[inline]
    pub fn get_by_route_id(
        &mut self,
        route_id: usize,
    ) -> Option<&mut ContextGridItem<T>> {
        self.inner
            .values_mut()
            .find(|item| item.val.route_id == route_id)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn panel_count(&self) -> usize {
        self.inner.len()
    }

    pub fn should_draw_borders(&self) -> bool {
        self.panel_config.border.is_visible(self.panel_count())
    }

    fn try_update_size(&mut self, width: f32, height: f32) -> Result<(), TaffyError> {
        // Subtract window margin from available size
        let available_width = ((width - self.scaled_margin.right).round()
            - self.scaled_margin.left.round())
        .max(0.0);
        let available_height = ((height - self.scaled_margin.bottom).round()
            - self.scaled_margin.top.round())
        .max(0.0);

        let mut style = self.tree.style(self.root_node)?.clone();
        style.size = geometry::Size {
            width: length(available_width),
            height: length(available_height),
        };
        self.tree.set_style(self.root_node, style)?;
        Ok(())
    }

    fn compute_layout(&mut self) -> Result<(), TaffyError> {
        let available = geometry::Size {
            width: AvailableSpace::MaxContent,
            height: AvailableSpace::MaxContent,
        };
        self.tree.compute_layout(self.root_node, available)?;
        self.resolve_geometry()
    }

    /// One traversal resolves the pane boxes and the partition centers used by
    /// divider painting, pointer resizing, and keyboard resizing.
    fn resolve_geometry(&mut self) -> Result<(), TaffyError> {
        self.dividers.clear();
        let root = self.tree.layout(self.root_node)?;
        let x = self.scaled_margin.left.round();
        let y = self.scaled_margin.top.round();
        let root_bounds = [
            x + root.location.x,
            y + root.location.y,
            root.size.width,
            root.size.height,
        ];
        let mut stack = Vec::with_capacity(self.inner.len());
        stack.push((self.root_node, x, y));
        let framed = self.should_draw_borders();
        while let Some((node, parent_x, parent_y)) = stack.pop() {
            let layout = self.tree.layout(node)?;
            let abs_x = parent_x + layout.location.x;
            let abs_y = parent_y + layout.location.y;
            if let Some(item) = self.inner.get_mut(&node) {
                item.geometry = PaneGeometry::resolve(
                    [abs_x, abs_y, layout.size.width, layout.size.height],
                    root_bounds,
                    &self.panel_config,
                    self.scale,
                    framed,
                );
                continue;
            }

            let child_count = self.tree.child_count(node);
            let style = self.tree.style(node)?;
            let is_row = matches!(
                style.flex_direction,
                taffy::FlexDirection::Row | taffy::FlexDirection::RowReverse
            );
            let reverse = matches!(
                style.flex_direction,
                taffy::FlexDirection::RowReverse | taffy::FlexDirection::ColumnReverse
            );
            let child_at = |index| {
                self.tree.get_child_id(
                    node,
                    if reverse {
                        child_count - 1 - index
                    } else {
                        index
                    },
                )
            };
            for index in 0..child_count {
                let child = child_at(index);
                stack.push((child, abs_x, abs_y));
                if index + 1 < child_count {
                    let next = child_at(index + 1);
                    let child_layout = self.tree.layout(child)?;
                    let (direction, center, span) = if is_row {
                        (
                            BorderDirection::Vertical,
                            abs_x + child_layout.location.x + child_layout.size.width,
                            [abs_y, abs_y + layout.size.height],
                        )
                    } else {
                        (
                            BorderDirection::Horizontal,
                            abs_y + child_layout.location.y + child_layout.size.height,
                            [abs_x, abs_x + layout.size.width],
                        )
                    };
                    // Regions meet at divider centers. Nested lines therefore
                    // join their parent even when their painted panes have gaps.
                    self.dividers.push(Divider {
                        border: PanelBorder {
                            direction,
                            left_or_top: child,
                            right_or_bottom: next,
                        },
                        center,
                        span,
                    });
                }
            }
        }
        Ok(())
    }

    pub fn find_context_at_position(&self, x: f32, y: f32) -> Option<NodeId> {
        for (&node_id, item) in &self.inner {
            let [left, top, width, height] = item.geometry.frame;
            if x >= left && x < left + width && y >= top && y < top + height {
                return Some(node_id);
            }
        }
        None
    }

    /// Find a draggable border near the given mouse position (physical pixels).
    /// Returns None if no border is within the hit threshold.
    pub fn find_border_at_position(&self, x: f32, y: f32) -> Option<PanelBorder> {
        if self.inner.len() <= 1 {
            return None;
        }

        const RESIZE_SLOP: f32 = 3.0;
        let hit_half = (self.divider_width / 2.0 + RESIZE_SLOP) * self.scale;
        self.dividers.iter().find_map(|divider| {
            let (axis, cross) = match divider.border.direction {
                BorderDirection::Vertical => (x, y),
                BorderDirection::Horizontal => (y, x),
            };
            ((axis - divider.center).abs() < hit_half
                && cross >= divider.span[0]
                && cross <= divider.span[1])
                .then_some(divider.border)
        })
    }

    /// Divider width affects paint only, never pane allocation or spacing.
    pub fn for_each_divider(&self, mut draw: impl FnMut([f32; 4])) {
        let width = self.divider_width * self.scale;
        if width <= 0.0 {
            return;
        }
        for divider in &self.dividers {
            let Divider {
                center,
                span,
                border,
            } = *divider;
            draw(match border.direction {
                BorderDirection::Vertical => {
                    [center - width / 2.0, span[0], width, span[1] - span[0]]
                }
                BorderDirection::Horizontal => {
                    [span[0], center - width / 2.0, span[1] - span[0], width]
                }
            });
        }
    }

    /// Get the current size of a node along the relevant axis for a border direction.
    /// Works for both panel leaves and container nodes.
    pub fn get_panel_size(&self, node: NodeId, direction: BorderDirection) -> f32 {
        if let Ok(layout) = self.tree.layout(node) {
            match direction {
                BorderDirection::Vertical => layout.size.width,
                BorderDirection::Horizontal => layout.size.height,
            }
        } else {
            0.0
        }
    }

    /// Resize two adjacent panels by moving their shared border.
    /// `delta` is in physical pixels (positive = right/down).
    pub fn resize_border(
        &mut self,
        border: &PanelBorder,
        original_sizes: (f32, f32),
        delta: f32,
    ) {
        if self.set_border_delta(border, original_sizes, delta) {
            self.apply_taffy_layout(true);
        }
    }

    fn set_border_delta(
        &mut self,
        border: &PanelBorder,
        sizes: (f32, f32),
        delta: f32,
    ) -> bool {
        let row = border.direction == BorderDirection::Vertical;
        let floor = |node| {
            self.get_panel_size(node, border.direction) - self.shrink_room(node, row)
        };
        let lowest = floor(border.left_or_top) - sizes.0;
        let highest = sizes.1 - floor(border.right_or_bottom);
        if lowest > highest {
            return false;
        }
        let delta = delta.clamp(lowest, highest);
        let sizes = (sizes.0 + delta, sizes.1 - delta);
        if sizes
            == (
                self.get_panel_size(border.left_or_top, border.direction),
                self.get_panel_size(border.right_or_bottom, border.direction),
            )
        {
            return false;
        }
        let result = match border.direction {
            BorderDirection::Vertical => self
                .set_panel_size(border.left_or_top, Some(sizes.0), None)
                .and_then(|_| {
                    self.set_panel_size(border.right_or_bottom, Some(sizes.1), None)
                }),
            BorderDirection::Horizontal => self
                .set_panel_size(border.left_or_top, None, Some(sizes.0))
                .and_then(|_| {
                    self.set_panel_size(border.right_or_bottom, None, Some(sizes.1))
                }),
        };
        result.is_ok()
    }

    fn panel_style() -> Style {
        // Flex allocates regions only. Visual insets would otherwise change
        // sibling weights when a leaf is replaced by a nested split container.
        Style {
            display: Display::Flex,
            flex_basis: length(0.0_f32),
            flex_grow: 1.0,
            flex_shrink: 1.0,
            min_size: geometry::Size {
                width: length(0.0_f32),
                height: length(0.0_f32),
            },
            ..Default::default()
        }
    }

    fn try_split_right(&mut self) -> Result<NodeId, TaffyError> {
        self.split_panel(taffy::FlexDirection::Row)
    }

    fn try_split_down(&mut self) -> Result<NodeId, TaffyError> {
        self.split_panel(taffy::FlexDirection::Column)
    }

    fn split_panel(
        &mut self,
        direction: taffy::FlexDirection,
    ) -> Result<NodeId, TaffyError> {
        // Current is already the NodeId
        let current_node = self.current;
        if !self.inner.contains_key(&current_node) {
            return Err(TaffyError::InvalidInputNode(self.root_node));
        }

        // Find the parent of the current node
        let parent_node = self.tree.parent(current_node).unwrap_or(self.root_node);

        // Inherit the current panel's flex properties so the container
        // keeps the same proportion in its parent (e.g. 80/20 split).
        // The container has no margin, so it takes the panel's margin
        // box or the margins would turn into free space that shifts
        // every divider in the parent.
        let current_style = self.tree.style(current_node)?.clone();
        let margin = self.margin_along(current_node, parent_node);
        let container_style = Style {
            display: Display::Flex,
            flex_direction: direction,
            flex_basis: basis_plus(current_style.flex_basis, margin),
            // The weights are already in margin box units; with no
            // padding the container's content box is its basis.
            flex_grow: current_style.flex_grow,
            flex_shrink: current_style.flex_shrink,
            min_size: geometry::Size {
                width: length(0.0_f32),
                height: length(0.0_f32),
            },
            ..Default::default()
        };
        let container_node = self.tree.new_leaf(container_style)?;

        // Reset the current panel to flexible sizing inside the new container
        let mut reset_style = current_style;
        reset_style.flex_basis = length(0.0_f32);
        reset_style.flex_grow = 1.0;
        reset_style.flex_shrink = 1.0;
        self.tree.set_style(current_node, reset_style)?;

        // Create the new panel node
        let new_node = self.tree.new_leaf(Self::panel_style())?;

        // Get the index of current_node in its parent
        let children = self.tree.children(parent_node)?;
        let current_index = children.iter().position(|&n| n == current_node);

        // Remove current_node from parent
        self.tree.remove_child(parent_node, current_node)?;

        // Add current_node and new_node as children of container
        self.tree.add_child(container_node, current_node)?;
        self.tree.add_child(container_node, new_node)?;

        // Insert container at the same position in parent
        if let Some(idx) = current_index {
            self.tree
                .insert_child_at_index(parent_node, idx, container_node)?;
        } else {
            self.tree.add_child(parent_node, container_node)?;
        }

        Ok(new_node)
    }

    fn set_panel_size(
        &mut self,
        node: NodeId,
        width: Option<f32>,
        height: Option<f32>,
    ) -> Result<(), TaffyError> {
        let mut style = self.tree.style(node)?.clone();

        // The basis is the border-box size, the unit taffy lays out in
        // for a panel and a container alike, so a split or a collapse
        // can hand one node's sizing to the other kind unchanged and the
        // pair lands on the sizes asked for. Grow weighted by the same
        // size keeps the proportion when the window grows. Taffy shares
        // negative space by flex_shrink times the content-box basis, so
        // a panel's padding would make it give up less than its share
        // when the window shrinks; weighting shrink by border box over
        // content box puts the share back in proportion to what is seen.
        if let Some(size) = width.or(height) {
            style.flex_basis = length(size);
            let (grow, shrink) =
                self.flex_weights(node, style.flex_basis, width.is_some());
            style.flex_grow = grow;
            style.flex_shrink = shrink;
        }

        self.tree.set_style(node, style)?;
        Ok(())
    }

    /// Grow and shrink weights for a node with a sized basis, in one
    /// unit for panels and containers alike: the margin box. A panel's
    /// basis is its border box and its margins sit outside; a container
    /// has no margin and its basis is the margin box of the panel it
    /// replaced. Taffy grows by `flex_grow` and shrinks by `flex_shrink`
    /// times the content box, so grow is the margin box and shrink is
    /// margin box over content box: a panel's padding does not shield
    /// it, and a basis handed between a panel and a container keeps the
    /// proportion. An unsized node keeps the default weights.
    fn flex_weights(
        &self,
        node: NodeId,
        basis: taffy::Dimension,
        row: bool,
    ) -> (f32, f32) {
        let Some(size) = basis.into_option() else {
            return (1.0, 1.0);
        };
        let Ok(layout) = self.tree.layout(node) else {
            return (size, 1.0);
        };
        let (margins, edges) = if row {
            (
                layout.margin.left + layout.margin.right,
                layout.padding.left
                    + layout.padding.right
                    + layout.border.left
                    + layout.border.right,
            )
        } else {
            (
                layout.margin.top + layout.margin.bottom,
                layout.padding.top
                    + layout.padding.bottom
                    + layout.border.top
                    + layout.border.bottom,
            )
        };
        let visible = size + margins;
        let inner = size - edges;
        let shrink = if inner > 0.0 { visible / inner } else { 1.0 };
        (visible, shrink)
    }

    /// Reset all panels to flexible sizing so they expand to fill available space
    /// Reset all nodes (panels and containers) to flexible sizing.
    fn reset_panel_styles_to_flexible(&mut self) {
        let mut stack = vec![self.root_node];
        while let Some(node) = stack.pop() {
            if let Ok(mut style) = self.tree.style(node).cloned() {
                style.flex_basis = length(0.0_f32);
                style.flex_grow = 1.0;
                style.flex_shrink = 1.0;
                let _ = self.tree.set_style(node, style);
            }
            if let Ok(children) = self.tree.children(node) {
                for child in children {
                    stack.push(child);
                }
            }
        }
    }

    /// Remove containers that have only one child by promoting the child
    /// to the container's parent. Repeats until no single-child containers remain.
    fn collapse_single_child_containers(&mut self) {
        loop {
            let mut collapsed = false;
            let mut stack = vec![self.root_node];

            while let Some(node) = stack.pop() {
                let children = match self.tree.children(node) {
                    Ok(c) => c,
                    _ => continue,
                };

                for &child in &children {
                    // Only consider non-panel nodes (containers)
                    if self.inner.contains_key(&child) {
                        continue;
                    }

                    let grandchildren = match self.tree.children(child) {
                        Ok(gc) => gc,
                        _ => continue,
                    };

                    if grandchildren.len() == 1 {
                        // Promote the single grandchild to replace this container,
                        // inheriting the container's flex sizing so siblings keep
                        // their proportions.
                        let grandchild = grandchildren[0];
                        let child_idx = children.iter().position(|&c| c == child);

                        if let Some(idx) = child_idx {
                            // Copy container's flex properties to the promoted child.
                            // A promoted panel brings its margins along, so a
                            // sized basis gives them back (see split_panel).
                            if let Ok(container_style) = self.tree.style(child).cloned() {
                                if let Ok(mut gc_style) =
                                    self.tree.style(grandchild).cloned()
                                {
                                    let margin = if self.inner.contains_key(&grandchild) {
                                        self.margin_along(grandchild, node)
                                    } else {
                                        0.0
                                    };
                                    gc_style.flex_basis =
                                        basis_plus(container_style.flex_basis, -margin);
                                    // Grow is in margin box units on both sides;
                                    // shrink is the promoted node's own.
                                    gc_style.flex_grow = container_style.flex_grow;
                                    let row = self
                                        .tree
                                        .style(node)
                                        .map(|s| is_row_direction(s.flex_direction))
                                        .unwrap_or(true);
                                    gc_style.flex_shrink = self
                                        .flex_weights(
                                            grandchild,
                                            gc_style.flex_basis,
                                            row,
                                        )
                                        .1;
                                    let _ = self.tree.set_style(grandchild, gc_style);
                                }
                            }

                            let _ = self.tree.remove_child(child, grandchild);
                            let _ = self.tree.remove_child(node, child);
                            let _ =
                                self.tree.insert_child_at_index(node, idx, grandchild);
                            collapsed = true;
                            break; // Tree changed, restart
                        }
                    } else if grandchildren.is_empty() {
                        // Empty container — remove it
                        let _ = self.tree.remove_child(node, child);
                        collapsed = true;
                        break;
                    } else {
                        stack.push(child);
                    }
                }

                if collapsed {
                    break;
                }
            }

            if !collapsed {
                break;
            }
        }
    }

    /// What `node`'s margins add along `parent`'s main axis. A panel
    /// occupies its border box plus these; a container has none, so a
    /// size handed from one kind to the other moves by this much.
    fn margin_along(&self, node: NodeId, parent: NodeId) -> f32 {
        let Ok(layout) = self.tree.layout(node) else {
            return 0.0;
        };
        let row = self
            .tree
            .style(parent)
            .map(|style| is_row_direction(style.flex_direction))
            .unwrap_or(true);
        if row {
            layout.margin.left + layout.margin.right
        } else {
            layout.margin.top + layout.margin.bottom
        }
    }

    /// The children of `node` in visual order along its main axis, when
    /// that axis is the one a divider move travels: `row` for left and
    /// right, column otherwise. `None` for a node laid out the other way.
    fn children_along(&self, node: NodeId, row: bool) -> Option<Vec<NodeId>> {
        let direction = self.tree.style(node).ok()?.flex_direction;
        if is_row_direction(direction) != row {
            return None;
        }
        let mut children = self.tree.children(node).ok()?;
        if matches!(
            direction,
            taffy::FlexDirection::RowReverse | taffy::FlexDirection::ColumnReverse
        ) {
            children.reverse();
        }
        Some(children)
    }

    /// The two siblings whose shared divider a key moves for `node`, in
    /// visual order: the divider on the side the key names (before the
    /// panel for left and up, after it for right and down), or, when the
    /// panel sits at the window edge on that side, the only divider it
    /// has, the one on its other side.
    ///
    /// Walks up from the panel through every container laid out along
    /// the axis, so a pane whose own container has no sibling on that
    /// side still reaches the divider an enclosing container holds. The
    /// pair can be containers: a panel nested in a column moves the
    /// divider between its column and the neighbour column. Geometry is
    /// not consulted, so panel margins and gaps cannot hide a neighbour.
    fn divider_siblings(
        &self,
        node: NodeId,
        row: bool,
        toward_start: bool,
    ) -> Option<(NodeId, NodeId)> {
        if !self.inner.contains_key(&node) {
            return None;
        }
        let mut other_side = None;
        let mut child = node;
        while let Some(parent) = self.tree.parent(child) {
            if let Some(children) = self.children_along(parent, row) {
                if children.len() >= 2 {
                    let index = children.iter().position(|&c| c == child)?;
                    let before = (index > 0).then(|| (children[index - 1], child));
                    let after = children.get(index + 1).map(|&next| (child, next));
                    let (named, other) = if toward_start {
                        (before, after)
                    } else {
                        (after, before)
                    };
                    if named.is_some() {
                        return named;
                    }
                    if other_side.is_none() {
                        other_side = other;
                    }
                }
            }
            child = parent;
        }
        other_side
    }

    /// How far `node` can lose along an axis before a panel in it
    /// reaches the floor the mouse drag keeps: a panel's size less the
    /// floor. A container across the axis passes the whole loss to each
    /// child; one along the axis shares it in proportion to the
    /// children's sizes, so a child's room counts for the container
    /// scaled by the children's total over that child's size.
    fn shrink_room(&self, node: NodeId, row: bool) -> f32 {
        let extent = |layout: &taffy::Layout| {
            if row {
                layout.size.width
            } else {
                layout.size.height
            }
        };
        let Ok(layout) = self.tree.layout(node) else {
            return 0.0;
        };
        if self.inner.contains_key(&node) {
            return extent(layout) - MIN_RESIZE_SIZE * self.scale;
        }
        let Ok(children) = self.tree.children(node) else {
            return 0.0;
        };
        let along = self.children_along(node, row).is_some();
        let sizes: Vec<f32> = children
            .iter()
            .map(|&child| self.tree.layout(child).map(extent).unwrap_or(0.0))
            .collect();
        let total: f32 = sizes.iter().sum();
        children
            .iter()
            .zip(sizes)
            .map(|(&child, size)| {
                let room = self.shrink_room(child, row);
                if along && size > 0.0 {
                    room * total / size
                } else {
                    room
                }
            })
            .fold(f32::INFINITY, f32::min)
    }

    /// Move the divider a key names for the current panel by `amount`
    /// logical pixels along an axis, toward the window's start (left or
    /// up) or end. The side the divider moves toward shrinks and the
    /// other grows, whichever of the two is current; a container on
    /// either side spreads the change over its panels in proportion,
    /// as the mouse drag of that same border does. No panel on the
    /// shrinking side drops under the floor the mouse drag keeps, and
    /// nothing changes when the divider is already there. Styles only:
    /// the caller recomputes the layout.
    fn resize_divider(&mut self, row: bool, toward_start: bool, amount: f32) -> bool {
        let Some((first, second)) =
            self.divider_siblings(self.current, row, toward_start)
        else {
            return false;
        };
        let size = |layout: &taffy::Layout| {
            if row {
                layout.size.width
            } else {
                layout.size.height
            }
        };
        let (Ok(first_layout), Ok(second_layout)) =
            (self.tree.layout(first), self.tree.layout(second))
        else {
            return false;
        };
        let (first_size, second_size) = (size(first_layout), size(second_layout));

        let shrinking = if toward_start { first } else { second };
        let room = self.shrink_room(shrinking, row);
        // Under half a pixel the rounded layout would not change.
        let amount = (amount * self.scale).min(room);
        if amount < 0.5 {
            return false;
        }
        let (new_first, new_second) = if toward_start {
            (first_size - amount, second_size + amount)
        } else {
            (first_size + amount, second_size - amount)
        };

        let set = |grid: &mut Self, node, value| {
            if row {
                grid.set_panel_size(node, Some(value), None)
            } else {
                grid.set_panel_size(node, None, Some(value))
            }
        };
        // A pair set by halves would surface on the next unrelated
        // layout pass, so the first is put back if the second fails.
        let Ok(first_style) = self.tree.style(first).cloned() else {
            return false;
        };
        if set(self, first, new_first).is_err() {
            return false;
        }
        if set(self, second, new_second).is_err() {
            let _ = self.tree.set_style(first, first_style);
            return false;
        }
        true
    }

    /// Lay the tree out and push the result to the panels it moved;
    /// see [`ContextGridItem::push_layout`] for what each one gets.
    fn apply_taffy_layout(&mut self, mid_drag: bool) -> bool {
        if self.compute_layout().is_err() {
            return false;
        }
        self.update_context_dimensions(mid_drag);
        true
    }

    fn update_context_dimensions(&mut self, mid_drag: bool) {
        for item in self.inner.values_mut() {
            let [_, _, width, height] = item.geometry.content;

            // Clear margin since Taffy layout already accounts for spacing
            item.val.dimension.margin = Margin::all(0.0);
            item.val.dimension.update_width(width);
            item.val.dimension.update_height(height);
            item.push_layout(mid_drag);
        }
    }

    /// The pointer let go of a divider: panels whose pixel size moved
    /// during the drag get their final winsize now.
    pub fn end_border_resize(&mut self) {
        for item in self.inner.values_mut() {
            item.flush_winsize();
        }
    }

    #[inline]
    pub fn contexts_mut(&mut self) -> &mut FxHashMap<NodeId, ContextGridItem<T>> {
        &mut self.inner
    }

    #[inline]
    pub fn contexts(&self) -> &FxHashMap<NodeId, ContextGridItem<T>> {
        &self.inner
    }

    /// Get contexts ordered by visual position (top-to-bottom, left-to-right)
    pub fn get_ordered_keys(&self) -> Vec<NodeId> {
        let mut panels: Vec<(NodeId, f32, f32)> = self
            .inner
            .iter()
            .map(|(&id, item)| (id, item.geometry.content[1], item.geometry.content[0])) // (id, y, x)
            .collect();

        // Sort by Y first (top to bottom), then X (left to right)
        panels.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
        });

        panels.into_iter().map(|(id, _, _)| id).collect()
    }

    #[inline]
    pub fn select_next_split(&mut self) {
        if self.inner.len() == 1 {
            return;
        }

        let keys = self.get_ordered_keys();
        if let Some(current_pos) = keys.iter().position(|&k| k == self.current) {
            if current_pos >= keys.len() - 1 {
                self.current = keys[0];
            } else {
                self.current = keys[current_pos + 1];
            }
        }
    }

    #[inline]
    pub fn select_next_split_no_loop(&mut self) -> bool {
        if self.inner.len() == 1 {
            return false;
        }

        let keys = self.get_ordered_keys();
        if let Some(current_pos) = keys.iter().position(|&k| k == self.current) {
            if current_pos >= keys.len() - 1 {
                return false;
            } else {
                self.current = keys[current_pos + 1];
                return true;
            }
        }
        false
    }

    #[inline]
    pub fn select_prev_split(&mut self) {
        if self.inner.len() == 1 {
            return;
        }

        let keys = self.get_ordered_keys();
        if let Some(current_pos) = keys.iter().position(|&k| k == self.current) {
            if current_pos == 0 {
                self.current = keys[keys.len() - 1];
            } else {
                self.current = keys[current_pos - 1];
            }
        }
    }

    #[inline]
    pub fn select_prev_split_no_loop(&mut self) -> bool {
        if self.inner.len() == 1 {
            return false;
        }

        let keys = self.get_ordered_keys();
        if let Some(current_pos) = keys.iter().position(|&k| k == self.current) {
            if current_pos == 0 {
                return false;
            } else {
                self.current = keys[current_pos - 1];
                return true;
            }
        }
        false
    }

    #[inline]
    pub fn current_item(&self) -> Option<&ContextGridItem<T>> {
        self.inner.get(&self.current)
    }

    pub fn current(&self) -> &Context<T> {
        if let Some(item) = self.inner.get(&self.current) {
            &item.val
        } else {
            // This should never happen, but if it does, return the first context
            tracing::error!("Current key {:?} not found in grid", self.current);
            if let Some(root) = self.root {
                if let Some(item) = self.inner.get(&root) {
                    return &item.val;
                }
            }
            // If even root is not found, panic as this indicates a serious bug
            panic!("Grid is in an invalid state - no contexts available");
        }
    }

    #[inline]
    pub fn current_mut(&mut self) -> &mut Context<T> {
        let current_key = self.current;

        // Check if current key exists, if not try to fix it
        if !self.inner.contains_key(&current_key) {
            tracing::error!("Current key {:?} not found in grid", current_key);
            if let Some(root) = self.root {
                self.current = root;
            } else if let Some(first_key) = self.inner.keys().next() {
                self.current = *first_key;
                self.root = Some(*first_key);
            } else {
                panic!("Grid is in an invalid state - no contexts available");
            }
        }

        // Now get the mutable reference
        let current_key = self.current;
        if let Some(item) = self.inner.get_mut(&current_key) {
            &mut item.val
        } else {
            panic!(
                "Grid is in an invalid state - current key not found after fix attempt"
            );
        }
    }

    pub fn current_context_with_computed_dimension(&self) -> (&Context<T>, Margin) {
        let item = self
            .inner
            .get(&self.current)
            .or_else(|| self.root.and_then(|root| self.inner.get(&root)))
            .expect("Grid is in an invalid state - no contexts available");
        let [abs_x, abs_y, _, _] = item.geometry.content;
        (
            &item.val,
            Margin {
                left: abs_x,
                top: abs_y,
                right: self.scaled_margin.right,
                bottom: self.scaled_margin.bottom,
            },
        )
    }

    #[inline]
    /// Select panel based on mouse position using Taffy layout.
    /// Returns true only when focus actually changed to a different panel.
    pub fn select_current_based_on_mouse(&mut self, mouse: &Mouse) -> bool {
        if self.inner.len() <= 1 {
            return false;
        }

        let x = mouse.x as f32;
        let y = mouse.y as f32;

        // Use Taffy's find_context_at_position to find the panel
        if let Some(context_id) = self.find_context_at_position(x, y) {
            if context_id != self.current {
                self.current = context_id;
                return true;
            }
        }

        false
    }

    #[inline]
    pub fn grid_dimension(&self) -> ContextDimension {
        if let Some(current_item) = self.inner.get(&self.current) {
            let current_context_dimension = current_item.val.dimension;
            let scale = current_context_dimension.dimension.scale;
            // scaled_margin is already in physical pixels, but
            // ContextDimension::build scales the margin again via compute(),
            // so unscale it here to avoid double-scaling.
            let unscaled_margin = if scale > 0.0 {
                Margin::new(
                    self.scaled_margin.top / scale,
                    self.scaled_margin.right / scale,
                    self.scaled_margin.bottom / scale,
                    self.scaled_margin.left / scale,
                )
            } else {
                self.scaled_margin
            };
            ContextDimension::build(
                self.width,
                self.height,
                current_context_dimension.dimension,
                current_context_dimension.cell,
                current_context_dimension.line_height,
                current_context_dimension.font_size,
                unscaled_margin,
            )
        } else {
            tracing::error!("Current key {:?} not found in grid", self.current);
            ContextDimension::default()
        }
    }

    pub fn update_scaled_margin(&mut self, scaled_margin: Margin) {
        self.scaled_margin = scaled_margin;
        // Keep the taffy root size in sync with the new margins,
        // otherwise panels keep stale sizes until the next window
        // resize recomputes the available space.
        let _ = self.try_update_size(self.width, self.height);
    }

    pub fn update_scale(&mut self, new_scale: f32) {
        self.scale = new_scale;
    }

    pub fn update_layout_config(
        &mut self,
        panel: rio_backend::config::layout::Panel,
        divider_width: f32,
    ) {
        self.panel_config = panel;
        self.divider_width = divider_width;
    }

    pub fn update_line_height(&mut self, line_height: f32) {
        for context in self.inner.values_mut() {
            context.val.dimension.update_line_height(line_height);
        }
    }

    /// Recompute every panel's cell metrics and lay the tree out again.
    /// Each panel's `font_size`, `line_height` and scale drive the
    /// result, so panels with per-panel zoom keep independent strides.
    pub fn update_dimensions(&mut self, metrics: &impl CellMetricsSource) {
        for context in self.inner.values_mut() {
            let dim = &mut context.val.dimension;
            if dim.font_size <= 0.0 {
                continue;
            }
            let (text_dims, cell) =
                metrics.cell_metrics(dim.font_size, dim.line_height, dim.dimension.scale);
            dim.update_dimensions(text_dims, cell);
        }

        // Always apply Taffy layout for consistent positioning
        self.apply_taffy_layout(false);
    }

    /// Resize grid - always uses Taffy for consistent layout
    pub fn resize(&mut self, new_width: f32, new_height: f32) {
        self.width = new_width;
        self.height = new_height;

        // Update Taffy size and recompute layout
        let _ = self.try_update_size(new_width, new_height);

        // Apply layout - works for both single and multi-panel
        self.apply_taffy_layout(false);
    }

    #[inline]
    pub fn calculate_positions(&mut self) {
        if self.inner.is_empty() {
            return;
        }

        // Resolve the complete layout, including pane boxes and divider geometry.
        let _ = self.compute_layout();
    }

    /// Remove the panel owning `route_id`, wherever it sits in the
    /// grid. The grid's focused panel is preserved unless it is the
    /// one being removed, in which case `remove_current`'s sibling
    /// selection applies. Returns what the panel held in sugarloaf,
    /// or `None` when no panel was removed.
    #[must_use = "release the panel's images with RemovedPanel::release"]
    pub fn remove_by_route(&mut self, route_id: usize) -> Option<RemovedPanel> {
        let key = self
            .inner
            .iter()
            .find(|(_, item)| item.val.route_id == route_id)
            .map(|(key, _)| *key)?;

        let previous = self.current;
        self.current = key;
        let removed = self.remove_current();
        if previous != key && self.inner.contains_key(&previous) {
            self.current = previous;
        }
        removed
    }

    /// Remove the current panel and select its neighbour in visual
    /// order. Returns what the panel held in sugarloaf, for the caller
    /// to release; `None` when nothing was removed, as for the last
    /// panel of a grid.
    #[must_use = "release the panel's images with RemovedPanel::release"]
    pub fn remove_current(&mut self) -> Option<RemovedPanel> {
        if self.inner.is_empty() {
            tracing::error!("Attempted to remove from empty grid");
            return None;
        }

        // Can't remove the last panel
        if self.inner.len() == 1 {
            tracing::warn!("Cannot remove the last remaining context");
            return None;
        }

        let to_remove = self.current;

        let Some(item) = self.inner.get(&to_remove) else {
            tracing::error!("Current key {:?} not found in grid", to_remove);
            return None;
        };
        let removed = RemovedPanel {
            rich_text_id: item.val.rich_text_id,
            route_id: item.val.route_id,
        };

        // Select next panel before removing (use visual ordering)
        let ordered_keys = self.get_ordered_keys();
        let current_pos = ordered_keys.iter().position(|&k| k == to_remove);
        let next_current = if let Some(pos) = current_pos {
            // Try next panel, or previous if we're at the end
            if pos + 1 < ordered_keys.len() {
                ordered_keys[pos + 1]
            } else if pos > 0 {
                ordered_keys[pos - 1]
            } else {
                // Fallback to any other panel
                *ordered_keys
                    .iter()
                    .find(|&&k| k != to_remove)
                    .unwrap_or(&to_remove)
            }
        } else {
            // Fallback to first panel
            *self
                .inner
                .keys()
                .find(|&&k| k != to_remove)
                .unwrap_or(&to_remove)
        };

        // Remove from Taffy - to_remove IS the NodeId
        let _ = self.tree.remove(to_remove);

        // Remove from inner map
        self.inner.remove(&to_remove);
        self.shape += 1;

        // Update root if necessary
        if Some(to_remove) == self.root {
            self.root = self.inner.keys().next().copied();
        }

        // Set new current
        self.current = next_current;

        // Collapse single-child containers left behind by removal
        self.collapse_single_child_containers();

        // Recompute layout
        if self.panel_count() > 0 {
            // When back to a single panel, reset to flexible so it fills the window
            if self.panel_count() == 1 {
                self.reset_panel_styles_to_flexible();
            }
            self.apply_taffy_layout(false);
        }
        Some(removed)
    }

    pub fn split_right(&mut self, context: Context<T>) {
        if !self.inner.contains_key(&self.current) {
            return;
        }

        // Create taffy node first, then item
        if let Ok(new_node) = self.try_split_right() {
            let new_context = ContextGridItem::new(context);
            self.inner.insert(new_node, new_context);
            self.shape += 1;
            self.apply_taffy_layout(false);
            self.current = new_node;
        }
    }

    /// Split down - create new panel below using Taffy
    pub fn split_down(&mut self, context: Context<T>) {
        if !self.inner.contains_key(&self.current) {
            return;
        }

        // Create taffy node first, then item
        if let Ok(new_node) = self.try_split_down() {
            let new_context = ContextGridItem::new(context);
            self.inner.insert(new_node, new_context);
            self.shape += 1;
            self.apply_taffy_layout(false);
            self.current = new_node;
        }
    }

    pub fn move_divider_up(&mut self, amount: f32) -> bool {
        self.resize_divider(false, true, amount) && self.apply_taffy_layout(false)
    }

    pub fn move_divider_down(&mut self, amount: f32) -> bool {
        self.resize_divider(false, false, amount) && self.apply_taffy_layout(false)
    }

    pub fn move_divider_left(&mut self, amount: f32) -> bool {
        self.resize_divider(true, true, amount) && self.apply_taffy_layout(false)
    }

    pub fn move_divider_right(&mut self, amount: f32) -> bool {
        self.resize_divider(true, false, amount) && self.apply_taffy_layout(false)
    }

    /// Clear image overlays for every panel in this grid.
    #[inline]
    pub fn clear_image_overlays(&self, images: &mut impl PanelImages) {
        for item in self.inner.values() {
            images.clear_image_overlays_for(item.val.rich_text_id);
        }
    }

    /// Release everything this grid's panels hold in sugarloaf: image
    /// overlays and the images they drew.
    pub fn remove_from_sugarloaf(&self, images: &mut impl PanelImages) {
        for item in self.inner.values() {
            images.clear_image_overlays_for(item.val.rich_text_id);
            images.remove_route_images(item.val.route_id);
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct ContextDimension {
    pub width: f32,
    pub height: f32,
    pub columns: usize,
    pub lines: usize,
    pub dimension: TextDimensions,
    pub margin: Margin,
    pub line_height: f32,
    /// Logical-point font size for this panel. Per-panel zoom updates
    /// here; the global `RootStyle.font_size` is the default that new
    /// panels inherit at create time.
    pub font_size: f32,
    /// Font size at panel creation (or last `update_config`). Drives
    /// the "reset font size" action so zoom in/out always returns to
    /// the user's configured size.
    pub original_font_size: f32,
    /// `font_size * scale_factor` — physical-pixel size used by the
    /// grid emit path to drive glyph rasterization.
    pub scaled_font_size: f32,
    /// Canonical cell metrics — single source of truth shared by the
    /// GPU grid uniform, col/row count math, and mouse hit testing.
    /// Rounded `u32` cell width / height / baseline plus unrounded
    /// `f64` face dimensions for downstream subpixel math. Avoids
    /// drift between painted cell stride and click→cell mapping.
    pub cell: rio_backend::sugarloaf::layout::CellMetrics,
}

impl Default for ContextDimension {
    fn default() -> ContextDimension {
        ContextDimension {
            width: 0.,
            height: 0.,
            columns: MIN_COLS,
            lines: MIN_LINES,
            line_height: 1.,
            font_size: 0.,
            original_font_size: 0.,
            scaled_font_size: 0.,
            dimension: TextDimensions::default(),
            margin: Margin::default(),
            cell: rio_backend::sugarloaf::layout::CellMetrics::default(),
        }
    }
}

impl ContextDimension {
    pub fn build(
        width: f32,
        height: f32,
        dimension: TextDimensions,
        cell: rio_backend::sugarloaf::layout::CellMetrics,
        line_height: f32,
        font_size: f32,
        margin: Margin,
    ) -> Self {
        let (columns, lines) = compute(width, height, cell, margin, dimension.scale);
        Self {
            width,
            height,
            columns,
            lines,
            dimension,
            margin,
            line_height,
            font_size,
            original_font_size: font_size,
            scaled_font_size: font_size * dimension.scale,
            cell,
        }
    }

    #[inline]
    pub fn update_width(&mut self, width: f32) {
        self.width = width;
        self.update();
    }

    #[inline]
    pub fn update_height(&mut self, height: f32) {
        self.height = height;
        self.update();
    }

    #[inline]
    pub fn update_line_height(&mut self, line_height: f32) {
        self.line_height = line_height;
        self.update();
    }

    #[inline]
    pub fn update_font_size(&mut self, font_size: f32) {
        self.font_size = font_size;
        self.scaled_font_size = font_size * self.dimension.scale;
        // Caller is responsible for re-running `compute_cell_metrics`
        // and feeding the result back via `update_dimensions` —
        // `font_size` alone doesn't change the cell stride, the
        // recomputed metrics do.
    }

    /// Update only the stored scale factor. Caller must follow with
    /// `compute_cell_metrics` + `update_dimensions` so width/height
    /// and canonical cell stride are recomputed for the new DPI.
    #[inline]
    pub fn update_scale(&mut self, scale: f32) {
        self.dimension.scale = scale;
        self.scaled_font_size = self.font_size * scale;
    }

    /// Re-baseline the font size — both current and "original".
    /// Called from `update_config` so a config edit becomes the new
    /// reset target. Per-panel zoom (`change_font_size`) uses
    /// `update_font_size` instead so the original stays put.
    #[inline]
    pub fn rebaseline_font_size(&mut self, font_size: f32) {
        self.font_size = font_size;
        self.original_font_size = font_size;
        self.scaled_font_size = font_size * self.dimension.scale;
    }

    /// Increment the panel's font size by 1 logical point. Returns
    /// `true` if the size changed (i.e. wasn't already at the upper
    /// clamp). Caller must follow with `compute_cell_metrics` +
    /// `update_dimensions` to refresh the cell stride.
    #[inline]
    pub fn increase_font_size(&mut self) -> bool {
        if self.font_size < 100.0 {
            self.update_font_size(self.font_size + 1.0);
            true
        } else {
            false
        }
    }

    #[inline]
    pub fn decrease_font_size(&mut self) -> bool {
        if self.font_size > 6.0 {
            self.update_font_size(self.font_size - 1.0);
            true
        } else {
            false
        }
    }

    #[inline]
    pub fn reset_font_size(&mut self) -> bool {
        if (self.font_size - self.original_font_size).abs() > f32::EPSILON {
            self.update_font_size(self.original_font_size);
            true
        } else {
            false
        }
    }

    #[inline]
    pub fn update_dimensions(
        &mut self,
        dimensions: TextDimensions,
        cell: rio_backend::sugarloaf::layout::CellMetrics,
    ) {
        self.dimension = dimensions;
        self.cell = cell;
        self.scaled_font_size = self.font_size * dimensions.scale;
        self.update();
    }

    #[inline]
    fn update(&mut self) {
        let (columns, lines) = compute(
            self.width,
            self.height,
            self.cell,
            self.margin,
            self.dimension.scale,
        );

        self.columns = columns;
        self.lines = lines;
    }
}

impl Dimensions for ContextDimension {
    #[inline]
    fn columns(&self) -> usize {
        self.columns
    }

    #[inline]
    fn screen_lines(&self) -> usize {
        self.lines
    }

    #[inline]
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn square_width(&self) -> f32 {
        self.dimension.width
    }

    fn square_height(&self) -> f32 {
        self.dimension.height
    }
}
