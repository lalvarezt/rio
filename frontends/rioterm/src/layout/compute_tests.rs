use super::*;

#[test]
fn splitting_a_resized_pane_preserves_the_existing_partition() {
    use crate::context::{create_dead_context, ContextManager};
    use rio_backend::event::{VoidListener, WindowId};

    let mut manager =
        ContextManager::start_with_capacity(1, VoidListener {}, WindowId::from(0))
            .unwrap();
    let grid = manager.current_grid_mut();
    grid.width = 1200.0;
    grid.height = 800.0;
    grid.update_scaled_margin(Margin::all(10.0));
    let context = create_dead_context(
        VoidListener {},
        WindowId::from(0),
        1,
        0,
        grid.current().dimension,
    );
    let right = grid.try_split_right().unwrap();
    grid.inner.insert(right, ContextGridItem::new(context));
    grid.calculate_positions();
    let border = grid.dividers[0].border;
    let sizes = (
        grid.get_panel_size(border.left_or_top, BorderDirection::Vertical),
        grid.get_panel_size(border.right_or_bottom, BorderDirection::Vertical),
    );
    assert!(grid.set_border_delta(&border, sizes, 200.0));
    grid.calculate_positions();
    let ratio = (sizes.0 + 200.0) / (sizes.0 + sizes.1);
    for width in [700.0, 1800.0, 1200.0] {
        grid.width = width;
        grid.update_scaled_margin(Margin::all(10.0));
        grid.calculate_positions();
        assert!(
            (grid.get_panel_size(border.left_or_top, BorderDirection::Vertical)
                - (width - 20.0) * ratio)
                .abs()
                <= 1.0
        );
    }
    let left_before = grid.inner[&border.left_or_top].geometry;
    let center_before = grid.dividers[0].center;
    grid.current = right;
    let context = create_dead_context(
        VoidListener {},
        WindowId::from(0),
        2,
        0,
        grid.current().dimension,
    );
    let bottom = grid.try_split_down().unwrap();
    grid.inner.insert(bottom, ContextGridItem::new(context));
    grid.calculate_positions();
    assert_eq!(grid.inner[&border.left_or_top].geometry, left_before);
    assert_eq!(
        grid.dividers
            .iter()
            .find(|d| d.border.direction == BorderDirection::Vertical)
            .unwrap()
            .center,
        center_before
    );
    let centers: Vec<_> = grid.dividers.iter().map(|d| (d.center, d.span)).collect();
    for spacing in [0.0, 13.0, 60.0] {
        let mut panel = grid.panel_config;
        panel.margin = Margin::all(spacing);
        panel.padding = Margin::all(spacing);
        panel.row_gap = spacing;
        panel.column_gap = spacing;
        panel.border_width = spacing;
        grid.update_panel_config(panel);
        grid.calculate_positions();
        assert_eq!(
            grid.dividers
                .iter()
                .map(|d| (d.center, d.span))
                .collect::<Vec<_>>(),
            centers
        );
    }
}

#[test]
fn pane_insets_stay_inside_the_allocation_when_space_runs_out() {
    use rio_backend::config::layout::Panel;
    for size in [0.0, 1.0, 20.0, 200.0] {
        for scale in [1.0, 1.25, 2.0] {
            let root = [10.0, 20.0, size, size];
            let panel = Panel {
                margin: Margin::new(100.0, 8.0, 20.0, 40.0),
                padding: Margin::new(10.0, 80.0, 30.0, 8.0),
                row_gap: 40.0,
                column_gap: 100.0,
                border_width: 12.0,
                border_radius: 200.0,
                ..Panel::default()
            };
            let geometry = PaneGeometry::resolve(root, root, &panel, scale, true);
            for (outer, inner) in
                [(root, geometry.frame), (geometry.frame, geometry.content)]
            {
                assert!(inner.iter().all(|value| value.is_finite()));
                assert!(inner[2] >= 0.0 && inner[3] >= 0.0);
                assert!(inner[0] >= outer[0] && inner[1] >= outer[1]);
                assert!(inner[0] + inner[2] <= outer[0] + outer[2]);
                assert!(inner[1] + inner[3] <= outer[1] + outer[3]);
            }
            assert!(geometry.radius <= geometry.frame[2].min(geometry.frame[3]) / 2.0);
        }
    }
}

#[test]
fn pane_box_model_reserves_only_the_visible_border() {
    use crate::context::ContextManager;
    use rio_backend::config::layout::Panel;
    use rio_backend::event::{VoidListener, WindowId};

    for scale in [1.0, 1.25, 1.5, 2.0] {
        let mut manager =
            ContextManager::start_with_capacity(1, VoidListener {}, WindowId::from(0))
                .unwrap();
        let grid = manager.current_grid_mut();
        grid.width = 1200.0 * scale;
        grid.height = 800.0 * scale;
        grid.update_scale(scale);
        grid.update_scaled_margin(Margin::all(10.0 * scale));
        let mut panel = Panel {
            margin: Margin::all(4.0),
            padding: Margin::all(8.0),
            border_radius: 12.0,
            ..Panel::default()
        };
        let mut expected_frame = None;
        for width in [0.0, 1.0, 5.0] {
            panel.border_width = width;
            grid.update_panel_config(panel);
            grid.calculate_positions();
            let geometry = grid.current_item().unwrap().geometry;
            assert_eq!(
                *expected_frame.get_or_insert(geometry.frame),
                geometry.frame
            );
            assert_eq!(geometry.border_width, width);
            assert_eq!(geometry.content[0] - geometry.frame[0], 8.0 * scale + width);
            assert_eq!(geometry.content[1] - geometry.frame[1], 8.0 * scale + width);
            assert_eq!(
                geometry.frame[2] - geometry.content[2],
                16.0 * scale + 2.0 * width
            );
            assert_eq!(
                geometry.frame[3] - geometry.content[3],
                16.0 * scale + 2.0 * width
            );
            let (_, mouse_origin) = grid.current_context_with_computed_dimension();
            assert_eq!([mouse_origin.left, mouse_origin.top], geometry.content[..2]);
        }
        let framed = grid.current_item().unwrap().geometry;
        panel.hide_if_single = true;
        grid.update_panel_config(panel);
        grid.calculate_positions();
        let hidden = grid.current_item().unwrap().geometry;
        assert_eq!(hidden.frame, framed.frame);
        assert_eq!(hidden.radius, framed.radius);
        assert_eq!(hidden.border_width, 0.0);
        assert_eq!(hidden.content[0], framed.content[0] - panel.border_width);
        assert_eq!(
            hidden.content[2],
            framed.content[2] + 2.0 * panel.border_width
        );
    }
}

#[test]
fn nested_resizing_uses_siblings_and_preserves_available_space() {
    use crate::context::{create_dead_context, ContextManager};
    use rio_backend::event::{VoidListener, WindowId};

    let mut manager =
        ContextManager::start_with_capacity(1, VoidListener {}, WindowId::from(0))
            .unwrap();
    let grid = manager.current_grid_mut();
    grid.width = 1200.0;
    grid.height = 800.0;
    grid.update_scaled_margin(Margin::all(10.0));
    for horizontal in [true, false] {
        let context = create_dead_context(
            VoidListener {},
            WindowId::from(0),
            grid.len(),
            0,
            grid.current().dimension,
        );
        let node = if horizontal {
            grid.try_split_right()
        } else {
            grid.try_split_down()
        }
        .unwrap();
        grid.inner.insert(node, ContextGridItem::new(context));
        grid.current = node;
    }
    grid.calculate_positions();
    for direction in [BorderDirection::Vertical, BorderDirection::Horizontal] {
        let (border, second) = grid.neighboring_divider(grid.current, direction).unwrap();
        assert!(second);
        assert_eq!(
            grid.tree.parent(border.left_or_top),
            grid.tree.parent(border.right_or_bottom)
        );
        let sizes = (
            grid.get_panel_size(border.left_or_top, direction),
            grid.get_panel_size(border.right_or_bottom, direction),
        );
        // Dragging far beyond a pane's minimum must not enlarge their parent.
        assert!(grid.set_border_delta(&border, sizes, 10000.0));
        grid.calculate_positions();
        let resized = (
            grid.get_panel_size(border.left_or_top, direction),
            grid.get_panel_size(border.right_or_bottom, direction),
        );
        assert_eq!(resized.0 + resized.1, sizes.0 + sizes.1);
        assert_eq!(resized.1, MIN_RESIZE_SIZE);
        // Returning the pointer to its drag origin restores the original split.
        assert!(grid.set_border_delta(&border, sizes, 0.0));
        grid.calculate_positions();
        assert_eq!(grid.get_panel_size(border.left_or_top, direction), sizes.0);
        assert_eq!(
            grid.get_panel_size(border.right_or_bottom, direction),
            sizes.1
        );
        let divider = grid
            .dividers
            .iter()
            .find(|d| {
                d.border.left_or_top == border.left_or_top
                    && d.border.right_or_bottom == border.right_or_bottom
            })
            .unwrap();
        let point = match direction {
            BorderDirection::Vertical => {
                [divider.center, (divider.span[0] + divider.span[1]) / 2.0]
            }
            BorderDirection::Horizontal => {
                [(divider.span[0] + divider.span[1]) / 2.0, divider.center]
            }
        };
        let hit = grid.find_border_at_position(point[0], point[1]).unwrap();
        assert_eq!(hit.left_or_top, border.left_or_top);
        assert_eq!(hit.right_or_bottom, border.right_or_bottom);
    }
}

#[test]
fn split_dividers_share_centers_and_leave_pane_geometry_unchanged() {
    use crate::context::{create_dead_context, ContextManager};
    use rio_backend::config::layout::Panel;
    use rio_backend::event::{VoidListener, WindowId};

    for scale in [1.0, 1.25, 2.0] {
        let mut manager =
            ContextManager::start_with_capacity(1, VoidListener {}, WindowId::from(0))
                .unwrap();
        let grid = manager.current_grid_mut();
        grid.width = 1200.0 * scale;
        grid.height = 800.0 * scale;
        grid.update_scale(scale);
        grid.update_scaled_margin(Margin::all(10.0 * scale));
        let mut panel = Panel {
            margin: Margin::new(3.0, 5.0, 7.0, 11.0),
            row_gap: 20.0,
            column_gap: 30.0,
            ..Panel::default()
        };
        grid.update_panel_config(panel);
        grid.calculate_positions();
        grid.for_each_divider(|_| panic!("a single pane has no divider"));
        let mut nodes = vec![grid.current];
        for horizontal in [true, false] {
            let context = create_dead_context(
                VoidListener {},
                WindowId::from(0),
                grid.len(),
                0,
                grid.current().dimension,
            );
            let node = if horizontal {
                grid.try_split_right()
            } else {
                grid.try_split_down()
            }
            .unwrap();
            grid.inner.insert(node, ContextGridItem::new(context));
            grid.current = node;
            nodes.push(node);
        }
        grid.calculate_positions();
        let panes: Vec<_> = grid
            .inner
            .iter()
            .map(|(&key, item)| (key, item.geometry.content))
            .collect();
        let mut centers = None;
        for width in [1.0, 7.0, 0.0, 2.0] {
            panel.divider_width = width;
            grid.update_panel_config(panel);
            grid.calculate_positions();
            for &(key, rect) in &panes {
                assert_eq!(grid.inner[&key].geometry.content, rect);
            }
            let mut lines = Vec::new();
            grid.for_each_divider(|rect| lines.push(rect));
            if width == 0.0 {
                assert!(lines.is_empty());
                continue;
            }
            assert_eq!(lines.len(), 2);
            let vertical = lines.iter().find(|r| r[2] == width * scale).unwrap();
            let horizontal = lines.iter().find(|r| r[3] == width * scale).unwrap();
            let current_centers = [
                vertical[0] + vertical[2] / 2.0,
                horizontal[1] + horizontal[3] / 2.0,
            ];
            assert_eq!(*centers.get_or_insert(current_centers), current_centers);
            let left = grid.inner[&nodes[0]].geometry.frame;
            let top = grid.inner[&nodes[1]].geometry.frame;
            let bottom = grid.inner[&nodes[2]].geometry.frame;
            for (actual, expected) in [
                (
                    current_centers[0] - left[0] - left[2],
                    panel.margin.right + panel.column_gap / 2.0,
                ),
                (
                    top[0] - current_centers[0],
                    panel.margin.left + panel.column_gap / 2.0,
                ),
                (
                    current_centers[1] - top[1] - top[3],
                    panel.margin.bottom + panel.row_gap / 2.0,
                ),
                (
                    bottom[1] - current_centers[1],
                    panel.margin.top + panel.row_gap / 2.0,
                ),
            ] {
                assert!((actual - expected * scale).abs() <= 1.0);
            }
            // A nested line joins the parent's center across margins and gaps.
            assert_eq!(horizontal[0], current_centers[0]);
            assert_eq!(vertical[1], grid.scaled_margin.top.round());
            assert_eq!(
                horizontal[0] + horizontal[2],
                (grid.width - grid.scaled_margin.right).round()
            );
            assert_eq!(
                vertical[3],
                grid.height - grid.scaled_margin.top - grid.scaled_margin.bottom
            );
            for (rect, direction) in [
                (vertical, BorderDirection::Vertical),
                (horizontal, BorderDirection::Horizontal),
            ] {
                let hit = grid
                    .find_border_at_position(
                        rect[0] + rect[2] / 2.0,
                        rect[1] + rect[3] / 2.0,
                    )
                    .unwrap();
                assert_eq!(hit.direction, direction);
            }
        }
    }
}

#[test]
fn new_tabs_keep_window_size_after_padded_layout() {
    use crate::context::ContextManager;
    use rio_backend::event::{VoidListener, WindowId};

    let mut manager =
        ContextManager::start_with_capacity(4, VoidListener {}, WindowId::from(0))
            .unwrap();
    let grid = manager.current_grid_mut();
    grid.width = 1200.0;
    grid.height = 800.0;
    grid.try_update_size(grid.width, grid.height).unwrap();
    grid.calculate_positions();
    let expected_bounds = grid.current_item().unwrap().geometry.content;
    // Model the applied pane layout: terminal dimensions describe its padded
    // content viewport, while the grid retains the complete window dimensions.
    grid.current_mut().dimension.margin = Margin::all(0.0);
    grid.current_mut()
        .dimension
        .update_width(expected_bounds[2]);
    grid.current_mut()
        .dimension
        .update_height(expected_bounds[3]);

    for count in 2..=4 {
        manager.add_context(true, 0);
        assert_eq!(manager.len(), count);
        let grid = manager.current_grid();
        assert_eq!([grid.width, grid.height], [1200.0, 800.0]);
        assert_eq!(
            grid.current_item().unwrap().geometry.content,
            expected_bounds
        );
        assert_eq!(grid.current().dimension.width, expected_bounds[2]);
        assert_eq!(grid.current().dimension.height, expected_bounds[3]);
    }
}

#[test]
fn program_status_each_tab_uses_only_its_own_focused_split() {
    use crate::context::{create_dead_context, ContextManager};
    use rio_backend::event::{VoidListener, WindowId};
    use rio_backend::performer::handler::Processor;
    use rio_backend::program_status::ProgramState;

    let mut cm =
        ContextManager::start_with_capacity(3, VoidListener {}, WindowId::from(0))
            .unwrap();
    let mut parser = Processor::default();
    let first_node = cm.current_grid().current;
    let first_route = cm.current().route_id;
    parser.advance(
        &mut *cm.current().terminal.lock(),
        b"\x1b]7501;state=blocked:app=first\x07",
    );

    // Construct a real split tree without a renderer; geometry is unrelated
    // to which pane's status the tab selects.
    let second_route = usize::MAX - 1;
    let second = create_dead_context(
        VoidListener {},
        WindowId::from(0),
        second_route,
        0,
        cm.current().dimension,
    );
    let grid = cm.current_grid_mut();
    let second_node = grid.try_split_right().unwrap();
    grid.inner.insert(second_node, ContextGridItem::new(second));
    grid.current = second_node;
    parser.advance(
        &mut *cm.current().terminal.lock(),
        b"\x1b]7501;state=working:app=second:progress=40\x07",
    );
    assert_eq!(
        cm.program_status_for_tab(0).unwrap().state,
        ProgramState::Working
    );
    assert!(!cm.is_route_focused_in_tab(first_route));
    assert!(cm.is_route_focused_in_tab(second_route));

    cm.add_context(true, 0);
    parser.advance(
        &mut *cm.current().terminal.lock(),
        b"\x1b]7501;state=done:app=third\x07",
    );
    assert_eq!(
        cm.program_status_for_tab(0).unwrap().app.as_deref(),
        Some("second")
    );
    assert_eq!(
        cm.program_status_for_tab(1).unwrap().state,
        ProgramState::Done
    );
    cm.contexts_mut()[0].current = first_node;
    assert_eq!(
        cm.program_status_for_tab(0).unwrap().state,
        ProgramState::Blocked
    );
    assert!(cm.is_route_focused_in_tab(first_route));
    assert!(!cm.is_route_focused_in_tab(second_route));
    cm.contexts_mut()[0].current = second_node;
    let second_terminal = cm.contexts_mut()[0].current().terminal.clone();
    parser.advance(&mut *second_terminal.lock(), b"\x1b]7501;state=clear\x07");
    assert!(cm.program_status_for_tab(0).is_none());
    assert!(cm.program_status_for_tab(usize::MAX).is_none());
    assert_eq!(
        cm.program_status_for_tab(1).unwrap().state,
        ProgramState::Done
    );
}

// This file tests compute function on different layouts.
// I've added some real scenarios so I can make sure it doesn't go off again.

/// Build a `CellMetrics` whose integer cell stride matches the given
/// `TextDimensions`. Used by tests that construct dimensions
/// directly without going through sugarloaf's font path.
fn cell_for(dims: TextDimensions) -> rio_backend::sugarloaf::layout::CellMetrics {
    rio_backend::sugarloaf::layout::CellMetrics {
        cell_width: dims.width.round().max(1.0) as u32,
        cell_height: dims.height.round().max(1.0) as u32,
        cell_baseline: 0,
        face_width: dims.width as f64,
        face_height: dims.height as f64,
        face_y: 0.0,
    }
}

/// note: Computes the renderer's actual per-line height in physical pixels.
///
/// The renderer gets metrics from Metrics::for_rich_text() which packs
/// cell_height as (ascent, descent, 0.0). cell_height is computed by
/// Metrics::calc at physical font_size scale, with ceil applied.
///
/// basically renderer line_height = ceil((ascent + descent + leading) * scale) * line_height_mod
fn renderer_line_height(
    ascent: f32,
    descent: f32,
    leading: f32,
    line_height_mod: f32,
    scale: f32,
) -> f32 {
    // Matches the Metrics::calc path: scale to physical, then ceil
    let cell_height = ((ascent + descent + leading) * scale).ceil();
    cell_height * line_height_mod
}

fn sugar_height(
    ascent: f32,
    descent: f32,
    leading: f32,
    line_height_mod: f32,
    scale: f32,
) -> f32 {
    ((ascent + descent + leading) * line_height_mod * scale).ceil()
}

/// Verifies that compute() row count fits when rendered.
#[allow(clippy::too_many_arguments)]
fn assert_rows_fit(
    panel_width: f32,
    panel_height: f32,
    sugar_width: f32,
    scale: f32,
    line_height_mod: f32,
    ascent: f32,
    descent: f32,
    leading: f32,
) {
    let sh = sugar_height(ascent, descent, leading, line_height_mod, scale);
    let dimensions = TextDimensions {
        width: sugar_width,
        height: sh,
        scale,
    };

    let (cols, rows) = compute(
        panel_width,
        panel_height,
        cell_for(dimensions),
        Margin::all(0.0),
        scale,
    );
    let _ = line_height_mod; // line_height already baked into `dimensions.height`.

    let actual_line_height =
        renderer_line_height(ascent, descent, leading, line_height_mod, scale);
    let rendered_height = rows as f32 * actual_line_height;

    assert!(
        rendered_height <= panel_height,
        "Rows overflow! {} rows * {:.2}px = {:.2}px rendered, but panel is only {:.2}px tall \
         (cols={}, sugar={:.2}x{:.2}, scale={:.1}, lh_mod={:.1})",
        rows,
        actual_line_height,
        rendered_height,
        panel_height,
        cols,
        sugar_width,
        sh,
        scale,
        line_height_mod,
    );
}

#[test]
fn test_user_case_1834x1436() {
    assert_rows_fit(1834.0, 1436.0, 16.41, 2.0, 1.0, 13.0, 3.5, 0.0);
}

#[test]
fn test_user_case_3766x1996() {
    assert_rows_fit(3766.0, 1996.0, 16.41, 2.0, 1.0, 13.0, 3.5, 0.0);
}

#[test]
fn test_user_case_5104x2736() {
    assert_rows_fit(5104.0, 2736.0, 16.41, 2.0, 1.0, 13.0, 3.5, 0.0);
}

#[test]
fn test_rows_fit_various_sizes() {
    for height in (500..=3000).step_by(50) {
        for width in [800.0, 1600.0, 2400.0, 3200.0] {
            assert_rows_fit(width, height as f32, 16.41, 2.0, 1.0, 13.0, 3.5, 0.0);
        }
    }
}

#[test]
fn test_rows_fit_with_nonzero_leading() {
    let test_cases: Vec<(f32, f32, f32)> = vec![
        (12.0, 3.0, 0.5),
        (12.0, 3.0, 1.0),
        (14.0, 4.0, 0.25),
        (10.0, 3.0, 2.0),
    ];

    for (ascent, descent, leading) in test_cases {
        for height in (500..=2000).step_by(100) {
            assert_rows_fit(
                1600.0,
                height as f32,
                16.0,
                2.0,
                1.0,
                ascent,
                descent,
                leading,
            );
        }
    }
}

#[test]
fn test_rows_fit_with_line_height_modifier() {
    for lh_mod in [1.1, 1.2, 1.5, 2.0] {
        for height in (500..=2000).step_by(100) {
            assert_rows_fit(1600.0, height as f32, 16.0, 2.0, lh_mod, 12.0, 3.0, 0.5);
        }
    }
}

#[test]
fn test_rows_fit_scale_1() {
    for height in (300..=1200).step_by(50) {
        assert_rows_fit(800.0, height as f32, 8.0, 1.0, 1.0, 13.0, 3.5, 0.0);
    }
}

#[test]
fn test_rows_fit_zero_leading() {
    for height in (500..=2000).step_by(100) {
        assert_rows_fit(1600.0, height as f32, 16.0, 2.0, 1.0, 12.77, 3.50, 0.0);
    }
}

#[test]
fn test_rows_fit_fractional_metrics() {
    // Fractional ascent+descent that would produce different results
    // with and without ceil
    assert_rows_fit(1600.0, 1000.0, 16.0, 2.0, 1.0, 12.3, 3.4, 0.1);
    assert_rows_fit(1600.0, 1000.0, 16.0, 2.0, 1.0, 11.9, 4.6, 0.3);
    assert_rows_fit(1600.0, 1000.0, 16.0, 1.5, 1.0, 12.0, 3.0, 0.5);
}

#[test]
fn test_compute_returns_min_for_zero_dimensions() {
    let dims = TextDimensions {
        width: 16.0,
        height: 32.0,
        scale: 2.0,
    };
    let (cols, rows) = compute(0.0, 0.0, cell_for(dims), Margin::all(0.0), 2.0);
    assert_eq!(cols, MIN_COLS);
    assert_eq!(rows, MIN_LINES);
}

#[test]
fn test_compute_returns_min_for_negative_dimensions() {
    let dims = TextDimensions {
        width: 16.0,
        height: 32.0,
        scale: 2.0,
    };
    let (cols, rows) = compute(-100.0, -100.0, cell_for(dims), Margin::all(0.0), 2.0);
    assert_eq!(cols, MIN_COLS);
    assert_eq!(rows, MIN_LINES);
}

#[test]
fn test_compute_returns_min_for_zero_scale() {
    let dims = TextDimensions {
        width: 16.0,
        height: 32.0,
        scale: 0.0,
    };
    let (cols, rows) = compute(1600.0, 900.0, cell_for(dims), Margin::all(0.0), 0.0);
    assert_eq!(cols, MIN_COLS);
    assert_eq!(rows, MIN_LINES);
}

#[test]
fn test_compute_basic_grid() {
    let dims = TextDimensions {
        width: 16.0,
        height: 33.0,
        scale: 2.0,
    };
    let (cols, rows) = compute(1600.0, 825.0, cell_for(dims), Margin::all(0.0), 2.0);
    assert_eq!(cols, 100);
    assert_eq!(rows, 25);
}

#[test]
fn test_compute_floors_fractional_rows() {
    // 840px / 33px = 25.45 → floor → 25
    let dims = TextDimensions {
        width: 16.0,
        height: 33.0,
        scale: 1.0,
    };
    let (_, rows) = compute(1600.0, 840.0, cell_for(dims), Margin::all(0.0), 1.0);
    assert_eq!(rows, 25);
}

#[test]
fn test_compute_respects_margins() {
    let dims = TextDimensions {
        width: 16.0,
        height: 32.0,
        scale: 2.0,
    };
    let margin = Margin::new(0.0, 10.0, 0.0, 10.0);
    let (cols, _) = compute(1600.0, 800.0, cell_for(dims), margin, 2.0);
    // available = 1600 - 10*2 - 10*2 = 1560, cols = 1560/16 = 97
    assert_eq!(cols, 97);
}

#[test]
fn test_compute_margin_exceeds_size() {
    let dims = TextDimensions {
        width: 16.0,
        height: 32.0,
        scale: 2.0,
    };
    let margin = Margin::new(0.0, 0.0, 0.0, 1000.0);
    let (cols, rows) = compute(100.0, 800.0, cell_for(dims), margin, 2.0);
    assert_eq!(cols, MIN_COLS);
    assert_eq!(rows, MIN_LINES);
}

#[test]
fn test_context_dimension_update_scale_refreshes_scaled_font_size() {
    let dims = TextDimensions {
        width: 8.0,
        height: 16.0,
        scale: 1.0,
    };
    let cell = cell_for(dims);
    let mut context =
        ContextDimension::build(800.0, 600.0, dims, cell, 1.0, 14.0, Margin::all(10.0));

    context.update_scale(2.0);

    assert_eq!(context.dimension.scale, 2.0);
    assert_eq!(context.scaled_font_size, 28.0);
    // Width/height-derived layout is recomputed separately after the new
    // cell metrics are installed, so update_scale itself must not clobber
    // the current row/column counts.
    assert_eq!(context.columns, 97);
    assert_eq!(context.lines, 36);
}

#[test]
fn test_context_dimension_build() {
    let dims = TextDimensions {
        width: 16.0,
        height: 33.0,
        scale: 2.0,
    };
    let cd = ContextDimension::build(
        1650.0,
        825.0,
        dims,
        cell_for(dims),
        1.0,
        14.0,
        Margin::all(0.0),
    );
    assert_eq!(cd.columns, 103);
    assert_eq!(cd.lines, 25);
}

#[test]
fn test_context_dimension_update_width() {
    let dims = TextDimensions {
        width: 16.0,
        height: 33.0,
        scale: 2.0,
    };
    let mut cd = ContextDimension::build(
        1600.0,
        825.0,
        dims,
        cell_for(dims),
        1.0,
        14.0,
        Margin::all(0.0),
    );
    assert_eq!(cd.columns, 100);

    cd.update_width(800.0);
    assert_eq!(cd.columns, 50);
    assert_eq!(cd.lines, 25);
}

#[test]
fn test_context_dimension_update_height() {
    let dims = TextDimensions {
        width: 16.0,
        height: 33.0,
        scale: 2.0,
    };
    let mut cd = ContextDimension::build(
        1600.0,
        825.0,
        dims,
        cell_for(dims),
        1.0,
        14.0,
        Margin::all(0.0),
    );
    assert_eq!(cd.lines, 25);

    cd.update_height(660.0);
    assert_eq!(cd.lines, 20);
    assert_eq!(cd.columns, 100);
}

#[test]
fn test_context_dimension_update_dimensions() {
    let dims = TextDimensions {
        width: 16.0,
        height: 33.0,
        scale: 1.0,
    };
    let mut cd = ContextDimension::build(
        1600.0,
        825.0,
        dims,
        cell_for(dims),
        1.0,
        14.0,
        Margin::all(0.0),
    );
    assert_eq!(cd.lines, 25);

    let new_dims = TextDimensions {
        width: 16.0,
        height: 66.0,
        scale: 1.0,
    };
    cd.update_dimensions(new_dims, cell_for(new_dims));
    assert_eq!(cd.lines, 12); // 825/66 = 12.5 → 12
}
