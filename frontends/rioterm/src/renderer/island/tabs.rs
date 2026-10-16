// Copyright (c) 2023-present, Raphael Amorim.
// Licensed under the MIT license in the root of this source tree.

use super::*;
use crate::renderer::window_chrome::{alpha, fade, snap};
use rio_backend::config::navigation::Animation;
use std::time::Duration;

fn flag(value: bool) -> f32 {
    u8::from(value) as f32
}

const SETTLE_TIME: f32 = 0.18;
const SHRINK_DELAY: Duration = Duration::from_millis(120);
const CLOSE_SPACE: f32 = CLOSE_HOVER_HALF * 2.0;
const TITLE_END_INSET: f32 =
    CLOSE_MARGIN_RIGHT + CLOSE_HOVER_HALF + TAB_GAP / 2.0 + TAB_ICON_GAP;

struct Position {
    target: f32,
    offset: Spring,
}

impl Position {
    fn new(value: f32) -> Self {
        Self {
            target: value,
            offset: Spring::new(),
        }
    }

    fn value(&self) -> f32 {
        self.target + self.offset.position
    }

    fn target(&mut self, target: f32) {
        self.offset.position += self.target - target;
        self.target = target;
    }

    fn advance(&mut self, dt: f32, scale: f32, animation: Animation) -> bool {
        let settle_time = match animation {
            Animation::None => {
                self.offset.reset();
                return false;
            }
            Animation::Medium => 0.12,
            Animation::Full => SETTLE_TIME,
        };
        self.offset.settle(dt, settle_time, scale)
    }

    fn nearly_settled(&self, scale: f32) -> bool {
        self.offset.position.abs() * scale < 0.5
            && self.offset.velocity.abs() * scale < 0.5 * 6.0 / SETTLE_TIME
    }
}

struct Tab {
    content: TabPresentation,
    left: Position,
    width: Position,
    opacity: f32,
    hover: f32,
    live: bool,
    preferred_width: f32,
    measured_width: f32,
    pending_shrink: Option<Instant>,
    measurement: Option<[f32; 4]>,
}

struct Selection {
    id: usize,
    left: Position,
    width: Position,
    travelling: bool,
    wrap_period: f32,
    target_offset: f32,
}

#[derive(Default)]
pub(super) struct Tabs {
    tabs: FxHashMap<usize, Tab>,
    order: Vec<usize>,
    selection: Option<Selection>,
    floating: Option<usize>,
    elevation: f32,
    animating: bool,
    snap_reorder: bool,
    navigation: Option<(usize, usize, i8)>,
}

impl Tabs {
    pub(super) fn invalidate_measurements(&mut self) {
        for tab in self.tabs.values_mut() {
            tab.measurement = None;
        }
    }
    pub(super) fn pointer_target(&self, x: Option<f32>) -> Option<(usize, bool)> {
        let x = x?;
        let floating = self.floating.filter(|id| self.order.contains(id));
        floating
            .iter()
            .chain(self.order.iter().rev().filter(|id| Some(**id) != floating))
            .find_map(|id| {
                let tab = &self.tabs[id];
                let left = tab.left.value();
                let width = tab.width.value();
                (x >= left && x < left + width).then(|| {
                    let (ix, _, iw, _, _) = island_rect(left, width);
                    let close = floating != Some(*id)
                        && close_button_center(ix, iw)
                            .is_some_and(|cx| (x - cx).abs() <= CLOSE_HIT_HALF_WIDTH);
                    (*id, close)
                })
            })
    }
    pub(super) fn next_frame_in(
        &self,
        scale: f32,
        hovered: Option<f32>,
    ) -> Option<Duration> {
        if self.animating {
            let geometry_settled =
                self.tabs.values().all(|tab| {
                    tab.left.nearly_settled(scale) && tab.width.nearly_settled(scale)
                }) && self.selection.as_ref().is_none_or(|selection| {
                    selection.left.nearly_settled(scale)
                        && selection.width.nearly_settled(scale)
                });
            let hovered_id = self.pointer_target(hovered).map(|(id, _)| id);
            let fades_settled = self.tabs.iter().all(|(&id, tab)| {
                (tab.opacity - flag(tab.live)).abs() < 0.01
                    && (tab.hover - flag(Some(id) == hovered_id)).abs() < 0.01
            }) && self.elevation < 0.01;
            if geometry_settled && fades_settled {
                return Some(Duration::from_millis(33));
            }
            return Some(Duration::ZERO);
        }
        let now = Instant::now();
        self.tabs
            .values()
            .filter_map(|tab| {
                tab.pending_shrink
                    .map(|at| (at + SHRINK_DELAY).saturating_duration_since(now))
            })
            .min()
    }

    pub(super) fn move_tab(&mut self, from: usize, to: usize) {
        if from < self.order.len() && to < self.order.len() {
            let id = self.order.remove(from);
            self.order.insert(to, id);
        }
    }

    pub(super) fn swap_tabs(&mut self, a: usize, b: usize) {
        if a < self.order.len() && b < self.order.len() {
            self.order.swap(a, b);
            self.snap_reorder = a.abs_diff(b) > 1;
            if self.snap_reorder {
                self.floating = None;
                self.elevation = 0.0;
            } else {
                self.floating = Some(self.order[b]);
            }
        }
    }

    pub(super) fn release(&mut self, index: usize, left: f32) {
        if let Some(tab) = self.order.get(index).and_then(|id| self.tabs.get_mut(id)) {
            tab.left.offset.position = left - tab.left.target;
            tab.left.offset.velocity = 0.0;
            self.animating = true;
        }
    }

    fn target_layout(
        &self,
        width: f32,
        scale: f32,
        min: f32,
        max: f32,
    ) -> TabStripLayout {
        let mut layout = tab_strip_layout(width, scale, self.order.len(), f32::MAX);
        let available = layout.tabs_width;
        layout.widths = self
            .order
            .iter()
            .map(|id| self.tabs[id].preferred_width.clamp(min, max))
            .collect();
        let desired: f32 = layout.widths.iter().sum();
        if desired > available {
            let minimum = min * self.order.len() as f32;
            if minimum < available {
                let shrink = (desired - available) / (desired - minimum);
                for width in &mut layout.widths {
                    *width -= (*width - min) * shrink;
                }
            } else {
                layout
                    .widths
                    .fill(available / self.order.len().max(1) as f32);
            }
        }
        layout.tabs_width = layout.widths.iter().sum();
        layout.tab_width = layout.tabs_width / self.order.len().max(1) as f32;
        layout
    }

    pub(super) fn layout(
        &self,
        width: f32,
        scale: f32,
        context_manager: &ContextManager<EventProxy>,
        max: f32,
    ) -> TabStripLayout {
        let count = context_manager.len();
        let mut layout = tab_strip_layout(width, scale, count, max);
        layout.topmost = self
            .floating
            .and_then(|id| (0..count).find(|&index| context_manager.tab_id(index) == id));
        let mut right = layout.left_margin;
        for index in 0..count {
            let tab = self.tabs.get(&context_manager.tab_id(index));
            let left = tab.map_or(layout.left_margin, |t| t.left.value());
            let width = tab.map_or(0.0, |t| t.width.value().max(0.0));
            layout.positions.push(left);
            layout.widths.push(width);
            right = right.max(left + width);
        }
        layout.tabs_width = right - layout.left_margin;
        layout
    }

    #[allow(clippy::too_many_arguments)]
    fn update(
        &mut self,
        contents: Vec<TabPresentation>,
        sugarloaf: &mut Sugarloaf,
        dimensions: (f32, f32),
        limits: (f32, f32),
        icon_size: f32,
        selected: usize,
        hovered: Option<f32>,
        drag: Option<usize>,
        dt: f32,
        now: Instant,
        animation: Animation,
    ) {
        let (window_width, scale) = dimensions;
        let (min, max) = limits;
        let initial = self.order.is_empty();
        let navigation = self.navigation.take();
        // With two tabs, either move reaches an endpoint. Only crossing the
        // edge in the requested direction is a circular move.
        let wrap_direction = navigation
            .filter(|(from, to, direction)| {
                self.order.len() > 1
                    && ((*direction < 0 && *from == 0 && *to + 1 == self.order.len())
                        || (*direction > 0 && *from + 1 == self.order.len() && *to == 0))
            })
            .map_or(0, |(_, _, direction)| direction);
        if wrap_direction != 0
            && self.selection.as_ref().is_some_and(|s| s.id == selected)
        {
            self.snap_reorder = true;
            self.floating = None;
            self.elevation = 0.0;
        }
        let measurement = Some([scale, min, max, icon_size]);
        let opts = DrawOpts {
            font_size: snap(TITLE_FONT_SIZE, scale),
            font_id: Some(rio_backend::sugarloaf::font::FONT_ID_REGULAR),
            ..DrawOpts::default()
        };
        for tab in self.tabs.values_mut() {
            tab.live = false;
        }
        self.order.clear();
        for content in contents {
            let id = content.id;
            self.order.push(id);
            let existing = self.tabs.get(&id);
            let changed = existing.is_none_or(|tab| {
                tab.measurement != measurement
                    || tab.content.title != content.title
                    || tab.content.remote != content.remote
                    || tab.content.bell != content.bell
            });
            let measured = if changed {
                let padding = tab_title_padding(content.remote, icon_size)
                    + TAB_TITLE_PADDING_RIGHT
                    + if content.bell {
                        BELL_DOT_SIZE + BELL_GAP
                    } else {
                        0.0
                    };
                ((sugarloaf.text_mut().measure(&content.title, &opts) + padding) * scale)
                    .ceil()
                    / scale
            } else {
                existing.unwrap().measured_width
            }
            .clamp(min, max);
            match self.tabs.entry(id) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let tab = entry.get_mut();
                    tab.content = content;
                    tab.live = true;
                    tab.measurement = measurement;
                    if animation == Animation::None || measured >= tab.preferred_width {
                        tab.preferred_width = measured;
                        tab.pending_shrink = None;
                    } else {
                        if measured != tab.measured_width || tab.pending_shrink.is_none()
                        {
                            tab.pending_shrink = Some(now);
                        }
                        if tab
                            .pending_shrink
                            .is_some_and(|at| now.duration_since(at) >= SHRINK_DELAY)
                        {
                            tab.preferred_width = measured;
                            tab.pending_shrink = None;
                        }
                    }
                    tab.measured_width = measured;
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Tab {
                        content,
                        left: Position::new(0.0),
                        width: Position::new(
                            if initial || animation == Animation::None {
                                measured
                            } else {
                                0.0
                            },
                        ),
                        opacity: if initial || animation != Animation::Full {
                            1.0
                        } else {
                            0.0
                        },
                        hover: 0.0,
                        live: true,
                        preferred_width: measured,
                        measured_width: measured,
                        pending_shrink: None,
                        measurement,
                    });
                }
            }
        }
        let target = self.target_layout(window_width, scale, min, max);
        let mut moving = false;
        let drag_id = drag.and_then(|i| self.order.get(i)).copied();
        let mut left = target.left_margin;
        for (i, id) in self.order.iter().enumerate() {
            let tab = self.tabs.get_mut(id).unwrap();
            if initial || self.snap_reorder {
                tab.left = Position::new(left);
                tab.width = Position::new(target.width(i));
            } else if tab.left.target == 0.0 {
                tab.left = Position::new(left);
            }
            tab.left.target(left);
            tab.width.target(target.width(i));
            left += target.width(i);
        }
        self.snap_reorder = false;
        for tab in self.tabs.values_mut() {
            if !tab.live {
                tab.width.target(0.0);
                tab.pending_shrink = None;
            }
            moving |= tab.left.advance(dt, scale, animation)
                | tab.width.advance(dt, scale, animation);
        }
        let hovered_id = self.pointer_target(hovered).map(|(id, _)| id);
        for (&id, tab) in &mut self.tabs {
            fade(&mut tab.opacity, flag(tab.live), dt, animation);
            fade(
                &mut tab.hover,
                flag(Some(id) == hovered_id && drag_id.is_none()),
                dt,
                animation,
            );
            moving |= tab.opacity != flag(tab.live)
                || tab.hover != flag(Some(id) == hovered_id && drag_id.is_none());
        }
        self.tabs.retain(|_, tab| tab.live || tab.opacity > 0.0);
        let selected_tab = &self.tabs[&selected];
        let x = selected_tab.left.value();
        let w = selected_tab.width.value();
        let selection = self.selection.get_or_insert_with(|| Selection {
            id: selected,
            left: Position::new(x),
            width: Position::new(w),
            travelling: false,
            wrap_period: 0.0,
            target_offset: 0.0,
        });
        let interrupted = selection.travelling;
        if selection.id != selected || wrap_direction != 0 {
            if selection.wrap_period > 0.0 {
                let cycles = ((selection.left.value() - target.left_margin)
                    / selection.wrap_period)
                    .floor();
                selection.left.target -= cycles * selection.wrap_period;
                selection.target_offset -= cycles * selection.wrap_period;
            }
            selection.travelling = !initial;
            selection.id = selected;
            if wrap_direction != 0 {
                selection.wrap_period = target.tabs_width;
                selection.target_offset += wrap_direction as f32 * target.tabs_width;
            } else if navigation.is_none() {
                // A direct click or close can interrupt a circular move.
                selection.wrap_period = 0.0;
                selection.target_offset = 0.0;
            }
        }
        if selection.travelling {
            if interrupted && navigation.is_some() && selection.wrap_period > 0.0 {
                // Repeated input replaces unfinished circular travel. Aim at
                // the nearest copy of the latest tab instead of queuing laps.
                selection.target_offset = ((selection.left.value()
                    - selected_tab.left.target)
                    / selection.wrap_period)
                    .round()
                    * selection.wrap_period;
            }
            selection
                .left
                .target(selected_tab.left.target + selection.target_offset);
            selection.width.target(selected_tab.width.target);
            selection.travelling = selection.left.advance(dt, scale, animation)
                | selection.width.advance(dt, scale, animation);
            moving |= selection.travelling;
            if !selection.travelling {
                selection.left = Position::new(x);
                selection.wrap_period = 0.0;
                selection.target_offset = 0.0;
            }
        } else {
            selection.left = Position::new(x);
            selection.width = Position::new(w);
        }
        if let Some(id) = drag_id {
            self.floating = Some(id);
        }
        fade(&mut self.elevation, flag(drag_id.is_some()), dt, animation);
        if drag_id.is_none()
            && self.elevation == 0.0
            && self.floating.is_some_and(|id| {
                self.tabs
                    .get(&id)
                    .is_none_or(|t| t.left.offset.position == 0.0)
            })
        {
            self.floating = None;
        }
        if self
            .floating
            .is_some_and(|id| !self.tabs.contains_key(&id) || id != selected)
        {
            self.floating = None;
            self.elevation = 0.0;
        }
        moving |= self.elevation != flag(drag_id.is_some());
        self.animating = moving;
    }
}

impl Island {
    pub fn animate_tab_navigation(&mut self, from: usize, to: usize, direction: i8) {
        if from != to {
            self.tabs.navigation = Some((from, to, direction));
        }
    }
    /// Reorder thresholds use destination slots, while clicks use displayed geometry.
    pub fn drag_layout(&self, width: f32, scale: f32, count: usize) -> TabStripLayout {
        if self.tabs.order.len() != count {
            return tab_strip_layout(width, scale, count, self.max_tab_width);
        }
        self.tabs
            .target_layout(width, scale, self.min_tab_width, self.max_tab_width)
    }

    pub(super) fn render_tabs(
        &mut self,
        sugarloaf: &mut Sugarloaf,
        window_width: f32,
        scale: f32,
        context_manager: &ContextManager<EventProxy>,
        bg: [f32; 4],
    ) {
        self.sync_focused_program_status(context_manager);
        let count = context_manager.len();
        #[cfg(not(target_os = "windows"))]
        {
            self.window_header.toolbar_enabled = !(self.hide_if_single && count == 1);
        }
        self.window_header.render(
            sugarloaf,
            window_width / scale,
            self.active_text_color,
            self.animation,
        );
        self.window_progress_visible = self.uses_window_progress(count);
        if self.hide_if_single && count == 1 {
            self.tabs = Tabs::default();
            self.drag = None;
            let y = if cfg!(any(target_os = "windows", target_os = "macos")) {
                ISLAND_HEIGHT
            } else {
                0.0
            };
            self.render_progress_bar(sugarloaf, window_width, scale, y);
            return;
        }
        let selected = context_manager.current_index();
        if count == 1
            || self.drag.as_ref().is_some_and(|d| {
                d.tab_index != selected
                    || self
                        .tabs
                        .order
                        .get(d.tab_index)
                        .is_some_and(|id| *id != context_manager.tab_id(selected))
            })
        {
            self.drag = None;
        }
        let now = Instant::now();
        let dt = if self.tabs.animating {
            now.duration_since(self.last_anim_frame)
                .as_secs_f32()
                .min(DRAG_MAX_DT)
        } else {
            0.0
        };
        self.last_anim_frame = now;
        let contents = (0..count)
            .map(|index| {
                let title = self.get_title_for_tab(context_manager, index);
                let status = self.status_for_tab(context_manager, index);
                let (icon, remote) = context_manager
                    .application_for_tab(index)
                    .map(|app| {
                        let (icon, transport) = self.app_icons.resolve(
                            &app.program,
                            &app.title,
                            status.as_ref(),
                            app.reported,
                        );
                        (icon, app.remote || transport)
                    })
                    .unwrap_or((super::super::app_icons::DEFAULT_ICON, false));
                TabPresentation {
                    id: context_manager.tab_id(index),
                    custom_color: context_manager.custom_color(index),
                    title: Self::status_title(status.as_ref(), &title),
                    status,
                    icon,
                    remote,
                    bell: context_manager.bell(index),
                }
            })
            .collect();
        self.tabs.update(
            contents,
            sugarloaf,
            (window_width, scale),
            (self.min_tab_width, self.max_tab_width),
            self.tab_icon_size,
            context_manager.tab_id(selected),
            self.hover_x,
            self.drag_index(),
            dt,
            now,
            self.animation,
        );
        let floating_left = self
            .drag_index()
            .and_then(|_| {
                self.drag_floating_left(&self.drag_layout(window_width, scale, count))
            })
            .or_else(|| {
                self.tabs
                    .floating
                    .and_then(|id| self.tabs.tabs.get(&id).map(|t| t.left.value()))
            });
        let floating = self.tabs.floating;
        let occlusion = floating_left.and_then(|left| {
            floating.map(|id| {
                let (x, _, w, _, _) =
                    island_rect(left, self.tabs.tabs[&id].width.value());
                [x, x + w]
            })
        });
        let fills = tab_fills(bg);
        let bounds = tab_strip_layout(window_width, scale, count, f32::MAX);
        let right = bounds.left_margin + bounds.tabs_width;
        let title_y = crate::renderer::window_chrome::centered_text_y(
            sugarloaf,
            "Mg",
            TAB_CENTER_Y + 2.0,
            &DrawOpts {
                font_size: snap(TITLE_FONT_SIZE, scale),
                font_id: Some(rio_backend::sugarloaf::font::FONT_ID_REGULAR),
                ..DrawOpts::default()
            },
        );
        // Exiting presentations have no input target and paint beneath live tabs.
        for tab in self.tabs.tabs.values().filter(|t| !t.live) {
            self.draw_tab(
                sugarloaf, tab, false, count, right, title_y, &fills, occlusion, None,
            );
        }
        if floating.is_none() {
            if let Some(selection) = &self.tabs.selection {
                let fill = self.tabs.tabs[&selection.id]
                    .content
                    .custom_color
                    .unwrap_or(fills.active);
                let period = selection.wrap_period;
                let copies = [0.0, -period, period];
                // A closing strip contracts before the pill settles. Clip to
                // its width only for circular copies, preserving rounded ends.
                let clip = [
                    bounds.left_margin,
                    0.0,
                    if period > 0.0 {
                        period.min(bounds.tabs_width)
                    } else {
                        bounds.tabs_width
                    },
                    ISLAND_HEIGHT,
                ];
                sugarloaf.with_shape_clip(clip, |sugarloaf| {
                    for offset in copies.iter().take(if period > 0.0 { 3 } else { 1 }) {
                        let (x, y, w, h, radius) = island_rect(
                            selection.left.value() + offset,
                            selection.width.value(),
                        );
                        if x + w > clip[0] && x < clip[0] + clip[2] {
                            draw_island(
                                sugarloaf,
                                x,
                                y,
                                w,
                                h,
                                radius,
                                fill,
                                self.tab_glass_intensity,
                                0,
                            );
                        }
                    }
                });
            }
        }
        for id in &self.tabs.order {
            if Some(*id) != floating {
                self.draw_tab(
                    sugarloaf,
                    &self.tabs.tabs[id],
                    *id == context_manager.tab_id(selected),
                    count,
                    right,
                    title_y,
                    &fills,
                    occlusion,
                    None,
                );
            }
        }
        if let (Some(id), Some(left)) = (floating, floating_left) {
            self.draw_tab(
                sugarloaf,
                &self.tabs.tabs[&id],
                true,
                count,
                right,
                title_y,
                &fills,
                None,
                Some(left),
            );
        }
        if let Some(index) = self.color_picker_tab.filter(|i| *i < count) {
            let layout = self.tab_layout(window_width, scale, count, context_manager);
            self.render_color_picker(
                sugarloaf,
                layout.left(index),
                layout.width(index),
                context_manager.custom_color(index),
            );
        }
        if self.window_progress_visible {
            self.render_progress_bar(sugarloaf, window_width, scale, ISLAND_HEIGHT);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_tab(
        &self,
        sugarloaf: &mut Sugarloaf,
        tab: &Tab,
        selected: bool,
        count: usize,
        right: f32,
        title_y: f32,
        fills: &IslandFills,
        occlusion: Option<[f32; 2]>,
        floating: Option<f32>,
    ) {
        let left = floating.unwrap_or_else(|| tab.left.value());
        let width = tab.width.value().min((right - left).max(0.0));
        let (x, y, w, h, radius) = island_rect(left, width);
        if w <= 0.0 || tab.opacity == 0.0 {
            return;
        }
        let order = if floating.is_some() { 11 } else { 0 };
        let text_start = sugarloaf.text_mut().instances().len();
        let color = alpha(
            if selected {
                self.active_text_color
            } else {
                self.inactive_text_color
            },
            tab.opacity,
        );
        let fill = if floating.is_some() {
            sugarloaf.rounded_rect(
                None,
                x - 2.0,
                y - 1.0,
                w + 4.0,
                h + 3.0,
                [0.0, 0.0, 0.0, self.tabs.elevation * 0.18],
                0.05,
                radius + 2.0,
                order,
            );
            Some(tab.content.custom_color.unwrap_or(fills.active))
        } else if !selected {
            Some(
                tab.content
                    .custom_color
                    .map(|c| alpha(c, INACTIVE_CUSTOM_MUTE))
                    .unwrap_or_else(|| alpha(fills.active, tab.hover * 0.35)),
            )
        } else {
            None
        };
        if let Some(fill) = fill.filter(|f| f[3] > 0.0) {
            draw_island(
                sugarloaf,
                x,
                y,
                w,
                h,
                radius,
                alpha(fill, tab.opacity),
                self.tab_glass_intensity
                    * if floating.is_some() {
                        1.0
                    } else {
                        tab.hover * 0.3
                    },
                order,
            );
        }
        if let Some(color) = tab
            .content
            .status
            .as_ref()
            .and_then(|s| self.status_color(s))
        {
            sugarloaf.rounded_rect(
                None,
                x,
                ISLAND_HEIGHT - PROGRESS_BAR_HEIGHT,
                w,
                PROGRESS_BAR_HEIGHT,
                alpha(color, tab.opacity),
                0.0,
                PROGRESS_BAR_HEIGHT / 2.0,
                order + 1,
            );
        }
        let padding = tab_title_padding(tab.content.remote, self.tab_icon_size);
        let bell_width = if tab.content.bell {
            BELL_DOT_SIZE + BELL_GAP
        } else {
            0.0
        };
        let end_inset = if count > 1 && tab.hover > 0.0 && floating.is_none() {
            TITLE_END_INSET
        } else {
            TAB_TITLE_PADDING_RIGHT
        };
        let budget = (width - padding - end_inset - bell_width).max(0.0);
        if budget >= TITLE_FONT_SIZE {
            let opts = DrawOpts {
                font_size: snap(TITLE_FONT_SIZE, sugarloaf.scale_factor()),
                font_id: Some(rio_backend::sugarloaf::font::FONT_ID_REGULAR),
                color: color_u8(color),
                ..DrawOpts::default()
            };
            let title = fit_title_to_width(sugarloaf, &tab.content.title, budget, &opts);
            let text_x = snap(left + padding, sugarloaf.scale_factor());
            if tab.content.bell
                && !occlusion.is_some_and(|[start, end]| {
                    text_x + BELL_DOT_SIZE > start && text_x < end
                })
            {
                sugarloaf.rounded_rect(
                    None,
                    text_x,
                    title_y + TITLE_FONT_SIZE * (2.0 / 3.0) - BELL_DOT_SIZE / 2.0,
                    BELL_DOT_SIZE,
                    BELL_DOT_SIZE,
                    color,
                    0.0,
                    BELL_DOT_SIZE / 2.0,
                    order + 1,
                );
            }
            sugarloaf
                .text_mut()
                .draw(text_x + bell_width, title_y, &title, &opts);
        }
        self.render_tab_icons(sugarloaf, &tab.content, x, w, color, order + 1);
        if count > 1 && tab.live && floating.is_none() && tab.hover > 0.0 {
            if let Some(cx) = close_button_center(x, w) {
                let cx = snap(cx, sugarloaf.scale_factor());
                if occlusion.is_some_and(|[start, end]| {
                    cx + CLOSE_HOVER_HALF > start && cx - CLOSE_HOVER_HALF < end
                }) {
                    sugarloaf
                        .text_mut()
                        .exclude_horizontal_range_since(text_start, occlusion.unwrap());
                    return;
                }
                let hot = self
                    .hover_x
                    .is_some_and(|pointer| (pointer - cx).abs() <= CLOSE_HIT_HALF_WIDTH);
                if hot {
                    sugarloaf.rounded_rect(
                        None,
                        cx - CLOSE_HOVER_HALF,
                        TAB_CENTER_Y - CLOSE_HOVER_HALF,
                        CLOSE_SPACE,
                        CLOSE_SPACE,
                        alpha(fills.close_hover, tab.hover),
                        0.05,
                        CLOSE_HOVER_CORNER_RADIUS,
                        order + 1,
                    );
                }
                draw_close_button(sugarloaf, cx, alpha(color, tab.hover), hot, order + 2);
            }
        }
        if let Some(range) = occlusion {
            sugarloaf
                .text_mut()
                .exclude_horizontal_range_since(text_start, range);
        }
    }
}
