use crate::simd_base64;
use rio_graphics::{ColorType, GraphicData, GraphicId, ResizeCommand, ResizeParameter};
use smallvec::SmallVec;
use tracing::debug;

#[path = "kitty_file.rs"]
mod kitty_file;
#[cfg(feature = "graphics")]
#[path = "kitty_png.rs"]
mod kitty_png;

/// Maximum width or height (per axis) we accept for a kitty-graphics
/// image. Matches upstream kitty. Anything larger is a DoS
/// vector — we refuse with `EINVAL: dimensions too large`.
const MAX_DIMENSION: u32 = 10_000;

/// Maximum decoded payload size (bytes) we accept. 400 MiB matches
/// upstream kitty. Guards against runaway base64 blobs filling
/// memory before `create_graphic_data` validates them.
const MAX_SIZE: usize = 400 * 1024 * 1024;
const MAX_INCOMPLETE_BYTES: usize = MAX_SIZE;

/// Per-screen state for Kitty graphics uploads and automatic image IDs.
#[derive(Debug, Default)]
pub struct KittyGraphicsState {
    /// One load per screen. Direct transmission chunks continue this load
    /// regardless of their IDs or repeated metadata.
    loading: Option<KittyGraphicsCommand>,

    /// Next candidate for implicit image IDs. Numbered transmissions instead
    /// start at one, and the image store skips IDs already in use.
    next_auto_image_id: u32,

    /// Explicit image replaced by a newly started transmission. Drained by
    /// the dispatcher even when image decoding or chunk assembly fails.
    started_transmission: Option<u32>,
}

impl KittyGraphicsState {
    pub(crate) fn take_started_transmission(&mut self) -> Option<u32> {
        self.started_transmission.take()
    }

    /// Allocate a fresh image_id for an implicit transmission.
    fn allocate_image_id(
        &mut self,
        implicit: bool,
        available_id: &mut impl FnMut(u32) -> u32,
    ) -> u32 {
        let candidate = if implicit {
            if self.next_auto_image_id == 0 {
                self.next_auto_image_id = 2147483647;
            }
            self.next_auto_image_id
        } else {
            1
        };
        let id = available_id(candidate);
        if implicit {
            self.next_auto_image_id = id.wrapping_add(1).max(1);
        }
        id
    }
}

#[derive(Debug)]
pub struct KittyGraphicsResponse {
    pub image_number: u32,
    pub animation_request: Option<super::kitty_animation::Request>,
    pub graphic_data: Option<GraphicData>,
    pub placement_request: Option<PlacementRequest>,
    pub delete_request: Option<DeleteRequest>,
    pub response: Option<String>,
    /// Pre-encoded failure reply for actions whose outcome only the
    /// terminal state layer knows (today: `a=p`, which needs the image
    /// store to tell whether the referenced image exists). The
    /// dispatcher sends `response` on success and this on failure.
    /// Built at parse time because only the parser knows the ids and
    /// the `q=` quiet level.
    pub error_response: Option<String>,
    /// True when this "response" is just a chunk-accumulation
    /// acknowledgement — the parser stored the chunk and is waiting
    /// for more. The dispatcher should treat this as a successful
    /// no-op and must NOT log a parse failure for it (yazi and other
    /// TUIs send hundreds of chunked frames per second; spamming
    /// warnings on each chunk is the bug fix this field enables).
    pub incomplete: bool,
}

impl KittyGraphicsResponse {
    /// Reuse the encoded identity and quiet policy of the completed command,
    /// which can differ from the controls repeated on its final chunk.
    pub(crate) fn placement_failure(&self, message: &str) -> Option<String> {
        let (header, _) = self.error_response.as_ref()?.split_once(';')?;
        Some(format!("{header};{message}\x1b\\"))
    }

    /// Sentinel returned for an in-progress chunked transmission. The
    /// dispatcher recognises this as "data accumulated, no action
    /// needed", as opposed to `None` which now means "real parse
    /// error".
    fn pending_chunk() -> Self {
        Self {
            image_number: 0,
            animation_request: None,
            graphic_data: None,
            placement_request: None,
            delete_request: None,
            response: None,
            error_response: None,
            incomplete: true,
        }
    }
}

#[derive(Debug)]
pub struct PlacementRequest {
    pub relative: Option<super::graphics::RelativePlacement>,
    pub image_id: u32,
    pub placement_id: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub columns: u32,
    pub rows: u32,
    pub z_index: i32,
    /// Set when the request came in with `U=1` — the kitty Unicode-
    /// placeholder mode. The terminal should only register the
    /// placement metadata; the application emits the U+10EEEE
    /// placeholder cells itself afterwards (see kitty
    /// `kittens/icat/transmit.go:221` `write_unicode_placeholder`). The
    /// renderer scans visible cells for U+10EEEE and composites the
    /// image at the matching positions.
    pub virtual_placement: bool,
    /// Value of `u=N` (decimal codepoint that the application embedded
    /// in the placement). Distinct from `virtual_placement` (which is
    /// the uppercase `U=1` flag). Currently informational only.
    pub unicode_placeholder: u32,
    pub cursor_movement: u8, // 0 = move cursor to after image (default), 1 = don't move cursor
    /// kitty `X=`/`Y=` pixel offset, supports sub-cell positioning.
    pub cell_x_offset: u32,
    pub cell_y_offset: u32,
}

#[derive(Debug)]
pub struct DeleteRequest {
    pub frame_number: u32,
    pub action: u8,
    pub image_id: u32,
    /// Image number (I= key) — used by `d=n/N` variants to resolve an
    /// image via the client-assigned number rather than its id.
    pub image_number: u32,
    pub placement_id: u32,
    pub x: u32,
    pub y: u32,
    pub z_index: i32,
    pub delete_data: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Action {
    Transmit,
    TransmitAndDisplay,
    Query,
    Put,
    Delete,
    Frame,
    Animate,
    Compose,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Format {
    Gray,      // 1 byte per pixel
    GrayAlpha, // 2 bytes per pixel
    Rgb24,     // 3 bytes per pixel
    Rgba32,    // 4 bytes per pixel
    Png,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TransmissionMedium {
    Direct,
    File,
    TempFile,
    SharedMemory,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Compression {
    None,
    Zlib,
}

#[derive(Debug, Clone)]
pub struct KittyGraphicsCommand {
    animation_target: Option<super::kitty_animation::ImageIdentity>,
    // Action
    action: Action,
    quiet: u8,

    /// True when `image_id` was auto-assigned because the client did
    /// not supply `i=` or `I=`. Per kitty spec we must not echo a
    /// response back for these commands even though we now have an id
    /// internally.
    implicit_id: bool,

    // Image transmission
    format: Format,
    format_unknown: bool,
    medium: TransmissionMedium,
    width: u32,
    height: u32,
    size: u32,
    offset: u32,
    image_id: u32,
    image_number: u32,
    placement_id: u32,
    compression: Compression,
    more: bool,

    // Image display
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
    cell_x_offset: u32,
    cell_y_offset: u32,
    columns: u32,
    rows: u32,
    cursor_movement: u8,
    virtual_placement: bool,
    z_index: i32,
    parent_id: u32,
    parent_placement_id: u32,
    relative_x: i32,
    relative_y: i32,

    // Animation frame loading
    frame_number: u32,
    base_frame: u32,
    frame_gap: i32,
    composition_mode: u8,
    background_color: u32,

    // Animation control
    animation_state: u8,
    loop_count: u32,
    current_frame: u32,

    // Delete
    delete_action: u8,

    // Placeholder
    unicode_placeholder: u32,

    /// Payload, always stored as already-base64-decoded bytes.
    ///
    /// We decode each APC command's base64 payload up
    /// front in `parse()` so that clients which pad every chunk
    /// independently (e.g. chafa) don't produce a concatenated base64
    /// string with `=` bytes stuck in the middle when multiple chunks
    /// are merged.
    ///
    /// 64 bytes inline covers most control-only commands (query, delete,
    /// placement) while still handling large image data by spilling to
    /// heap.
    payload: SmallVec<[u8; 64]>,
}

impl Default for KittyGraphicsCommand {
    fn default() -> Self {
        Self {
            animation_target: None,
            action: Action::Transmit,
            quiet: 0,
            implicit_id: false,
            format: Format::Rgba32,
            format_unknown: false,
            medium: TransmissionMedium::Direct,
            width: 0,
            height: 0,
            size: 0,
            offset: 0,
            image_id: 0,
            image_number: 0,
            placement_id: 0,
            compression: Compression::None,
            more: false,
            source_x: 0,
            source_y: 0,
            source_width: 0,
            source_height: 0,
            cell_x_offset: 0,
            cell_y_offset: 0,
            columns: 0,
            rows: 0,
            cursor_movement: 0,
            virtual_placement: false,
            z_index: 0,
            parent_id: 0,
            parent_placement_id: 0,
            relative_x: 0,
            relative_y: 0,
            frame_number: 0,
            base_frame: 0,
            frame_gap: 0,
            composition_mode: 0,
            background_color: 0,
            animation_state: 0,
            loop_count: 0,
            current_frame: 0,
            delete_action: b'a',
            unicode_placeholder: 0,
            payload: SmallVec::new(),
        }
    }
}

/// Build an APC response string of the form
/// `\x1b_G<keys>;<message>\x1b\\`.
///
/// `image_id`, `image_number`, `placement_id` are all emitted (in that
/// order, comma-separated) when non-zero. When *all* of them are zero
/// this returns `None` — per kitty spec we don't send a response
/// without an identifier.
fn encode_response(
    image_id: u32,
    image_number: u32,
    placement_id: u32,
    message: &str,
) -> Option<String> {
    if image_id == 0 && image_number == 0 {
        return None;
    }

    let mut keys = String::new();
    if image_id > 0 {
        keys.push_str(&format!("i={image_id}"));
    }
    if image_number > 0 {
        if !keys.is_empty() {
            keys.push(',');
        }
        keys.push_str(&format!("I={image_number}"));
    }
    if placement_id > 0 {
        if !keys.is_empty() {
            keys.push(',');
        }
        keys.push_str(&format!("p={placement_id}"));
    }

    Some(format!("\x1b_G{keys};{message}\x1b\\"))
}

/// Same as `encode_response` but respects the `q=` quiet setting:
/// - `q=0`: emit both successes and failures
/// - `q=1`: emit only failures
/// - `q=2`: emit nothing
fn encode_response_quiet(
    image_id: u32,
    image_number: u32,
    placement_id: u32,
    message: &str,
    quiet: u8,
    is_error: bool,
) -> Option<String> {
    match quiet {
        0 => encode_response(image_id, image_number, placement_id, message),
        1 if is_error => encode_response(image_id, image_number, placement_id, message),
        _ => None,
    }
}

pub(crate) fn animation_response(
    identity: super::kitty_animation::Reply,
    result: Result<(u32, Option<u32>), super::kitty_animation::Failure>,
) -> Option<String> {
    let (id, frame, message, error) = match result {
        Ok((id, frame)) => {
            if identity.silent_success {
                return None;
            }
            (id, frame, "OK", false)
        }
        Err(error) => (
            identity.image_id,
            error.frame.or(identity.frame),
            error.message,
            true,
        ),
    };
    let mut response = encode_response_quiet(
        id,
        identity.image_number,
        identity.placement_id,
        message,
        identity.quiet,
        error,
    )?;
    if let Some(frame) = frame.filter(|frame| *frame != 0) {
        response.insert_str(response.find(';')?, &format!(",r={frame}"));
    }
    Some(response)
}

pub fn parse(
    params: &[&[u8]],
    state: &mut KittyGraphicsState,
) -> Option<KittyGraphicsResponse> {
    parse_with_resources(params, state, |candidate| candidate, |_, _| Ok(None))
}

pub(crate) fn parse_with_resources(
    params: &[&[u8]],
    state: &mut KittyGraphicsState,
    mut available_id: impl FnMut(u32) -> u32,
    mut image_identity: impl FnMut(
        u32,
        u32,
    ) -> Result<
        Option<super::kitty_animation::ImageIdentity>,
        &'static str,
    >,
) -> Option<KittyGraphicsResponse> {
    state.started_transmission = None;
    let Some(&b"G") = params.first() else {
        debug!("Kitty graphics parse failed: first param is not 'G'");
        return None;
    };
    debug!(
        "Kitty graphics parse: starting with {} params",
        params.len()
    );
    for (i, param) in params.iter().enumerate() {
        debug!(
            "  param[{}] length={}, preview={:?}",
            i,
            param.len(),
            std::str::from_utf8(&param[..param.len().min(50)])
                .unwrap_or("(invalid utf8)")
        );
    }

    let mut cmd = KittyGraphicsCommand::default();

    // Parse control data if present
    if let Some(control) = params.get(1) {
        if !control.is_empty() {
            let control_data = String::from_utf8_lossy(control);
            let valid = parse_control_data(&mut cmd, &control_data);
            if !valid || std::str::from_utf8(control).is_err() {
                return None;
            }
        }
    }

    let data_action = matches!(
        cmd.action,
        Action::Transmit | Action::TransmitAndDisplay | Action::Query | Action::Frame
    );
    let transmission = matches!(
        cmd.action,
        Action::Transmit | Action::TransmitAndDisplay | Action::Frame
    );
    if !transmission || cmd.medium != TransmissionMedium::Direct {
        cmd.more = false;
    }

    // Decode payload if present. We always decode base64 up front
    // so that each APC command's payload is
    // self-contained: clients like chafa which pad every chunk
    // independently can be merged by simply concatenating the decoded
    // byte streams, rather than trying to splice base64 text and running
    // into stray `=` padding in the middle.
    if let Some(payload) = params.get(2).filter(|_| data_action) {
        if !payload.is_empty() {
            let decoded = decode_payload_base64(payload)?;
            cmd.payload = SmallVec::from_vec(decoded);
        }
    }

    // Validation: `i=` and `I=` are mutually exclusive per kitty spec
    // (the image is either referenced by id or by number, never both).
    if cmd.image_id > 0 && cmd.image_number > 0 {
        return Some(KittyGraphicsResponse {
            image_number: 0,
            animation_request: None,
            graphic_data: None,
            placement_request: None,
            delete_request: None,
            response: encode_response_quiet(
                cmd.image_id,
                cmd.image_number,
                cmd.placement_id,
                "EINVAL: image ID and number are mutually exclusive",
                cmd.quiet,
                true,
            ),
            error_response: None,
            incomplete: false,
        });
    }

    // A query requires an image id per kitty spec. Without one we cannot
    // even build a response addressed to anything, so we surface EINVAL
    // instead of pretending success. Queries attempt a complete decode
    // without touching the screen's pending transmission.
    if cmd.action == Action::Query && cmd.image_id == 0 {
        return Some(KittyGraphicsResponse {
            image_number: 0,
            animation_request: None,
            graphic_data: None,
            placement_request: None,
            delete_request: None,
            response: encode_response_quiet(
                cmd.image_id,
                cmd.image_number,
                cmd.placement_id,
                "EINVAL: image ID required",
                cmd.quiet,
                true,
            ),
            error_response: None,
            incomplete: false,
        });
    }

    if transmission {
        // File, temporary-file and shared-memory commands begin a new load.
        // Only direct data continues the screen's current upload.
        if cmd.medium != TransmissionMedium::Direct {
            state.loading = None;
        }
        if let Some(loading) = state.loading.as_mut() {
            if cmd.quiet != 0 {
                loading.quiet = cmd.quiet;
            }
            if loading.payload.len().saturating_add(cmd.payload.len())
                > MAX_INCOMPLETE_BYTES
            {
                return Some(transmission_failure(loading, "EINVAL: invalid data", None));
            }
            let more = cmd.more;
            let mut loading = state.loading.take().unwrap();
            loading.payload.reserve_exact(cmd.payload.len());
            loading.payload.extend_from_slice(&cmd.payload);
            if more {
                state.loading = Some(loading);
                return Some(KittyGraphicsResponse::pending_chunk());
            }
            cmd = loading;
            cmd.more = false;
        } else {
            if cmd.action == Action::Frame {
                if cmd.image_id == 0 && cmd.image_number == 0 {
                    return Some(transmission_error(
                        &cmd,
                        "EINVAL: image ID or number required",
                    ));
                }
                match image_identity(cmd.image_id, cmd.image_number) {
                    Ok(target) => cmd.animation_target = target,
                    Err(message) => return Some(transmission_error(&cmd, message)),
                }
            }
            if cmd.image_id != 0
                && matches!(cmd.action, Action::Transmit | Action::TransmitAndDisplay)
            {
                state.started_transmission = Some(cmd.image_id);
            }
            if cmd.format_unknown {
                return Some(transmission_error(&cmd, "EINVAL: unsupported format"));
            }
            // Resolve non-direct payloads now. Their path or object name must
            // never be concatenated with the bytes of a later direct chunk.
            if cmd.medium != TransmissionMedium::Direct {
                match read_payload(&cmd) {
                    Ok(payload) => {
                        cmd.payload = SmallVec::from_vec(payload);
                        cmd.medium = TransmissionMedium::Direct;
                    }
                    Err(error) => return Some(transmission_error(&cmd, error.message())),
                }
            }
            if cmd.payload.len() > MAX_INCOMPLETE_BYTES {
                return Some(upload_limit_response(&cmd));
            }
            if cmd.image_id == 0
                && matches!(cmd.action, Action::Transmit | Action::TransmitAndDisplay)
            {
                cmd.implicit_id = cmd.image_number == 0;
                cmd.image_id =
                    state.allocate_image_id(cmd.implicit_id, &mut available_id);
            }
            if cmd.more {
                state.loading = Some(cmd);
                return Some(KittyGraphicsResponse::pending_chunk());
            }
        }
    }

    // Convert to GraphicData based on action
    debug!("Kitty graphics action: {:?}, format={:?}, width={}, height={}, image_id={}, payload_len={}",
        cmd.action, cmd.format, cmd.width, cmd.height, cmd.image_id, cmd.payload.len());
    if matches!(cmd.action, Action::Transmit | Action::TransmitAndDisplay)
        && cmd.image_id == 0
    {
        // I identifies a numbered transmission, not its storage key. Allocate
        // an id and retain the number so the terminal can bind it on admission.
        cmd.image_id = state.allocate_image_id(false, &mut available_id);
    }
    match cmd.action {
        Action::Transmit | Action::TransmitAndDisplay => {
            debug!("Creating graphic data: format={:?}, medium={:?}, compression={:?}, width={}, height={}, payload_len={}",
                cmd.format, cmd.medium, cmd.compression, cmd.width, cmd.height, cmd.payload.len());
            let graphic_data = match create_graphic_data(&mut cmd) {
                Ok(g) => g,
                Err(err) => {
                    return Some(KittyGraphicsResponse {
                        image_number: 0,
                        animation_request: None,
                        graphic_data: None,
                        placement_request: None,
                        delete_request: None,
                        response: if cmd.implicit_id {
                            None
                        } else {
                            encode_response_quiet(
                                cmd.image_id,
                                cmd.image_number,
                                cmd.placement_id,
                                err.message(),
                                cmd.quiet,
                                true,
                            )
                        },
                        error_response: None,
                        incomplete: false,
                    });
                }
            };
            debug!(
                "Graphic data created successfully: {}x{}",
                graphic_data.width, graphic_data.height
            );
            let response = if cmd.implicit_id {
                None
            } else {
                encode_response_quiet(
                    graphic_data.id.get() as u32,
                    cmd.image_number,
                    cmd.placement_id,
                    "OK",
                    cmd.quiet,
                    false,
                )
            };

            let placement_request = if cmd.action == Action::TransmitAndDisplay {
                Some(PlacementRequest {
                    relative: (cmd.parent_id != 0).then_some(
                        super::graphics::RelativePlacement {
                            image_id: cmd.parent_id,
                            placement_id: u64::from(cmd.parent_placement_id),
                            horizontal: cmd.relative_x,
                            vertical: cmd.relative_y,
                        },
                    ),
                    image_id: cmd.image_id,
                    placement_id: cmd.placement_id,
                    x: cmd.source_x,
                    y: cmd.source_y,
                    width: cmd.source_width,
                    height: cmd.source_height,
                    columns: cmd.columns,
                    rows: cmd.rows,
                    z_index: cmd.z_index,
                    virtual_placement: cmd.virtual_placement,
                    unicode_placeholder: cmd.unicode_placeholder,
                    cursor_movement: cmd.cursor_movement,
                    cell_x_offset: cmd.cell_x_offset,
                    cell_y_offset: cmd.cell_y_offset,
                })
            } else {
                None
            };

            Some(KittyGraphicsResponse {
                image_number: cmd.image_number,
                animation_request: None,
                graphic_data: Some(graphic_data),
                placement_request,
                delete_request: None,
                response,
                error_response: if cmd.implicit_id {
                    None
                } else {
                    encode_response_quiet(
                        cmd.image_id,
                        cmd.image_number,
                        cmd.placement_id,
                        "ENOSPC:graphics resource limit",
                        cmd.quiet,
                        true,
                    )
                },
                incomplete: false,
            })
        }
        Action::Put => {
            // Handle placement request
            let placement = PlacementRequest {
                relative: (cmd.parent_id != 0).then_some(
                    super::graphics::RelativePlacement {
                        image_id: cmd.parent_id,
                        placement_id: u64::from(cmd.parent_placement_id),
                        horizontal: cmd.relative_x,
                        vertical: cmd.relative_y,
                    },
                ),
                image_id: cmd.image_id,
                placement_id: cmd.placement_id,
                x: cmd.source_x,
                y: cmd.source_y,
                width: cmd.source_width,
                height: cmd.source_height,
                columns: cmd.columns,
                rows: cmd.rows,
                z_index: cmd.z_index,
                virtual_placement: cmd.virtual_placement,
                unicode_placeholder: cmd.unicode_placeholder,
                cursor_movement: cmd.cursor_movement,
                cell_x_offset: cmd.cell_x_offset,
                cell_y_offset: cmd.cell_y_offset,
            };
            let response = if cmd.implicit_id {
                None
            } else {
                encode_response_quiet(
                    cmd.image_id,
                    cmd.image_number,
                    cmd.placement_id,
                    "OK",
                    cmd.quiet,
                    false,
                )
            };
            // Whether the referenced image exists only the terminal
            // state layer knows; it picks this reply over `response`
            // when the placement fails. Kitty answers ENOENT there,
            // and clients rely on it to retransmit evicted images.
            let error_response = encode_response_quiet(
                cmd.image_id,
                cmd.image_number,
                cmd.placement_id,
                "ENOENT: image not found",
                cmd.quiet,
                true,
            );
            Some(KittyGraphicsResponse {
                image_number: cmd.image_number,
                animation_request: None,
                graphic_data: None,
                placement_request: Some(placement),
                delete_request: None,
                response,
                error_response,
                incomplete: false,
            })
        }
        Action::Delete => {
            state.loading = None;
            // Handle delete request
            let delete_data = cmd.delete_action.is_ascii_uppercase();
            let delete = DeleteRequest {
                frame_number: cmd.rows,
                action: cmd.delete_action.to_ascii_lowercase(),
                image_id: cmd.image_id,
                image_number: cmd.image_number,
                placement_id: cmd.placement_id,
                x: cmd.source_x,
                y: cmd.source_y,
                z_index: cmd.z_index,
                delete_data,
            };
            Some(KittyGraphicsResponse {
                image_number: 0,
                animation_request: None,
                graphic_data: None,
                placement_request: None,
                delete_request: Some(delete),
                response: None,
                error_response: None,
                incomplete: false,
            })
        }
        Action::Query => {
            // A query must behave exactly like a transmission — decode
            // the payload, read the file, or map and unlink the shared
            // memory — and report whether that worked, without storing
            // anything. The kitty spec has clients probe support this
            // way; an unconditional OK would disable their fallbacks.
            let response = match create_graphic_data(&mut cmd) {
                Ok(_) => encode_response_quiet(
                    cmd.image_id,
                    cmd.image_number,
                    cmd.placement_id,
                    "OK",
                    cmd.quiet,
                    false,
                ),
                Err(err) => encode_response_quiet(
                    cmd.image_id,
                    cmd.image_number,
                    cmd.placement_id,
                    err.message(),
                    cmd.quiet,
                    true,
                ),
            };
            Some(KittyGraphicsResponse {
                image_number: 0,
                animation_request: None,
                graphic_data: None,
                placement_request: None,
                delete_request: None,
                response,
                error_response: None,
                incomplete: false,
            })
        }
        Action::Frame | Action::Animate | Action::Compose => {
            use super::kitty_animation::{
                Command, Composition, Control, FrameUpload, Reply, Request,
            };
            let identity = Reply {
                image_id: cmd
                    .animation_target
                    .map_or(cmd.image_id, |target| target.image_id),
                image_number: cmd.image_number,
                placement_id: cmd.placement_id,
                quiet: cmd.quiet,
                frame: None,
                silent_success: cmd.action == Action::Animate,
            };
            let mut response = KittyGraphicsResponse::pending_chunk();
            response.incomplete = false;
            if identity.image_id == 0 && identity.image_number == 0 {
                response.response = animation_response(
                    identity,
                    Err("EINVAL: image ID or number required".into()),
                );
                return Some(response);
            }
            let command = match cmd.action {
                Action::Frame => {
                    if let Some(target) = cmd.animation_target {
                        let result = image_identity(cmd.image_id, cmd.image_number);
                        let error = match result {
                            Ok(Some(current)) if current != target => {
                                Some("ENOENT: image not found")
                            }
                            Err(message) => Some(message),
                            _ => None,
                        };
                        if let Some(message) = error {
                            response.response =
                                animation_response(identity, Err(message.into()));
                            return Some(response);
                        }
                    }
                    let data = match create_graphic_data(&mut cmd) {
                        Ok(data) => data,
                        Err(error) => {
                            response.response =
                                animation_response(identity, Err(error.message().into()));
                            return Some(response);
                        }
                    };
                    Command::Frame(FrameUpload {
                        data,
                        number: cmd.frame_number,
                        base: cmd.base_frame,
                        x: cmd.source_x,
                        y: cmd.source_y,
                        gap: cmd.frame_gap,
                        overwrite: cmd.composition_mode == 1,
                        background: cmd.background_color,
                    })
                }
                Action::Animate => Command::Control(Control {
                    frame: cmd.frame_number,
                    gap: cmd.frame_gap,
                    current: cmd.current_frame,
                    state: cmd.animation_state,
                    loops: cmd.loop_count,
                }),
                Action::Compose => Command::Compose(Composition {
                    source: cmd.frame_number,
                    destination: cmd.base_frame,
                    source_x: cmd.cell_x_offset,
                    source_y: cmd.cell_y_offset,
                    x: cmd.source_x,
                    y: cmd.source_y,
                    width: cmd.source_width,
                    height: cmd.source_height,
                    overwrite: cmd.composition_mode != 0,
                }),
                _ => unreachable!(),
            };
            response.animation_request = Some(Request {
                target: cmd.animation_target,
                image_id: cmd.image_id,
                image_number: identity.image_number,
                placement_id: identity.placement_id,
                quiet: identity.quiet,
                command,
            });
            Some(response)
        }
    }
}

fn upload_limit_response(cmd: &KittyGraphicsCommand) -> KittyGraphicsResponse {
    transmission_error(cmd, "EINVAL: invalid data")
}

#[cfg(test)]
mod resource_tests {
    use super::*;

    #[test]
    fn chunk_ids_do_not_allocate_concurrent_uploads() {
        let mut state = KittyGraphicsState::default();
        for id in 1..=64 {
            let control = format!("i={id},m=1,S=4294967295");
            assert!(
                parse(&[b"G", control.as_bytes(), b"AAAA"], &mut state)
                    .unwrap()
                    .incomplete
            );
        }
        let loading = state.loading.as_ref().unwrap();
        assert_eq!(loading.image_id, 1);
        assert_eq!(loading.payload.len(), 64 * 3);
        assert!(loading.payload.capacity() < 1024);
    }

    #[test]
    fn oversized_continuation_preserves_the_bounded_upload() {
        let mut state = KittyGraphicsState {
            loading: Some(KittyGraphicsCommand {
                image_id: 1,
                payload: SmallVec::from_vec(vec![0; MAX_INCOMPLETE_BYTES]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let rejected = parse(&[b"G", b"i=2,m=1,q=1", b"AAAA"], &mut state).unwrap();
        assert!(rejected.response.unwrap().contains("EINVAL: invalid data"));
        let loading = state.loading.as_ref().unwrap();
        assert_eq!(loading.image_id, 1);
        assert_eq!(loading.payload.len(), MAX_INCOMPLETE_BYTES);
        assert_eq!(loading.quiet, 1);
        let loading = state.loading.as_mut().unwrap();
        loading.action = Action::Frame;
        loading.frame_number = 9;
        let rejected = parse(&[b"G", b"i=2,m=1,q=1", b"AAAA"], &mut state).unwrap();
        assert_eq!(
            rejected.response.as_deref(),
            Some("\x1b_Gi=1;EINVAL: invalid data\x1b\\")
        );
        state.loading = None;
        let accepted = parse(&[b"G", b"i=3,m=1", b"AAAA"], &mut state).unwrap();
        assert!(accepted.incomplete);
    }
}

fn transmission_error(
    cmd: &KittyGraphicsCommand,
    message: &'static str,
) -> KittyGraphicsResponse {
    transmission_failure(
        cmd,
        message,
        (cmd.action == Action::Frame).then_some(cmd.frame_number),
    )
}

fn transmission_failure(
    cmd: &KittyGraphicsCommand,
    message: &'static str,
    frame: Option<u32>,
) -> KittyGraphicsResponse {
    KittyGraphicsResponse {
        image_number: 0,
        animation_request: None,
        graphic_data: None,
        placement_request: None,
        delete_request: None,
        response: animation_response(
            super::kitty_animation::Reply {
                image_id: cmd
                    .animation_target
                    .map_or(cmd.image_id, |target| target.image_id),
                image_number: cmd.image_number,
                placement_id: cmd.placement_id,
                quiet: cmd.quiet,
                frame,
                silent_success: false,
            },
            Err(message.into()),
        ),
        error_response: None,
        incomplete: false,
    }
}

fn parse_control_data(cmd: &mut KittyGraphicsCommand, control_data: &str) -> bool {
    let mut valid = true;
    // First pass: parse action to determine context
    for pair in control_data.split(',') {
        if let Some((key, value)) = pair.split_once('=') {
            if key == "a" {
                cmd.action = parse_action(value);
                break;
            }
        }
    }

    // Second pass: parse remaining keys based on action context
    for pair in control_data.split(',') {
        if let Some((key, value)) = pair.split_once('=') {
            // Defaults apply to omitted keys. Explicit invalid values must not
            // silently become a different operation or a zero-valued field.
            valid &= match key {
                "a" => matches!(value, "t" | "T" | "q" | "p" | "d" | "f" | "a" | "c"),
                "f" => value.parse::<u32>().is_ok(),
                "t" => matches!(value, "d" | "f" | "t" | "s"),
                "o" => value == "z",
                "q" | "m" | "U" => value.parse::<u32>().is_ok(),
                "H" | "V" | "z" => value.parse::<i32>().is_ok(),
                "C" if cmd.action == Action::Compose => value.parse::<u32>().is_ok(),
                "C" => value.parse::<u32>().is_ok(),
                "s" if cmd.action == Action::Animate => value.parse::<u8>().is_ok(),
                "X" if cmd.action == Action::Frame => value.parse::<u32>().is_ok(),
                "s" | "v" | "S" | "O" | "i" | "I" | "p" | "x" | "y" | "w" | "h" | "X"
                | "Y" | "c" | "r" | "P" | "Q" | "u" => value.parse::<u32>().is_ok(),
                _ => true,
            };
            match key {
                // Action (already parsed)
                "a" => {}
                "q" => cmd.quiet = value.parse::<u32>().unwrap_or(0).min(2) as u8,

                // Image transmission
                "f" => {
                    cmd.format = parse_format(value);
                    cmd.format_unknown =
                        !matches!(value.parse::<u32>(), Ok(0 | 24 | 32 | 100));
                }
                "t" => cmd.medium = parse_transmission_medium(value),
                "s" => match cmd.action {
                    Action::Animate => cmd.animation_state = value.parse().unwrap_or(0),
                    _ => cmd.width = value.parse().unwrap_or(0),
                },
                "v" => match cmd.action {
                    Action::Animate => cmd.loop_count = value.parse().unwrap_or(0),
                    _ => cmd.height = value.parse().unwrap_or(0),
                },
                "S" => cmd.size = value.parse().unwrap_or(0),
                "O" => cmd.offset = value.parse().unwrap_or(0),
                "i" => cmd.image_id = value.parse().unwrap_or(0),
                "I" => cmd.image_number = value.parse().unwrap_or(0),
                "p" => cmd.placement_id = value.parse().unwrap_or(0),
                "o" => cmd.compression = parse_compression(value),
                "m" => cmd.more = value.parse::<u32>().unwrap_or(0) != 0,

                // Context-dependent keys
                "x" => match cmd.action {
                    Action::Delete => cmd.source_x = value.parse().unwrap_or(0),
                    _ => cmd.source_x = value.parse().unwrap_or(0),
                },
                "y" => match cmd.action {
                    Action::Delete => cmd.source_y = value.parse().unwrap_or(0),
                    _ => cmd.source_y = value.parse().unwrap_or(0),
                },
                "w" => cmd.source_width = value.parse().unwrap_or(0),
                "h" => cmd.source_height = value.parse().unwrap_or(0),
                "X" => match cmd.action {
                    Action::Frame => {
                        cmd.composition_mode =
                            (value.parse::<u32>().unwrap_or(0) == 1) as u8
                    }
                    _ => cmd.cell_x_offset = value.parse().unwrap_or(0),
                },
                "Y" => match cmd.action {
                    Action::Frame => cmd.background_color = value.parse().unwrap_or(0),
                    _ => cmd.cell_y_offset = value.parse().unwrap_or(0),
                },
                "c" => match cmd.action {
                    Action::Frame | Action::Compose => {
                        cmd.base_frame = value.parse().unwrap_or(0)
                    }
                    Action::Animate => cmd.current_frame = value.parse().unwrap_or(0),
                    _ => cmd.columns = value.parse().unwrap_or(0),
                },
                "r" => match cmd.action {
                    Action::Frame | Action::Compose | Action::Animate => {
                        cmd.frame_number = value.parse().unwrap_or(0)
                    }
                    _ => cmd.rows = value.parse().unwrap_or(0),
                },
                "z" => match cmd.action {
                    Action::Frame | Action::Animate => {
                        cmd.frame_gap = value.parse().unwrap_or(0)
                    }
                    _ => cmd.z_index = value.parse().unwrap_or(0),
                },

                // Other display keys
                "C" if cmd.action == Action::Compose => {
                    cmd.composition_mode = (value.parse::<u32>().unwrap_or(0) != 0) as u8
                }
                "C" => {
                    cmd.cursor_movement = (value.parse::<u32>().unwrap_or(0) == 1) as u8
                }
                "U" => cmd.virtual_placement = value.parse::<u32>().unwrap_or(0) != 0,
                "P" => cmd.parent_id = value.parse().unwrap_or(0),
                "Q" => cmd.parent_placement_id = value.parse().unwrap_or(0),
                "H" => cmd.relative_x = value.parse().unwrap_or(0),
                "V" => cmd.relative_y = value.parse().unwrap_or(0),

                // Delete
                "d" => {
                    cmd.delete_action = value.as_bytes().first().copied().unwrap_or(b'a')
                }

                // Placeholder
                "u" => cmd.unicode_placeholder = value.parse().unwrap_or(0),

                _ => {} // Ignore unknown keys
            }
        }
    }
    valid
}

fn parse_action(value: &str) -> Action {
    match value {
        "t" => Action::Transmit,
        "T" => Action::TransmitAndDisplay,
        "q" => Action::Query,
        "p" => Action::Put,
        "d" => Action::Delete,
        "f" => Action::Frame,
        "a" => Action::Animate,
        "c" => Action::Compose,
        _ => Action::Transmit,
    }
}

fn parse_format(value: &str) -> Format {
    match value.parse::<u32>() {
        Ok(8) => Format::Gray,
        Ok(16) => Format::GrayAlpha,
        Ok(24) => Format::Rgb24,
        Ok(0 | 32) => Format::Rgba32,
        Ok(100) => Format::Png,
        _ => Format::Rgba32,
    }
}

fn parse_transmission_medium(value: &str) -> TransmissionMedium {
    match value {
        "d" => TransmissionMedium::Direct,
        "f" => TransmissionMedium::File,
        "t" => TransmissionMedium::TempFile,
        "s" => TransmissionMedium::SharedMemory,
        _ => TransmissionMedium::Direct,
    }
}

fn parse_compression(value: &str) -> Compression {
    match value {
        "z" => Compression::Zlib,
        _ => Compression::None,
    }
}

/// Decode a single APC command's base64 payload.
///
/// Tries the standard (padded) decoder first, falling back to the
/// no-padding variant so that chunks from spec-compliant clients
/// (which don't pad intermediate chunks) also decode cleanly.
///
/// Callers decode each APC command's payload independently, so that
/// per-chunk padding from clients like
/// chafa is contained within its own chunk instead of contaminating the
/// merged byte stream.
fn decode_payload_base64(payload: &[u8]) -> Option<Vec<u8>> {
    if payload.is_empty() {
        return Some(Vec::new());
    }
    if let Some(data) = simd_base64::decode(payload) {
        return Some(data);
    }
    if let Some(data) = simd_base64::decode_no_pad(payload) {
        return Some(data);
    }
    debug!("Base64 payload decode failed");
    None
}

/// Error emitted from `create_graphic_data`. Maps directly to kitty
/// protocol EINVAL/ENOENT/E2BIG message strings so the caller can
/// surface them in a response.
#[derive(Debug)]
#[allow(dead_code)] // UnsupportedFormat/Medium are platform-gated
enum GraphicError {
    DimensionsTooLarge,
    DimensionsRequired,
    TooLarge,
    UnsupportedFormat,
    UnsupportedMedium,
    InvalidData,
    InsufficientData,
    DecompressionFailed,
    FileNotFound,
}

impl GraphicError {
    fn message(&self) -> &'static str {
        match self {
            GraphicError::DimensionsTooLarge => "EINVAL: dimensions too large",
            GraphicError::DimensionsRequired => "EINVAL: dimensions required",
            GraphicError::TooLarge => "E2BIG: image too large",
            GraphicError::UnsupportedFormat => "EINVAL: unsupported format",
            GraphicError::UnsupportedMedium => "EINVAL: unsupported medium",
            GraphicError::InvalidData => "EINVAL: invalid data",
            GraphicError::InsufficientData => "ENODATA: insufficient data",
            GraphicError::DecompressionFailed => "EINVAL: decompression failed",
            GraphicError::FileNotFound => "ENOENT: file not found",
        }
    }
}

fn read_payload(cmd: &KittyGraphicsCommand) -> Result<Vec<u8>, GraphicError> {
    Ok(match cmd.medium {
        TransmissionMedium::Direct => {
            if cmd.payload.len() > MAX_SIZE {
                return Err(GraphicError::TooLarge);
            }
            debug!("Using decoded Direct payload: {} bytes", cmd.payload.len());
            cmd.payload.to_vec()
        }
        TransmissionMedium::File | TransmissionMedium::TempFile => kitty_file::read(
            &cmd.payload,
            cmd.offset,
            cmd.size,
            cmd.medium == TransmissionMedium::TempFile,
        )?,
        TransmissionMedium::SharedMemory => {
            #[cfg(unix)]
            {
                use std::ffi::CString;
                use std::os::unix::io::RawFd;

                // Payload is already base64-decoded by parse(); the bytes
                // directly represent the shared memory name.
                debug!("Shared memory name payload: {} bytes", cmd.payload.len());
                let shm_name_str = std::str::from_utf8(&cmd.payload)
                    .map_err(|_| GraphicError::InvalidData)?;
                let shm_name =
                    CString::new(shm_name_str).map_err(|_| GraphicError::InvalidData)?;

                debug!("Opening shared memory: {}", shm_name_str);

                unsafe {
                    // Open shared memory
                    let fd: RawFd = libc::shm_open(shm_name.as_ptr(), libc::O_RDONLY, 0);

                    if fd < 0 {
                        let err = std::io::Error::last_os_error();
                        let errno = err.raw_os_error().unwrap_or(-1);
                        debug!(
                            "Failed to open shared memory '{}': {} (errno: {})",
                            shm_name_str, err, errno
                        );
                        return Err(GraphicError::InvalidData);
                    }

                    // Get size of shared memory
                    let mut stat: libc::stat = std::mem::zeroed();
                    if libc::fstat(fd, &mut stat) < 0 {
                        libc::close(fd);
                        libc::shm_unlink(shm_name.as_ptr());
                        debug!("Failed to fstat shared memory");
                        return Err(GraphicError::InvalidData);
                    }

                    let shm_size = match usize::try_from(stat.st_size) {
                        Ok(size) if size > 0 => size,
                        _ => {
                            libc::close(fd);
                            libc::shm_unlink(shm_name.as_ptr());
                            return Err(GraphicError::InvalidData);
                        }
                    };
                    debug!("Shared memory size: {} bytes", shm_size);

                    if cmd.format != Format::Png
                        && (cmd.width > MAX_DIMENSION || cmd.height > MAX_DIMENSION)
                    {
                        libc::close(fd);
                        libc::shm_unlink(shm_name.as_ptr());
                        return Err(GraphicError::DimensionsTooLarge);
                    }
                    let offset = cmd.offset as usize;
                    let Some(available) = shm_size.checked_sub(offset) else {
                        libc::close(fd);
                        libc::shm_unlink(shm_name.as_ptr());
                        return Err(GraphicError::InvalidData);
                    };
                    // Shared objects may be page padded. Raw uncompressed images
                    // need only their declared pixels; compressed/PNG data uses
                    // the remaining object unless an explicit size is supplied.
                    let data_size = if cmd.size > 0 {
                        cmd.size as usize
                    } else if cmd.compression == Compression::None
                        && cmd.format != Format::Png
                    {
                        let bpp = match cmd.format {
                            Format::Gray => 1,
                            Format::GrayAlpha => 2,
                            Format::Rgb24 => 3,
                            Format::Rgba32 => 4,
                            Format::Png => unreachable!(),
                        };
                        (cmd.width as usize) * (cmd.height as usize) * bpp
                    } else {
                        available
                    };

                    if data_size > available {
                        libc::close(fd);
                        libc::shm_unlink(shm_name.as_ptr());
                        debug!(
                            "Requested size {} exceeds shared memory size {}",
                            data_size, shm_size
                        );
                        return Err(GraphicError::InvalidData);
                    }

                    if data_size > MAX_SIZE {
                        libc::close(fd);
                        libc::shm_unlink(shm_name.as_ptr());
                        return Err(GraphicError::InvalidData);
                    }

                    #[cfg(target_os = "linux")]
                    let data = {
                        use std::os::fd::FromRawFd;
                        use std::os::unix::fs::FileExt;
                        // A mapped copy can SIGBUS if the client truncates the
                        // object after fstat. Linux supports bounded file reads,
                        // which turn that race into a normal short-read error.
                        // The surrounding cleanup retains ownership of fd.
                        let file =
                            std::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(fd));
                        let mut data = vec![0; data_size];
                        if file.read_exact_at(&mut data, offset as u64).is_err() {
                            libc::close(fd);
                            libc::shm_unlink(shm_name.as_ptr());
                            return Err(GraphicError::InvalidData);
                        }
                        data
                    };
                    #[cfg(not(target_os = "linux"))]
                    let data = {
                        // Some Unix platforms require mmap for POSIX shm.
                        let ptr = libc::mmap(
                            std::ptr::null_mut(),
                            shm_size,
                            libc::PROT_READ,
                            libc::MAP_SHARED,
                            fd,
                            0,
                        );

                        if ptr == libc::MAP_FAILED {
                            libc::close(fd);
                            libc::shm_unlink(shm_name.as_ptr());
                            debug!("Failed to mmap shared memory");
                            return Err(GraphicError::InvalidData);
                        }

                        // Copy data from shared memory
                        let data = std::slice::from_raw_parts(
                            (ptr as *const u8).add(offset),
                            data_size,
                        )
                        .to_vec();

                        libc::munmap(ptr, shm_size);
                        data
                    };
                    libc::close(fd);
                    libc::shm_unlink(shm_name.as_ptr());

                    debug!("Successfully read {} bytes from shared memory", data.len());
                    data
                }
            }
            #[cfg(windows)]
            {
                use std::ffi::OsStr;
                use std::os::windows::ffi::OsStrExt;
                use windows_sys::Win32::Foundation::CloseHandle;
                use windows_sys::Win32::System::Memory::OpenFileMappingW;
                use windows_sys::Win32::System::Memory::{
                    MapViewOfFile, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ,
                    MEMORY_BASIC_INFORMATION,
                };

                // Payload is already base64-decoded by parse(); the bytes
                // directly represent the shared memory name.
                debug!("Shared memory name payload: {} bytes", cmd.payload.len());
                let shm_name_str = std::str::from_utf8(&cmd.payload)
                    .map_err(|_| GraphicError::InvalidData)?;

                debug!("Opening shared memory: {}", shm_name_str);

                unsafe {
                    // Convert to wide string for Windows API
                    let wide_name: Vec<u16> = OsStr::new(shm_name_str)
                        .encode_wide()
                        .chain(std::iter::once(0))
                        .collect();

                    // Open the file mapping
                    let handle = OpenFileMappingW(FILE_MAP_READ, 0, wide_name.as_ptr());

                    if handle.is_null() {
                        let err = std::io::Error::last_os_error();
                        debug!(
                            "Failed to open shared memory '{}': {}",
                            shm_name_str, err
                        );
                        return Err(GraphicError::FileNotFound);
                    }

                    // Map view of file
                    let base_ptr = MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 0);

                    if base_ptr.Value.is_null() {
                        let err = std::io::Error::last_os_error();
                        debug!("Failed to map view of file: {}", err);
                        CloseHandle(handle);
                        return Err(GraphicError::InvalidData);
                    }

                    // Query memory to get size
                    let mut mem_info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
                    if VirtualQuery(
                        base_ptr.Value,
                        &mut mem_info,
                        std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                    ) == 0
                    {
                        debug!("Failed to query memory information");
                        UnmapViewOfFile(base_ptr);
                        CloseHandle(handle);
                        return Err(GraphicError::InvalidData);
                    }

                    let shm_size = mem_info.RegionSize;
                    debug!("Shared memory size: {} bytes", shm_size);

                    // Use cmd.size if specified, otherwise use the full shm size
                    let data_size = if cmd.size > 0 {
                        cmd.size as usize
                    } else {
                        shm_size
                    };

                    // Validate offset and size
                    if cmd.offset as usize + data_size > shm_size {
                        debug!(
                            "Requested offset {} + size {} exceeds shared memory size {}",
                            cmd.offset, data_size, shm_size
                        );
                        UnmapViewOfFile(base_ptr);
                        CloseHandle(handle);
                        return Err(GraphicError::InvalidData);
                    }

                    if data_size > MAX_SIZE {
                        UnmapViewOfFile(base_ptr);
                        CloseHandle(handle);
                        return Err(GraphicError::TooLarge);
                    }

                    // Copy data from shared memory
                    let data_ptr = (base_ptr.Value as *const u8).add(cmd.offset as usize);
                    let data = std::slice::from_raw_parts(data_ptr, data_size).to_vec();

                    // Cleanup
                    UnmapViewOfFile(base_ptr);
                    CloseHandle(handle);

                    debug!("Successfully read {} bytes from shared memory", data.len());
                    data
                }
            }
            #[cfg(not(any(unix, windows)))]
            {
                debug!("SharedMemory transmission not supported on this platform");
                return Err(GraphicError::UnsupportedMedium);
            }
        }
    })
}

fn decompress_zlib(data: &[u8], limit: usize) -> Result<Vec<u8>, GraphicError> {
    use flate2::{Decompress, FlushDecompress, Status};

    let mut decoder = Decompress::new(true);
    let mut output = Vec::with_capacity(data.len().saturating_mul(2).clamp(32, 65536));
    loop {
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        let start = output.len();
        let end = start
            .saturating_add(output.capacity().clamp(32, 65536))
            .min(limit.saturating_add(1));
        output.resize(end, 0);
        let status = decoder
            .decompress(
                &data[before_in as usize..],
                &mut output[start..],
                FlushDecompress::None,
            )
            .map_err(|_| GraphicError::DecompressionFailed)?;
        output.truncate(start + (decoder.total_out() - before_out) as usize);
        if output.len() > limit {
            return Err(GraphicError::DecompressionFailed);
        }
        if status == Status::StreamEnd {
            return if decoder.total_in() as usize == data.len() {
                Ok(output)
            } else {
                Err(GraphicError::DecompressionFailed)
            };
        }
        // Read-based decoders may accept EOF before the checksum or end marker.
        // A completed stream is required, even when all pixels were produced.
        if decoder.total_in() == before_in && decoder.total_out() == before_out {
            return Err(GraphicError::DecompressionFailed);
        }
    }
}

fn create_graphic_data(
    cmd: &mut KittyGraphicsCommand,
) -> Result<GraphicData, GraphicError> {
    if cmd.format_unknown {
        return Err(GraphicError::UnsupportedFormat);
    }
    // Payload is already base64-decoded by parse(). Pick up the bytes
    // based on the transmission medium — direct means they're the image
    // bytes, file/shm means they're a path/name.
    let raw_data = if cmd.medium == TransmissionMedium::Direct {
        if cmd.payload.len() > MAX_SIZE {
            return Err(GraphicError::TooLarge);
        }
        std::mem::take(&mut cmd.payload).into_vec()
    } else {
        read_payload(cmd)?
    };

    // Decompress if needed
    let pixel_data = match cmd.compression {
        Compression::None => raw_data,
        Compression::Zlib => decompress_zlib(&raw_data, MAX_SIZE)?,
    };

    // Parse based on format
    match cmd.format {
        Format::Png => {
            #[cfg(not(feature = "graphics"))]
            let result = {
                // Image decoding needs the `graphics` feature.
                let _ = &pixel_data;
                Err(GraphicError::InvalidData)
            };
            #[cfg(feature = "graphics")]
            let result = {
                let (width, height, mut pixels) = kitty_png::decode(&pixel_data)?;
                // Wuffs clears samples matching a grayscale/RGB tRNS key.
                // Explicit alpha and palette transparency retain their RGB.
                if matches!(pixel_data.get(25), Some(0 | 2)) {
                    for pixel in pixels.chunks_exact_mut(4) {
                        if pixel[3] == 0 {
                            pixel[..3].fill(0);
                        }
                    }
                }

                // Check if image is opaque
                let is_opaque = pixels.chunks(4).all(|chunk| chunk[3] == 255);

                // Create resize command if columns/rows specified
                // When both c= and r= are given, stretch to fill (no aspect ratio).
                // When only one is given, compute the other preserving aspect ratio.
                let resize = if cmd.columns > 0 || cmd.rows > 0 {
                    let both_specified = cmd.columns > 0 && cmd.rows > 0;
                    Some(ResizeCommand {
                        width: if cmd.columns > 0 {
                            ResizeParameter::Cells(cmd.columns)
                        } else {
                            ResizeParameter::Auto
                        },
                        height: if cmd.rows > 0 {
                            ResizeParameter::Cells(cmd.rows)
                        } else {
                            ResizeParameter::Auto
                        },
                        preserve_aspect_ratio: !both_specified,
                    })
                } else {
                    None
                };

                Ok(GraphicData {
                    id: GraphicId::new(cmd.image_id as u64),
                    width,
                    height,
                    color_type: ColorType::Rgba,
                    pixels,
                    is_opaque,
                    resize,
                    display_width: None,
                    display_height: None,
                    transmit_time: crate::time::Instant::now(),
                })
            };
            result
        }
        Format::Gray | Format::GrayAlpha | Format::Rgb24 | Format::Rgba32 => {
            let bytes_per_pixel = match cmd.format {
                Format::Gray => 1,
                Format::GrayAlpha => 2,
                Format::Rgb24 => 3,
                Format::Rgba32 => 4,
                _ => unreachable!(),
            };

            if cmd.width == 0 || cmd.height == 0 {
                return Err(GraphicError::DimensionsRequired);
            }
            if cmd.width > MAX_DIMENSION || cmd.height > MAX_DIMENSION {
                return Err(GraphicError::DimensionsTooLarge);
            }

            // Validate data size
            let expected_size =
                cmd.width as usize * cmd.height as usize * bytes_per_pixel;
            if expected_size > MAX_SIZE {
                return Err(GraphicError::TooLarge);
            }
            if cmd.action == Action::Frame && pixel_data.len() < expected_size {
                return Err(GraphicError::InsufficientData);
            }
            if cmd.action != Action::Frame && pixel_data.len() != expected_size {
                return Err(GraphicError::InvalidData);
            }

            // Animation frames may carry padding. Images require an exact length.
            let mut pixel_data = pixel_data;
            pixel_data.truncate(expected_size);

            // Convert all formats to RGBA (GPU only supports RGBA)
            let (pixels, is_opaque) = match cmd.format {
                Format::Gray => {
                    // 1 bpp: R=G=B=gray, A=255
                    let mut rgba =
                        Vec::with_capacity(cmd.width as usize * cmd.height as usize * 4);
                    for &g in &pixel_data {
                        rgba.extend_from_slice(&[g, g, g, 255]);
                    }
                    (rgba, true)
                }
                Format::GrayAlpha => {
                    // 2 bpp: R=G=B=gray, A=alpha
                    let mut rgba =
                        Vec::with_capacity(cmd.width as usize * cmd.height as usize * 4);
                    let mut opaque = true;
                    for chunk in pixel_data.chunks_exact(2) {
                        let g = chunk[0];
                        let a = chunk[1];
                        if a != 255 {
                            opaque = false;
                        }
                        rgba.extend_from_slice(&[g, g, g, a]);
                    }
                    (rgba, opaque)
                }
                Format::Rgb24 => {
                    // 3 bpp: add A=255
                    let mut rgba =
                        Vec::with_capacity(cmd.width as usize * cmd.height as usize * 4);
                    for chunk in pixel_data.chunks_exact(3) {
                        rgba.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
                    }
                    (rgba, true)
                }
                Format::Rgba32 => {
                    // Already RGBA
                    let is_opaque = pixel_data.chunks(4).all(|chunk| chunk[3] == 255);
                    (pixel_data, is_opaque)
                }
                _ => unreachable!(),
            };

            // Create resize command if columns/rows specified
            // When both c= and r= are given, stretch to fill (no aspect ratio).
            // When only one is given, compute the other preserving aspect ratio.
            let resize = if cmd.columns > 0 || cmd.rows > 0 {
                let both_specified = cmd.columns > 0 && cmd.rows > 0;
                Some(ResizeCommand {
                    width: if cmd.columns > 0 {
                        ResizeParameter::Cells(cmd.columns)
                    } else {
                        ResizeParameter::Auto
                    },
                    height: if cmd.rows > 0 {
                        ResizeParameter::Cells(cmd.rows)
                    } else {
                        ResizeParameter::Auto
                    },
                    preserve_aspect_ratio: !both_specified,
                })
            } else {
                None
            };

            Ok(GraphicData {
                id: GraphicId::new(cmd.image_id as u64),
                width: cmd.width as usize,
                height: cmd.height as usize,
                color_type: ColorType::Rgba, // Always RGBA after conversion
                pixels,
                is_opaque,
                resize,
                display_width: None,
                display_height: None,
                transmit_time: crate::time::Instant::now(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine as _;

    #[test]
    fn zlib_requires_one_complete_stream_with_bounded_output() {
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;

        let pixels = vec![73; 200_000];
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&pixels).unwrap();
        let compressed = encoder.finish().unwrap();
        assert_eq!(decompress_zlib(&compressed, pixels.len()).unwrap(), pixels);
        for limit in [0, 1, 31, 65535, pixels.len() - 1] {
            assert!(matches!(
                decompress_zlib(&compressed, limit),
                Err(GraphicError::DecompressionFailed)
            ));
        }
        for end in [0, 1, compressed.len() - 4, compressed.len() - 1] {
            assert!(matches!(
                decompress_zlib(&compressed[..end], pixels.len()),
                Err(GraphicError::DecompressionFailed)
            ));
        }
        let mut trailing = compressed.clone();
        trailing.push(0);
        assert!(decompress_zlib(&trailing, pixels.len()).is_err());
        let mut concatenated = compressed.clone();
        concatenated.extend_from_slice(&compressed);
        assert!(decompress_zlib(&concatenated, pixels.len() * 2).is_err());
    }

    fn parse_kitty_graphics_protocol(
        keys: &str,
        payload: &str,
    ) -> Option<KittyGraphicsResponse> {
        // Convert keys and payload to the format expected by parse()
        let params = if keys.is_empty() && payload.is_empty() {
            vec![b"G".as_ref()]
        } else if payload.is_empty() {
            vec![b"G".as_ref(), keys.as_bytes()]
        } else {
            vec![b"G".as_ref(), keys.as_bytes(), payload.as_bytes()]
        };

        let mut state = KittyGraphicsState::default();
        parse(&params, &mut state)
    }

    #[test]
    fn test_parse_basic_transmit() {
        // 1x1 RGBA pixel (4 bytes) - base64 encoded [255, 0, 0, 255] (red pixel)
        let payload = "/wAA/w==";
        let result = parse_kitty_graphics_protocol("a=t,f=32,s=1,v=1", payload);
        assert!(result.is_some());

        let response = result.unwrap();
        assert!(response.graphic_data.is_some());
        assert!(response.placement_request.is_none());
        assert!(response.delete_request.is_none());
    }

    #[test]
    fn test_parse_transmit_and_display() {
        // 1x1 RGBA pixel - base64 encoded [255, 0, 0, 255] (red pixel)
        let payload = "/wAA/w==";
        let result = parse_kitty_graphics_protocol("a=T,f=32,s=1,v=1,i=1", payload);
        assert!(result.is_some());

        let response = result.unwrap();
        assert!(response.graphic_data.is_some());
        assert!(response.placement_request.is_some());

        let placement = response.placement_request.unwrap();
        assert_eq!(placement.image_id, 1);
    }

    #[test]
    fn test_parse_placement() {
        let result = parse_kitty_graphics_protocol("a=p,i=1,x=10,y=20,c=5,r=3,z=2", "");
        assert!(result.is_some());

        let response = result.unwrap();
        assert!(response.graphic_data.is_none());
        assert!(response.placement_request.is_some());

        let placement = response.placement_request.unwrap();
        assert_eq!(placement.image_id, 1);
        assert_eq!(placement.x, 10);
        assert_eq!(placement.y, 20);
        assert_eq!(placement.columns, 5);
        assert_eq!(placement.rows, 3);
        assert_eq!(placement.z_index, 2);
    }

    #[test]
    fn test_parse_placement_subcell_offset() {
        // `X=`/`Y=` are the sub-cell pixel offset within the placement's
        // first cell. They must flow through to the PlacementRequest so the
        // renderer can position the image at pixel (not just cell)
        // granularity, for smooth sub-cell scrolling.
        let result = parse_kitty_graphics_protocol("a=p,i=1,X=7,Y=9", "");
        let placement = result
            .expect("parse ok")
            .placement_request
            .expect("placement");
        assert_eq!(placement.cell_x_offset, 7);
        assert_eq!(placement.cell_y_offset, 9);
    }

    #[test]
    fn test_parse_placement_subcell_offset_defaults_zero() {
        // Omitting `X=`/`Y=` leaves the offset at the cell boundary.
        let result = parse_kitty_graphics_protocol("a=p,i=1", "");
        let placement = result
            .expect("parse ok")
            .placement_request
            .expect("placement");
        assert_eq!(placement.cell_x_offset, 0);
        assert_eq!(placement.cell_y_offset, 0);
    }

    #[test]
    fn test_parse_delete() {
        let result = parse_kitty_graphics_protocol("a=d,d=i,i=1", "");
        assert!(result.is_some());

        let response = result.unwrap();
        assert!(response.delete_request.is_some());

        let delete = response.delete_request.unwrap();
        assert_eq!(delete.action, b'i');
        assert_eq!(delete.image_id, 1);
        assert!(!delete.delete_data);
    }

    #[test]
    fn test_parse_delete_uppercase() {
        let result = parse_kitty_graphics_protocol("a=d,d=I,i=1", "");
        assert!(result.is_some());

        let response = result.unwrap();
        assert!(response.delete_request.is_some());

        let delete = response.delete_request.unwrap();
        assert_eq!(delete.action, b'i');
        assert_eq!(delete.image_id, 1);
        assert!(delete.delete_data);
    }

    #[test]
    fn test_parse_query() {
        // A valid query runs the real decode path and reports OK.
        let result = parse_kitty_graphics_protocol("a=q,i=1,f=32,s=1,v=1", "AAAAAA==");
        assert!(result.is_some());

        let response = result.unwrap();
        assert!(response.graphic_data.is_none(), "queries must not store");
        let body = response.response.expect("response expected");
        assert!(body.contains(";OK"), "valid query must succeed: {body}");

        // A query the terminal cannot actually load reports the failure
        // instead of a blanket OK, so clients keep their fallbacks.
        let result = parse_kitty_graphics_protocol("a=q,i=1", "");
        let body = result
            .expect("response struct must exist")
            .response
            .expect("error response expected");
        assert!(
            !body.contains(";OK"),
            "undecodable query must not report OK: {body}"
        );
    }

    #[test]
    fn test_parse_with_compression() {
        // zlib compressed single RGBA pixel [255, 0, 0, 255]
        let payload = "eJz7z8DwHwAE/wH/";
        let result = parse_kitty_graphics_protocol("a=t,f=32,s=1,v=1,o=z", payload);
        assert!(result.is_some());

        let response = result.unwrap();
        assert!(response.graphic_data.is_some());
    }

    #[test]
    fn test_parse_with_unicode_placeholder() {
        let result = parse_kitty_graphics_protocol("a=p,i=1,u=128512", ""); // 😀
        assert!(result.is_some());

        let response = result.unwrap();
        assert!(response.placement_request.is_some());

        let placement = response.placement_request.unwrap();
        assert_eq!(placement.unicode_placeholder, 128512);
        // `u=N` is an informational hint, not the virtual-placement
        // trigger — leave that flag clear.
        assert!(!placement.virtual_placement);
    }

    #[test]
    fn test_parse_with_virtual_placement() {
        // What `kitten icat --unicode-placeholder` actually emits:
        // `_Ga=p,U=1,i=N,c=cols,r=rows,q=2\e\` to register the placement.
        // The parser must propagate `U=1` so `place_graphic` routes to
        // the virtual-placement path (metadata only, no cell writes).
        let result = parse_kitty_graphics_protocol("a=p,U=1,i=42,c=10,r=4,q=2", "");
        let response = result.expect("parse ok");
        let placement = response.placement_request.expect("placement");
        assert!(placement.virtual_placement);
        assert_eq!(placement.image_id, 42);
        assert_eq!(placement.columns, 10);
        assert_eq!(placement.rows, 4);
        // `U=1` doesn't set the lowercase `u` field.
        assert_eq!(placement.unicode_placeholder, 0);
    }

    #[cfg(feature = "graphics")]
    #[test]
    fn test_parse_png_format() {
        // Small 1x1 red PNG
        let png_data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==";
        let result = parse_kitty_graphics_protocol("a=t,f=100,i=1", png_data);
        assert!(result.is_some());

        let response = result.unwrap();
        assert!(response.graphic_data.is_some());
    }

    #[cfg(feature = "graphics")]
    #[test]
    fn png_16_bit_samples_keep_the_high_byte() {
        use image_rs::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};

        let values = [
            0u16, 1, 128, 129, 254, 255, 256, 257, 32895, 32896, 65534, 65535,
        ];
        for (color, channels, alpha) in [
            (ExtendedColorType::L16, 1, false),
            (ExtendedColorType::La16, 2, true),
            (ExtendedColorType::Rgb16, 3, false),
            (ExtendedColorType::Rgba16, 4, true),
        ] {
            let samples: Vec<_> = values
                .iter()
                .flat_map(|value| (0..channels).flat_map(move |_| value.to_ne_bytes()))
                .collect();
            let mut png = Vec::new();
            PngEncoder::new(&mut png)
                .write_image(&samples, values.len() as u32, 1, color)
                .unwrap();
            let response =
                parse_kitty_graphics_protocol("a=t,f=100,i=1", &BASE64.encode(png))
                    .unwrap();
            let graphic = response.graphic_data.unwrap();
            let expected: Vec<_> = values
                .iter()
                .flat_map(|value| {
                    let sample = (value >> 8) as u8;
                    [sample, sample, sample, if alpha { sample } else { 255 }]
                })
                .collect();
            assert_eq!(graphic.pixels, expected, "{color:?}");
            assert_eq!(graphic.is_opaque, !alpha);
        }
    }

    #[cfg(feature = "graphics")]
    #[test]
    fn apng_uses_first_animation_frame_instead_of_poster() {
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAACGFjVEwAAAABAAAAALQt6aAAAAANSURBVHicY/goqPgfAAU9AiNv1J+pAAAAGmZjVEwAAAAAAAAAAQAAAAEAAAAAAAAAAAABAAoAAFp/MNAAAAARZmRBVAAAAAF4nGMQfqn/HwAEaQIr8Rs2WQAAAABJRU5ErkJggg==";
        let response = parse_kitty_graphics_protocol("a=t,f=100,i=1", png).unwrap();
        let graphic = response.graphic_data.unwrap();
        assert_eq!((graphic.width, graphic.height), (1, 1));
        assert_eq!(graphic.pixels, [19, 233, 47, 255]);
    }

    #[cfg(feature = "graphics")]
    #[test]
    fn apng_first_subframe_has_a_transparent_canvas() {
        for blend in [png::BlendOp::Source, png::BlendOp::Over] {
            let mut bytes = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut bytes, 2, 2);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                encoder.set_animated(1, 0).unwrap();
                encoder.set_sep_def_img(true).unwrap();
                let mut writer = encoder.write_header().unwrap();
                writer
                    .write_image_data(&[241, 17, 33, 255].repeat(4))
                    .unwrap();
                writer.set_frame_dimension(1, 1).unwrap();
                writer.set_frame_position(1, 1).unwrap();
                writer.set_blend_op(blend).unwrap();
                writer.write_image_data(&[19, 233, 47, 128]).unwrap();
            }
            let response =
                parse_kitty_graphics_protocol("a=t,f=100,i=1", &BASE64.encode(bytes))
                    .unwrap();
            let graphic = response.graphic_data.unwrap();
            assert_eq!((graphic.width, graphic.height), (2, 2));
            assert_eq!(
                graphic.pixels,
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 19, 233, 47, 128]
            );
        }
    }

    #[cfg(feature = "graphics")]
    #[test]
    fn png_dimension_error_follows_payload_validation() {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, MAX_DIMENSION + 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&[19, 233, 47, 255].repeat(MAX_DIMENSION as usize + 1))
                .unwrap();
        }
        let result =
            parse_kitty_graphics_protocol("a=t,f=100,i=1", &BASE64.encode(&bytes))
                .unwrap();
        assert!(result
            .response
            .unwrap()
            .contains("EINVAL: dimensions too large"));
        let mut offset = 8;
        while &bytes[offset + 4..offset + 8] != b"IDAT" {
            offset += u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap())
                as usize
                + 12;
        }
        bytes[offset + 8] ^= 1;
        let result =
            parse_kitty_graphics_protocol("a=t,f=100,i=1", &BASE64.encode(bytes))
                .unwrap();
        assert!(result.response.unwrap().contains("EINVAL: invalid data"));
    }

    #[test]
    fn test_animation_frame_is_decoded_and_forwarded() {
        let response = parse_kitty_graphics_protocol(
            "a=f,i=1,r=2,c=1,s=1,v=1,f=32,x=3,y=4,z=-1,X=1,Y=16909060",
            "AQIDBA==",
        )
        .unwrap();
        assert!(response.response.is_none());
        let request = response.animation_request.unwrap();
        assert_eq!(request.image_id, 1);
        let super::super::kitty_animation::Command::Frame(frame) = request.command else {
            panic!("frame request expected")
        };
        assert_eq!(
            (
                frame.number,
                frame.base,
                frame.x,
                frame.y,
                frame.gap,
                frame.background
            ),
            (2, 1, 3, 4, -1, 16909060)
        );
        assert!(frame.overwrite);
        assert_eq!(frame.data.pixels, [1, 2, 3, 4]);
    }

    #[test]
    fn test_animation_control_is_forwarded_without_a_premature_reply() {
        let response =
            parse_kitty_graphics_protocol("a=a,i=42,s=3,c=2,r=1,z=80,v=2", "").unwrap();
        assert!(response.response.is_none());
        let request = response.animation_request.unwrap();
        assert_eq!(request.image_id, 42);
        let super::super::kitty_animation::Command::Control(control) = request.command
        else {
            panic!("control expected")
        };
        assert_eq!(
            (
                control.state,
                control.current,
                control.frame,
                control.gap,
                control.loops
            ),
            (3, 2, 1, 80, 2)
        );
    }

    #[test]
    fn test_animation_compose_uses_source_offsets_and_c_mode() {
        let response = parse_kitty_graphics_protocol(
            "a=c,i=7,r=1,c=2,X=300,Y=400,C=256,x=3,y=4,w=5,h=6",
            "",
        )
        .unwrap();
        assert!(response.response.is_none());
        let request = response.animation_request.unwrap();
        let super::super::kitty_animation::Command::Compose(composition) =
            request.command
        else {
            panic!("composition expected")
        };
        assert_eq!(
            (
                composition.source,
                composition.destination,
                composition.source_x,
                composition.source_y
            ),
            (1, 2, 300, 400)
        );
        assert_eq!(
            (
                composition.x,
                composition.y,
                composition.width,
                composition.height
            ),
            (3, 4, 5, 6)
        );
        assert!(composition.overwrite);
    }

    #[test]
    fn test_animation_request_preserves_image_number() {
        let response =
            parse_kitty_graphics_protocol("a=f,I=99,r=2,s=1,v=1,f=32", "AQIDBA==")
                .unwrap();
        let request = response.animation_request.unwrap();
        assert_eq!((request.image_id, request.image_number), (0, 99));
    }

    #[test]
    fn test_animation_error_suppressed_when_quiet_2() {
        // q=2 should suppress error responses too
        let result =
            parse_kitty_graphics_protocol("a=f,i=1,r=2,s=1,v=1,f=32,q=2", "AAAA");
        let response = result.expect("response struct should still exist");
        assert!(
            response.response.is_none(),
            "q=2 should suppress error response"
        );
    }

    #[test]
    fn test_parse_invalid_action() {
        let result = parse_kitty_graphics_protocol("a=x", "");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_empty_keys() {
        let mut state = KittyGraphicsState::default();

        // Empty params should return None
        let result = parse(&[], &mut state);
        assert!(result.is_none());

        // Just "G" with no control data defaults to action=t
        // (Transmit), then fails create_graphic_data because width=0 /
        // height=0 are not valid dimensions. The response carries no id
        // (implicit) so nothing is emitted.
        let result = parse(&[b"G"], &mut state);
        let response = result.expect("response struct must exist even on error");
        assert!(response.graphic_data.is_none());
        assert!(
            response.response.is_none(),
            "implicit-id failure must not emit a response"
        );

        // "G" with empty control data: same path
        let result = parse(&[b"G", b""], &mut state);
        assert!(result.is_some());
    }

    #[test]
    fn test_incomplete_image_accumulation() {
        // Use a single state instance across all chunks.
        // Per kitty spec each chunk must be a multiple of 4 base64 chars
        // (i.e. aligned on 3-byte binary boundaries) with padding only on
        // the final chunk. Full base64 for [255, 0, 0, 255] is "/wAA/w==".
        let mut state = KittyGraphicsState::default();

        // First chunk: 4 chars → 3 decoded bytes [0xFF, 0x00, 0x00]
        let params1 = vec![b"G".as_ref(), b"a=t,f=32,s=1,v=1,m=1,i=100", b"/wAA"];
        let result1 = parse(&params1, &mut state).expect(
            "intermediate chunks must return a `pending_chunk` response, not None",
        );
        assert!(result1.incomplete, "first chunk must be marked incomplete");
        assert!(result1.graphic_data.is_none());

        // Final chunk: 4 chars with padding → 1 decoded byte [0xFF]
        let params2 = vec![b"G".as_ref(), b"a=t,f=32,s=1,v=1,m=0,i=100", b"/w=="];
        let result2 = parse(&params2, &mut state);
        let response = result2.expect("final chunk must produce a response");
        assert!(!response.incomplete, "final chunk must not be incomplete");
        let graphic = response
            .graphic_data
            .expect("final chunk must produce graphic data");
        assert_eq!(
            graphic.pixels,
            vec![0xFF, 0x00, 0x00, 0xFF],
            "decoded bytes must equal the full decoded payload"
        );
    }

    #[test]
    fn test_incomplete_image_accumulation_padded_chunks() {
        let mut state = KittyGraphicsState::default();
        // The padding on each independently encoded chunk must not be
        // concatenated into the middle of another base64 string.
        let first = parse(
            &[b"G", b"a=t,f=24,s=3,v=1,m=1,i=200", b"AQIDBA=="],
            &mut state,
        )
        .unwrap();
        assert!(first.incomplete);
        let last = parse(
            &[b"G", b"a=t,f=24,s=3,v=1,m=0,i=200", b"BQYHCAk="],
            &mut state,
        )
        .unwrap();
        assert_eq!(
            last.graphic_data.unwrap().pixels,
            [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255]
        );
    }

    #[test]
    fn test_chunked_matches_single_shot() {
        // Parsing an image as a single command or as several chunks must
        // produce the same decoded payload — decoding per chunk must not
        // introduce drift.
        let single_pixel_b64 = "/wAA/w=="; // [0xFF, 0x00, 0x00, 0xFF]

        let single =
            parse_kitty_graphics_protocol("a=t,f=32,s=1,v=1,i=301", single_pixel_b64)
                .and_then(|r| r.graphic_data)
                .expect("single-shot must succeed");

        let mut state = KittyGraphicsState::default();
        let p1 = vec![b"G".as_ref(), b"a=t,f=32,s=1,v=1,m=1,i=302", b"/wAA"];
        parse(&p1, &mut state).expect("first chunk");
        let p2 = vec![b"G".as_ref(), b"a=t,f=32,s=1,v=1,m=0,i=302", b"/w=="];
        let chunked = parse(&p2, &mut state)
            .and_then(|r| r.graphic_data)
            .expect("chunked must succeed");

        assert_eq!(
            single.pixels, chunked.pixels,
            "chunked and single-shot decode must agree"
        );
        assert_eq!(single.width, chunked.width);
        assert_eq!(single.height, chunked.height);
    }

    #[test]
    fn test_chunked_preserves_first_chunk_metadata() {
        // Only the first chunk carries the full control data. Subsequent
        // chunks (including the terminating m=0 one) may omit width,
        // height, format, etc. — the stored metadata from the first
        // chunk must be used.
        let mut state = KittyGraphicsState::default();

        let p1 = vec![b"G".as_ref(), b"a=T,f=32,s=1,v=1,i=500,z=7,m=1", b"/wAA"];
        parse(&p1, &mut state).expect("first chunk");

        // Final chunk: only `m=0` and `i=500`. Width/height/format are
        // intentionally missing and must be inherited from the first.
        let p2 = vec![b"G".as_ref(), b"m=0,i=500", b"/w=="];
        let response =
            parse(&p2, &mut state).expect("final chunk must produce a response");

        let graphic = response.graphic_data.expect("graphic data");
        assert_eq!(graphic.width, 1);
        assert_eq!(graphic.height, 1);
        assert_eq!(graphic.pixels, vec![0xFF, 0x00, 0x00, 0xFF]);

        // Placement should reflect the first chunk's z-index because it
        // used `a=T` (transmit and display).
        let placement = response
            .placement_request
            .expect("a=T must emit a placement request");
        assert_eq!(placement.z_index, 7);
    }

    #[test]
    fn test_chunked_with_zlib_compression() {
        // chafa transmits RGBA with zlib compression (o=z). The o= flag
        // is only carried on the first chunk, so the decompression must
        // happen after all chunks are merged.
        use flate2::write::ZlibEncoder;
        use flate2::Compression as FlateCompression;
        use std::io::Write;

        // 2 pixels, 8 bytes RGBA: red + green
        let raw = vec![0xFF, 0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF];
        let mut encoder = ZlibEncoder::new(Vec::new(), FlateCompression::default());
        encoder.write_all(&raw).unwrap();
        let compressed = encoder.finish().unwrap();
        let encoded = BASE64.encode(&compressed);

        // Split the base64 text on a 4-char boundary so each chunk is
        // spec-compliant.
        assert!(encoded.len() >= 4, "test payload should be chunkable");
        let split_at = (encoded.len() / 2) & !0x3; // nearest lower multiple of 4
        let (first, second) = encoded.split_at(split_at);

        let mut state = KittyGraphicsState::default();
        let p1_ctrl = String::from("a=t,f=32,s=2,v=1,o=z,m=1,i=600");
        let p1 = vec![b"G".as_ref(), p1_ctrl.as_bytes(), first.as_bytes()];
        parse(&p1, &mut state).expect("first chunk");

        let p2_ctrl = String::from("m=0,i=600");
        let p2 = vec![b"G".as_ref(), p2_ctrl.as_bytes(), second.as_bytes()];
        let response =
            parse(&p2, &mut state).expect("final chunk must parse and decompress");

        let graphic = response.graphic_data.expect("graphic data");
        assert_eq!(
            graphic.pixels, raw,
            "zlib-compressed chunked payload must decompress to original"
        );
    }

    #[test]
    fn test_max_dimension_rejects_oversized_images() {
        // Width above the cap must fail with EINVAL:dimensions too large.
        let result = parse_kitty_graphics_protocol("a=t,f=32,s=10001,v=1,i=1", "AAAA")
            .expect("response struct must exist");
        assert!(result.graphic_data.is_none());
        let body = result.response.expect("error message expected");
        assert!(
            body.contains("dimensions too large"),
            "expected dimensions-too-large: {body}"
        );

        // Height above the cap must also fail.
        let result = parse_kitty_graphics_protocol("a=t,f=32,s=1,v=10001,i=1", "AAAA")
            .expect("response struct must exist");
        assert!(result.graphic_data.is_none());
        let body = result.response.expect("error message expected");
        assert!(body.contains("dimensions too large"));

        // Exactly 10000 must be accepted (boundary).
        // (We don't actually feed 10000*10000*4 bytes here, the
        // create_graphic_data path will reject due to missing data, but
        // that's a different error than the dimension cap.)
        let result = parse_kitty_graphics_protocol("a=t,f=32,s=10000,v=1,i=1", "AAAA")
            .expect("response struct must exist");
        let body = result.response.unwrap_or_default();
        assert!(
            !body.contains("dimensions too large"),
            "10000 must not trip the dimension cap: {body}"
        );
    }

    #[test]
    fn test_iid_mutually_exclusive() {
        // Setting both i= and I= must be rejected with a specific EINVAL.
        let result = parse_kitty_graphics_protocol("a=t,f=32,s=1,v=1,i=5,I=6", "AAAA")
            .expect("response struct must exist");
        assert!(result.graphic_data.is_none());
        let body = result.response.expect("error message expected");
        assert!(
            body.contains("mutually exclusive"),
            "expected mutually-exclusive error: {body}"
        );
        assert!(body.contains("i=5"));
        assert!(body.contains("I=6"));
    }

    #[test]
    fn test_query_requires_image_id() {
        // Query without i= AND without I=: no identifier to address
        // the response to, so nothing is emitted.
        let result =
            parse_kitty_graphics_protocol("a=q", "").expect("response struct must exist");
        assert!(result.response.is_none());

        // Query with only I= set (no i=): we surface the EINVAL
        // addressed by image_number, so the client can correlate.
        let result = parse_kitty_graphics_protocol("a=q,I=7", "")
            .expect("response struct must exist");
        let body = result.response.expect("error message expected");
        assert!(
            body.contains("image ID required"),
            "expected image-id-required error: {body}"
        );
        assert!(body.contains("I=7"));

        // With an explicit id, query succeeds with OK.
        let result = parse_kitty_graphics_protocol("a=q,i=42,f=32,s=1,v=1", "AAAAAA==")
            .expect("response struct must exist");
        let body = result.response.expect("OK response expected");
        assert!(body.contains("i=42"));
        assert!(body.contains(";OK"));
    }

    #[cfg(feature = "graphics")]
    #[test]
    fn test_response_combines_image_number_and_placement() {
        // When placement_id is set alongside image_id, the response
        // must carry both keys.
        let png_data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==";
        let result = parse_kitty_graphics_protocol("a=T,f=100,i=7,p=13", png_data)
            .expect("response struct must exist");
        let body = result.response.expect("OK response expected");
        assert!(
            body.contains("i=7") && body.contains("p=13"),
            "both id and placement must appear: {body}"
        );
        assert!(body.contains(";OK"));
    }

    #[test]
    fn test_animation_params_are_parsed() {
        // Frame loading (a=f) should populate frame_number, base_frame,
        // frame_gap, composition_mode, background_color fields. We
        // still return EINVAL for the action itself, but clients can
        // extend support later and the values must already be correct.
        let mut cmd = KittyGraphicsCommand::default();
        parse_control_data(&mut cmd, "a=f,i=1,r=3,c=2,z=100,X=1,Y=4294901760");
        assert_eq!(cmd.action, Action::Frame);
        assert_eq!(cmd.frame_number, 3);
        assert_eq!(cmd.base_frame, 2);
        assert_eq!(cmd.frame_gap, 100);
        assert_eq!(cmd.composition_mode, 1);
        assert_eq!(cmd.background_color, 4_294_901_760);

        // Animation control (a=a) populates animation_state, loop_count,
        // current_frame, frame_number, frame_gap.
        let mut cmd = KittyGraphicsCommand::default();
        parse_control_data(&mut cmd, "a=a,i=2,s=3,v=5,c=1,r=2,z=50");
        assert_eq!(cmd.action, Action::Animate);
        assert_eq!(cmd.animation_state, 3);
        assert_eq!(cmd.loop_count, 5);
        assert_eq!(cmd.current_frame, 1);
        assert_eq!(cmd.frame_number, 2);
        assert_eq!(cmd.frame_gap, 50);
    }

    fn parse_delete(keys: &str) -> DeleteRequest {
        let resp = parse_kitty_graphics_protocol(keys, "")
            .expect("delete must produce a response");
        resp.delete_request
            .expect("delete_request must be populated")
    }

    #[test]
    fn test_delete_variants_all_parse() {
        // d=a/A — delete all
        let d = parse_delete("a=d,d=a");
        assert_eq!(d.action, b'a');
        assert!(!d.delete_data);
        let d = parse_delete("a=d,d=A");
        assert_eq!(d.action, b'a');
        assert!(d.delete_data, "uppercase form must set delete_data=true");

        // d=i/I — by image id
        let d = parse_delete("a=d,d=i,i=7");
        assert_eq!(d.action, b'i');
        assert_eq!(d.image_id, 7);

        // d=n/N — by image number (I= channel)
        let d = parse_delete("a=d,d=n,I=11");
        assert_eq!(d.action, b'n');
        assert_eq!(
            d.image_number, 11,
            "d=n must pick up the number from I=, not i="
        );

        // d=c/C — intersecting cursor
        let d = parse_delete("a=d,d=C");
        assert_eq!(d.action, b'c');
        assert!(d.delete_data);

        // d=p/P — at cell position
        let d = parse_delete("a=d,d=p,x=3,y=5");
        assert_eq!(d.action, b'p');
        assert_eq!(d.x, 3);
        assert_eq!(d.y, 5);

        // d=q/Q — at cell + z-index
        let d = parse_delete("a=d,d=q,x=2,y=4,z=-3");
        assert_eq!(d.action, b'q');
        assert_eq!(d.z_index, -3);

        // d=r/R — id range (x=start, y=end)
        let d = parse_delete("a=d,d=R,x=10,y=20");
        assert_eq!(d.action, b'r');
        assert_eq!(d.x, 10);
        assert_eq!(d.y, 20);
        assert!(d.delete_data);

        // d=x/X — by column
        let d = parse_delete("a=d,d=x,x=5");
        assert_eq!(d.action, b'x');
        assert_eq!(d.x, 5);

        // d=y/Y — by row
        let d = parse_delete("a=d,d=Y,y=9");
        assert_eq!(d.action, b'y');
        assert_eq!(d.y, 9);
        assert!(d.delete_data);

        // d=z/Z — by z-index
        let d = parse_delete("a=d,d=z,z=42");
        assert_eq!(d.action, b'z');
        assert_eq!(d.z_index, 42);
    }

    #[test]
    fn placement_and_query_preserve_the_pending_upload() {
        let mut state = KittyGraphicsState::default();
        assert!(
            parse(&[b"G", b"a=t,f=32,s=1,v=1,m=1,i=777", b"AQI="], &mut state)
                .unwrap()
                .incomplete
        );
        for control in ["a=p,i=99", "a=q,i=888,f=32,s=1,v=1"] {
            let _ = parse(&[b"G", control.as_bytes(), b"AQIDBA=="], &mut state);
            assert_eq!(state.loading.as_ref().unwrap().image_id, 777);
        }
        let done = parse(&[b"G", b"i=999,m=0", b"AwQ="], &mut state).unwrap();
        assert_eq!(done.graphic_data.unwrap().pixels, [1, 2, 3, 4]);
        assert!(state.loading.is_none());
    }

    #[test]
    fn test_padding_in_middle_of_raw_concat_would_fail() {
        // Sanity check: confirm the exact failure mode we fixed — a
        // naive concat of padded chunks produces a string with `=` in
        // the middle that BASE64.decode rejects. If this ever becomes a
        // non-error it means the decoder changed and our fix may be
        // masking something else.
        let concatenated = b"3q2+7w==yv66".as_slice();
        assert!(
            BASE64.decode(concatenated).is_err(),
            "strict base64 must reject `=` in the middle of the input"
        );
    }

    #[test]
    fn test_pending_chunk_distinct_from_parse_error() {
        // Regression for the yazi log spam: an in-progress chunked
        // transmission must be distinguishable from a real parse error.
        // Pending chunks return `Some { incomplete: true }`; real
        // errors return a response with an EINVAL/ENOENT message
        // instead of a graphic.
        let mut state = KittyGraphicsState::default();

        // m=1: pending — Some(incomplete=true), no graphic, no error msg
        let params = vec![b"G".as_ref(), b"a=t,f=32,s=1,v=1,m=1,i=42", b"/wAA"];
        let resp = parse(&params, &mut state).expect("pending must be Some");
        assert!(resp.incomplete);
        assert!(resp.response.is_none());

        // Real error path: blocked path → error response addressed to
        // i=99. The exact error varies by platform — on Linux the
        // `/proc/` sensitive-path match triggers EINVAL, on macOS the
        // missing file triggers ENOENT. Either is correct.
        let proc_path = BASE64.encode("/proc/self/environ".as_bytes());
        let bad = vec![
            b"G".as_ref(),
            b"a=t,t=f,f=32,s=1,v=1,i=99",
            proc_path.as_bytes(),
        ];
        let resp = parse(&bad, &mut KittyGraphicsState::default())
            .expect("real errors now carry a response, not None");
        assert!(!resp.incomplete, "real errors must not look like pending");
        assert!(resp.graphic_data.is_none());
        let body = resp.response.expect("error message expected");
        assert!(
            body.contains("i=99"),
            "error must be addressed to i=99: {body}"
        );
        assert!(
            body.contains("EINVAL") || body.contains("ENOENT"),
            "blocked path should surface EINVAL or ENOENT: {body}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn shared_memory_byte_ranges() {
        use std::ffi::CString;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);

        for (offset, size, accepted) in
            [(1, 4, true), (1, 0, true), (5, 4, false), (9, 0, false)]
        {
            let name = CString::new(format!(
                "/rio-kitty-range-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
            .unwrap();
            unsafe {
                let fd = libc::shm_open(
                    name.as_ptr(),
                    libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                    0o600,
                );
                assert!(fd >= 0);
                assert_eq!(libc::ftruncate(fd, 8), 0);
                let ptr = libc::mmap(
                    std::ptr::null_mut(),
                    8,
                    libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    0,
                );
                assert_ne!(ptr, libc::MAP_FAILED);
                std::ptr::copy_nonoverlapping(
                    [0u8, 255, 0, 0, 255, 0, 0, 0].as_ptr(),
                    ptr.cast(),
                    8,
                );
                libc::munmap(ptr, 8);
                libc::close(fd);
            }
            let encoded = BASE64.encode(name.as_bytes());
            let response = parse_kitty_graphics_protocol(
                &format!("a=t,t=s,f=32,s=1,v=1,i=1,O={offset},S={size}"),
                &encoded,
            )
            .unwrap();
            // Cleanup even when the assertion fails on an older implementation.
            unsafe {
                libc::shm_unlink(name.as_ptr());
            }
            assert_eq!(
                response.graphic_data.is_some(),
                accepted,
                "offset={offset}, size={size}"
            );
        }
    }

    #[test]
    fn malformed_commands_are_silent_without_dispatch() {
        for invalid in ["a=?", "t=?", "o=?", "s=-1", "v=oops", "O=4294967296"] {
            for quiet in 0..=2 {
                let control = format!("a=t,f=32,s=1,v=1,i=7,p=3,{invalid},q={quiet}");
                assert!(
                    parse_kitty_graphics_protocol(&control, "AQIDBA==").is_none(),
                    "{control}"
                );
            }
        }
        for control in [b"i=7,q=1".as_slice(), b"i=7,q=1,a=\xff".as_slice()] {
            assert!(parse(
                &[b"G", control, b"!invalid!"],
                &mut KittyGraphicsState::default()
            )
            .is_none());
        }
    }

    #[test]
    fn unsupported_wire_formats_fail_when_starting_a_load() {
        for format in [8, 16, 99] {
            let mut state = KittyGraphicsState::default();
            let control = format!("a=t,f={format},s=1,v=1,i=7,p=3,m=1");
            let result =
                parse(&[b"G", control.as_bytes(), b"AQIDBA=="], &mut state).unwrap();
            assert_eq!(
                result.response.as_deref(),
                Some("\x1b_Gi=7,p=3;EINVAL: unsupported format\x1b\\")
            );
            assert_eq!(state.take_started_transmission(), Some(7));
            assert!(state.loading.is_none());
        }
    }

    #[test]
    fn malformed_continuation_preserves_the_pending_upload() {
        let mut state = KittyGraphicsState::default();
        assert!(
            parse(&[b"G", b"a=t,f=32,s=1,v=1,i=7,m=1", b"AQI="], &mut state)
                .unwrap()
                .incomplete
        );
        assert!(parse(&[b"G", b"i=8,m=0", b"!"], &mut state).is_none());
        assert_eq!(state.loading.as_ref().unwrap().image_id, 7);
        let done = parse(&[b"G", b"i=8,m=0", b"AwQ="], &mut state).unwrap();
        let data = done.graphic_data.unwrap();
        assert_eq!(data.id.get(), 7);
        assert_eq!(data.pixels, [1, 2, 3, 4]);
    }

    #[test]
    fn test_file_transmission_medium() {
        // Create a temporary file
        use std::io::Write;
        let temp_path = std::env::temp_dir().join("test_kitty_image.rgba");
        let temp_path = temp_path.to_str().unwrap();
        let mut file = std::fs::File::create(temp_path).unwrap();
        file.write_all(&[255, 0, 0, 255]).unwrap(); // 1x1 red pixel
        drop(file);

        // Encode the file path as base64 (as kitty does)
        let encoded_path = BASE64.encode(temp_path.as_bytes());
        let result =
            parse_kitty_graphics_protocol("a=t,t=f,f=32,s=1,v=1,i=1", &encoded_path);
        assert!(result.is_some());

        let response = result.unwrap();
        assert!(response.graphic_data.is_some());

        // Cleanup
        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn test_temp_file_transmission_medium() {
        // Create a temporary file with required naming
        use std::io::Write;
        let temp_path = std::env::temp_dir().join("tty-graphics-protocol-test.rgba");
        let temp_path = temp_path.to_str().unwrap();
        let mut file = std::fs::File::create(temp_path).unwrap();
        file.write_all(&[255, 0, 0, 255]).unwrap(); // 1x1 red pixel
        drop(file);

        // Encode the file path as base64 (as kitty does)
        let encoded_path = BASE64.encode(temp_path.as_bytes());
        let result =
            parse_kitty_graphics_protocol("a=t,t=t,f=32,s=1,v=1,i=1", &encoded_path);

        // File should be deleted after reading
        assert!(!std::path::Path::new(temp_path).exists());

        assert!(result.is_some());
        let response = result.unwrap();
        assert!(response.graphic_data.is_some());
    }

    #[test]
    fn test_security_checks() {
        // Commands with no i=/I= get an implicit id, which suppresses
        // the response entirely — but we still must NOT load the file.
        // Include i=1 so we can assert on the EINVAL response.
        for path in &["/proc/self/environ", "/sys/class/net", "/dev/null"] {
            let encoded = BASE64.encode(path.as_bytes());
            let response =
                parse_kitty_graphics_protocol("a=t,t=f,f=32,s=1,v=1,i=1", &encoded)
                    .expect("must return a response carrying the EINVAL");
            assert!(response.graphic_data.is_none(), "{path} must not load");
            let body = response.response.expect("error message expected");
            assert!(body.contains("i=1"));
            assert!(
                body.contains("EINVAL") || body.contains("ENOENT"),
                "blocked path should surface EINVAL/ENOENT: {body}"
            );
        }
    }

    #[test]
    fn test_quiet_mode() {
        // q=1 suppresses OK but keeps errors
        let result = parse_kitty_graphics_protocol("a=p,i=1,q=1", "");
        let response = result.expect("response struct must exist");
        assert!(response.response.is_none(), "q=1 must suppress OK");

        // q=2 suppresses everything, including errors. / kitty
        // both document this as "absolute silence".
        let result = parse_kitty_graphics_protocol("a=q,i=1,q=2", "");
        let response = result.expect("response struct must exist");
        assert!(response.response.is_none(), "q=2 must suppress all output");
    }
}
