pub mod renderable;
pub mod title;

use crate::ansi::CursorShape;
use crate::context::title::{update_title, ContextTitle};
use crate::event::sync::FairMutex;
use crate::event::{Msg, RioEvent};
pub use crate::layout::{ContextDimension, ContextGrid, ContextGridItem};
use crate::messenger::Messenger;
use crate::performer::{self, Machine};
use renderable::Cursor;
use renderable::RenderableContent;
use rio_backend::config::layout::Margin;
use rio_backend::config::Shell;
use smallvec::{smallvec, SmallVec};

use rio_backend::crosswords::{Crosswords, MIN_COLUMNS, MIN_LINES};
use rio_backend::error::{RioError, RioErrorLevel, RioErrorType};
use rio_backend::event::EventListener;
use rio_backend::event::WindowId;
use rio_backend::selection::SelectionRange;
use rio_backend::sugarloaf::{font::SugarloafFont, Sugarloaf, SugarloafErrors};
use std::error::Error;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

// Global atomic counter for generating unique route IDs
static ROUTE_ID_COUNTER: AtomicUsize = AtomicUsize::new(1);

// Global atomic counter for generating unique rich text IDs
static RICH_TEXT_ID_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Generate a unique rich text ID for terminal contexts
pub fn next_rich_text_id() -> usize {
    RICH_TEXT_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[cfg(target_os = "windows")]
use teletypewriter::create_pty;
#[cfg(not(target_os = "windows"))]
use teletypewriter::{create_pty_with_fork, create_pty_with_spawn};

pub struct TabApplication {
    pub program: String,
    pub title: String,
    pub remote: bool,
    pub reported: bool,
}

pub struct Context<T: EventListener> {
    pub route_id: usize,
    pub terminal: Arc<FairMutex<Crosswords<T>>>,
    pub metadata: Arc<parking_lot::Mutex<rio_backend::crosswords::TerminalMetadata>>,
    pub renderable_content: RenderableContent,
    pub messenger: Messenger,
    #[cfg(not(target_os = "windows"))]
    pub main_fd: Arc<i32>,
    #[cfg(not(target_os = "windows"))]
    pub shell_pid: u32,
    #[cfg(not(target_os = "windows"))]
    child_terminator: teletypewriter::ChildTerminator,
    pub rich_text_id: usize,
    pub dimension: ContextDimension,
    pub title: ContextTitle,
    /// An OSC title change arrived while this pane was hidden: the
    /// render was skipped and must run when the pane surfaces.
    pub title_dirty: bool,
    /// Display name of the command this pane spawned (the configured
    /// shell or program), fixed for the pane's lifetime. Fills
    /// `{{ program }}` and the empty-title strip fallback without ever
    /// inspecting the foreground process.
    pub spawned_program: String,
    /// Present for panes launched directly through Rio's SSH connector.
    pub ssh: Option<crate::ssh::Connection>,
    ssh_session: Option<crate::ssh::native::PaneSession>,
    _io_thread: Option<JoinHandle<(Machine<teletypewriter::Pty, T>, performer::State)>>,
}

impl<T: rio_backend::event::EventListener> Drop for Context<T> {
    fn drop(&mut self) {
        self.terminal.lock().invalidate_clipboard_requests();
        // Shutdown the terminal's PTY.
        let _ = self.messenger.channel.send(Msg::Shutdown);
        // Also hang up synchronously: quit paths call process::exit
        // right after dropping routes, before the reader thread can run
        // its shutdown escalation. The handle is a no-op once the child
        // was reaped, so no stale PID is ever signaled.
        #[cfg(not(target_os = "windows"))]
        let _ = self.child_terminator.hangup();
    }
}

impl<T: EventListener> Context<T> {
    #[inline]
    pub fn set_selection(&mut self, selection_range: Option<SelectionRange>) {
        let old_selection = self.renderable_content.selection_range;
        let has_updated = old_selection != selection_range;

        if has_updated {
            let mut terminal = self.terminal.lock();
            // Only the old and new visible selection rows need repainting, not all text.
            let top = -(terminal.display_offset() as i32);
            let bottom = top + terminal.screen_lines() as i32 - 1;
            for selection in [old_selection, selection_range].into_iter().flatten() {
                for line in
                    selection.start.row.0.max(top)..=selection.end.row.0.min(bottom)
                {
                    terminal.grid[rio_backend::crosswords::pos::Line(line)].dirty = true;
                }
            }
            self.renderable_content
                .pending_update
                .set_terminal_damage(rio_backend::event::TerminalDamage::Partial);
        }

        self.renderable_content.selection_range = selection_range;
    }

    #[inline]
    pub fn cursor_from_ref(&self) -> Cursor {
        Cursor {
            state: self.renderable_content.cursor.state.new_from_self(),
            content: self.renderable_content.cursor.content,
        }
    }
}

#[derive(Clone, Default)]
pub struct ContextManagerConfig {
    /// Build contexts without spawning a PTY (see
    /// `create_dead_context`). Unit tests fork one real `$SHELL` per
    /// context otherwise, which is slow and flaky under the parallel
    /// test runner (fork failures surface as random test panics).
    #[cfg(test)]
    pub dead_pty: bool,
    pub shell: Shell,
    pub ssh: Option<crate::ssh::Connection>,
    #[cfg(not(target_os = "windows"))]
    pub use_fork: bool,
    pub shell_integration: bool,
    pub working_dir: Option<String>,
    pub spawn_performer: bool,
    pub cwd: bool,
    pub panel: rio_backend::config::layout::Panel,
    pub divider_width: f32,
    pub title: rio_backend::config::title::Title,
    pub keyboard: rio_backend::config::keyboard::Keyboard,
    pub scrollback_history_limit: usize,
    pub scrollback_history_byte_limit: Option<usize>,
    pub grapheme_clustering: bool,
    pub clipboard: rio_backend::clipboard::ClipboardPolicy,
}

const DEFAULT_CONTEXT_CAPACITY: usize = 28;

pub struct ContextManager<T: EventListener> {
    contexts: SmallVec<[ContextGrid<T>; DEFAULT_CONTEXT_CAPACITY]>,
    current_index: usize,
    current_route: usize,
    #[allow(unused)]
    capacity: usize,
    event_proxy: T,
    window_id: WindowId,
    pub config: ContextManagerConfig,
}

/// Display name for the command a pane spawns: the configured program,
/// else the user's shell (what the PTY spawn itself falls back to),
/// reduced to its basename.
fn spawned_program_name(config: &ContextManagerConfig) -> String {
    let program = match config.shell.program.as_deref() {
        Some(program) if !program.is_empty() => program.to_string(),
        _ => {
            #[cfg(unix)]
            {
                teletypewriter::default_shell_program()
            }
            #[cfg(not(unix))]
            {
                String::from("powershell")
            }
        }
    };
    std::path::Path::new(&program)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or(program)
}

pub fn create_dead_context<T: rio_backend::event::EventListener>(
    event_proxy: T,
    window_id: WindowId,
    route_id: usize,
    rich_text_id: usize,
    dimension: ContextDimension,
) -> Context<T> {
    let terminal = Crosswords::new(
        dimension,
        CursorShape::Block,
        event_proxy,
        window_id,
        route_id,
        // Dead context never sees new input — no scrollback needed.
        0,
    );
    let metadata = terminal.metadata.clone();
    let terminal: Arc<FairMutex<Crosswords<T>>> = Arc::new(FairMutex::new(terminal));
    let (sender, _receiver) = corcovado::channel::channel();

    Context {
        route_id,
        #[cfg(not(target_os = "windows"))]
        main_fd: Arc::new(-1),
        #[cfg(not(target_os = "windows"))]
        shell_pid: 1,
        #[cfg(not(target_os = "windows"))]
        child_terminator: teletypewriter::ChildTerminator::retired(),
        messenger: Messenger::new(sender),
        renderable_content: RenderableContent::new(Cursor::default()),
        metadata,
        terminal,
        rich_text_id,
        dimension,
        title: ContextTitle::default(),
        title_dirty: false,
        spawned_program: String::new(),
        ssh: None,
        ssh_session: None,
        _io_thread: None,
    }
}

#[cfg(test)]
pub fn create_mock_context<
    T: rio_backend::event::EventListener + Clone + std::marker::Send + 'static,
>(
    event_proxy: T,
    window_id: WindowId,
    rich_text_id: usize,
    dimension: ContextDimension,
) -> Context<T> {
    let config = ContextManagerConfig {
        dead_pty: true,
        ..ContextManagerConfig::default()
    };
    ContextManager::create_context(
        (&Cursor::default(), false),
        event_proxy.clone(),
        window_id,
        rich_text_id,
        dimension,
        &config,
    )
    .unwrap()
}

/// Where `current_index` lands after removing the tab at `removed`:
/// the focused tab falls back to its left neighbor, and a background
/// removal shifts the focused index left only when the removed tab
/// sat before it. Pure, so the arithmetic the tmux SIGHUP crash
/// hinged on stays testable without a GPU.
#[inline]
fn current_index_after_tab_removal(current: usize, removed: usize) -> usize {
    if removed <= current {
        current.saturating_sub(1)
    } else {
        current
    }
}

impl<T: EventListener + Clone + std::marker::Send + 'static> ContextManager<T> {
    #[inline]
    fn create_context(
        cursor_state: (&Cursor, bool),
        event_proxy: T,
        window_id: WindowId,
        rich_text_id: usize,
        dimension: ContextDimension,
        config: &ContextManagerConfig,
    ) -> Result<Context<T>, Box<dyn Error>> {
        #[cfg(feature = "profiling")]
        let _timing = crate::profiling::span("startup.context");
        let route_id = ROUTE_ID_COUNTER.fetch_add(1, Ordering::SeqCst);

        #[cfg(test)]
        if config.dead_pty {
            return Ok(create_dead_context(
                event_proxy,
                window_id,
                route_id,
                rich_text_id,
                dimension,
            ));
        }

        let cols: u16 = dimension.columns.try_into().unwrap_or(MIN_COLUMNS as u16);
        let rows: u16 = dimension.lines.try_into().unwrap_or(MIN_LINES as u16);
        #[cfg(not(target_os = "windows"))]
        let initial_winsize = crate::renderer::utils::terminal_dimensions(&dimension);

        let mut terminal = Crosswords::new(
            dimension,
            CursorShape::from_char(cursor_state.0.content),
            event_proxy.clone(),
            window_id,
            route_id,
            config.scrollback_history_limit,
        );
        terminal.set_grapheme_clustering(config.grapheme_clustering);
        terminal.set_clipboard_policy(config.clipboard);
        terminal.graphics.coalesce_updates = true;
        terminal.set_scrollback_limits(
            config.scrollback_history_limit,
            config.scrollback_history_byte_limit,
        );
        terminal.blinking_cursor = cursor_state.1;
        let metadata = terminal.metadata.clone();
        let terminal: Arc<FairMutex<Crosswords<T>>> = Arc::new(FairMutex::new(terminal));

        let mut integration =
            if crate::ssh::Connection::from_shell(&config.shell).is_some() {
                crate::ssh::native::prepare_shell(&config.shell)
            } else if config.shell_integration {
                crate::shell_integration::prepare(
                    config.shell.program.as_deref(),
                    &config.shell.args,
                )
            } else {
                crate::shell_integration::SpawnIntegration::default()
            };
        let ssh_session = crate::ssh::native::PaneSession::new(route_id);
        if let Some(session) = &ssh_session {
            integration.env.push(session.env());
        }
        let (shell_program, shell_args) = match &integration.command {
            Some((program, args)) => (program.as_deref(), args.as_slice()),
            None => (
                config.shell.program.as_deref(),
                config.shell.args.as_slice(),
            ),
        };

        let pty;
        #[cfg(not(target_os = "windows"))]
        {
            if config.use_fork {
                tracing::info!("rio -> teletypewriter: create_pty_with_fork");
                pty = match create_pty_with_fork(
                    shell_program,
                    shell_args,
                    &config.working_dir,
                    &integration.env,
                    cols,
                    rows,
                    initial_winsize.width,
                    initial_winsize.height,
                ) {
                    Ok(created_pty) => created_pty,
                    Err(err) => {
                        tracing::error!("{err:?}");
                        return Err(Box::new(err));
                    }
                }
            } else {
                tracing::info!("rio -> teletypewriter: create_pty_with_spawn");
                pty = match create_pty_with_spawn(
                    shell_program,
                    shell_args.to_vec(),
                    &config.working_dir,
                    (!integration.env.is_empty()).then(|| integration.env.clone()),
                    cols,
                    rows,
                    initial_winsize.width,
                    initial_winsize.height,
                ) {
                    Ok(created_pty) => created_pty,
                    Err(err) => {
                        tracing::error!("{err:?}");
                        return Err(Box::new(err));
                    }
                }
            };
        }

        #[cfg(not(target_os = "windows"))]
        let main_fd = pty.child.id.clone();
        #[cfg(not(target_os = "windows"))]
        let shell_pid = *pty.child.pid.clone() as u32;
        #[cfg(not(target_os = "windows"))]
        let child_terminator = pty.child.terminator();

        #[cfg(target_os = "windows")]
        {
            #[cfg(feature = "profiling")]
            let _timing = crate::profiling::span("startup.conpty");
            pty = match create_pty(
                shell_program,
                shell_args.to_vec(),
                &config.working_dir,
                (!integration.env.is_empty()).then(|| integration.env.clone()),
                cols,
                rows,
            ) {
                Ok(created_pty) => created_pty,
                Err(err) => {
                    tracing::error!("{err:?}");
                    return Err(Box::new(err));
                }
            }
        }

        let machine = Machine::new(
            Arc::clone(&terminal),
            pty,
            event_proxy.clone(),
            window_id,
            route_id,
        )?;
        let channel = machine.channel();
        let io_thread = if config.spawn_performer {
            Some(machine.spawn())
        } else {
            None
        };

        let messenger = Messenger::new(channel);

        let mut context = Context {
            route_id,
            #[cfg(not(target_os = "windows"))]
            main_fd,
            #[cfg(not(target_os = "windows"))]
            shell_pid,
            #[cfg(not(target_os = "windows"))]
            child_terminator,
            messenger,
            metadata,
            terminal,
            rich_text_id,
            renderable_content: RenderableContent::new(cursor_state.0.clone()),
            dimension,
            ssh: config
                .ssh
                .clone()
                .or_else(|| crate::ssh::Connection::from_shell(&config.shell)),
            ssh_session,
            title: ContextTitle::default(),
            title_dirty: false,
            spawned_program: spawned_program_name(config),
            _io_thread: io_thread,
        };
        context.title = ContextTitle {
            content: update_title(&config.title.content, &context, None),
        };
        Ok(context)
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        cursor_state: (&Cursor, bool),
        event_proxy: T,
        window_id: WindowId,
        route_id: usize,
        rich_text_id: usize,
        ctx_config: ContextManagerConfig,
        size: ContextDimension,
        scaled_margin: Margin,
        sugarloaf_errors: Option<SugarloafErrors>,
    ) -> Result<Self, Box<dyn Error>> {
        let initial_context = match ContextManager::create_context(
            cursor_state,
            event_proxy.clone(),
            window_id,
            rich_text_id,
            size,
            &ctx_config,
        ) {
            Ok(context) => context,
            Err(err_message) => {
                tracing::error!("{:?}", err_message);

                event_proxy.send_event(
                    RioEvent::ReportToAssistant(RioError {
                        report: RioErrorType::InitializationError(
                            err_message.to_string(),
                        ),
                        level: RioErrorLevel::Error,
                    }),
                    window_id,
                );

                create_dead_context(
                    event_proxy.clone(),
                    window_id,
                    route_id,
                    0,
                    ContextDimension::default(),
                )
            }
        };

        // Sugarloaf has found errors and context need to notify it for the user
        if let Some(errors) = sugarloaf_errors {
            if !errors.fonts_not_found.is_empty() {
                event_proxy.send_event(
                    RioEvent::ReportToAssistant({
                        RioError {
                            report: RioErrorType::FontsNotFound(errors.fonts_not_found),
                            level: RioErrorLevel::Warning,
                        }
                    }),
                    window_id,
                );
            }
        }

        let mut manager = ContextManager {
            current_index: 0,
            current_route: 0,
            contexts: smallvec![ContextGrid::new(
                initial_context,
                scaled_margin,
                ctx_config.panel,
                ctx_config.divider_width,
            )],
            capacity: DEFAULT_CONTEXT_CAPACITY,
            event_proxy,
            window_id,
            config: ctx_config,
        };
        // The native titlebar starts as the placeholder; one poke makes
        // it converge on the displayed title even for shells that never
        // emit an OSC title or OSC 7.
        manager.sync_window_title();
        Ok(manager)
    }

    #[cfg(test)]
    pub fn start_with_capacity(
        capacity: usize,
        event_proxy: T,
        window_id: WindowId,
    ) -> Result<Self, Box<dyn Error>> {
        let config = ContextManagerConfig {
            #[cfg(test)]
            dead_pty: true,
            ..ContextManagerConfig::default()
        };
        let initial_context = ContextManager::create_context(
            (&Cursor::default(), false),
            event_proxy.clone(),
            window_id,
            0,
            ContextDimension::default(),
            &config,
        )?;

        Ok(ContextManager {
            current_index: 0,
            current_route: 0,
            contexts: smallvec![ContextGrid::new(
                initial_context,
                Margin::default(),
                config.panel,
                config.divider_width,
            )],
            capacity,
            event_proxy,
            window_id,
            config,
        })
    }

    #[inline]
    pub fn should_close_context_manager(
        &mut self,
        route_id: usize,
        sugarloaf: &mut Sugarloaf,
    ) -> bool {
        // Called when terminal.exit() fires: a tab-close action (the
        // context is already gone, nothing found below, return false)
        // or a PTY that exited on its own. The exiting PTY can live
        // ANYWHERE: a background tab, or a background split of any
        // tab (`tmux new -X` SIGHUPs a shell the user is not even
        // looking at), so the search covers every panel of every tab
        // rather than assuming the focused one.
        let Some(tab_index) = self.tab_index_for_route(route_id) else {
            return self.contexts.is_empty();
        };

        // A split dies: remove just that panel, keep the tab. When
        // the tab is focused and its focused panel was the one that
        // died, the sibling selection becomes the current route.
        if self.contexts[tab_index].len() > 1 {
            self.contexts[tab_index].remove_by_route(route_id, sugarloaf);
            if tab_index == self.current_index {
                self.sync_current_route();
            }
            return false;
        }

        // A whole tab dies.
        self.contexts[tab_index].remove_from_sugarloaf(sugarloaf);
        self.contexts.remove(tab_index);

        if self.contexts.is_empty() {
            return true;
        }

        // Removing a tab shifts every index after it. Adjusting only
        // when the focused tab itself died leaves `current_index`
        // past the end when a background tab exits first (the tmux
        // SIGHUP crash: len 1, index 1).
        let new_index = current_index_after_tab_removal(self.current_index, tab_index);
        if tab_index == self.current_index {
            self.set_current(new_index);
        } else {
            self.current_index = new_index;
        }

        self.clear_inactive_image_overlays(sugarloaf);
        false
    }

    #[inline]
    pub fn request_render(&mut self) {
        self.event_proxy
            .send_event(RioEvent::RenderRoute(self.current_route), self.window_id);
    }

    #[inline]
    pub fn blink_cursor(&mut self, scheduled_time: u64) {
        // PrepareRender will force a render for any route that is focused on window
        // PrepareRenderOnRoute only call render function for specific route ids.
        self.event_proxy.send_event(
            RioEvent::BlinkCursor(scheduled_time, self.current_route),
            self.window_id,
        );
    }

    #[inline]
    pub fn schedule_render(&mut self, millis: u64) {
        self.event_proxy
            .send_event(RioEvent::PrepareRender(millis), self.window_id);
    }

    #[inline]
    pub fn report_error_fonts_not_found(&mut self, fonts_not_found: Vec<SugarloafFont>) {
        if !fonts_not_found.is_empty() {
            self.event_proxy.send_event(
                RioEvent::ReportToAssistant({
                    RioError {
                        report: RioErrorType::FontsNotFound(fonts_not_found),
                        level: RioErrorLevel::Warning,
                    }
                }),
                self.window_id,
            );
        }
    }

    #[inline]
    pub fn create_new_window(&self) {
        self.event_proxy
            .send_event(RioEvent::CreateWindow, self.window_id);
    }

    #[inline]
    pub fn toggle_quake(&self) {
        self.event_proxy
            .send_event(RioEvent::ToggleQuake, self.window_id);
    }

    #[inline]
    pub fn close_unfocused_tabs(&mut self, sugarloaf: &mut Sugarloaf) {
        let current_route_id = self.current().route_id;
        self.contexts.retain(|ctx| {
            let keep = ctx.current().route_id == current_route_id;
            if !keep {
                ctx.remove_from_sugarloaf(sugarloaf);
            }
            keep
        });
        self.current_route = self.contexts[0].current().route_id;
        self.set_current(0);
    }

    #[inline]
    pub fn set_last_typing(&mut self) {
        self.current_mut().renderable_content.last_typing = Some(Instant::now());
    }

    #[inline]
    pub fn select_next_split(&mut self) {
        self.contexts[self.current_index].select_next_split();
        self.sync_current_route();
    }

    #[inline]
    pub fn select_prev_split(&mut self) {
        self.contexts[self.current_index].select_prev_split();
        self.sync_current_route();
    }

    #[inline]
    pub fn switch_to_next_split_or_tab(&mut self) {
        if self.contexts[self.current_index].select_next_split_no_loop() {
            self.sync_current_route();
            return;
        }
        self.switch_to_next();
        // Make sure first split is selected - get the root key
        let current_tab = &mut self.contexts[self.current_index];
        if let Some(root) = current_tab.root {
            current_tab.current = root;
        }
        self.sync_current_route();
    }

    #[inline]
    pub fn switch_to_prev_split_or_tab(&mut self) {
        if self.contexts[self.current_index].select_prev_split_no_loop() {
            self.sync_current_route();
            return;
        }
        self.switch_to_prev();
        // Make sure last split is selected - get the last key in order
        let current_tab = &mut self.contexts[self.current_index];
        let ordered_keys = current_tab.get_ordered_keys();
        if let Some(&last_key) = ordered_keys.last() {
            current_tab.current = last_key;
        }
        self.sync_current_route();
    }

    #[inline]
    pub fn move_divider_up(&mut self, amount: f32, sugarloaf: &mut Sugarloaf) -> bool {
        self.contexts[self.current_index].move_divider_up(amount, sugarloaf)
    }

    #[inline]
    pub fn move_divider_down(&mut self, amount: f32, sugarloaf: &mut Sugarloaf) -> bool {
        self.contexts[self.current_index].move_divider_down(amount, sugarloaf)
    }

    #[inline]
    pub fn move_divider_left(&mut self, amount: f32, sugarloaf: &mut Sugarloaf) -> bool {
        self.contexts[self.current_index].move_divider_left(amount, sugarloaf)
    }

    #[inline]
    pub fn move_divider_right(&mut self, amount: f32, sugarloaf: &mut Sugarloaf) -> bool {
        self.contexts[self.current_index].move_divider_right(amount, sugarloaf)
    }

    #[inline]
    pub fn select_tab(&mut self, tab_index: usize) {
        self.set_current(tab_index);
    }

    #[inline]
    pub fn toggle_full_screen(&mut self) {
        self.event_proxy
            .send_event(RioEvent::ToggleFullScreen, self.window_id);
    }

    #[inline]
    pub fn toggle_appearance_theme(&mut self) {
        self.event_proxy
            .send_event(RioEvent::ToggleAppearanceTheme, self.window_id);
    }

    #[inline]
    pub fn minimize(&mut self) {
        self.event_proxy
            .send_event(RioEvent::Minimize(true), self.window_id);
    }

    #[inline]
    pub fn hide(&mut self) {
        self.event_proxy.send_event(RioEvent::Hide, self.window_id);
    }

    #[inline]
    pub fn quit(&mut self) {
        self.event_proxy.send_event(RioEvent::Quit, self.window_id);
    }

    #[cfg(target_os = "macos")]
    #[inline]
    pub fn hide_other_apps(&mut self) {
        self.event_proxy
            .send_event(RioEvent::HideOtherApplications, self.window_id);
    }

    #[inline]
    pub fn select_last_tab(&mut self) {
        self.set_current(self.contexts.len() - 1);
    }

    #[inline]
    pub fn switch_to_settings(&mut self) {
        self.event_proxy
            .send_event(RioEvent::CreateConfigEditor, self.window_id);
    }

    #[inline]
    pub fn select_route_from_current_grid(&mut self) {
        self.sync_current_route();
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.contexts.len()
    }

    pub fn tab_id(&self, index: usize) -> usize {
        self.contexts[index].tab_id
    }

    #[cfg(test)]
    pub fn title(&self, index: usize) -> Option<&ContextTitle> {
        self.contexts.get(index).map(|grid| &grid.current().title)
    }

    #[inline]
    pub fn custom_title(&self, index: usize) -> Option<&str> {
        self.contexts
            .get(index)
            .and_then(|grid| grid.custom_title.as_deref())
    }

    /// Each tab displays the status of its own focused split, even while
    /// another tab is selected. Other splits in that tab do not contribute.
    pub fn program_status_for_tab(
        &self,
        index: usize,
    ) -> Option<rio_backend::program_status::ProgramStatusRecord> {
        let context = self.contexts.get(index)?.current();
        context.metadata.lock().status.clone()
    }

    pub fn application_for_tab(&self, index: usize) -> Option<TabApplication> {
        #[cfg(feature = "profiling")]
        let _timing = crate::profiling::span("tabs.application");
        let context = self.contexts.get(index)?.current();
        let metadata = context.metadata.lock();
        let reported_app = metadata.app.as_ref();
        let program = reported_app
            .cloned()
            .unwrap_or_else(|| context.spawned_program.clone());
        Some(TabApplication {
            program,
            title: metadata.title.clone(),
            remote: metadata.remote,
            reported: reported_app.is_some(),
        })
    }

    /// A pane can be the focused split of a tab that is not selected.
    pub fn is_route_focused_in_tab(&self, route_id: usize) -> bool {
        self.contexts
            .iter()
            .any(|grid| grid.current().route_id == route_id)
    }

    #[inline]
    /// A rename changes what `displayed_title_for_tab` returns, so the
    /// native titlebar gets the same convergence poke every other
    /// displayed-title mutation sends.
    pub fn set_custom_title(&mut self, index: usize, title: Option<String>) {
        if let Some(grid) = self.contexts.get_mut(index) {
            grid.custom_title = title;
        }
        self.sync_window_title();
    }

    /// Drop every tab's bell mark, used when the indicator is disabled
    /// by a live config reload: the ring-time gate stops new marks but
    /// cannot retract ones already set.
    pub fn clear_all_bells(&mut self) {
        for grid in self.contexts.iter_mut() {
            grid.bell = false;
        }
    }

    #[inline]
    pub fn custom_color(&self, index: usize) -> Option<[f32; 4]> {
        self.contexts.get(index).and_then(|grid| grid.custom_color)
    }

    #[inline]
    pub fn set_custom_color(&mut self, index: usize, color: Option<[f32; 4]>) {
        if let Some(grid) = self.contexts.get_mut(index) {
            grid.custom_color = color;
        }
    }

    /// Index of the tab containing `route_id`'s pane. Per-route state
    /// can live ANYWHERE (a background split of a background tab
    /// included), so the search covers every panel of every tab.
    #[inline]
    pub fn tab_index_for_route(&mut self, route_id: usize) -> Option<usize> {
        self.contexts
            .iter_mut()
            .position(|grid| grid.get_by_route_id(route_id).is_some())
    }

    /// A pane rang the bell. Flags its tab so the strip can surface it.
    /// The current tab is skipped only while the window has focus: the
    /// user is already looking at it then, but a ring in the visible
    /// tab of an UNFOCUSED window would otherwise leave no trace at
    /// all. Edge-triggered: a BEL flood marks once and repaints once.
    #[inline]
    pub fn ring_bell(&mut self, route_id: usize, window_focused: bool) -> bool {
        let Some(tab_index) = self.tab_index_for_route(route_id) else {
            return false;
        };

        if window_focused && tab_index == self.current_index {
            return false;
        }

        !std::mem::replace(&mut self.contexts[tab_index].bell, true)
    }

    #[inline]
    pub fn bell(&self, index: usize) -> bool {
        self.contexts.get(index).is_some_and(|grid| grid.bell)
    }

    /// Clears the focused tab's bell flag. Called every FOCUSED frame so
    /// the mark drops the moment the tab is shown to the user, whatever
    /// brought it to the front; an unfocused window still renders on PTY
    /// damage, and clearing there would wipe a mark nobody has seen yet.
    #[inline]
    pub fn clear_current_bell(&mut self) -> bool {
        let grid = &mut self.contexts[self.current_index];
        std::mem::replace(&mut grid.bell, false)
    }

    #[inline]
    pub fn resize_all_grids(
        &mut self,
        width: f32,
        height: f32,
        sugarloaf: &mut Sugarloaf,
    ) {
        for context_grid in self.contexts.iter_mut() {
            context_grid.resize(width, height, sugarloaf);
        }
    }

    /// Re-render one pane's title. Returns whether the displayed text
    /// changed (the empty-content fallback is the pane's static
    /// spawned program, so displayed text changes exactly when the
    /// content does).
    fn refresh_item_title(
        template: &str,
        context: &mut Context<T>,
        prefetched_title: Option<&str>,
    ) -> bool {
        let content = update_title(template, context, prefetched_title);
        if content == context.title.content {
            return false;
        }
        context.title = ContextTitle { content };
        true
    }

    /// A pane's title data changed: an OSC 0/2 title (carried in
    /// `raw_title`, so the common `{{ title }}` render never locks the
    /// terminal) or an OSC 7 working directory (`raw_title` None: the
    /// render re-reads the stored directory). A DISPLAYED pane (its
    /// tab's current) re-renders immediately; a hidden pane is only
    /// marked dirty (one flag write, no locks, no render) and renders
    /// when it surfaces, so a background split streaming titles costs
    /// nothing visible. One route scan serves every decision. Returns
    /// whether the strip must repaint.
    pub fn on_title_change(&mut self, route_id: usize, raw_title: Option<&str>) -> bool {
        let Some(tab_index) = self.tab_index_for_route(route_id) else {
            return false;
        };
        if self.contexts[tab_index].current().route_id != route_id {
            if let Some(item) = self.contexts[tab_index].get_by_route_id(route_id) {
                item.context_mut().title_dirty = true;
            }
            return false;
        }
        let template = self.config.title.content.clone();
        let context = self.contexts[tab_index].current_mut();
        context.title_dirty = false;
        Self::refresh_item_title(&template, context, raw_title)
    }

    /// Mark every pane's title stale. For changes that affect panes no
    /// walk re-renders (a resize changing `{{columns}}`, a config
    /// reload changing the template), hidden splits and unwalked tabs
    /// re-render lazily when they surface via `sync_current_route`.
    pub fn mark_all_titles_dirty(&mut self) {
        for grid in self.contexts.iter_mut() {
            for item in grid.contexts_mut().values_mut() {
                item.context_mut().title_dirty = true;
            }
        }
    }

    /// Re-render tab titles from local state: a config reload can
    /// change the template, and a resize changes `{{columns}}`/
    /// `{{lines}}`. OSC title and OSC 7 changes arrive via
    /// `on_title_change` instead; nothing calls this on a timer. The
    /// chrome repaint rides the returned flag, and one unconditional
    /// titlebar poke per run makes the native title CONVERGE on the
    /// displayed text (the poke is payload-less and deduped at the
    /// sink, so a run that changed nothing costs one no-op event).
    /// `only_current` restricts the walk to the displayed tab: with
    /// the tab strip absent (navigation disabled) background tabs'
    /// titles render nowhere, so refreshing them buys nothing.
    pub fn update_titles(&mut self, only_current: bool) -> bool {
        let template = self.config.title.content.clone();
        let mut repaint = false;
        let range = if only_current {
            self.current_index..self.current_index + 1
        } else {
            0..self.contexts.len()
        };
        for index in range {
            let context = self.contexts[index].current_mut();
            context.title_dirty = false;
            repaint |= Self::refresh_item_title(&template, context, None);
        }
        self.sync_window_title();
        repaint
    }

    /// The title the strip displays for `index`'s tab: the user rename,
    /// else the rendered content, else the SSH destination or spawned
    /// program, else "~". A transport executable title also uses the
    /// SSH destination. The native titlebar reads the same chain.
    pub fn displayed_title_for_tab(&self, index: usize) -> String {
        if let Some(custom) = self.custom_title(index) {
            return custom.to_string();
        }
        if let Some(grid) = self.contexts.get(index) {
            let context = grid.current();
            if let Some(connection) = &context.ssh {
                if context.title.content.is_empty()
                    || context.title.content == context.spawned_program
                {
                    return connection.destination.clone();
                }
            }
            if !context.title.content.is_empty() {
                return context.title.content.clone();
            }
            if !context.spawned_program.is_empty() {
                return context.spawned_program.clone();
            }
        }
        String::from("~")
    }

    #[inline]
    pub fn displayed_title_for_current_tab(&self) -> String {
        self.displayed_title_for_tab(self.current_index)
    }

    /// Ask the event loop to refresh the native titlebar. Payload-less
    /// on purpose: the handler re-reads the displayed title at handling
    /// time, so a queued poke can never overwrite a newer title with a
    /// stale snapshot, and redundant pokes dedupe at the sink.
    fn sync_window_title(&mut self) {
        self.event_proxy
            .send_event(RioEvent::SyncWindowTitle, self.window_id);
    }

    /// Point `current_route` at the pane the user now sees and poke the
    /// titlebar. Every displayed-pane change (tab switch, split
    /// selection, split death, new splits) funnels here so the titlebar
    /// can never be left showing a pane that is not on screen.
    fn sync_current_route(&mut self) {
        self.current_route = self.current().route_id;
        if self.current().title_dirty {
            let template = self.config.title.content.clone();
            let context = self.contexts[self.current_index].current_mut();
            context.title_dirty = false;
            Self::refresh_item_title(&template, context, None);
        }
        self.sync_window_title();
    }

    #[inline]
    pub fn get_by_route_id(
        &mut self,
        route_id: usize,
    ) -> Option<&mut ContextGridItem<T>> {
        // Search every tab, current first: per-route events (damage marks,
        // titles, color/size requests) must reach panes in background tabs,
        // otherwise their state is silently dropped until the pane's own
        // PTY speaks again.
        let current = self.current_index;
        if self.contexts[current].get_by_route_id(route_id).is_some() {
            return self.contexts[current].get_by_route_id(route_id);
        }
        self.contexts
            .iter_mut()
            .enumerate()
            .filter(|(i, _)| *i != current)
            .find_map(|(_, grid)| grid.get_by_route_id(route_id))
    }

    #[inline]
    pub fn contexts_mut(
        &mut self,
    ) -> &mut SmallVec<[ContextGrid<T>; DEFAULT_CONTEXT_CAPACITY]> {
        &mut self.contexts
    }

    #[inline]
    pub fn current_grid_len(&self) -> usize {
        self.contexts[self.current_index].len()
    }

    #[inline]
    pub fn remove_current_grid(&mut self, sugarloaf: &mut Sugarloaf) {
        self.contexts[self.current_index].remove_current(sugarloaf);
        self.sync_current_route();
    }

    #[inline]
    pub fn current_grid_mut(&mut self) -> &mut ContextGrid<T> {
        &mut self.contexts[self.current_index]
    }

    #[inline]
    pub fn current_grid(&self) -> &ContextGrid<T> {
        &self.contexts[self.current_index]
    }

    pub fn is_route_visible(&self, route_id: usize) -> bool {
        self.current_grid()
            .contexts()
            .values()
            .any(|item| item.val.route_id == route_id)
    }

    pub fn update_scrollback_config(&mut self, lines: usize, bytes: Option<usize>) {
        self.config.scrollback_history_limit = lines;
        self.config.scrollback_history_byte_limit = bytes;
    }

    pub fn update_layout_config(
        &mut self,
        panel: rio_backend::config::layout::Panel,
        divider_width: f32,
    ) {
        self.config.panel = panel;
        self.config.divider_width = divider_width;
        for grid in &mut self.contexts {
            grid.update_layout_config(panel, divider_width);
        }
    }

    #[cfg(test)]
    pub fn increase_capacity(&mut self, inc_val: usize) {
        self.capacity += inc_val;
    }

    #[inline]
    pub fn set_current(&mut self, context_id: usize) {
        if context_id < self.contexts.len() {
            self.current_index = context_id;
            self.sync_current_route();
        }
    }

    #[cfg(target_os = "macos")]
    pub fn request_close_window(&self) {
        self.event_proxy
            .send_event(RioEvent::CloseWindow, self.window_id);
    }

    #[inline]
    pub fn close_current_context(&mut self, sugarloaf: &mut Sugarloaf) {
        if self.contexts.len() == 1 {
            // MacOS: Close last tab will work, leading to hide and
            // keep Rio running in background.
            #[cfg(target_os = "macos")]
            {
                self.request_close_window();
            }
            return;
        }

        let index_to_remove = self.current_index;
        let mut should_set_current = false;
        if index_to_remove > 1 {
            self.set_current(self.current_index - 1);
        } else {
            should_set_current = true;
        }

        // Remove all rich text from the grid before removing the context
        self.contexts[index_to_remove].remove_from_sugarloaf(sugarloaf);
        self.contexts.remove(index_to_remove);

        if should_set_current {
            self.set_current(0);
        }

        self.clear_inactive_image_overlays(sugarloaf);
    }

    pub fn close_unfocused_tab(&mut self, index: usize, sugarloaf: &mut Sugarloaf) {
        if index >= self.contexts.len() || index == self.current_index {
            return;
        }
        self.contexts[index].remove_from_sugarloaf(sugarloaf);
        self.contexts.remove(index);
        if index < self.current_index {
            self.current_index -= 1;
        }
        self.clear_inactive_image_overlays(sugarloaf);
    }

    #[inline]
    pub fn current_index(&self) -> usize {
        self.current_index
    }

    #[inline]
    pub fn current_route(&self) -> usize {
        self.current_route
    }

    #[inline]
    pub fn current(&self) -> &Context<T> {
        self.contexts[self.current_index].current()
    }

    #[inline]
    pub fn current_mut(&mut self) -> &mut Context<T> {
        self.contexts[self.current_index].current_mut()
    }

    #[inline]
    pub fn switch_to_next(&mut self) {
        if self.contexts.len() - 1 == self.current_index {
            self.current_index = 0;
        } else {
            self.current_index += 1;
        }

        self.sync_current_route();
    }

    #[inline]
    pub fn switch_to_prev(&mut self) {
        if self.current_index == 0 {
            self.current_index = self.contexts.len() - 1;
        } else {
            self.current_index -= 1;
        }

        self.sync_current_route();
    }

    #[inline]
    pub fn move_current_to_prev(&mut self) {
        let len = self.contexts.len();
        if len <= 1 {
            return;
        }

        let current = self.current_index;
        let target_index = if current == 0 { len - 1 } else { current - 1 };
        self.contexts.swap(current, target_index);
        self.select_tab(target_index);
    }

    #[inline]
    pub fn move_current_to_next(&mut self) {
        let len = self.contexts.len();
        if len <= 1 {
            return;
        }

        let current = self.current_index;
        let target_index = if current == len - 1 { 0 } else { current + 1 };
        self.contexts.swap(current, target_index);
        self.select_tab(target_index);
    }

    #[inline]
    pub fn move_current_tab_to(&mut self, target: usize) {
        let current = self.current_index;
        if target == current || target >= self.contexts.len() {
            return;
        }

        let grid = self.contexts.remove(current);
        self.contexts.insert(target, grid);
        self.set_current(target);
    }

    pub fn current_ssh(&self) -> Option<crate::ssh::Connection> {
        if let Some(connection) = self
            .current()
            .ssh_session
            .as_ref()
            .and_then(|session| session.connection())
        {
            return Some(connection);
        }
        if let Some(connection) = &self.current().ssh {
            return Some(connection.clone());
        }
        #[cfg(not(target_os = "windows"))]
        {
            let current = self.current();
            let mut argv = teletypewriter::foreground_process_args(
                *current.main_fd,
                current.shell_pid,
            )
            .ok()?
            .into_iter();
            let shell = Shell {
                program: argv.next(),
                args: argv.collect(),
            };
            crate::ssh::Connection::from_shell(&shell)
        }
        #[cfg(target_os = "windows")]
        None
    }

    pub fn report_ssh_error(&self, message: String) {
        self.event_proxy
            .send_event(RioEvent::SshError(message), self.window_id);
    }

    /// Tabs and splits share the same host, directory, and authentication rules.
    fn config_here(&self, inherit_ssh: bool) -> Result<ContextManagerConfig, String> {
        let mut config = self.config.clone();
        if let Some(connection) = inherit_ssh.then(|| self.current_ssh()).flatten() {
            let directory = {
                let terminal = self.current().terminal.lock();
                terminal
                    .remote_directory
                    .as_ref()
                    .map(|(_, path)| path.clone())
                    .or_else(|| {
                        let reported_remote =
                            terminal.user_vars.get("RIO_SSH").is_some_and(|v| v == "1");
                        (self.current().ssh.is_some() || reported_remote)
                            .then_some(terminal.current_directory.clone())
                            .flatten()
                    })
            };
            let directory = config.cwd.then_some(directory).flatten();
            // The retained recipe or live SSH argv identifies the endpoint.
            // OSC 7 can report a machine name while ssh -G reports an IP or
            // localhost tunnel. Keep the directory from this SSH pane.
            config.shell = connection.duplicate_shell(directory.as_deref(), None)?;
            config.ssh = Some(connection);
            // Remote paths are arguments to the remote shell, never a local cwd.
            config.shell_integration = false;
            return Ok(config);
        }
        if !inherit_ssh && crate::ssh::Connection::from_shell(&config.shell).is_some() {
            config.shell = Shell::default();
        }
        config.ssh = None;
        if config.cwd {
            #[cfg(not(target_os = "windows"))]
            if let Ok(path) = teletypewriter::foreground_process_path(
                *self.current().main_fd,
                self.current().shell_pid,
            ) {
                config.working_dir = Some(path.to_string_lossy().into_owned());
            }
            #[cfg(target_os = "windows")]
            if let Some(path) = self
                .current()
                .terminal
                .lock()
                .current_directory
                .as_ref()
                .filter(|p| p.is_dir())
            {
                config.working_dir = Some(path.to_string_lossy().into_owned());
            }
        }
        Ok(config)
    }

    pub fn split(
        &mut self,
        rich_text_id: usize,
        split_down: bool,
        sugarloaf: &mut Sugarloaf,
    ) {
        let cloned_config = match self.config_here(true) {
            Ok(config) => config,
            Err(message) => {
                self.report_ssh_error(message);
                return;
            }
        };

        let current = self.current();
        let cursor = current.cursor_from_ref();

        match ContextManager::create_context(
            (&cursor, current.renderable_content.has_blinking_enabled),
            self.event_proxy.clone(),
            self.window_id,
            rich_text_id,
            self.current().dimension,
            &cloned_config,
        ) {
            Ok(new_context) => {
                let new_route_id = new_context.route_id;
                if split_down {
                    self.contexts[self.current_index].split_down(new_context, sugarloaf);
                } else {
                    self.contexts[self.current_index].split_right(new_context, sugarloaf);
                }

                self.current_route = new_route_id;
                self.sync_window_title();
            }
            Err(..) => {
                tracing::error!("not able to create a new context");
            }
        }
    }

    pub fn split_from_config(
        &mut self,
        rich_text_id: usize,
        split_down: bool,
        config: rio_backend::config::Config,
        sugarloaf: &mut Sugarloaf,
    ) {
        let (shell, working_dir) = process_open_url(
            config.shell.to_owned(),
            config.working_dir.to_owned(),
            config.editor.to_owned(),
            None,
        );

        let context_manager_config = ContextManagerConfig {
            #[cfg(test)]
            dead_pty: false,
            cwd: config.navigation.current_working_directory,
            shell,
            ssh: None,
            working_dir,
            spawn_performer: true,
            #[cfg(not(target_os = "windows"))]
            use_fork: config.use_fork,
            shell_integration: config.shell_integration,
            panel: config.panel(),
            divider_width: config.window.divider_width(),
            title: config.title,
            keyboard: config.keyboard,
            scrollback_history_limit: config.scrollback_history_limit,
            scrollback_history_byte_limit: config.scrollback_history_byte_limit,
            grapheme_clustering: config.grapheme_clustering,
            clipboard: config.clipboard,
        };

        let current = self.current();
        let cursor = current.cursor_from_ref();

        match ContextManager::create_context(
            (&cursor, current.renderable_content.has_blinking_enabled),
            self.event_proxy.clone(),
            self.window_id,
            rich_text_id,
            self.current().dimension,
            &context_manager_config,
        ) {
            Ok(new_context) => {
                let new_route_id = new_context.route_id;
                if split_down {
                    self.contexts[self.current_index].split_down(new_context, sugarloaf);
                } else {
                    self.contexts[self.current_index].split_right(new_context, sugarloaf);
                }

                self.current_route = new_route_id;
                self.sync_window_title();
            }
            Err(..) => {
                tracing::error!("not able to create a new context");
            }
        }
    }

    #[inline]
    pub fn add_context(&mut self, redirect: bool, rich_text_id: usize) {
        self.add_context_here(redirect, rich_text_id, true);
    }

    pub fn add_context_here(
        &mut self,
        redirect: bool,
        rich_text_id: usize,
        inherit_ssh: bool,
    ) {
        match self.config_here(inherit_ssh) {
            Ok(config) => self.add_context_with_config(redirect, rich_text_id, config),
            Err(message) => self.report_ssh_error(message),
        }
    }

    pub fn connect_ssh(
        &mut self,
        rich_text_id: usize,
        connection: crate::ssh::Connection,
    ) {
        match connection.connect_shell() {
            Ok(shell) => {
                let mut config = self.config.clone();
                config.shell = shell;
                config.ssh = Some(connection.clone());
                config.shell_integration = false;
                self.add_context_with_config(true, rich_text_id, config);
                if let Err(message) = crate::ssh::remember(&connection) {
                    tracing::warn!("Could not save SSH connection: {message}");
                }
            }
            Err(message) => self.report_ssh_error(message),
        }
    }

    fn add_context_with_config(
        &mut self,
        redirect: bool,
        rich_text_id: usize,
        cloned_config: ContextManagerConfig,
    ) {
        let size = self.contexts.len();
        if size < self.capacity {
            let last_index = self.contexts.len();

            let current = self.current();
            let cursor = current.cursor_from_ref();
            // Pane dimensions exclude padding and margins even without splits.
            // A new tab's layout needs the whole window, not that inner viewport.
            let dimension = self.current_grid().grid_dimension();

            match ContextManager::create_context(
                (&cursor, current.renderable_content.has_blinking_enabled),
                self.event_proxy.clone(),
                self.window_id,
                rich_text_id,
                dimension,
                &cloned_config,
            ) {
                Ok(new_context) => {
                    let previous_scaled_margin =
                        self.contexts[self.current_index].scaled_margin;
                    self.contexts.push(ContextGrid::new(
                        new_context,
                        previous_scaled_margin,
                        self.config.panel,
                        self.config.divider_width,
                    ));
                    if redirect {
                        self.current_index = last_index;
                        self.sync_current_route();
                    }
                }
                Err(..) => {
                    tracing::error!("not able to create a new context");
                }
            }
        }
    }

    /// Clear image overlays for inactive tabs.
    #[inline]
    pub fn clear_inactive_image_overlays(&self, sugarloaf: &mut Sugarloaf) {
        for (idx, context) in self.contexts.iter().enumerate() {
            // Skip the current tab
            if idx == self.current_index {
                continue;
            }

            context.clear_image_overlays(sugarloaf);
        }
    }

    /// Clear image overlays for a tab being hidden.
    #[inline]
    pub fn clear_context_image_overlays(
        &self,
        sugarloaf: &mut Sugarloaf,
        old_index: usize,
    ) {
        if let Some(old_context) = self.contexts.get(old_index) {
            old_context.clear_image_overlays(sugarloaf);
        }
    }
}

pub fn process_open_url(
    mut shell: Shell,
    mut working_dir: Option<String>,
    editor: Shell,
    open_url: Option<&str>,
) -> (Shell, Option<String>) {
    if open_url.is_none() {
        return (shell, working_dir);
    }

    if let Ok(url) = url::Url::parse(open_url.unwrap_or_default()) {
        if let Ok(path_buf) = url.to_file_path() {
            if path_buf.exists() {
                if path_buf.is_file() {
                    let mut args = editor.args;
                    args.push(path_buf.display().to_string());
                    shell = Shell {
                        program: editor.program,
                        args,
                    }
                } else if path_buf.is_dir() {
                    working_dir = Some(path_buf.display().to_string());
                }
            }
        }
    }

    (shell, working_dir)
}

#[cfg(test)]
pub mod test {
    use super::*;
    use crate::event::VoidListener;

    #[test]
    fn typed_ssh_uses_local_pane_recipe_and_local_tabs_clear_it() {
        let mut manager =
            ContextManager::start_with_capacity(1, VoidListener {}, WindowId::from(0))
                .unwrap();
        let session = crate::ssh::native::PaneSession::new(usize::MAX - 3).unwrap();
        let path = std::path::PathBuf::from(session.env().1)
            .join(std::process::id().to_string());
        let connection = crate::ssh::Connection::new("native-host", false).unwrap();
        std::fs::write(&path, toml::to_string(&connection).unwrap()).unwrap();
        manager.current_mut().ssh_session = Some(session);
        assert_eq!(manager.current_ssh(), Some(connection));
        assert!(manager.config_here(false).unwrap().ssh.is_none());
        std::fs::remove_file(path).unwrap();
        assert!(manager.current_ssh().is_none());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires RIO_TEST_SSH_TARGET and remote Rio shell integration"]
    fn live_windows_ssh_connection_uses_conpty() {
        #[derive(Clone, Default)]
        struct SshEvents(Arc<std::sync::Mutex<Vec<(usize, String)>>>);
        impl SshEvents {
            fn reply_to(&self, context: &Context<Self>) {
                self.0.lock().unwrap().retain(|(route, text)| {
                    if *route == context.route_id {
                        context.messenger.send_write(text.clone().into_bytes());
                        false
                    } else {
                        true
                    }
                });
            }
        }
        impl EventListener for SshEvents {
            fn send_event(&self, event: RioEvent, _: WindowId) {
                if let RioEvent::PtyWrite(route, text) = event {
                    self.0.lock().unwrap().push((route, text));
                }
            }
        }
        let events = SshEvents::default();
        let target = std::env::var("RIO_TEST_SSH_TARGET").unwrap();
        let mut args = vec!["-A".into()];
        if let Ok(config) = std::env::var("RIO_TEST_SSH_CONFIG") {
            args.extend(["-F".into(), config]);
        }
        args.push(target);
        if let Ok(command) = std::env::var("RIO_TEST_SSH_COMMAND") {
            args.push(command);
        }
        let connection = crate::ssh::Connection::from_shell(&Shell {
            program: Some("ssh".into()),
            args: args.clone(),
        })
        .unwrap();
        let shell = connection.connect_shell().unwrap();
        let resolved_destination = connection.destination.clone();
        let dimension = ContextDimension::build(
            800.0,
            600.0,
            rio_backend::sugarloaf::layout::TextDimensions::default(),
            rio_backend::sugarloaf::layout::CellMetrics {
                cell_width: 8,
                cell_height: 16,
                cell_baseline: 12,
                face_width: 8.0,
                face_height: 16.0,
                face_y: 0.0,
            },
            1.0,
            14.0,
            Margin::default(),
        );
        let mut manager = ContextManager::start(
            (&Cursor::default(), false),
            events.clone(),
            WindowId::from(0),
            0,
            0,
            ContextManagerConfig {
                dead_pty: true,
                ..Default::default()
            },
            dimension,
            Margin::default(),
            None,
        )
        .unwrap();
        let mut config = manager.config.clone();
        config.dead_pty = false;
        config.spawn_performer = true;
        config.shell_integration = false;
        config.shell = shell;
        config.ssh = Some(connection.clone());
        if let Ok(wrapper) = std::env::var("RIO_TEST_SSH_WRAPPER_BIN") {
            use base64::Engine as _;
            let quote = |value: &str| format!("'{}'", value.replace('\'', "''"));
            let args = args
                .iter()
                .map(|arg| quote(arg))
                .collect::<Vec<_>>()
                .join(" ");
            let script = format!(
                "$env:RIO_BIN={};\n{}\nssh -t {args}",
                quote(&wrapper),
                include_str!("../shell_integration/rio-ssh.ps1"),
            );
            let bytes: Vec<u8> =
                script.encode_utf16().flat_map(u16::to_le_bytes).collect();
            config.shell = Shell {
                program: Some("powershell.exe".into()),
                args: vec![
                    "-NoLogo".into(),
                    "-NoExit".into(),
                    "-EncodedCommand".into(),
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                ],
            };
            config.ssh = None;
        }
        manager.add_context_with_config(true, 1, config);
        let deadline = Instant::now() + std::time::Duration::from_secs(20);
        loop {
            events.reply_to(manager.current());
            if manager
                .current()
                .terminal
                .lock()
                .user_vars
                .get("RIO_SSH")
                .is_some_and(|value| value == "1")
            {
                break;
            }
            if Instant::now() >= deadline {
                let terminal = manager.current().terminal.lock();
                let output: String =
                    terminal.grid.display_iter().map(|cell| cell.c()).collect();
                panic!("SSH did not start a remote shell over ConPTY: {output}");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        manager
            .current()
            .messenger
            .send_write("printf '\\033]1337;SetUserVar=RIO_TEST_TRANSPORT=%s\\007' \"$(printf %s \"$SSH_CONNECTION\" | base64 -w0)\"; printf '\\033]1337;SetUserVar=RIO_TEST_INPUT=%s\\007' \"$(printf %s 'é λ 中文' | base64 -w0)\"; printf 'UNICODE: \\303\\251 \\316\\273 \\344\\270\\255\\346\\226\\207\\n'; cd /tmp\r".as_bytes().to_vec());
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while !manager
            .current()
            .terminal
            .lock()
            .remote_directory
            .as_ref()
            .is_some_and(|(_, path)| path == std::path::Path::new("/tmp"))
        {
            events.reply_to(manager.current());
            assert!(
                Instant::now() < deadline,
                "remote directory was not reported: {:?}",
                manager.current().terminal.lock().remote_directory
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // Check visible output before resize/redraw commands scroll it away.
        {
            let terminal = manager.current().terminal.lock();
            assert_eq!(
                terminal.user_vars.get("RIO_TEST_INPUT").map(String::as_str),
                Some("é λ 中文"),
                "SSH input did not preserve UTF-8"
            );
            let output: String = terminal
                .grid
                .display_iter()
                .filter(|cell| {
                    cell.wide() != rio_backend::crosswords::square::Wide::Spacer
                })
                .map(|cell| cell.c())
                .filter(|c| *c != '\0')
                .collect();
            assert!(
                output.contains("UNICODE: é λ 中文"),
                "SSH output did not preserve UTF-8: {output}"
            );
        }
        manager.config.dead_pty = false;
        // Exercise the real ConPTY -> native SSH window-change path. Merely
        // reflowing Rio's grid hides a remote PTY stuck at its default 80x24.
        for (columns, lines) in [(113, 31), (47, 12), (120, 40)] {
            manager
                .current()
                .messenger
                .send_resize(rio_backend::event::WindowSize {
                    cols: columns,
                    rows: lines,
                    width: columns * 8,
                    height: lines * 16,
                })
                .unwrap();
            let expected = format!("{lines} {columns}");
            let deadline = Instant::now() + std::time::Duration::from_secs(5);
            loop {
                manager.current().messenger.send_write(
                    b"printf '\\033]1337;SetUserVar=RIO_TEST_SIZE=%s\\007' \"$(stty size | tr -d '\\n' | base64 -w0)\"\r".to_vec(),
                );
                std::thread::sleep(std::time::Duration::from_millis(200));
                events.reply_to(manager.current());
                let actual = manager
                    .current()
                    .terminal
                    .lock()
                    .user_vars
                    .get("RIO_TEST_SIZE")
                    .cloned();
                if actual.as_deref() == Some(expected.as_str()) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "SSH PTY did not resize to {expected}; remote reported {actual:?}"
                );
            }
        }
        manager.current().messenger.send_write(
            b"printf '\\033]1337;SetUserVar=RIO_TEST_SLEEP=cmVhZHk=\\007'; sleep 30\r"
                .to_vec(),
        );
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while !manager
            .current()
            .terminal
            .lock()
            .user_vars
            .contains_key("RIO_TEST_SLEEP")
        {
            events.reply_to(manager.current());
            assert!(Instant::now() < deadline, "remote sleep did not start");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        manager.current().messenger.send_write(vec![3]);
        for _ in 0..15 {
            events.reply_to(manager.current());
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        manager.current().messenger.send_write(
            b"printf '\\033]1337;SetUserVar=RIO_TEST_INTERRUPT=YWxpdmU=\\007'\r".to_vec(),
        );
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while !manager
            .current()
            .terminal
            .lock()
            .user_vars
            .contains_key("RIO_TEST_INTERRUPT")
        {
            events.reply_to(manager.current());
            assert!(
                Instant::now() < deadline,
                "Ctrl+C did not interrupt the remote program and preserve its SSH shell"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let transport = manager
            .current()
            .terminal
            .lock()
            .user_vars
            .get("RIO_TEST_TRANSPORT")
            .cloned()
            .expect("parent did not report its SSH transport");
        manager.config.spawn_performer = true;
        manager.config.cwd = false;
        let home_config = manager.config_here(true).unwrap();
        assert!(home_config.shell.args.contains(&resolved_destination));
        manager.config.cwd = true;
        manager.add_context(true, 2);
        assert_eq!(manager.len(), 3);
        assert_eq!(
            manager.current().ssh.as_ref().unwrap().destination,
            connection.destination
        );
        // Exercise the same recipe/config path used by new tabs and both splits.
        // Report the actual sibling cwd; an echoed command cannot satisfy this.
        manager.current().messenger.send_write(
            b"printf '\\033]1337;SetUserVar=RIO_TEST_TRANSPORT=%s\\007' \"$(printf %s \"$SSH_CONNECTION\" | base64 -w0)\"; printf '\\033]1337;SetUserVar=RIO_TEST_CWD=%s\\007' \"$(printf %s \"$PWD\" | base64 -w0)\"\r".to_vec()
        );
        let deadline = Instant::now() + std::time::Duration::from_secs(20);
        loop {
            events.reply_to(manager.current());
            if manager
                .current()
                .terminal
                .lock()
                .user_vars
                .get("RIO_TEST_CWD")
                .is_some_and(|value| value == "/tmp")
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "sibling did not inherit the remote cwd"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        manager.contexts.remove(1);
        manager.current_index -= 1;
        assert_ne!(
            manager
                .current()
                .terminal
                .lock()
                .user_vars
                .get("RIO_TEST_TRANSPORT"),
            Some(&transport),
            "native Windows SSH sibling reused the original transport"
        );
        manager.current().messenger.send_write(
            b"printf '\\033]1337;SetUserVar=RIO_TEST_SURVIVED=YWxpdmU=\\007'\r".to_vec(),
        );
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while manager
            .current()
            .terminal
            .lock()
            .user_vars
            .get("RIO_TEST_SURVIVED")
            .is_none()
        {
            events.reply_to(manager.current());
            assert!(
                Instant::now() < deadline,
                "sibling stopped after closing the original pane"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        manager.add_context_here(true, 3, false);
        assert!(
            manager.current().ssh.is_none(),
            "New Local Tab inherited SSH"
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires RIO_TEST_SSH_TARGET and an already authenticated Windows SSH master"]
    fn live_ssh_tabs_and_split_launches_reuse_connection_and_directory() {
        use std::time::Duration;
        #[derive(Clone, Default)]
        struct SshEvents(Arc<std::sync::Mutex<Vec<(usize, String)>>>);
        impl EventListener for SshEvents {
            fn send_event(&self, event: RioEvent, _: WindowId) {
                if let RioEvent::PtyWrite(route, text) = event {
                    self.0.lock().unwrap().push((route, text));
                }
            }
        }
        let events = SshEvents::default();
        let target =
            std::env::var("RIO_TEST_SSH_TARGET").expect("set RIO_TEST_SSH_TARGET");
        let connection = crate::ssh::Connection::new(&target, true).unwrap();
        let shell = connection
            .duplicate_shell(None, None)
            .expect("authenticate the SSH master first");
        let dimension = ContextDimension::build(
            800.0,
            600.0,
            rio_backend::sugarloaf::layout::TextDimensions::default(),
            rio_backend::sugarloaf::layout::CellMetrics {
                cell_width: 8,
                cell_height: 16,
                cell_baseline: 12,
                face_width: 8.0,
                face_height: 16.0,
                face_y: 0.0,
            },
            1.0,
            14.0,
            Margin::default(),
        );
        let mut manager = ContextManager::start(
            (&Cursor::default(), false),
            events.clone(),
            WindowId::from(0),
            0,
            0,
            ContextManagerConfig {
                dead_pty: true,
                ..Default::default()
            },
            dimension,
            Margin::default(),
            None,
        )
        .unwrap();
        manager.config.dead_pty = false;
        manager.config.spawn_performer = true;
        manager.config.cwd = true;
        let mut config = manager.config.clone();
        config.shell = shell;
        manager.add_context_with_config(true, 1, config);

        let wait_for = |context: &Context<SshEvents>, variable: &str| -> String {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                // Native terminals send cursor/device queries before drawing.
                // Mirror the window event loop's replies in this headless test.
                {
                    let mut replies = events.0.lock().unwrap();
                    let mut index = 0;
                    while index < replies.len() {
                        if replies[index].0 == context.route_id {
                            let (_, text) = replies.remove(index);
                            context.messenger.send_write(text.into_bytes());
                        } else {
                            index += 1;
                        }
                    }
                }
                if let Some(value) =
                    context.terminal.lock().user_vars.get(variable).cloned()
                {
                    return value;
                }
                if context
                    ._io_thread
                    .as_ref()
                    .is_some_and(|thread| thread.is_finished())
                    || Instant::now() >= deadline
                {
                    let terminal = context.terminal.lock();
                    use rio_backend::crosswords::pos::{Column, Line, Pos};
                    let output = terminal.bounds_to_string(
                        Pos::new(Line(0), Column(0)),
                        Pos::new(
                            Line(context.dimension.lines as i32 - 1),
                            Column(context.dimension.columns - 1),
                        ),
                    );
                    panic!("SSH pane {} did not report {variable} before exiting or timing out: {output}", context.route_id);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let report = |context: &Context<SshEvents>, variable: &str, expression: &str| {
            context.messenger.send_write(format!(
                "[Console]::Write(\"$([char]27)]1337;SetUserVar={variable}=$([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes([string]({expression}))))$([char]7)\")\r"
            ).into_bytes());
        };
        // Wait for integration, then move the original shell without modifying
        // any remote files. The connection's server-side port proves reuse.
        wait_for(manager.current(), "RIO_SSH");
        manager.current().messenger.send_write(b"$global:RioReuseProbe = 'parent'; Set-Location -LiteralPath 'C:\\Windows'\r".to_vec());
        report(manager.current(), "RIO_TEST_READY", "$PWD.Path");
        assert_eq!(wait_for(manager.current(), "RIO_TEST_READY"), "C:\\Windows");
        report(
            manager.current(),
            "RIO_TEST_TRANSPORT",
            "$env:SSH_CONNECTION",
        );
        let transport = wait_for(manager.current(), "RIO_TEST_TRANSPORT");

        // Also cover SSH launched inside a shell: identify the live foreground
        // argv when Rio did not retain a connection recipe at creation time.
        manager.current_mut().ssh = None;
        assert_eq!(manager.current_ssh().unwrap().destination, target);

        manager.add_context(true, 2);
        assert_eq!(manager.len(), 3);
        wait_for(manager.current(), "RIO_SSH");
        report(manager.current(), "RIO_TEST_DIRECTORY", "$PWD.Path");
        assert_eq!(
            wait_for(manager.current(), "RIO_TEST_DIRECTORY"),
            "C:\\Windows"
        );
        report(
            manager.current(),
            "RIO_TEST_TRANSPORT",
            "$env:SSH_CONNECTION",
        );
        assert_eq!(wait_for(manager.current(), "RIO_TEST_TRANSPORT"), transport);
        report(manager.current(), "RIO_TEST_INDEPENDENT", "$null -eq (Get-Variable RioReuseProbe -Scope Global -ErrorAction SilentlyContinue)");
        assert_eq!(wait_for(manager.current(), "RIO_TEST_INDEPENDENT"), "True");

        // Both split directions call config_here and create_context. Exercise
        // that exact launch path without requiring a graphical window or GPU.
        for split in ["right", "down"] {
            let config = manager.config_here(true).unwrap();
            let pane = ContextManager::create_context(
                (&Cursor::default(), false),
                events.clone(),
                WindowId::from(0),
                3,
                manager.current().dimension,
                &config,
            )
            .unwrap();
            wait_for(&pane, "RIO_SSH");
            report(&pane, "RIO_TEST_DIRECTORY", "$PWD.Path");
            assert_eq!(
                wait_for(&pane, "RIO_TEST_DIRECTORY"),
                "C:\\Windows",
                "{split} split"
            );
            report(&pane, "RIO_TEST_TRANSPORT", "$env:SSH_CONNECTION");
            assert_eq!(
                wait_for(&pane, "RIO_TEST_TRANSPORT"),
                transport,
                "{split} split"
            );
        }

        // Closing the original pane must not close its sibling shell.
        manager.contexts.remove(1);
        manager.current_index -= 1;
        report(manager.current(), "RIO_TEST_SURVIVED", "'alive'");
        assert_eq!(wait_for(manager.current(), "RIO_TEST_SURVIVED"), "alive");
    }

    #[test]
    fn test_capacity() {
        let window_id: WindowId = WindowId::from(0);

        let context_manager =
            ContextManager::start_with_capacity(5, VoidListener {}, window_id).unwrap();
        assert_eq!(context_manager.capacity, 5);

        let mut context_manager =
            ContextManager::start_with_capacity(5, VoidListener {}, window_id).unwrap();
        context_manager.increase_capacity(3);
        assert_eq!(context_manager.capacity, 8);
    }

    #[test]
    fn test_add_context() {
        let window_id: WindowId = WindowId::from(0);

        let mut context_manager =
            ContextManager::start_with_capacity(5, VoidListener {}, window_id).unwrap();
        assert_eq!(context_manager.capacity, 5);
        assert_eq!(context_manager.current_index, 0);

        let should_redirect = false;
        context_manager.add_context(should_redirect, 0);
        assert_eq!(context_manager.capacity, 5);
        assert_eq!(context_manager.current_index, 0);

        let should_redirect = true;
        context_manager.add_context(should_redirect, 0);
        assert_eq!(context_manager.capacity, 5);
        assert_eq!(context_manager.current_index, 2);
    }

    #[test]
    fn test_add_context_start_with_capacity_limit() {
        let window_id: WindowId = WindowId::from(0);

        let mut context_manager =
            ContextManager::start_with_capacity(3, VoidListener {}, window_id).unwrap();
        assert_eq!(context_manager.capacity, 3);
        assert_eq!(context_manager.current_index, 0);
        let should_redirect = false;
        context_manager.add_context(should_redirect, 0);
        assert_eq!(context_manager.len(), 2);
        context_manager.add_context(should_redirect, 0);
        assert_eq!(context_manager.len(), 3);

        for _ in 0..20 {
            context_manager.add_context(should_redirect, 0);
        }

        assert_eq!(context_manager.len(), 3);
        assert_eq!(context_manager.capacity, 3);
    }

    /// The tmux `new -ADXs` crash (#1848): SIGHUP kills a BACKGROUND
    /// tab's shell, the tab at a lower index is removed, and the
    /// focused index must shift left with it or the very next
    /// `contexts[current_index]` is out of bounds (len 1, index 1).
    #[test]
    fn current_index_adjusts_after_tab_removal() {
        // Background tab before the focused one dies: shift left.
        assert_eq!(current_index_after_tab_removal(1, 0), 0);
        assert_eq!(current_index_after_tab_removal(3, 1), 2);
        // Background tab after the focused one: nothing shifts.
        assert_eq!(current_index_after_tab_removal(0, 1), 0);
        assert_eq!(current_index_after_tab_removal(2, 3), 2);
        // The focused tab itself: fall back to the left neighbor.
        assert_eq!(current_index_after_tab_removal(0, 0), 0);
        assert_eq!(current_index_after_tab_removal(2, 2), 1);
    }

    #[test]
    fn test_set_current() {
        let window_id: WindowId = WindowId::from(0);

        let mut context_manager =
            ContextManager::start_with_capacity(8, VoidListener {}, window_id).unwrap();
        let should_redirect = true;

        context_manager.add_context(should_redirect, 0);
        assert_eq!(context_manager.current_index, 1);
        context_manager.set_current(0);
        assert_eq!(context_manager.current_index, 0);
        assert_eq!(context_manager.len(), 2);
        assert_eq!(context_manager.capacity, 8);

        let should_redirect = false;
        context_manager.add_context(should_redirect, 0);
        context_manager.add_context(should_redirect, 0);
        context_manager.set_current(3);
        assert_eq!(context_manager.current_index, 3);

        context_manager.set_current(8);
        assert_eq!(context_manager.current_index, 3);
    }

    #[test]
    #[ignore = "manual tab chrome contention measurement"]
    fn measure_tab_chrome_contention() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let mut cm =
            ContextManager::start_with_capacity(8, VoidListener {}, WindowId::from(0))
                .unwrap();
        for _ in 1..8 {
            cm.add_context(false, 0);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(std::sync::Barrier::new(9));
        let producers: Vec<_> = cm
            .contexts
            .iter()
            .map(|grid| {
                let terminal = grid.current().terminal.clone();
                let stop = stop.clone();
                let ready = ready.clone();
                std::thread::spawn(move || {
                    let bytes =
                        b"\x1b[31mred\x1b[0m \x1b[1;32mgreen\x1b[0m\r\n".repeat(8192);
                    let mut parser = crate::performer::handler::Processor::default();
                    ready.wait();
                    while !stop.load(Ordering::Relaxed) {
                        let _lease = terminal.lease();
                        let mut terminal = terminal.lock_unfair();
                        parser.advance(&mut *terminal, &bytes);
                    }
                })
            })
            .collect();
        ready.wait();
        let mut locked = Vec::new();
        let mut published = Vec::new();
        for _ in 0..100 {
            let start = Instant::now();
            for grid in &cm.contexts {
                let context = grid.current();
                let terminal = context.terminal.lock();
                std::hint::black_box((
                    terminal.title.clone(),
                    terminal.user_vars.get("RIO_APP").cloned(),
                    terminal.user_vars.get("RIO_SSH").cloned(),
                ));
                drop(terminal);
                std::hint::black_box(
                    context.terminal.lock().program_status.display_status(),
                );
            }
            locked.push(start.elapsed().as_micros());
            let start = Instant::now();
            for index in 0..cm.len() {
                std::hint::black_box(cm.application_for_tab(index));
                std::hint::black_box(cm.program_status_for_tab(index));
            }
            published.push(start.elapsed().as_micros());
        }
        stop.store(true, Ordering::Relaxed);
        for producer in producers {
            producer.join().unwrap();
        }
        locked.sort_unstable();
        published.sort_unstable();
        println!("8 busy tabs, 100 samples: terminal_lock_p50_us={} terminal_lock_p95_us={} metadata_p50_us={} metadata_p95_us={}", locked[50], locked[95], published[50], published[95]);
    }

    #[test]
    fn tab_chrome_does_not_wait_for_pty_terminal_lock() {
        let cm =
            ContextManager::start_with_capacity(2, VoidListener {}, WindowId::from(0))
                .unwrap();
        let terminal = cm.current().terminal.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let producer = std::thread::spawn(move || {
            let _lease = terminal.lease();
            let _terminal = terminal.lock_unfair();
            ready_tx.send(()).unwrap();
            done_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .is_ok()
        });
        ready_rx.recv().unwrap();
        assert!(cm.application_for_tab(0).is_some());
        assert!(cm.program_status_for_tab(0).is_none());
        let _ = done_tx.send(());
        assert!(
            producer.join().unwrap(),
            "tab chrome waited for the PTY lock"
        );
    }

    #[test]
    fn bell_marks_background_tabs_only_and_clears_on_focus() {
        let mut cm =
            ContextManager::start_with_capacity(5, VoidListener {}, WindowId::from(0))
                .unwrap();
        cm.add_context(true, 0);
        cm.add_context(true, 0);
        cm.set_current(0);

        let background_route = cm.contexts[2].current().route_id;
        let focused_route = cm.contexts[0].current().route_id;

        assert!(cm.ring_bell(background_route, true));
        assert!(cm.bell(2));

        assert!(!cm.ring_bell(background_route, true));
        assert!(cm.bell(2));

        assert!(!cm.ring_bell(focused_route, true));
        assert!(!cm.bell(0));

        assert!(cm.ring_bell(focused_route, false));
        assert!(cm.bell(0));
        cm.contexts[0].bell = false;

        // Unknown routes (already-closed panes) are a no-op.
        assert!(!cm.ring_bell(usize::MAX, true));

        // Focusing the tab drops the mark on the next rendered frame.
        cm.set_current(2);
        assert!(cm.bell(2));
        assert!(cm.clear_current_bell());
        assert!(!cm.bell(2));
    }

    #[test]
    fn tab_paths_follow_local_and_remote_osc_directory_reports() {
        use rio_backend::performer::handler::Processor;

        let mut cm =
            ContextManager::start_with_capacity(2, VoidListener {}, WindowId::from(0))
                .unwrap();
        cm.config.title = rio_backend::config::title::Title::default();
        cm.current_mut().spawned_program = "ssh.exe".into();
        cm.current_mut().ssh =
            Some(crate::ssh::Connection::new("user@remote", false).unwrap());
        let mut parser = Processor::default();
        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]2;ssh.exe\x07\x1b]7;file://remote.invalid/home/user/project\x07",
        );
        cm.update_titles(true);
        assert_eq!(cm.displayed_title_for_current_tab(), "/home/user/project");
        assert!(cm.current().terminal.lock().current_directory.is_none());

        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]7;file://remote.invalid/home/user/other\x07",
        );
        cm.update_titles(true);
        assert_eq!(cm.displayed_title_for_current_tab(), "/home/user/other");

        let route = cm.current().route_id;
        assert!(!cm.on_title_change(route, Some("vim remote.txt")));
        assert_eq!(cm.displayed_title_for_current_tab(), "/home/user/other");
        assert!(!cm.on_title_change(route, Some("")));
        assert_eq!(cm.displayed_title_for_current_tab(), "/home/user/other");

        cm.add_context_here(true, 0, false);
        cm.current_mut().spawned_program = "fish".into();
        #[cfg(windows)]
        let (directory_report, expected_path) = (
            "\x1b]7;file:///C:/rio-local-project\x07",
            "C:/rio-local-project",
        );
        #[cfg(not(windows))]
        let (directory_report, expected_path) =
            ("\x1b]7;file:///rio-local-project\x07", "/rio-local-project");
        parser.advance(
            &mut *cm.current().terminal.lock(),
            directory_report.as_bytes(),
        );
        parser.advance(&mut *cm.current().terminal.lock(), b"\x1b]2;fish\x07");
        cm.update_titles(true);
        assert_eq!(cm.displayed_title_for_current_tab(), expected_path);
        cm.set_current(0);
        assert_eq!(cm.displayed_title_for_current_tab(), "/home/user/other");
    }

    #[test]
    fn ssh_tab_title_uses_destination_instead_of_transport_program() {
        let mut cm =
            ContextManager::start_with_capacity(1, VoidListener {}, WindowId::from(0))
                .unwrap();
        cm.current_mut().spawned_program = "ssh.exe".into();
        cm.current_mut().ssh =
            Some(crate::ssh::Connection::new("user@remote", false).unwrap());
        cm.config.title.content = "{{ TITLE || PROGRAM }}".into();

        // Windows' default template falls back to the native SSH executable
        // when the remote shell does not send an OSC title.
        cm.update_titles(true);
        assert_eq!(cm.displayed_title_for_current_tab(), "user@remote");

        let route = cm.current().route_id;
        cm.on_title_change(route, Some("ssh.exe"));
        assert_eq!(cm.displayed_title_for_current_tab(), "user@remote");
        cm.on_title_change(route, Some("vim remote.txt"));
        assert_eq!(cm.displayed_title_for_current_tab(), "vim remote.txt");

        cm.set_custom_title(0, Some("Production".into()));
        assert_eq!(cm.displayed_title_for_current_tab(), "Production");
        cm.set_custom_title(0, None);
        cm.current_mut().title.content.clear();
        assert_eq!(cm.displayed_title_for_current_tab(), "user@remote");

        cm.current_mut().ssh = None;
        assert_eq!(cm.displayed_title_for_current_tab(), "ssh.exe");
    }

    #[test]
    fn update_titles_only_current_leaves_other_tabs_dirty() {
        let mut cm =
            ContextManager::start_with_capacity(5, VoidListener {}, WindowId::from(0))
                .unwrap();
        cm.add_context(false, 0);
        cm.config.title.content = "{{ columns }}".to_string();

        cm.mark_all_titles_dirty();
        assert!(cm.contexts[0].current().title_dirty);
        assert!(cm.contexts[1].current().title_dirty);

        // A current-only walk (tab strip absent) must not silently
        // clear panes it never re-rendered.
        assert!(cm.update_titles(true));
        assert!(!cm.contexts[0].current().title_dirty);
        assert!(cm.contexts[1].current().title_dirty);
        let columns = cm.contexts[0].current().dimension.columns.to_string();
        assert_eq!(cm.contexts[0].current().title.content, columns);

        // Surfacing the stale tab re-renders it via sync_current_route.
        cm.set_current(1);
        assert!(!cm.contexts[1].current().title_dirty);
        let columns = cm.contexts[1].current().dimension.columns.to_string();
        assert_eq!(cm.contexts[1].current().title.content, columns);
    }

    #[test]
    fn on_title_change_renders_displayed_pane_from_event_string() {
        let mut cm =
            ContextManager::start_with_capacity(5, VoidListener {}, WindowId::from(0))
                .unwrap();
        cm.add_context(false, 0);
        cm.config.title.content = "{{ title }}".to_string();

        let background_route = cm.contexts[1].current().route_id;
        cm.contexts[1].current_mut().title_dirty = true;
        assert!(cm.on_title_change(background_route, Some("hello")));
        assert_eq!(cm.contexts[1].current().title.content, "hello");
        assert!(!cm.contexts[1].current().title_dirty);

        // The same title again changes nothing: no repaint requested.
        assert!(!cm.on_title_change(background_route, Some("hello")));

        // Unknown routes (already-closed panes) are a no-op.
        assert!(!cm.on_title_change(usize::MAX, Some("x")));
    }

    fn set_tab_title(cm: &mut ContextManager<VoidListener>, index: usize, content: &str) {
        cm.contexts[index].current_mut().title.content = content.to_string();
    }

    fn tab_titles(cm: &ContextManager<VoidListener>) -> Vec<String> {
        (0..cm.len())
            .map(|i| cm.title(i).unwrap().content.clone())
            .collect()
    }

    #[test]
    fn test_title_follows_tab_move() {
        let window_id = WindowId::from(0);
        let mut cm =
            ContextManager::start_with_capacity(5, VoidListener {}, window_id).unwrap();
        for _ in 0..3 {
            cm.add_context(false, 0);
        }
        assert_eq!(cm.len(), 4);
        for (i, label) in ["a", "b", "c", "d"].iter().enumerate() {
            set_tab_title(&mut cm, i, label);
        }

        // Drag tab 1 to slot 3 (rotate). The title must track the moved
        // tab immediately, without waiting on the next update_titles tick.
        cm.set_current(1);
        cm.move_current_tab_to(3);

        assert_eq!(tab_titles(&cm), ["a", "c", "d", "b"]);
        assert_eq!(cm.current().title.content, "b");
    }

    #[test]
    fn test_title_follows_tab_swap() {
        let window_id = WindowId::from(0);
        let mut cm =
            ContextManager::start_with_capacity(5, VoidListener {}, window_id).unwrap();
        for _ in 0..3 {
            cm.add_context(false, 0);
        }
        for (i, label) in ["a", "b", "c", "d"].iter().enumerate() {
            set_tab_title(&mut cm, i, label);
        }

        // Swap current (0) with its neighbor (1).
        cm.set_current(0);
        cm.move_current_to_next();

        assert_eq!(tab_titles(&cm), ["b", "a", "c", "d"]);
    }

    #[test]
    fn test_custom_title_follows_tab_move() {
        let window_id = WindowId::from(0);
        let mut cm =
            ContextManager::start_with_capacity(5, VoidListener {}, window_id).unwrap();
        for _ in 0..3 {
            cm.add_context(false, 0);
        }
        cm.set_custom_title(2, Some("work".to_string()));

        // Move tab 1 → 3 (rotate): the override on tab 2 shifts to slot 1,
        // with no remap bookkeeping.
        cm.set_current(1);
        cm.move_current_tab_to(3);

        assert_eq!(cm.custom_title(1), Some("work"));
        assert_eq!(cm.custom_title(2), None);

        // Clearing with None removes the override.
        cm.set_custom_title(1, None);
        assert_eq!(cm.custom_title(1), None);
    }

    #[test]
    fn test_custom_color_follows_tab_move() {
        let window_id = WindowId::from(0);
        let mut cm =
            ContextManager::start_with_capacity(5, VoidListener {}, window_id).unwrap();
        for _ in 0..3 {
            cm.add_context(false, 0);
        }
        let red = [1.0, 0.0, 0.0, 1.0];
        cm.set_custom_color(2, Some(red));

        // Move tab 1 → 3 (rotate): the color on tab 2 shifts to slot 1.
        cm.set_current(1);
        cm.move_current_tab_to(3);

        assert_eq!(cm.custom_color(1), Some(red));
        assert_eq!(cm.custom_color(2), None);

        cm.set_custom_color(1, None);
        assert_eq!(cm.custom_color(1), None);
    }

    #[test]
    fn test_switch_to_next() {
        let window_id: WindowId = WindowId::from(0);

        let mut context_manager =
            ContextManager::start_with_capacity(5, VoidListener {}, window_id).unwrap();
        let should_redirect = false;

        context_manager.add_context(should_redirect, 0);
        context_manager.add_context(should_redirect, 0);
        context_manager.add_context(should_redirect, 0);
        context_manager.add_context(should_redirect, 0);
        context_manager.add_context(should_redirect, 0);
        assert_eq!(context_manager.len(), 5);
        assert_eq!(context_manager.current_index, 0);

        context_manager.switch_to_next();
        assert_eq!(context_manager.current_index, 1);
        context_manager.switch_to_next();
        assert_eq!(context_manager.current_index, 2);
        context_manager.switch_to_next();
        assert_eq!(context_manager.current_index, 3);
        context_manager.switch_to_next();
        assert_eq!(context_manager.current_index, 4);
        context_manager.switch_to_next();
        assert_eq!(context_manager.current_index, 0);
        context_manager.switch_to_next();
        assert_eq!(context_manager.current_index, 1);
    }

    #[test]
    fn test_move_current_to_next() {
        let window_id = WindowId::from(0);

        let mut context_manager =
            ContextManager::start_with_capacity(5, VoidListener {}, window_id).unwrap();
        let should_redirect = false;

        context_manager.current_mut().rich_text_id = 1;
        context_manager.add_context(should_redirect, 0);
        context_manager.add_context(should_redirect, 0);
        context_manager.add_context(should_redirect, 0);
        context_manager.add_context(should_redirect, 0);

        assert_eq!(context_manager.len(), 5);
        assert_eq!(context_manager.current_index, 0);
        assert_eq!(context_manager.current().rich_text_id, 1);

        context_manager.move_current_to_next();
        assert_eq!(context_manager.current_index, 1);
        assert_eq!(context_manager.current().rich_text_id, 1);

        context_manager.move_current_to_next();
        assert_eq!(context_manager.current_index, 2);
        assert_eq!(context_manager.current().rich_text_id, 1);

        context_manager.move_current_to_next();
        assert_eq!(context_manager.current_index, 3);
        assert_eq!(context_manager.current().rich_text_id, 1);

        context_manager.move_current_to_next();
        assert_eq!(context_manager.current_index, 4);
        assert_eq!(context_manager.current().rich_text_id, 1);

        context_manager.move_current_to_next();
        assert_eq!(context_manager.current_index, 0);
        assert_eq!(context_manager.current().rich_text_id, 1);

        context_manager.move_current_to_next();
        assert_eq!(context_manager.current_index, 1);
        assert_eq!(context_manager.current().rich_text_id, 1);
    }

    #[test]
    fn test_move_current_to_prev() {
        let window_id = WindowId::from(0);

        let mut context_manager =
            ContextManager::start_with_capacity(5, VoidListener {}, window_id).unwrap();
        let should_redirect = false;

        context_manager.current_mut().rich_text_id = 1;
        context_manager.add_context(should_redirect, 0);
        context_manager.add_context(should_redirect, 0);
        context_manager.add_context(should_redirect, 0);
        context_manager.add_context(should_redirect, 0);

        assert_eq!(context_manager.len(), 5);
        assert_eq!(context_manager.current_index, 0);
        assert_eq!(context_manager.current().rich_text_id, 1);

        context_manager.move_current_to_prev();
        assert_eq!(context_manager.current_index, 4);
        assert_eq!(context_manager.current().rich_text_id, 1);

        context_manager.move_current_to_prev();
        assert_eq!(context_manager.current_index, 3);
        assert_eq!(context_manager.current().rich_text_id, 1);

        context_manager.move_current_to_prev();
        assert_eq!(context_manager.current_index, 2);
        assert_eq!(context_manager.current().rich_text_id, 1);

        context_manager.move_current_to_prev();
        assert_eq!(context_manager.current_index, 1);
        assert_eq!(context_manager.current().rich_text_id, 1);

        context_manager.move_current_to_prev();
        assert_eq!(context_manager.current_index, 0);
        assert_eq!(context_manager.current().rich_text_id, 1);

        context_manager.move_current_to_prev();
        assert_eq!(context_manager.current_index, 4);
        assert_eq!(context_manager.current().rich_text_id, 1);
    }

    #[test]
    fn test_move_current_tab_to() {
        let window_id = WindowId::from(0);

        let mut context_manager =
            ContextManager::start_with_capacity(5, VoidListener {}, window_id).unwrap();
        let should_redirect = false;

        context_manager.add_context(should_redirect, 0);
        context_manager.add_context(should_redirect, 0);
        context_manager.add_context(should_redirect, 0);
        context_manager.add_context(should_redirect, 0);

        // Tag every tab with its starting position.
        for i in 0..5 {
            context_manager.set_current(i);
            context_manager.current_mut().rich_text_id = i;
        }

        let order = |cm: &mut ContextManager<VoidListener>| -> Vec<usize> {
            (0..5)
                .map(|i| {
                    cm.set_current(i);
                    cm.current().rich_text_id
                })
                .collect()
        };

        // Multi-slot jump forward: tabs in between shift left by one.
        context_manager.set_current(1);
        context_manager.move_current_tab_to(3);
        assert_eq!(context_manager.current_index, 3);
        assert_eq!(context_manager.current().rich_text_id, 1);
        assert_eq!(order(&mut context_manager), vec![0, 2, 3, 1, 4]);

        // Multi-slot jump backward: tabs in between shift right by one.
        context_manager.set_current(3);
        context_manager.move_current_tab_to(0);
        assert_eq!(context_manager.current_index, 0);
        assert_eq!(context_manager.current().rich_text_id, 1);
        assert_eq!(order(&mut context_manager), vec![1, 0, 2, 3, 4]);

        // No-op cases: same index and out-of-bounds target.
        context_manager.set_current(2);
        context_manager.move_current_tab_to(2);
        assert_eq!(context_manager.current_index, 2);
        context_manager.move_current_tab_to(5);
        assert_eq!(context_manager.current_index, 2);
        assert_eq!(order(&mut context_manager), vec![1, 0, 2, 3, 4]);
    }
}
