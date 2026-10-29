//! Parser for virtual terminal escape sequences.
//!
//! [`Parser`] implements [Paul Williams' ANSI parser state machine]. The state
//! machine doesn't assign meaning to the parsed data — that's the job of the
//! [`Perform`] implementer.
//!
//! Forked from Alacritty's VTE; previously the standalone `copa` crate. The
//! [`Perform`] trait keeps a single dispatch shape so the same state machine
//! drives the production [`Performer`], unit-test dispatchers, and external
//! consumers that need a raw escape-sequence parser (e.g. byte-stream
//! trackers) without pulling in a second VTE implementation.
//!
//! [Paul Williams' ANSI parser state machine]: https://vt100.net/emu/dec_ansi_parser
//! [`Performer`]: super::handler::Performer

#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use)]

use std::str;

mod params;

pub use params::{Params, ParamsIter};

const MAX_INTERMEDIATES: usize = 4;
const MAX_OSC_PARAMS: usize = 16;

/// Inline OSC byte capacity. Sized to absorb common OSCs (titles, color
/// queries, hyperlink URLs, kitty graphics control headers) without
/// allocation. Larger payloads (e.g. OSC 52 clipboard pastes) spill into
/// `OscBuffer::overflow`.
const OSC_FIXED_LEN: usize = 2048;

/// Ordinary control strings and reusable buffers need no more than 64 KiB.
pub(super) const MAX_CONTROL_BYTES: usize = 64 * 1024;
/// Encoded OSC 52 data, allowing clipboard transfers up to about 12 MiB.
const MAX_CLIPBOARD_BYTES: usize = 16 * 1024 * 1024;
/// Base64 expansion of the existing 400 MiB Kitty image limit, plus headers.
pub(super) const MAX_GRAPHICS_BYTES: usize =
    (400 * 1024 * 1024usize).div_ceil(3) * 4 + MAX_CONTROL_BYTES;

/// Parser for raw _VTE_ protocol which delegates actions to a [`Perform`].
#[derive(Default)]
pub struct Parser {
    state: State,
    intermediates: [u8; MAX_INTERMEDIATES],
    intermediate_idx: usize,
    params: Params,
    param: u16,
    osc_raw: OscBuffer,
    osc_params: [(usize, usize); MAX_OSC_PARAMS],
    osc_num_params: usize,
    /// Zero selects the ordinary limit until the command number is complete.
    osc_max_bytes: usize,
    osc_discarded: bool,
    osc_command: Option<u16>,
    ignoring: bool,
    partial_utf8: [u8; 4],
    partial_utf8_len: usize,
    /// Reused output buffer for [`simdutf::convert_utf8_to_utf32`]. Grows
    /// to fit the largest non-ASCII ground-state chunk seen so far; empty
    /// in steady state when sessions are pure ASCII.
    decode_buf: Vec<u32>,
}

/// OSC accumulator with a fixed-size inline buffer and a heap fallback.
///
/// The first `OSC_FIXED_LEN` bytes of any OSC sequence land in `fixed`
/// (zero allocation). On overflow, the populated prefix of `fixed` is copied
/// into `overflow` once and all subsequent writes go to the `Vec` only — so
/// at any moment a single backing slice holds the contiguous payload.
struct OscBuffer {
    fixed: [u8; OSC_FIXED_LEN],
    fixed_len: usize,
    overflow: Vec<u8>,
}

impl Default for OscBuffer {
    fn default() -> Self {
        Self {
            fixed: [0; OSC_FIXED_LEN],
            fixed_len: 0,
            overflow: Vec::new(),
        }
    }
}

impl OscBuffer {
    #[inline]
    fn len(&self) -> usize {
        if self.overflow.is_empty() {
            self.fixed_len
        } else {
            self.overflow.len()
        }
    }

    #[inline]
    fn try_push(&mut self, byte: u8, limit: usize) -> bool {
        debug_assert!(limit >= OSC_FIXED_LEN);
        if self.overflow.is_empty() && self.fixed_len < OSC_FIXED_LEN {
            self.fixed[self.fixed_len] = byte;
            self.fixed_len += 1;
            return true;
        }
        if self.len() >= limit {
            return false;
        }
        if self.overflow.is_empty() {
            // Spill: promote the current contents to the heap once, then
            // append. After this point, `overflow.len() >= OSC_FIXED_LEN`,
            // so the `is_empty()` check above stays false until `clear`.
            self.overflow
                .extend_from_slice(&self.fixed[..self.fixed_len]);
        }
        self.overflow.push(byte);
        true
    }

    /// Bulk append, equivalent to pushing every byte: fills the fixed
    /// buffer when the slice fits, otherwise spills to the heap once
    /// and appends there (same final contents as per-byte pushes; the
    /// stale `fixed_len` is never read while `overflow` is non-empty).
    #[inline]
    fn try_extend_from_slice(&mut self, bytes: &[u8], limit: usize) -> bool {
        debug_assert!(limit >= OSC_FIXED_LEN);
        if self.overflow.is_empty() {
            let fits = OSC_FIXED_LEN - self.fixed_len;
            if bytes.len() <= fits {
                self.fixed[self.fixed_len..self.fixed_len + bytes.len()]
                    .copy_from_slice(bytes);
                self.fixed_len += bytes.len();
                return true;
            }
        }
        if bytes.len() > limit.saturating_sub(self.len()) {
            return false;
        }
        if self.overflow.is_empty() {
            self.overflow
                .extend_from_slice(&self.fixed[..self.fixed_len]);
        }
        self.overflow.extend_from_slice(bytes);
        true
    }

    #[inline]
    fn slice(&self, start: usize, end: usize) -> &[u8] {
        if self.overflow.is_empty() {
            &self.fixed[start..end]
        } else {
            &self.overflow[start..end]
        }
    }

    #[inline]
    fn clear(&mut self) {
        self.fixed_len = 0;
        if self.overflow.capacity() > MAX_CONTROL_BYTES {
            self.overflow = Vec::new();
        } else {
            self.overflow.clear();
        }
    }
}

impl Parser {
    /// Create a new parser in the ground state.
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    fn params(&self) -> &Params {
        &self.params
    }

    #[inline]
    fn intermediates(&self) -> &[u8] {
        &self.intermediates[..self.intermediate_idx]
    }

    /// Advance the parser state.
    ///
    /// Requires a [`Perform`] implementation to handle the triggered actions.
    #[inline]
    pub fn advance<P: Perform>(&mut self, performer: &mut P, bytes: &[u8]) {
        let mut i = 0;

        // Handle partial codepoints from previous calls to `advance`.
        if self.partial_utf8_len != 0 {
            i += self.advance_partial_utf8(performer, bytes);
        }

        while i != bytes.len() {
            match self.state {
                State::Ground => i += self.advance_ground(performer, &bytes[i..]),
                State::CsiParam => {
                    i += self.advance_csi_param_run(performer, &bytes[i..])
                }
                State::OscString => {
                    i += self.advance_osc_string_run(performer, &bytes[i..])
                }
                State::ApcString => {
                    i += self.advance_apc_string_run(performer, &bytes[i..])
                }
                State::SosString => {
                    i += self.advance_sos_string_run(performer, &bytes[i..])
                }
                State::PmString => {
                    i += self.advance_pm_string_run(performer, &bytes[i..])
                }
                State::DcsPassthrough => {
                    i += self.advance_dcs_passthrough_run(performer, &bytes[i..])
                }
                _ => {
                    // Inlining it results in worse codegen.
                    let byte = bytes[i];
                    self.change_state(performer, byte);
                    i += 1;
                }
            }
        }
    }

    /// Consume a run of bytes while in `CsiParam`, accumulating digit
    /// sub-runs into a local instead of paying the state dispatch and the
    /// `self.param` load/store per byte. Any byte outside the param set
    /// falls through to the generic per-byte path.
    fn advance_csi_param_run<P: Perform>(
        &mut self,
        performer: &mut P,
        bytes: &[u8],
    ) -> usize {
        let mut i = 0;
        while i < bytes.len() {
            let byte = bytes[i];
            match byte {
                0x30..=0x39 => {
                    if self.params.is_full() {
                        self.ignoring = true;
                        i += 1;
                    } else {
                        let mut param = self.param as u32;
                        while i < bytes.len() && bytes[i].is_ascii_digit() {
                            // The u16-clamped input fits a multiply-add in u32,
                            // so one final clamp replaces two saturating ops.
                            param = (param * 10 + (bytes[i] - b'0') as u32)
                                .min(u16::MAX as u32);
                            i += 1;
                        }
                        self.param = param as u16;
                    }
                }
                0x3A => {
                    self.action_subparam();
                    i += 1;
                }
                0x3B => {
                    self.action_param();
                    i += 1;
                }
                _ => {
                    self.advance_csi_param(performer, byte);
                    i += 1;
                    if self.state != State::CsiParam {
                        break;
                    }
                }
            }
        }
        i
    }

    #[inline(always)]
    fn change_state<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match self.state {
            State::CsiEntry => self.advance_csi_entry(performer, byte),
            State::CsiIgnore => self.advance_csi_ignore(performer, byte),
            State::CsiIntermediate => self.advance_csi_intermediate(performer, byte),
            State::CsiParam => self.advance_csi_param(performer, byte),
            State::DcsEntry => self.advance_dcs_entry(performer, byte),
            State::DcsIgnore => {
                if byte < 0x80 {
                    self.anywhere(performer, byte);
                }
            }
            State::DcsIntermediate => self.advance_dcs_intermediate(performer, byte),
            State::DcsParam => self.advance_dcs_param(performer, byte),
            State::DcsPassthrough => self.advance_dcs_passthrough(performer, byte),
            State::Escape => self.advance_esc(performer, byte),
            State::EscapeIntermediate => self.advance_esc_intermediate(performer, byte),
            State::OscString => self.advance_osc_string(performer, byte),
            State::SosString => self.advance_sos_string(performer, byte),
            State::ApcString => self.advance_apc_string(performer, byte),
            State::PmString => self.advance_pm_string(performer, byte),
            State::Ground => unreachable!(),
        }
    }

    #[inline(always)]
    fn advance_csi_entry<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
            0x20..=0x2F => {
                self.action_collect(byte);
                self.state = State::CsiIntermediate
            }
            0x30..=0x39 => {
                self.action_paramnext(byte);
                self.state = State::CsiParam
            }
            0x3A => {
                self.action_subparam();
                self.state = State::CsiParam
            }
            0x3B => {
                self.action_param();
                self.state = State::CsiParam
            }
            0x3C..=0x3F => {
                self.action_collect(byte);
                self.state = State::CsiParam
            }
            0x40..=0x7E => self.action_csi_dispatch(performer, byte),
            _ => self.anywhere(performer, byte),
        }
    }

    #[inline(always)]
    fn advance_csi_ignore<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
            0x20..=0x3F => (),
            0x40..=0x7E => self.state = State::Ground,
            0x7F => (),
            _ => self.anywhere(performer, byte),
        }
    }

    #[inline(always)]
    fn advance_csi_intermediate<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
            0x20..=0x2F => self.action_collect(byte),
            0x30..=0x3F => self.state = State::CsiIgnore,
            0x40..=0x7E => self.action_csi_dispatch(performer, byte),
            _ => self.anywhere(performer, byte),
        }
    }

    #[inline(always)]
    fn advance_csi_param<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
            0x20..=0x2F => {
                self.action_collect(byte);
                self.state = State::CsiIntermediate
            }
            0x30..=0x39 => self.action_paramnext(byte),
            0x3A => self.action_subparam(),
            0x3B => self.action_param(),
            0x3C..=0x3F => self.state = State::CsiIgnore,
            0x40..=0x7E => self.action_csi_dispatch(performer, byte),
            0x7F => (),
            _ => self.anywhere(performer, byte),
        }
    }

    #[inline(always)]
    fn advance_dcs_entry<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => (),
            0x20..=0x2F => {
                self.action_collect(byte);
                self.state = State::DcsIntermediate
            }
            0x30..=0x39 => {
                self.action_paramnext(byte);
                self.state = State::DcsParam
            }
            0x3A => self.state = State::DcsIgnore,
            0x3B => {
                self.action_param();
                self.state = State::DcsParam
            }
            0x3C..=0x3F => {
                self.action_collect(byte);
                self.state = State::DcsParam
            }
            0x40..=0x7E => self.action_hook(performer, byte),
            0x7F => (),
            _ => self.anywhere(performer, byte),
        }
    }

    #[inline(always)]
    fn advance_dcs_intermediate<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => (),
            0x20..=0x2F => self.action_collect(byte),
            0x30..=0x3F => self.state = State::DcsIgnore,
            0x40..=0x7E => self.action_hook(performer, byte),
            0x7F => (),
            _ => self.anywhere(performer, byte),
        }
    }

    #[inline(always)]
    fn advance_dcs_param<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => (),
            0x20..=0x2F => {
                self.action_collect(byte);
                self.state = State::DcsIntermediate
            }
            0x30..=0x39 => self.action_paramnext(byte),
            0x3A => self.state = State::DcsIgnore,
            0x3B => self.action_param(),
            0x3C..=0x3F => self.state = State::DcsIgnore,
            0x40..=0x7E => self.action_hook(performer, byte),
            0x7F => (),
            _ => self.anywhere(performer, byte),
        }
    }

    #[inline(always)]
    fn advance_dcs_passthrough<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x7E | 0x80..=0xFF => performer.put(byte),
            0x18 | 0x1A => {
                performer.unhook();
                performer.execute(byte);
                self.state = State::Ground
            }
            0x1B => {
                performer.unhook();
                self.reset_params();
                self.state = State::Escape
            }
            0x7F => (),
        }
    }

    #[inline(always)]
    fn advance_esc<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
            0x20..=0x2F => {
                self.action_collect(byte);
                self.state = State::EscapeIntermediate
            }
            0x30..=0x4F => {
                performer.esc_dispatch(self.intermediates(), self.ignoring, byte);
                self.state = State::Ground
            }
            0x50 => {
                self.reset_params();
                self.state = State::DcsEntry
            }
            0x51..=0x57 => {
                performer.esc_dispatch(self.intermediates(), self.ignoring, byte);
                self.state = State::Ground
            }
            0x58 => {
                performer.sos_start();
                self.state = State::SosString
            }
            0x59..=0x5A => {
                performer.esc_dispatch(self.intermediates(), self.ignoring, byte);
                self.state = State::Ground
            }
            0x5B => {
                self.reset_params();
                self.state = State::CsiEntry
            }
            0x5C => {
                performer.esc_dispatch(self.intermediates(), self.ignoring, byte);
                self.state = State::Ground
            }
            0x5D => {
                // `osc_end` already clears state at the end of every OSC, so the
                // buffer is guaranteed empty when re-entering OSC state.
                self.state = State::OscString
            }
            0x5E => {
                performer.pm_start();
                self.state = State::PmString
            }
            0x5F => {
                performer.apc_start();
                self.state = State::ApcString
            }
            0x60..=0x7E => {
                performer.esc_dispatch(self.intermediates(), self.ignoring, byte);
                self.state = State::Ground
            }
            // Anywhere.
            0x18 | 0x1A => {
                performer.execute(byte);
                self.state = State::Ground
            }
            0x1B => (),
            0x80..=0x9F => self.anywhere(performer, byte),
            _ => (),
        }
    }

    #[inline(always)]
    fn advance_esc_intermediate<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
            0x20..=0x2F => self.action_collect(byte),
            0x30..=0x7E => {
                performer.esc_dispatch(self.intermediates(), self.ignoring, byte);
                self.state = State::Ground
            }
            0x7F => (),
            _ => self.anywhere(performer, byte),
        }
    }

    #[inline(always)]
    /// Consume a run of bytes while in `OscString`: the payload span up
    /// to the next control byte or param separator is appended in one
    /// `extend_from_slice` instead of paying two state dispatches and a
    /// `Vec::push` per byte (OSC 52 payloads are megabytes of base64).
    /// The boundary byte goes through the per-byte state machine, which
    /// stays the single source of truth for semantics.
    fn advance_osc_string_run<P: Perform>(
        &mut self,
        performer: &mut P,
        bytes: &[u8],
    ) -> usize {
        let n = find_osc_boundary(bytes);
        if n != 0 {
            self.osc_extend(&bytes[..n]);
        }
        if n == bytes.len() {
            return n;
        }
        self.advance_osc_string(performer, bytes[n]);
        n + 1
    }

    /// Same shape for `ApcString` (kitty graphics payloads).
    fn advance_apc_string_run<P: Perform>(
        &mut self,
        performer: &mut P,
        bytes: &[u8],
    ) -> usize {
        let n = find_opaque_boundary(bytes);
        if n != 0 {
            performer.apc_put_slice(&bytes[..n]);
        }
        if n == bytes.len() {
            return n;
        }
        self.advance_apc_string(performer, bytes[n]);
        n + 1
    }

    /// Same shape for `SosString`.
    fn advance_sos_string_run<P: Perform>(
        &mut self,
        performer: &mut P,
        bytes: &[u8],
    ) -> usize {
        let n = find_opaque_boundary(bytes);
        if n != 0 {
            performer.sos_put_slice(&bytes[..n]);
        }
        if n == bytes.len() {
            return n;
        }
        self.advance_sos_string(performer, bytes[n]);
        n + 1
    }

    /// Same shape for `PmString`.
    fn advance_pm_string_run<P: Perform>(
        &mut self,
        performer: &mut P,
        bytes: &[u8],
    ) -> usize {
        let n = find_opaque_boundary(bytes);
        if n != 0 {
            performer.pm_put_slice(&bytes[..n]);
        }
        if n == bytes.len() {
            return n;
        }
        self.advance_pm_string(performer, bytes[n]);
        n + 1
    }

    /// Same shape for `DcsPassthrough` (sixel payloads). The put class
    /// here includes most C0 bytes and stops at 0x7F, so it has its own
    /// boundary scan.
    fn advance_dcs_passthrough_run<P: Perform>(
        &mut self,
        performer: &mut P,
        bytes: &[u8],
    ) -> usize {
        let n = find_dcs_boundary(bytes);
        if n != 0 {
            performer.put_slice(&bytes[..n]);
        }
        if n == bytes.len() {
            return n;
        }
        self.advance_dcs_passthrough(performer, bytes[n]);
        n + 1
    }

    fn advance_osc_string<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1C..=0x1F => (),
            0x07 => {
                self.osc_end(performer, byte);
                self.state = State::Ground
            }
            0x18 | 0x1A => {
                self.osc_end(performer, byte);
                performer.execute(byte);
                self.state = State::Ground
            }
            0x1B => {
                self.osc_end(performer, byte);
                self.reset_params();
                self.state = State::Escape
            }
            0x3B => self.action_osc_put_param(),
            _ => self.action_osc_put(byte),
        }
    }

    #[inline(always)]
    fn advance_apc_string<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        self.advance_opaque_string(performer, byte);
    }

    #[inline(always)]
    fn advance_sos_string<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        self.advance_opaque_string(performer, byte);
    }

    #[inline(always)]
    fn advance_pm_string<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        self.advance_opaque_string(performer, byte);
    }

    fn advance_opaque_string<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x18 | 0x1A | 0x1B | 0x80..=0x9F => {
                // CAN/SUB and C1 interruptions must not publish a partial
                // graphics command. ST is a proper terminator.
                match self.state {
                    State::ApcString if !matches!(byte, 0x1B | 0x9C) => {
                        performer.apc_abort()
                    }
                    State::ApcString => performer.apc_end(),
                    State::SosString => performer.sos_end(),
                    State::PmString => performer.pm_end(),
                    _ => unreachable!(),
                }
                self.anywhere(performer, byte);
            }
            0x00..=0x17 | 0x19 | 0x1C..=0x7F => match self.state {
                State::ApcString => performer.apc_put(byte),
                State::SosString => performer.sos_put(byte),
                State::PmString => performer.pm_put(byte),
                _ => unreachable!(),
            },
            // Opaque strings are seven-bit payloads. High bytes outside C1
            // have no action in Ghostty's sos_pm_apc_string state.
            _ => (),
        }
    }

    #[inline(always)]
    fn anywhere<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        match byte {
            0x18 | 0x1A | 0x80..=0x8F | 0x91..=0x97 | 0x99..=0x9A => {
                performer.execute(byte);
                self.state = State::Ground
            }
            0x1B => {
                self.reset_params();
                self.state = State::Escape
            }
            0x9C => self.state = State::Ground,
            0x90 | 0x98 | 0x9B | 0x9D..=0x9F => {
                self.reset_params();
                self.state = match byte {
                    0x90 => State::DcsEntry,
                    0x98 => {
                        performer.sos_start();
                        State::SosString
                    }
                    0x9B => State::CsiEntry,
                    0x9D => State::OscString,
                    0x9E => {
                        performer.pm_start();
                        State::PmString
                    }
                    0x9F => {
                        performer.apc_start();
                        State::ApcString
                    }
                    _ => unreachable!(),
                };
            }
            _ => (),
        }
    }

    #[inline]
    fn action_csi_dispatch<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.push(self.param);
        }
        performer.csi_dispatch(
            self.params(),
            self.intermediates(),
            self.ignoring,
            byte as char,
        );

        self.state = State::Ground
    }

    #[inline]
    fn action_hook<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.push(self.param);
        }
        performer.hook(
            self.params(),
            self.intermediates(),
            self.ignoring,
            byte as char,
        );
        self.state = State::DcsPassthrough;
    }

    #[inline]
    fn action_collect(&mut self, byte: u8) {
        if self.intermediate_idx == MAX_INTERMEDIATES {
            self.ignoring = true;
        } else {
            self.intermediates[self.intermediate_idx] = byte;
            self.intermediate_idx += 1;
        }
    }

    /// Advance to the next subparameter.
    #[inline]
    fn action_subparam(&mut self) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.extend(self.param);
            self.param = 0;
        }
    }

    /// Advance to the next parameter.
    #[inline]
    fn action_param(&mut self) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.push(self.param);
            self.param = 0;
        }
    }

    /// Advance inside the parameter without terminating it.
    #[inline]
    fn action_paramnext(&mut self, byte: u8) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            // Continue collecting bytes into param.
            self.param = self.param.saturating_mul(10);
            self.param = self.param.saturating_add((byte - b'0') as u16);
        }
    }

    /// Add OSC param separator.
    #[inline]
    fn action_osc_put_param(&mut self) {
        if self.osc_discarded {
            return;
        }
        if self.osc_payload_is_verbatim() {
            self.action_osc_put(b';');
        } else {
            self.push_osc_param();
        }
    }

    #[inline]
    fn osc_payload_is_verbatim(&self) -> bool {
        let param = |i: usize| {
            let (start, end) = self.osc_params[i];
            self.osc_raw.slice(start, end)
        };
        match self.osc_num_params {
            1 => param(0) == b"7",
            2 => param(0) == b"8" || (param(0) == b"9" && param(1) == b"9"),
            _ => false,
        }
    }

    #[inline]
    fn push_osc_param(&mut self) {
        if self.osc_discarded {
            return;
        }
        let idx = self.osc_raw.len();

        let param_idx = self.osc_num_params;
        match param_idx {
            // First param is special - 0 to current byte index.
            0 => self.osc_params[param_idx] = (0, idx),

            // Only process up to MAX_OSC_PARAMS.
            MAX_OSC_PARAMS => return,

            // All other params depend on previous indexing.
            _ => {
                let prev = self.osc_params[param_idx - 1];
                let begin = prev.1;
                self.osc_params[param_idx] = (begin, idx);
            }
        }

        self.osc_num_params += 1;
        if param_idx == 0 {
            self.osc_command = std::str::from_utf8(self.osc_raw.slice(0, idx))
                .ok()
                .and_then(|command| command.parse().ok());
            self.osc_max_bytes = match self.osc_raw.slice(0, idx) {
                b"7501" => crate::program_status::MAX_SEQUENCE_BYTES,
                b"52" => MAX_CLIPBOARD_BYTES,
                b"1337" => MAX_GRAPHICS_BYTES,
                _ => MAX_CONTROL_BYTES,
            };
        }
    }

    fn osc_extend(&mut self, bytes: &[u8]) {
        if self.osc_discarded {
            return;
        }
        if !self.osc_raw.try_extend_from_slice(bytes, self.osc_limit()) {
            self.discard_osc();
        }
    }

    #[inline(always)]
    fn action_osc_put(&mut self, byte: u8) {
        if self.osc_discarded {
            return;
        }
        if !self.osc_raw.try_push(byte, self.osc_limit()) {
            self.discard_osc();
        }
    }

    #[inline]
    fn osc_limit(&self) -> usize {
        if self.osc_max_bytes == 0 {
            MAX_CONTROL_BYTES
        } else {
            self.osc_max_bytes
        }
    }

    fn discard_osc(&mut self) {
        self.osc_discarded = true;
        self.osc_raw.clear();
        self.osc_num_params = 0;
    }

    fn osc_end<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        self.push_osc_param();
        if self.osc_discarded {
            performer.osc_discarded(self.osc_command, byte == 0x07);
        } else {
            self.osc_dispatch(performer, byte);
        }
        self.osc_raw.clear();
        self.osc_num_params = 0;
        self.osc_max_bytes = 0;
        self.osc_discarded = false;
        self.osc_command = None;
    }

    /// Reset escape sequence parameters and intermediates.
    #[inline]
    fn reset_params(&mut self) {
        self.intermediate_idx = 0;
        self.ignoring = false;
        self.param = 0;

        self.params.clear();
    }

    /// Separate method for osc_dispatch that borrows self as read-only.
    ///
    /// The aliasing is needed here for multiple slices into self.osc_raw.
    #[inline]
    fn osc_dispatch<P: Perform>(&self, performer: &mut P, byte: u8) {
        let mut slices = smallvec::SmallVec::<[&[u8]; MAX_OSC_PARAMS]>::new();
        for &(start, end) in self.osc_params.iter().take(self.osc_num_params) {
            slices.push(self.osc_raw.slice(start, end));
        }
        performer.osc_dispatch(&slices, byte == 0x07);
    }

    /// Advance the parser state from ground.
    ///
    /// Single-pass shape: `memchr` finds the next ESC, then
    /// [`Self::ground_dispatch`] walks the prefix, decoding non-ASCII runs
    /// through `simdutf` with inline Maximal-Subpart U+FFFD replacement for
    /// invalid UTF-8. The separate `validate` pass the previous shape ran
    /// is gone — validation is fused into the decode call.
    #[inline]
    fn advance_ground<P: Perform>(&mut self, performer: &mut P, bytes: &[u8]) -> usize {
        // Find the next escape character.
        let num_bytes = bytes.len();
        let plain_chars = memchr::memchr(0x1B, bytes).unwrap_or(num_bytes);

        // If the next character is ESC, just process it and short-circuit.
        if plain_chars == 0 {
            return self.start_ground_escape(bytes);
        }

        let has_trailing_esc = plain_chars < num_bytes;
        let prefix = &bytes[..plain_chars];

        // No ESC follows: peel any valid-so-far partial UTF-8 sequence off
        // the tail and stash it for the next call. With a trailing ESC, any
        // partial bytes before the ESC are pre-ESC garbage and get U+FFFD-
        // replaced by the MSP-aware decode loop instead.
        let process_len = if has_trailing_esc {
            plain_chars
        } else {
            trim_valid_partial_utf8(prefix)
        };

        self.ground_dispatch(performer, &prefix[..process_len]);

        if process_len < plain_chars {
            // Stash truncated bytes for the next call.
            let partial = &prefix[process_len..];
            let dst_start = self.partial_utf8_len;
            let dst_end = dst_start + partial.len();
            self.partial_utf8[dst_start..dst_end].copy_from_slice(partial);
            self.partial_utf8_len = dst_end;
            return num_bytes;
        }

        if has_trailing_esc {
            plain_chars + self.start_ground_escape(&bytes[plain_chars..])
        } else {
            num_bytes
        }
    }

    #[inline]
    fn start_ground_escape(&mut self, bytes: &[u8]) -> usize {
        self.reset_params();
        // Ground runs stop at ESC. Consume common two-byte introducers
        // together so SGR/OSC-heavy output avoids another state dispatch.
        // A split introducer still takes the ordinary Escape path.
        match bytes.get(1) {
            Some(b'[') => {
                self.state = State::CsiEntry;
                2
            }
            Some(b']') => {
                self.state = State::OscString;
                2
            }
            _ => {
                self.state = State::Escape;
                1
            }
        }
    }

    /// Advance the parser while processing a partial utf8 codepoint.
    #[inline]
    fn advance_partial_utf8<P: Perform>(
        &mut self,
        performer: &mut P,
        bytes: &[u8],
    ) -> usize {
        // Try to copy up to 3 more characters, to ensure the codepoint is complete.
        let old_bytes = self.partial_utf8_len;
        let to_copy = bytes.len().min(self.partial_utf8.len() - old_bytes);
        self.partial_utf8[old_bytes..old_bytes + to_copy]
            .copy_from_slice(&bytes[..to_copy]);
        self.partial_utf8_len += to_copy;

        // Parse the unicode character.
        match crate::simd_utf8::validate(&self.partial_utf8[..self.partial_utf8_len]) {
            // If the entire buffer is valid, use the first character and continue parsing.
            Ok(parsed) => {
                // SAFETY: `partial_utf8_len >= 1` (caller guarantee) and `parsed`
                // is the validated UTF-8 view of those bytes, so it has at least
                // one character.
                let c = unsafe { parsed.chars().next().unwrap_unchecked() };
                // A completed character has the same control semantics as one
                // decoded in a bulk run, including UTF-8-encoded C1 controls.
                Self::dispatch_codepoints(performer, &[c as u32]);

                self.partial_utf8_len = 0;
                c.len_utf8() - old_bytes
            }
            Err(err) => {
                let valid_bytes = err.valid_up_to();

                // If we have any valid bytes, that means we partially copied another
                // utf8 character into `partial_utf8`. Since we only care about the
                // first character, we just ignore the rest.
                if valid_bytes > 0 {
                    // SAFETY: `valid_bytes > 0` and the slice up to `valid_bytes` was
                    // reported as valid UTF-8, so it contains at least one full
                    // character.
                    let c = unsafe {
                        let parsed =
                            str::from_utf8_unchecked(&self.partial_utf8[..valid_bytes]);
                        parsed.chars().next().unwrap_unchecked()
                    };

                    Self::dispatch_codepoints(performer, &[c as u32]);

                    self.partial_utf8_len = 0;
                    return c.len_utf8() - old_bytes;
                }

                match err.error_len() {
                    // If the partial character was also invalid, emit the replacement
                    // character.
                    Some(invalid_len) => {
                        performer.print('�');

                        self.partial_utf8_len = 0;
                        invalid_len - old_bytes
                    }
                    // If the character still isn't complete, wait for more data.
                    None => to_copy,
                }
            }
        }
    }

    /// Handle ground dispatch of print/execute for a (possibly invalid)
    /// UTF-8 byte slice.
    ///
    /// Three batched paths:
    /// - ASCII printable runs (`0x20..=0x7E`) → one [`Perform::print_str`].
    /// - ASCII control bytes (`0x00..=0x1F`, `0x7F`) → [`Perform::execute`].
    /// - Runs containing multi-byte UTF-8 → SIMD-decoded in chunks via
    ///   `simdutf::convert_utf8_to_utf32_with_errors` into
    ///   [`Self::decode_buf`] with inline Maximal-Subpart U+FFFD
    ///   replacement, then dispatched as [`Perform::print_codepoints`]
    ///   runs (controls split out as individual `execute` in codepoint
    ///   space).
    #[inline]
    fn ground_dispatch<P: Perform>(&mut self, performer: &mut P, bytes: &[u8]) {
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];

            if (0x20..=0x7E).contains(&b) {
                let end = i + find_non_printable(&bytes[i..]);
                // SAFETY: every byte in 0x20..=0x7E is valid 1-byte UTF-8.
                let chunk = unsafe { std::str::from_utf8_unchecked(&bytes[i..end]) };
                performer.print_str(chunk);
                i = end;
            } else if b < 0x80 {
                performer.execute(b);
                i += 1;
            } else {
                // Multi-byte UTF-8 run. ASCII decodes to itself (ESC never
                // reaches here), so one simdutf call covers a mixed chunk;
                // controls split out in codepoint space after decode.
                let mut end = (i + DECODE_CHUNK).min(bytes.len());
                if end < bytes.len() {
                    let start = end;
                    while end > i && (bytes[end] & 0xC0) == 0x80 {
                        end -= 1;
                    }
                    if end == i {
                        end = start;
                    }
                }
                self.decode_codepoints(&bytes[i..end]);
                Self::dispatch_codepoints(performer, &self.decode_buf);
                i = end;
            }
        }
    }

    /// Scalar transcode. Used for every input on wasm, where the
    /// C++-backed `simdutf` cannot build, and for short runs everywhere
    /// else, where the out-of-line simdutf call costs more than the
    /// decode itself. Same contract as the SIMD path below: each invalid
    /// maximal subpart becomes one U+FFFD, including lone C1 bytes.
    #[inline]
    fn decode_codepoints_scalar(&mut self, src: &[u8]) {
        self.decode_buf.clear();
        self.decode_buf.reserve(src.len());
        self.append_codepoints_scalar(src);
    }

    #[inline]
    fn append_codepoints_scalar(&mut self, src: &[u8]) {
        let mut consumed = 0;
        while consumed < src.len() {
            match std::str::from_utf8(&src[consumed..]) {
                Ok(valid) => {
                    self.decode_buf.extend(valid.chars().map(|c| c as u32));
                    return;
                }
                Err(err) => {
                    let end = consumed + err.valid_up_to();
                    // SAFETY: the validator just confirmed this prefix.
                    let valid =
                        unsafe { std::str::from_utf8_unchecked(&src[consumed..end]) };
                    self.decode_buf.extend(valid.chars().map(|c| c as u32));

                    let subpart = maximal_subpart(&src[end..]);
                    self.decode_buf.push(0xFFFD);
                    consumed = end + subpart;
                }
            }
        }
    }

    /// Transcode a UTF-8 byte slice into [`Self::decode_buf`] as `u32`
    /// codepoints. SGR-dense streams produce a ground run of one glyph
    /// between escapes, where the FFI call into simdutf costs more than
    /// decoding in place, so short runs (and all of wasm, where simdutf
    /// cannot build) go scalar.
    #[inline]
    fn decode_codepoints(&mut self, src: &[u8]) {
        #[cfg(not(target_arch = "wasm32"))]
        if src.len() >= SIMD_DECODE_MIN {
            return self.decode_codepoints_simd(src);
        }
        self.decode_codepoints_scalar(src)
    }

    /// SIMD-transcode a UTF-8 byte slice into [`Self::decode_buf`] as `u32`
    /// codepoints, replacing each invalid UTF-8 maximal subpart with one
    /// U+FFFD inline (W3C/Unicode "Substitution of Maximal Subparts").
    #[cfg(not(target_arch = "wasm32"))]
    #[inline]
    fn decode_codepoints_simd(&mut self, src: &[u8]) {
        if let Err(err) = crate::simd_utf8::decode_to_utf32(src, &mut self.decode_buf) {
            // Dense malformed input otherwise re-enters the SIMD dispatcher
            // once per invalid byte. Decode the rest of this bounded chunk
            // scalarly, retaining maximal-subpart replacement.
            self.append_codepoints_scalar(&src[err.valid_up_to()..]);
        }
    }

    /// Split a decoded codepoint slice on control codepoints (C0
    /// `U+0000..=U+001F`, DEL, and C1 `U+0080..=U+009F`) and emit
    /// non-control runs as [`Perform::print_codepoints`], C0/DEL as
    /// [`Perform::execute`], and ignoring UTF-8-decoded C1 controls.
    #[inline]
    fn dispatch_codepoints<P: Perform>(performer: &mut P, codepoints: &[u32]) {
        let mut start = 0;
        for (i, &cp) in codepoints.iter().enumerate() {
            if cp < 0x20 || cp == 0x7F || (0x80..=0x9F).contains(&cp) {
                if start < i {
                    performer.print_codepoints(&codepoints[start..i]);
                }
                if cp < 0x80 {
                    performer.execute(cp as u8);
                }
                start = i + 1;
            }
        }
        if start < codepoints.len() {
            performer.print_codepoints(&codepoints[start..]);
        }
    }
}

/// Position of the first byte outside printable ASCII (`0x20..=0x7E`),
/// via AVX2/SSE2 on x86-64 and a portable SWAR scan elsewhere. An
/// early-exit iterator scan does not vectorize; this does the range
/// test on a whole word at once.
#[inline]
fn find_non_printable(bytes: &[u8]) -> usize {
    #[cfg(target_arch = "x86_64")]
    if bytes.len() >= 128 && std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: runtime detection guards the AVX2 implementation.
        return unsafe { find_boundary_avx2::<false>(bytes) };
    }
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;

    let mut i = 0;
    let len = bytes.len();
    #[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
    {
        use std::arch::x86_64::{
            _mm_cmpeq_epi8, _mm_cmpgt_epi8, _mm_loadu_si128, _mm_movemask_epi8,
            _mm_or_si128, _mm_set1_epi8,
        };
        // SSE2 is part of the x86-64 baseline. Signed bytes below space also
        // include every non-ASCII byte; DEL is the only other stop value.
        // Each unaligned load stays inside the input slice, including tails.
        unsafe {
            let space = _mm_set1_epi8(0x20);
            let delete = _mm_set1_epi8(0x7f);
            while i + 16 <= len {
                let word = _mm_loadu_si128(bytes.as_ptr().add(i).cast());
                let stop = _mm_movemask_epi8(_mm_or_si128(
                    _mm_cmpgt_epi8(space, word),
                    _mm_cmpeq_epi8(delete, word),
                )) as u32;
                if stop != 0 {
                    return i + stop.trailing_zeros() as usize;
                }
                i += 16;
            }
        }
    }
    while i + 8 <= len {
        let w = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let lt20 = w.wrapping_sub(LO * 0x20) & !w & HI;
        let ge80 = w & HI;
        let x = w ^ (LO * 0x7F);
        let eq7f = x.wrapping_sub(LO) & !x & HI;
        let stop = lt20 | ge80 | eq7f;
        if stop != 0 {
            return i + (stop.trailing_zeros() as usize) / 8;
        }
        i += 8;
    }
    while i < len {
        if !(0x20..=0x7E).contains(&bytes[i]) {
            return i;
        }
        i += 1;
    }
    len
}

/// First index whose byte ends an OSC payload run: any C0 byte
/// (terminator, abort, or ignored control) or the `;` param separator.
/// Same SWAR shape as `find_non_printable`: 8 bytes per iteration,
/// first-position exact (a borrow can only misflag a byte that sits
/// after an already-flagged one).
fn find_osc_boundary(bytes: &[u8]) -> usize {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;

    let mut i = 0;
    let len = bytes.len();
    while i + 8 <= len {
        let w = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let lt20 = w.wrapping_sub(LO * 0x20) & !w & HI;
        let x = w ^ (LO * 0x3B);
        let eq3b = x.wrapping_sub(LO) & !x & HI;
        let stop = lt20 | eq3b;
        if stop != 0 {
            return i + (stop.trailing_zeros() as usize) / 8;
        }
        i += 8;
    }
    while i < len {
        if bytes[i] < 0x20 || bytes[i] == 0x3B {
            return i;
        }
        i += 1;
    }
    len
}

/// First index whose byte ends an APC/SOS/PM payload run: any C0 or high
/// byte. C0 payload bytes and C1 transitions use the per-byte handler.
fn find_opaque_boundary(bytes: &[u8]) -> usize {
    #[cfg(target_arch = "x86_64")]
    if bytes.len() >= 128 && std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: runtime detection guards the AVX2 implementation.
        return unsafe { find_boundary_avx2::<true>(bytes) };
    }
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;

    let mut i = 0;
    let len = bytes.len();
    #[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
    {
        use std::arch::x86_64::*;

        // Signed bytes below space include both C0 and high bytes. DEL is
        // valid opaque payload, unlike in printable ground-state runs.
        // SAFETY: SSE2 is enabled and each unaligned load stays in the slice.
        unsafe {
            let space = _mm_set1_epi8(0x20);
            while i + 16 <= len {
                let word = _mm_loadu_si128(bytes.as_ptr().add(i).cast());
                let stop = _mm_cmpgt_epi8(space, word);
                let mask = _mm_movemask_epi8(stop) as u32;
                if mask != 0 {
                    return i + mask.trailing_zeros() as usize;
                }
                i += 16;
            }
        }
    }
    while i + 8 <= len {
        let w = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let lt20 = (w.wrapping_sub(LO * 0x20) & !w & HI) | (w & HI);
        if lt20 != 0 {
            return i + (lt20.trailing_zeros() as usize) / 8;
        }
        i += 8;
    }
    while i < len {
        if bytes[i] < 0x20 || bytes[i] >= 0x80 {
            return i;
        }
        i += 1;
    }
    len
}

// Dispatch only long runs here: shorter ones retain the inlined SSE2 path
// rather than paying for a feature check, function call and AVX transition.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn find_boundary_avx2<const ALLOW_DEL: bool>(bytes: &[u8]) -> usize {
    use std::arch::x86_64::*;

    let mut i = 0;
    let space = _mm256_set1_epi8(0x20);
    let del = _mm256_set1_epi8(0x7f);
    while i + 32 <= bytes.len() {
        // SAFETY: the loop bounds every unaligned load to the input slice.
        let word = unsafe { _mm256_loadu_si256(bytes.as_ptr().add(i).cast()) };
        let stop = if ALLOW_DEL {
            _mm256_cmpgt_epi8(space, word)
        } else {
            _mm256_or_si256(_mm256_cmpgt_epi8(space, word), _mm256_cmpeq_epi8(word, del))
        };
        let mask = _mm256_movemask_epi8(stop) as u32;
        if mask != 0 {
            return i + mask.trailing_zeros() as usize;
        }
        i += 32;
    }
    // At most 31 bytes remain, so the dispatch threshold cannot recurse.
    i + if ALLOW_DEL {
        find_opaque_boundary(&bytes[i..])
    } else {
        find_non_printable(&bytes[i..])
    }
}

/// First index whose byte ends a DCS passthrough run. The put class is
/// `0x00..=0x17 | 0x19 | 0x1C..=0x7E | 0x80..=0xFF`; everything else
/// (CAN, SUB, ESC, DEL) is a terminator or ignored and goes
/// through the per-byte state machine.
fn find_dcs_boundary(bytes: &[u8]) -> usize {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;

    #[inline(always)]
    fn eq(w: u64, b: u8) -> u64 {
        const LO: u64 = 0x0101_0101_0101_0101;
        const HI: u64 = 0x8080_8080_8080_8080;
        let x = w ^ (LO * b as u64);
        x.wrapping_sub(LO) & !x & HI
    }

    let mut i = 0;
    let len = bytes.len();
    while i + 8 <= len {
        let w = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let x = w ^ (LO * 0x7F);
        let eq7f = x.wrapping_sub(LO) & !x & HI;
        let stop = eq7f | eq(w, 0x18) | eq(w, 0x1A) | eq(w, 0x1B);
        if stop != 0 {
            return i + (stop.trailing_zeros() as usize) / 8;
        }
        i += 8;
    }
    while i < len {
        if matches!(bytes[i], 0x18 | 0x1A | 0x1B | 0x7F) {
            return i;
        }
        i += 1;
    }
    len
}

/// Byte cap per decode chunk in `ground_dispatch`, bounding `decode_buf`
/// growth. Chunks never split a UTF-8 sequence.
const DECODE_CHUNK: usize = 4096;

/// Runs shorter than this decode scalar instead of through simdutf.
#[cfg(not(target_arch = "wasm32"))]
const SIMD_DECODE_MIN: usize = 16;

/// Length of the maximal valid subpart of a UTF-8 sequence starting at
/// `p[0]`, per Unicode Table 3-7 / W3C "U+FFFD Substitution of Maximal
/// Subparts". Each maximal subpart maps to exactly one U+FFFD when
/// reporting invalid input.
fn maximal_subpart(p: &[u8]) -> usize {
    if p.is_empty() {
        return 0;
    }
    let b0 = p[0];

    // Continuation bytes, overlong leads, or invalid leads: each is its
    // own 1-byte maximal subpart.
    if !(0xC2..=0xF4).contains(&b0) {
        return 1;
    }

    // Determine expected sequence length and per-byte continuation ranges
    // from the lead byte (Unicode Table 3-7).
    let (seq_len, lo, hi): (usize, [u8; 3], [u8; 3]) = match b0 {
        0xC2..=0xDF => (2, [0x80, 0, 0], [0xBF, 0, 0]),
        0xE0 => (3, [0xA0, 0x80, 0], [0xBF, 0xBF, 0]),
        0xE1..=0xEC => (3, [0x80, 0x80, 0], [0xBF, 0xBF, 0]),
        0xED => (3, [0x80, 0x80, 0], [0x9F, 0xBF, 0]),
        0xEE..=0xEF => (3, [0x80, 0x80, 0], [0xBF, 0xBF, 0]),
        0xF0 => (4, [0x90, 0x80, 0x80], [0xBF, 0xBF, 0xBF]),
        0xF1..=0xF3 => (4, [0x80, 0x80, 0x80], [0xBF, 0xBF, 0xBF]),
        0xF4 => (4, [0x80, 0x80, 0x80], [0x8F, 0xBF, 0xBF]),
        _ => unreachable!(),
    };

    let mut valid = 1;
    for i in 0..seq_len - 1 {
        if valid >= p.len() {
            break;
        }
        let cb = p[valid];
        if cb < lo[i] || cb > hi[i] {
            break;
        }
        valid += 1;
    }
    valid
}

/// Length of `input` excluding any trailing valid-so-far UTF-8 sequence.
/// Bytes that form an invalid lead (`<C2` or `>F4`) at the tail are NOT
/// trimmed — they're left in place so the MSP-aware decode loop can
/// replace them with U+FFFD.
fn trim_valid_partial_utf8(input: &[u8]) -> usize {
    if input.is_empty() {
        return 0;
    }

    // A partial trailing sequence is at most 4 bytes (max UTF-8 length).
    let check_start = if input.len() > 4 { input.len() - 4 } else { 0 };
    let mut pos = input.len();
    while pos > check_start {
        let b = input[pos - 1];

        // Skip continuation bytes — they could belong to a partial sequence.
        if (b & 0xC0) == 0x80 {
            pos -= 1;
            continue;
        }

        // Found a non-continuation byte. Only a valid multi-byte lead
        // (C2-F4) can start a trim-worthy partial sequence.
        if !(0xC2..=0xF4).contains(&b) {
            return input.len();
        }

        let expected = if b <= 0xDF {
            2
        } else if b <= 0xEF {
            3
        } else {
            4
        };

        let seq_remaining = input.len() - (pos - 1);
        if seq_remaining >= expected {
            // Sequence is structurally complete — let the decoder handle it.
            return input.len();
        }

        let seq_start = pos - 1;
        let subpart = maximal_subpart(&input[seq_start..]);

        if subpart == seq_remaining {
            // All trailing bytes are part of a valid prefix → trim them.
            return seq_start;
        }

        // Sequence is ill-formed; let the MSP-aware decoder replace it.
        return input.len();
    }

    input.len()
}

#[derive(PartialEq, Eq, Debug, Default, Copy, Clone)]
enum State {
    CsiEntry,
    CsiIgnore,
    CsiIntermediate,
    CsiParam,
    DcsEntry,
    DcsIgnore,
    DcsIntermediate,
    DcsParam,
    DcsPassthrough,
    Escape,
    EscapeIntermediate,
    OscString,
    SosString,
    ApcString,
    PmString,
    #[default]
    Ground,
}

/// Performs actions requested by the [`Parser`].
///
/// Dispatch trait for [`Parser`] actions. The production implementer is
/// [`super::handler::Performer`]; external consumers can implement it to
/// drive their own byte-stream processing (only `print`, `execute`, and the
/// dispatch methods they care about — everything has a no-op default).
///
/// The methods correspond to actions described in
/// <http://vt100.net/emu/dec_ansi_parser>.
pub trait Perform {
    /// Cancel an APC without applying its partially accumulated command.
    /// The default retains the historical CAN marker for existing consumers.
    fn apc_abort(&mut self) {
        self.apc_put(0x18);
        self.apc_end();
    }
    /// A bounded OSC accumulator discarded a packet. Transactional consumers
    /// must abort staging rather than later publishing partial contents.
    fn osc_discarded(&mut self, _command: Option<u16>, _bell_terminated: bool) {}
    /// Draw a character to the screen and update states.
    fn print(&mut self, _c: char) {}

    /// Draw a contiguous run of characters in one batch. The default
    /// implementation iterates and calls [`Perform::print`]; implementers
    /// can specialize to skip per-char trait dispatch and per-char width
    /// lookup when the run is known to be all narrow / printable / non-
    /// combining (e.g. ASCII).
    fn print_str(&mut self, s: &str) {
        for c in s.chars() {
            self.print(c);
        }
    }

    /// Draw a contiguous run of pre-decoded Unicode codepoints in one batch.
    /// The default implementation iterates, converts each `u32` to `char`
    /// (replacing invalid encodings with U+FFFD), and calls
    /// [`Perform::print`]. Implementers can specialize to bulk-process
    /// codepoints — e.g. SIMD width lookup + bulk cell write.
    fn print_codepoints(&mut self, codepoints: &[u32]) {
        for &cp in codepoints {
            let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
            self.print(c);
        }
    }

    /// Execute a C0 or C1 control function.
    fn execute(&mut self, _byte: u8) {}

    /// Invoked when a final character arrives in first part of device control
    /// string.
    ///
    /// The control function should be determined from the private marker, final
    /// character, and execute with a parameter list. A handler should be
    /// selected for remaining characters in the string; the handler
    /// function should subsequently be called by `put` for every character in
    /// the control string.
    ///
    /// The `ignore` flag indicates that more than two intermediates arrived and
    /// subsequent characters were ignored.
    fn hook(
        &mut self,
        _params: &Params,
        _intermediates: &[u8],
        _ignore: bool,
        _action: char,
    ) {
    }

    /// Pass bytes as part of a device control string to the handle chosen in
    /// `hook`. C0 controls will also be passed to the handler.
    fn put(&mut self, _byte: u8) {}

    /// Bulk form of [`put`](Perform::put): a DCS passthrough span with
    /// no terminator or ignored bytes. The default loops over the
    /// per-byte hook; sinks whose per-byte handling carries cross-byte
    /// state (the sixel error path) should keep it.
    fn put_slice(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.put(byte);
        }
    }

    /// Called when a device control string is terminated.
    ///
    /// The previously selected handler should be notified that the DCS has
    /// terminated.
    fn unhook(&mut self) {}

    /// Dispatch an operating system command.
    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}

    /// A final character has arrived for a CSI sequence
    ///
    /// The `ignore` flag indicates that either more than two intermediates
    /// arrived or the number of parameters exceeded the maximum supported
    /// length, and subsequent characters were ignored.
    fn csi_dispatch(
        &mut self,
        _params: &Params,
        _intermediates: &[u8],
        _ignore: bool,
        _action: char,
    ) {
    }

    /// The final character of an escape sequence has arrived.
    ///
    /// The `ignore` flag indicates that more than two intermediates arrived and
    /// subsequent characters were ignored.
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}

    /// Invoked when the beginning of a new SOS (Start of String) sequence is
    /// encountered.
    fn sos_start(&mut self) {}

    /// Invoked for every valid byte (0x20-0xFF) in a SOS (Start of String)
    /// sequence.
    fn sos_put(&mut self, _byte: u8) {}

    /// Bulk form of [`sos_put`](Perform::sos_put): a payload span with
    /// no control bytes. The default loops over the per-byte hook.
    fn sos_put_slice(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.sos_put(byte);
        }
    }

    /// Invoked when the end of an SOS (Start of String) sequence is
    /// encountered.
    fn sos_end(&mut self) {}

    /// Invoked when the beginning of a new PM (Privacy Message) sequence is
    /// encountered.
    fn pm_start(&mut self) {}

    /// Invoked for every valid byte (0x20-0xFF) in a PM (Privacy Message)
    /// sequence.
    fn pm_put(&mut self, _byte: u8) {}

    /// Bulk form of [`pm_put`](Perform::pm_put).
    fn pm_put_slice(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.pm_put(byte);
        }
    }

    /// Invoked when the end of a PM (Privacy Message) sequence is encountered.
    fn pm_end(&mut self) {}

    /// Invoked when the beginning of a new APC (Application Program Command)
    /// sequence is encountered.
    fn apc_start(&mut self) {}

    /// Invoked for every valid byte (0x20-0xFF) in an APC (Application Program
    /// Command) sequence.
    fn apc_put(&mut self, _byte: u8) {}

    /// Bulk form of [`apc_put`](Perform::apc_put): a payload span with
    /// no control bytes. Kitty graphics stream megabytes through APC,
    /// so implementors should append the whole slice.
    fn apc_put_slice(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.apc_put(byte);
        }
    }
    /// Invoked when the end of an APC (Application Program Command) sequence is
    /// encountered.
    fn apc_end(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_scanners_check_every_byte_at_unaligned_boundaries() {
        for offset in 0..32 {
            for len in [
                0, 1, 7, 8, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129,
            ] {
                let mut bytes = vec![b'x'; offset + len];
                assert_eq!(find_non_printable(&bytes[offset..]), len);
                assert_eq!(find_opaque_boundary(&bytes[offset..]), len);
                for position in 0..len {
                    for value in 0..=255u8 {
                        bytes[offset + position] = value;
                        let expected = if (0x20..=0x7e).contains(&value) {
                            len
                        } else {
                            position
                        };
                        assert_eq!(
                            find_non_printable(&bytes[offset..]), expected,
                            "offset {offset}, length {len}, position {position}, byte {value:02x}"
                        );
                        assert_eq!(
                            find_opaque_boundary(&bytes[offset..]),
                            if (0x20..=0x7f).contains(&value) { len } else { position },
                            "C0 offset {offset}, length {len}, position {position}, byte {value:02x}"
                        );
                        assert_eq!(find_dcs_boundary(&bytes[offset..]), if matches!(value, 0x18 | 0x1a | 0x1b | 0x7f) { position } else { len }, "DCS offset {offset}, length {len}, position {position}, byte {value:02x}");
                    }
                    bytes[offset + position] = b'x';
                }
            }
        }
    }

    #[test]
    fn program_status_accumulation_is_bounded_and_recovers_after_oversize() {
        for chunk_size in [1, 17, 8192] {
            let mut parser = Parser::new();
            let mut dispatcher = Dispatcher::default();
            parser.advance(&mut dispatcher, b"\x1b]7501;");
            for chunk in vec![b'a'; 16 * 1024].chunks(chunk_size) {
                parser.advance(&mut dispatcher, chunk);
                assert!(
                    parser.osc_raw.len() <= crate::program_status::MAX_SEQUENCE_BYTES
                );
            }
            assert!(parser.osc_discarded);
            parser.advance(&mut dispatcher, b"\x07");
            assert!(dispatcher.dispatched.is_empty());
            parser.advance(&mut dispatcher, b"\x1b]7501;state=done\x07");
            assert_eq!(dispatcher.dispatched.len(), 1);
        }
    }

    #[test]
    fn oversized_osc_is_discarded_until_termination_across_read_boundaries() {
        for header in [b"\x1b]0;".as_slice(), b"\x1b]8;;", b"\x1b]"] {
            for terminator in [b"\x07".as_slice(), b"\x1b\\", b"\x18", b"\x1a"] {
                for chunk_size in [1, 17, 8192, MAX_CONTROL_BYTES + 1] {
                    let mut parser = Parser::new();
                    let mut dispatcher = Dispatcher::default();
                    parser.advance(&mut dispatcher, header);
                    for chunk in vec![b'x'; MAX_CONTROL_BYTES + 1].chunks(chunk_size) {
                        parser.advance(&mut dispatcher, chunk);
                        assert!(parser.osc_raw.len() <= MAX_CONTROL_BYTES);
                    }
                    assert!(parser.osc_discarded);
                    // Separators following a discarded verbatim OSC must not index
                    // the buffer that was just released or dispatch a partial command.
                    parser.advance(&mut dispatcher, b";8;;http://example.com;tail");
                    assert!(dispatcher.dispatched.is_empty());
                    parser.advance(&mut dispatcher, terminator);
                    dispatcher.dispatched.clear();
                    parser.advance(&mut dispatcher, b"ok\x1b]0;title\x07");
                    assert_eq!(
                        dispatcher.dispatched,
                        vec![
                            Sequence::Print('o'),
                            Sequence::Print('k'),
                            Sequence::Osc(vec![b"0".to_vec(), b"title".to_vec()], true),
                        ]
                    );
                }
            }
        }
    }

    #[test]
    fn osc_limits_preserve_clipboard_and_image_payloads_and_release_large_buffers() {
        for command in [b"52;c;".as_slice(), b"1337;File=:"] {
            let mut parser = Parser::new();
            let mut dispatcher = Dispatcher::default();
            parser.advance(&mut dispatcher, b"\x1b]");
            parser.advance(&mut dispatcher, command);
            parser.advance(&mut dispatcher, &vec![b'a'; 128 * 1024]);
            assert!(!parser.osc_discarded);
            assert!(parser.osc_raw.overflow.capacity() > MAX_CONTROL_BYTES);
            parser.advance(&mut dispatcher, b"\x07");
            assert_eq!(dispatcher.dispatched.len(), 1);
            assert_eq!(parser.osc_raw.overflow.capacity(), 0);
        }

        let mut parser = Parser::new();
        let mut dispatcher = Dispatcher::default();
        parser.advance(&mut dispatcher, b"\x1b]52;c;");
        let prefix = parser.osc_raw.len();
        parser.advance(&mut dispatcher, &vec![b'a'; MAX_CLIPBOARD_BYTES - prefix]);
        assert_eq!(parser.osc_raw.len(), MAX_CLIPBOARD_BYTES);
        assert!(!parser.osc_discarded);
        parser.advance(&mut dispatcher, b"a");
        assert!(parser.osc_discarded);
        assert_eq!(parser.osc_raw.overflow.capacity(), 0);
        parser.advance(&mut dispatcher, b"\x07");
        assert!(dispatcher.dispatched.is_empty());
    }

    /// The scalar and simdutf decoders implement the maximal-subpart /
    /// U+FFFD contract independently; with short runs routed
    /// scalar, only inputs >= SIMD_DECODE_MIN reach the simdutf error
    /// loop. Feed the same invalid corpus through both, below and above
    /// the threshold, and require identical output.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn scalar_and_simd_decode_agree() {
        let invalid_cases: &[&[u8]] = &[
            b"\x80",                     // lone continuation (C1: U+FFFD)
            b"\xBF",                     // lone continuation (non-C1: U+FFFD)
            b"\xE2\x96",                 // truncated 3-byte sequence
            b"\xF0\x9F\x92",             // truncated 4-byte sequence
            b"\xC0\xAF",                 // overlong encoding
            b"\xED\xA0\x80",             // UTF-16 surrogate
            b"\xF5\x80\x80\x80",         // above U+10FFFF
            b"a\xE2\x96b\x80c",          // interleaved with ASCII
            "▀\u{45}\u{300}".as_bytes(), // valid multibyte control
        ];
        for case in invalid_cases {
            for pad in [0usize, SIMD_DECODE_MIN + 4] {
                let mut input = case.to_vec();
                input.extend(vec![b'x'; pad]);

                let mut scalar = Parser::new();
                scalar.decode_codepoints_scalar(&input);
                let mut simd = Parser::new();
                simd.decode_codepoints_simd(&input);
                assert_eq!(
                    scalar.decode_buf, simd.decode_buf,
                    "decoder divergence on {case:?} pad {pad}",
                );
            }
        }
    }

    #[test]
    fn malformed_utf8_dispatch_does_not_depend_on_read_boundaries() {
        for invalid in [
            b"\xed\xa0\x80".as_slice(),
            b"\xe0\x80\x80",
            b"\xf0\x80\x80\x80",
            b"\xf4\x90\x80\x80",
            b"\xe2(\xa1",
            b"\xc0\xaf",
        ] {
            let input =
                [b"prefix ".as_slice(), invalid, b" suffix\x1b[31mred\x1b[0m"].concat();
            let mut expected = Dispatcher::default();
            Parser::default().advance(&mut expected, &input);
            for chunk_size in 1..=input.len() {
                let mut actual = Dispatcher::default();
                let mut parser = Parser::default();
                for chunk in input.chunks(chunk_size) {
                    parser.advance(&mut actual, chunk);
                }
                assert_eq!(
                    actual.dispatched, expected.dispatched,
                    "{invalid:02x?}, chunk {chunk_size}"
                );
            }
        }
    }

    #[test]
    fn all_byte_pairs_dispatch_identically_across_read_boundaries() {
        // Exercise both the bulk decoder and the carried UTF-8 path. A valid
        // suffix completes truncated input and verifies parser recovery, so
        // matching an empty output cannot hide a stuck sequence.
        for first in 0..=255u8 {
            for second in 0..=255u8 {
                let mut input = vec![b'x'; SIMD_DECODE_MIN + 4];
                input.extend_from_slice(&[first, second]);
                input.extend_from_slice(b"\x18\x1b[0mend");
                let mut expected = Dispatcher::default();
                Parser::new().advance(&mut expected, &input);
                for split in SIMD_DECODE_MIN + 4..=SIMD_DECODE_MIN + 6 {
                    let mut actual = Dispatcher::default();
                    let mut parser = Parser::new();
                    parser.advance(&mut actual, &input[..split]);
                    parser.advance(&mut actual, &input[split..]);
                    assert_eq!(
                        actual.dispatched, expected.dispatched,
                        "bytes {first:02x} {second:02x}, split {split}"
                    );
                }
            }
        }
    }

    #[test]
    fn encoded_c1_controls_are_ignored_across_read_boundaries() {
        for control in 0x80..=0x9f {
            // Test both the valid-buffer and valid-prefix branches in the
            // carried decoder, using an invalid byte after the completed C1.
            for suffix in [b"".as_slice(), b"\xff"] {
                let mut parser = Parser::new();
                let mut dispatcher = Dispatcher::default();
                parser.advance(&mut dispatcher, &[0xc2]);
                parser.advance(&mut dispatcher, &[&[control], suffix].concat());
                let expected = if suffix.is_empty() {
                    vec![]
                } else {
                    vec![Sequence::Print('\u{fffd}')]
                };
                assert_eq!(dispatcher.dispatched, expected);
            }
        }
    }

    #[test]
    fn raw_c1_ground_bytes_are_invalid_utf8() {
        // Ghostty stream.zig: ground-state UTF-8-decoded C1 controls are
        // ignored, whereas raw C1 bytes produce replacement characters.
        for control in 0x80..=0x9f {
            for pad in [0, SIMD_DECODE_MIN + 4, DECODE_CHUNK] {
                let mut input = "Ü".as_bytes().to_vec();
                input.extend(vec![b'x'; pad]);
                input.extend_from_slice(&[control, b'a']);
                input.extend(vec![b'x'; pad]);
                let mut expected = vec![Sequence::Print('Ü')];
                expected.extend(vec![Sequence::Print('x'); pad]);
                expected.extend([Sequence::Print('\u{fffd}'), Sequence::Print('a')]);
                expected.extend(vec![Sequence::Print('x'); pad]);
                for chunk_size in [1, 17, input.len()] {
                    let mut parser = Parser::new();
                    let mut dispatcher = Dispatcher::default();
                    for chunk in input.chunks(chunk_size) {
                        parser.advance(&mut dispatcher, chunk);
                    }
                    assert_eq!(
                        dispatcher.dispatched, expected,
                        "raw {control:02x}, pad {pad}, chunk {chunk_size}"
                    );
                }
            }
        }
    }

    #[test]
    fn dcs_high_bytes_are_payload_until_seven_bit_terminator() {
        // Ghostty parse_table.zig: DCS passthrough high bytes are payload,
        // including C1-range UTF-8 continuations such as the 9C in Ü.
        let payload: Vec<u8> = (0x80..=0xff).collect();
        let input = [b"\x1bPq".as_slice(), &payload, b"\x1b\\Z"].concat();
        for chunk_size in [1, 2, 7, 16, input.len()] {
            let mut parser = Parser::new();
            let mut dispatcher = Dispatcher::default();
            for chunk in input.chunks(chunk_size) {
                parser.advance(&mut dispatcher, chunk);
            }
            let actual: Vec<u8> = dispatcher
                .dispatched
                .iter()
                .filter_map(|s| {
                    if let Sequence::DcsPut(byte) = s {
                        Some(*byte)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(actual, payload, "payload with chunk size {chunk_size}");
            assert_eq!(
                dispatcher
                    .dispatched
                    .iter()
                    .filter(|s| matches!(s, Sequence::DcsUnhook))
                    .count(),
                1
            );
            assert_eq!(dispatcher.dispatched.last(), Some(&Sequence::Print('Z')));
        }
    }

    #[test]
    fn c1_transitions_match_ghostty_non_ground_state_table() {
        for state in [
            State::Escape,
            State::EscapeIntermediate,
            State::CsiEntry,
            State::CsiParam,
            State::CsiIntermediate,
            State::CsiIgnore,
            State::DcsEntry,
            State::DcsParam,
            State::DcsIntermediate,
            State::DcsIgnore,
            State::DcsPassthrough,
            State::OscString,
            State::ApcString,
            State::SosString,
            State::PmString,
        ] {
            for byte in 0x80..=0x9f {
                let expected = if matches!(
                    state,
                    State::DcsIgnore | State::DcsPassthrough | State::OscString
                ) {
                    state
                } else {
                    match byte {
                        0x90 => State::DcsEntry,
                        0x98 => State::SosString,
                        0x9b => State::CsiEntry,
                        0x9d => State::OscString,
                        0x9e => State::PmString,
                        0x9f => State::ApcString,
                        _ => State::Ground,
                    }
                };
                let mut parser = Parser::new();
                parser.state = state;
                let mut dispatcher = Dispatcher::default();
                parser.advance(&mut dispatcher, &[byte]);
                assert_eq!(parser.state, expected, "{state:?}, C1 {byte:02x}");
            }
        }
    }

    #[test]
    fn dcs_subparameters_discard_the_string_without_hooking() {
        for header in [":", "1:", "1:2", "1;:", "?1:2"] {
            let input = [
                b"\x1bP".as_slice(),
                header.as_bytes(),
                b"qhidden\x9b31m\x1b\\Z",
            ]
            .concat();
            for split in 0..=input.len() {
                let mut parser = Parser::new();
                let mut dispatcher = Dispatcher::default();
                parser.advance(&mut dispatcher, &input[..split]);
                parser.advance(&mut dispatcher, &input[split..]);
                assert_eq!(
                    dispatcher.dispatched,
                    [Sequence::Esc(vec![], false, b'\\'), Sequence::Print('Z')],
                    "header {header}, split {split}"
                );
            }
        }
    }

    #[test]
    fn encoded_c1_controls_are_ignored_in_bulk_utf8_runs() {
        for control in 0x80..=0x9f {
            for pad in [0, SIMD_DECODE_MIN + 4, DECODE_CHUNK] {
                let mut input = "Ü".as_bytes().to_vec();
                input.extend(vec![b'x'; pad]);
                input.extend([0xc2, control]);
                input.extend(vec![b'x'; pad]);
                input.extend("\u{a0}".as_bytes());
                let mut expected = vec![Sequence::Print('Ü')];
                expected.extend(vec![Sequence::Print('x'); pad * 2]);
                expected.push(Sequence::Print('\u{a0}'));
                for chunk_size in [1, 17, input.len()] {
                    let mut parser = Parser::new();
                    let mut dispatcher = Dispatcher::default();
                    for chunk in input.chunks(chunk_size) {
                        parser.advance(&mut dispatcher, chunk);
                    }
                    assert_eq!(
                        dispatcher.dispatched, expected,
                        "encoded {control:02x}, pad {pad}, chunk {chunk_size}"
                    );
                }
            }
        }
    }

    #[test]
    fn protocol_dispatch_does_not_depend_on_read_boundaries() {
        let fragments: &[&[u8]] = &[
            b"plain text\r\n",
            "é👩\u{200d}💻".as_bytes(),
            b"\xc2\x80\xc2\x9f",
            b"\xf0\x9f\x92",
            b"\xed\xa0\x80\xe0\x80\xbf",
            b"\x1b[38:2::255:128:0mcolor\x1b[0m",
            b"\x1b]8;id=test;https://example.test\x1b\\link\x1b]8;;\x07",
            b"\x1b]7501;state=running\x07",
            b"\x1bP1;2$qpayload\x1b\\",
            b"\x1b_Ga=T,f=32;YWJj\x1b\\",
            b"\x1bXignored\x1b\\\x1b^private\x1b\\",
            b"\x1b]2;cancelled\x18",
            b"\x1b[1;2\x1a",
            b"\x1b]2;unfinished\x1b",
        ];
        // Compose fragments so an incomplete sequence also encounters every
        // other kind of sequence. Compare semantic callbacks, not batching:
        // Dispatcher expands print_str/print_codepoints through Perform defaults.
        for (a, first) in fragments.iter().enumerate() {
            for (b, second) in fragments.iter().enumerate() {
                let input = [*first, *second, b"\x18\x1b[0mend"].concat();
                let mut expected = Dispatcher::default();
                Parser::new().advance(&mut expected, &input);
                for split in 0..=input.len() {
                    let mut actual = Dispatcher::default();
                    let mut parser = Parser::new();
                    parser.advance(&mut actual, &input[..split]);
                    parser.advance(&mut actual, &input[split..]);
                    assert_eq!(
                        actual.dispatched, expected.dispatched,
                        "fragments {a}, {b}, split {split}"
                    );
                }
            }
        }
    }

    const OSC_BYTES: &[u8] = &[
        0x1B, 0x5D, // Begin OSC
        b'2', b';', b'j', b'w', b'i', b'l', b'm', b'@', b'j', b'w', b'i', b'l', b'm',
        b'-', b'd', b'e', b's', b'k', b':', b' ', b'~', b'/', b'c', b'o', b'd', b'e',
        b'/', b'a', b'l', b'a', b'c', b'r', b'i', b't', b't', b'y', 0x07, // End OSC
    ];

    const ST_ESC_SEQUENCE: &[Sequence] = &[Sequence::Esc(vec![], false, 0x5C)];

    #[derive(Default)]
    struct Dispatcher {
        dispatched: Vec<Sequence>,
    }

    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    enum OpaqueSequenceKind {
        Sos,
        Pm,
        Apc,
    }

    /// Non-circular exhaustive check of the boundary scanners against
    /// the per-byte state machine: for every byte value, a byte is
    /// "put class" when feeding it to the real handler grows the payload.
    /// Opaque C0 payload bytes deliberately take the per-byte path; other
    /// payload bytes must not stop the scanner. Both the SWAR lane (16-byte
    /// input) and the scalar tail (1-byte input) are checked.
    #[test]
    fn boundary_scanners_match_state_machine() {
        for b in 0..=255u8 {
            // OSC: put iff osc_raw grows.
            let mut d = Dispatcher::default();
            let mut p = Parser::new();
            p.advance(&mut d, b"\x1b]");
            let before = p.osc_raw.len();
            p.advance_osc_string(&mut d, b);
            // Pure payload: the raw buffer grew and nothing else
            // happened. Bytes with side effects (terminators, aborts,
            // the param separator) must be boundaries even when the
            // handler also stores them.
            let put = p.osc_raw.len() > before && p.state == State::OscString;
            assert_eq!(find_osc_boundary(&[b]) == 1, put, "osc byte {b:#04x}");
            let wide = [b; 16];
            assert_eq!(
                find_osc_boundary(&wide) == 16,
                put,
                "osc swar byte {b:#04x}"
            );

            // APC / SOS / PM: put iff an OpaquePut event is recorded.
            for kind in ["apc", "sos", "pm"] {
                let mut d = Dispatcher::default();
                let mut p = Parser::new();
                match kind {
                    "apc" => p.advance(&mut d, b"\x1b_"),
                    "sos" => p.advance(&mut d, b"\x1bX"),
                    _ => p.advance(&mut d, b"\x1b^"),
                }
                let before = d.dispatched.len();
                match kind {
                    "apc" => p.advance_apc_string(&mut d, b),
                    "sos" => p.advance_sos_string(&mut d, b),
                    _ => p.advance_pm_string(&mut d, b),
                }
                let events = &d.dispatched[before..];
                let put = b >= 0x20
                    && events.len() == 1
                    && matches!(events[0], Sequence::OpaquePut(..))
                    && p.state != State::Ground
                    && p.state != State::Escape;
                assert_eq!(find_opaque_boundary(&[b]) == 1, put, "{kind} byte {b:#04x}");
                assert_eq!(
                    find_opaque_boundary(&[b; 16]) == 16,
                    put,
                    "{kind} swar byte {b:#04x}"
                );
            }

            // DCS passthrough: put iff a DcsPut event is recorded.
            let mut d = Dispatcher::default();
            let mut p = Parser::new();
            p.advance(&mut d, b"\x1bPq");
            let before = d.dispatched.len();
            p.advance_dcs_passthrough(&mut d, b);
            let events = &d.dispatched[before..];
            let put = events.len() == 1
                && matches!(events[0], Sequence::DcsPut(_))
                && p.state == State::DcsPassthrough;
            assert_eq!(find_dcs_boundary(&[b]) == 1, put, "dcs byte {b:#04x}");
            assert_eq!(
                find_dcs_boundary(&[b; 16]) == 16,
                put,
                "dcs swar byte {b:#04x}"
            );
        }
    }

    /// The batched string-state runs must be observationally identical
    /// to the per-byte state machine for every chunking of the input:
    /// the same stream is fed whole, byte-at-a-time, and split at
    /// random boundaries, and all three must dispatch the same events.
    #[test]
    fn string_state_runs_match_per_byte() {
        let big = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo=".repeat(300);
        let mut streams: Vec<Vec<u8>> = vec![
            // OSC 52 clipboard, BEL and ST terminated.
            format!("\x1b]52;c;{big}\x07next").into_bytes(),
            format!("\x1b]52;c;{big}\x1b\\next").into_bytes(),
            // OSC with many params and an abort.
            b"\x1b]4;1;rgb:aa/bb/cc;2;rgb:dd/ee/ff\x07".to_vec(),
            b"\x1b]0;title\x18after-can".to_vec(),
            // Kitty APC: ST/CAN termination and embedded BEL payload.
            format!("\x1b_Gf=100,a=T;{big}\x1b\\tail").into_bytes(),
            format!("\x1b_G{big}\x07").into_bytes(),
            b"\x1b_Gpayload\x18rest".to_vec(),
            // DCS sixel: C0s inside payload are put, DEL and >0x7E are
            // ignored, ST via ESC-backslash and embedded high-byte payload.
            b"\x1bP0;1q#0;2;0;0;0#1~~@@\x09data\x7fmore\x1b\\after".to_vec(),
            b"\x1bPq~~\xc3\xa9~~\x9cafter".to_vec(),
            b"\x1bPqdata\x1arest".to_vec(),
            // SOS and PM payloads.
            b"\x1bXsos payload 123\x1b\\g".to_vec(),
            b"\x1b^pm payload 456\x07g".to_vec(),
            // Interleaved with ground text and CSI.
            format!("plain\x1b[31m\x1b]52;c;{big}\x07\x1b[0mtext").into_bytes(),
        ];
        // One stream that exercises every boundary byte adjacent to
        // payload bytes in every string state.
        let mut soup = Vec::new();
        for chunk in &streams {
            soup.extend_from_slice(chunk);
        }
        streams.push(soup);

        let mut lcg: u64 = 0x5EED_CAFE;
        let mut rng = move |n: usize| {
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((lcg >> 33) as usize) % n
        };

        for stream in &streams {
            let whole = {
                let mut d = Dispatcher::default();
                let mut p = Parser::new();
                p.advance(&mut d, stream);
                d.dispatched
            };
            let bytewise = {
                let mut d = Dispatcher::default();
                let mut p = Parser::new();
                for &b in stream {
                    p.advance(&mut d, &[b]);
                }
                d.dispatched
            };
            assert_eq!(whole, bytewise, "whole vs per-byte");

            for _round in 0..8 {
                let mut d = Dispatcher::default();
                let mut p = Parser::new();
                let mut i = 0;
                while i < stream.len() {
                    let n = 1 + rng(stream.len() - i);
                    p.advance(&mut d, &stream[i..i + n]);
                    i += n;
                }
                assert_eq!(d.dispatched, whole, "random splits vs whole");
            }
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Sequence {
        Osc(Vec<Vec<u8>>, bool),
        Csi(Vec<Vec<u16>>, Vec<u8>, bool, char),
        Esc(Vec<u8>, bool, u8),
        DcsHook(Vec<Vec<u16>>, Vec<u8>, bool, char),
        DcsPut(u8),
        Print(char),
        Execute(u8),
        OpaqueStart(OpaqueSequenceKind),
        OpaquePut(OpaqueSequenceKind, u8),
        OpaqueEnd(OpaqueSequenceKind),
        DcsUnhook,
    }

    #[test]
    fn adjacent_escape_introducers_match_split_state_transitions() {
        let streams: &[&[u8]] = &[
            b"text\x1b[1;38;2;120;80;40mcolor\x1b[0m\r\n",
            b"\x1b[?2027h\x1b[4:3m\x1b[2 q\x1b[m",
            b"\x1b[65534;65535;65536;999999999999999999999999m",
            b"\x1b[38:2:65536:999999999999999999:0m",
            b"\x1b[1;2\x18\x1b[3m\x1b[4\x1b[5m",
            b"\x1b]8;id=link;https://example.org\x1b\\label\x1b]8;;\x07",
            b"\x1b]0;title\x07\x1b[31m\x1b]9;4;1;20\x1b\\",
            b"\x1b[1  q\x1b]0;\x18\x1b[?25h\x1b\x1b[0m",
        ];
        for stream in streams {
            let mut reference = Dispatcher::default();
            let mut parser = Parser::new();
            for byte in *stream {
                parser.advance(&mut reference, &[*byte]);
            }
            for split in 0..=stream.len() {
                let mut actual = Dispatcher::default();
                let mut parser = Parser::new();
                parser.advance(&mut actual, &stream[..split]);
                parser.advance(&mut actual, &stream[split..]);
                assert_eq!(actual.dispatched, reference.dispatched, "split {split}");
            }
        }
    }

    impl Perform for Dispatcher {
        fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
            let params = params.iter().map(|p| p.to_vec()).collect();
            self.dispatched.push(Sequence::Osc(params, bell_terminated));
        }

        fn csi_dispatch(
            &mut self,
            params: &Params,
            intermediates: &[u8],
            ignore: bool,
            c: char,
        ) {
            let params = params.iter().map(|subparam| subparam.to_vec()).collect();
            let intermediates = intermediates.to_vec();
            self.dispatched
                .push(Sequence::Csi(params, intermediates, ignore, c));
        }

        fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
            let intermediates = intermediates.to_vec();
            self.dispatched
                .push(Sequence::Esc(intermediates, ignore, byte));
        }

        fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, c: char) {
            let params = params.iter().map(|subparam| subparam.to_vec()).collect();
            let intermediates = intermediates.to_vec();
            self.dispatched
                .push(Sequence::DcsHook(params, intermediates, ignore, c));
        }

        fn put(&mut self, byte: u8) {
            self.dispatched.push(Sequence::DcsPut(byte));
        }

        fn unhook(&mut self) {
            self.dispatched.push(Sequence::DcsUnhook);
        }

        fn print(&mut self, c: char) {
            self.dispatched.push(Sequence::Print(c));
        }

        fn execute(&mut self, byte: u8) {
            self.dispatched.push(Sequence::Execute(byte));
        }

        fn sos_start(&mut self) {
            self.dispatched
                .push(Sequence::OpaqueStart(OpaqueSequenceKind::Sos));
        }

        fn sos_put(&mut self, byte: u8) {
            self.dispatched
                .push(Sequence::OpaquePut(OpaqueSequenceKind::Sos, byte));
        }

        fn sos_end(&mut self) {
            self.dispatched
                .push(Sequence::OpaqueEnd(OpaqueSequenceKind::Sos));
        }

        fn pm_start(&mut self) {
            self.dispatched
                .push(Sequence::OpaqueStart(OpaqueSequenceKind::Pm));
        }

        fn pm_put(&mut self, byte: u8) {
            self.dispatched
                .push(Sequence::OpaquePut(OpaqueSequenceKind::Pm, byte));
        }

        fn pm_end(&mut self) {
            self.dispatched
                .push(Sequence::OpaqueEnd(OpaqueSequenceKind::Pm));
        }

        fn apc_start(&mut self) {
            self.dispatched
                .push(Sequence::OpaqueStart(OpaqueSequenceKind::Apc));
        }

        fn apc_put(&mut self, byte: u8) {
            self.dispatched
                .push(Sequence::OpaquePut(OpaqueSequenceKind::Apc, byte));
        }

        fn apc_end(&mut self) {
            self.dispatched
                .push(Sequence::OpaqueEnd(OpaqueSequenceKind::Apc));
        }
    }

    #[test]
    fn parse_osc() {
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, OSC_BYTES);

        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::Osc(params, _) => {
                assert_eq!(params.len(), 2);
                assert_eq!(params[0], &OSC_BYTES[2..3]);
                assert_eq!(params[1], &OSC_BYTES[4..(OSC_BYTES.len() - 1)]);
            }
            _ => panic!("expected osc sequence"),
        }
    }

    #[test]
    fn parse_empty_osc() {
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, &[0x1B, 0x5D, 0x07]);

        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::Osc(..) => (),
            _ => panic!("expected osc sequence"),
        }
    }

    #[test]
    fn parse_osc_max_params() {
        let params = ";".repeat(params::MAX_PARAMS + 1);
        let input = format!("\x1b]{}\x1b", &params[..]).into_bytes();
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, &input);

        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::Osc(params, _) => {
                assert_eq!(params.len(), MAX_OSC_PARAMS);
                assert!(params.iter().all(Vec::is_empty));
            }
            _ => panic!("expected osc sequence"),
        }
    }

    #[test]
    fn osc_parameter_counts_match_at_every_split() {
        for count in 1..=MAX_OSC_PARAMS + 2 {
            let params: Vec<_> = (0..count).map(|i| format!("p{i}")).collect();
            let expected: Vec<_> = params
                .iter()
                .take(MAX_OSC_PARAMS)
                .map(|param| param.as_bytes().to_vec())
                .collect();
            for terminator in [0x07, 0x1b] {
                let mut input = format!("\x1b]{}", params.join(";")).into_bytes();
                input.push(terminator);
                for split in 0..=input.len() {
                    let mut parser = Parser::default();
                    let mut dispatcher = Dispatcher::default();
                    parser.advance(&mut dispatcher, &input[..split]);
                    parser.advance(&mut dispatcher, &input[split..]);
                    assert_eq!(
                        dispatcher.dispatched,
                        vec![Sequence::Osc(expected.clone(), terminator == 0x07)],
                        "count={count} terminator={terminator} split={split}"
                    );
                }
            }
        }
    }

    #[test]
    fn osc_verbatim_tails_survive_every_read_split() {
        let tail = format!("préfix中{};", ";;a".repeat(MAX_OSC_PARAMS + 3));
        for (header, mut expected) in [
            ("7;", vec![b"7".to_vec()]),
            (
                "8;id=one:other=two;",
                vec![b"8".to_vec(), b"id=one:other=two".to_vec()],
            ),
            ("9;9;", vec![b"9".to_vec(), b"9".to_vec()]),
        ] {
            expected.push(tail.as_bytes().to_vec());
            for terminator in ["\x07", "\x1b\\"] {
                let input = format!("\x1b]{header}{tail}{terminator}Z\x1b]9;4;1;50\x07");
                for split in 0..=input.len() {
                    let mut dispatcher = Dispatcher::default();
                    let mut parser = Parser::default();
                    parser.advance(&mut dispatcher, &input.as_bytes()[..split]);
                    parser.advance(&mut dispatcher, &input.as_bytes()[split..]);
                    let osc: Vec<_> = dispatcher
                        .dispatched
                        .iter()
                        .filter_map(|s| match s {
                            Sequence::Osc(params, _) => Some(params),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(
                        osc,
                        vec![
                            &expected,
                            &vec![
                                b"9".to_vec(),
                                b"4".to_vec(),
                                b"1".to_vec(),
                                b"50".to_vec()
                            ]
                        ],
                        "{header}, split={split}"
                    );
                    assert!(dispatcher.dispatched.contains(&Sequence::Print('Z')));
                }
                let mut bytewise = Dispatcher::default();
                let mut parser = Parser::default();
                for byte in input.bytes() {
                    parser.advance(&mut bytewise, &[byte]);
                }
                assert!(bytewise
                    .dispatched
                    .contains(&Sequence::Osc(expected.clone(), terminator == "\x07")));
            }
        }
    }

    #[test]
    fn osc_verbatim_tails_keep_empty_payloads() {
        for (input, expected) in [
            (b"\x1b]7;\x07".as_slice(), vec![b"7".to_vec(), vec![]]),
            (
                b"\x1b]8;id=one;\x07",
                vec![b"8".to_vec(), b"id=one".to_vec(), vec![]],
            ),
            (b"\x1b]9;9;\x07", vec![b"9".to_vec(), b"9".to_vec(), vec![]]),
        ] {
            let mut dispatcher = Dispatcher::default();
            Parser::default().advance(&mut dispatcher, input);
            assert_eq!(dispatcher.dispatched[0], Sequence::Osc(expected, true));
        }
    }

    #[test]
    fn osc_verbatim_tail_survives_inline_buffer_overflow() {
        let uri = format!("https://example.com/{};é", ";x".repeat(OSC_FIXED_LEN));
        let input = format!("\x1b]8;;{uri}\x1b\\\x1b]8;;\x07");
        for chunk_size in [1, 2, 17, input.len()] {
            let mut dispatcher = Dispatcher::default();
            let mut parser = Parser::default();
            for chunk in input.as_bytes().chunks(chunk_size) {
                parser.advance(&mut dispatcher, chunk);
            }
            assert_eq!(
                dispatcher.dispatched[0],
                Sequence::Osc(
                    vec![b"8".to_vec(), vec![], uri.as_bytes().to_vec()],
                    false
                )
            );
            assert!(dispatcher
                .dispatched
                .contains(&Sequence::Osc(vec![b"8".to_vec(), vec![], vec![]], true)));
        }
    }

    #[test]
    fn osc_bell_terminated() {
        const INPUT: &[u8] = b"\x1b]11;ff/00/ff\x07";
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::Osc(_, true) => (),
            _ => panic!("expected osc with bell terminator"),
        }
    }

    #[test]
    fn osc_c0_st_terminated() {
        const INPUT: &[u8] = b"\x1b]11;ff/00/ff\x1b\\";
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 2);
        match &dispatcher.dispatched[0] {
            Sequence::Osc(_, false) => (),
            _ => panic!("expected osc with ST terminator"),
        }
    }

    #[test]
    fn parse_osc_with_utf8_arguments() {
        const INPUT: &[u8] = &[
            0x0D, 0x1B, 0x5D, 0x32, 0x3B, 0x65, 0x63, 0x68, 0x6F, 0x20, 0x27, 0xC2, 0xAF,
            0x5C, 0x5F, 0x28, 0xE3, 0x83, 0x84, 0x29, 0x5F, 0x2F, 0xC2, 0xAF, 0x27, 0x20,
            0x26, 0x26, 0x20, 0x73, 0x6C, 0x65, 0x65, 0x70, 0x20, 0x31, 0x07,
        ];
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched[0], Sequence::Execute(b'\r'));
        let osc_data = INPUT[5..(INPUT.len() - 1)].into();
        assert_eq!(
            dispatcher.dispatched[1],
            Sequence::Osc(vec![vec![b'2'], osc_data], true)
        );
        assert_eq!(dispatcher.dispatched.len(), 2);
    }

    #[test]
    fn osc_containing_string_terminator() {
        const INPUT: &[u8] = b"\x1b]2;\xe6\x9c\xab\x1b\\";
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 2);
        match &dispatcher.dispatched[0] {
            Sequence::Osc(params, _) => {
                assert_eq!(params[1], &INPUT[4..(INPUT.len() - 2)]);
            }
            _ => panic!("expected osc sequence"),
        }
    }

    #[test]
    fn osc_fits_in_inline_buffer() {
        // Stay below `OSC_FIXED_LEN`; the spill `Vec` should never grow.
        const NUM_BYTES: usize = OSC_FIXED_LEN - 32;
        const INPUT_START: &[u8] = b"\x1b]52;s";
        const INPUT_END: &[u8] = b"\x07";

        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT_START);
        parser.advance(&mut dispatcher, &[b'a'; NUM_BYTES]);
        parser.advance(&mut dispatcher, INPUT_END);

        assert!(parser.osc_raw.overflow.capacity() == 0);
        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::Osc(params, _) => {
                assert_eq!(params.len(), 2);
                assert_eq!(params[0], b"52");
                assert_eq!(params[1].len(), NUM_BYTES + INPUT_END.len());
            }
            _ => panic!("expected osc sequence"),
        }
    }

    #[test]
    fn osc_spills_to_overflow() {
        // Push past `OSC_FIXED_LEN` to exercise the heap-fallback path.
        const NUM_BYTES: usize = OSC_FIXED_LEN + 512;
        const INPUT_START: &[u8] = b"\x1b]52;s";
        const INPUT_END: &[u8] = b"\x07";

        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT_START);
        parser.advance(&mut dispatcher, &[b'a'; NUM_BYTES]);
        parser.advance(&mut dispatcher, INPUT_END);

        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::Osc(params, _) => {
                assert_eq!(params.len(), 2);
                assert_eq!(params[0], b"52");
                assert_eq!(params[1].len(), NUM_BYTES + INPUT_END.len());
                assert_eq!(params[1][0], b's');
                assert!(params[1][1..].iter().all(|&b| b == b'a'));
            }
            _ => panic!("expected osc sequence"),
        }
    }

    #[test]
    fn parse_csi_max_params() {
        // This will build a list of repeating '1;'s
        // The length is MAX_PARAMS - 1 because the last semicolon is interpreted
        // as an implicit zero, making the total number of parameters MAX_PARAMS
        let params = "1;".repeat(params::MAX_PARAMS - 1);
        let input = format!("\x1b[{}p", &params[..]).into_bytes();

        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, &input);

        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::Csi(params, _, ignore, _) => {
                assert_eq!(params.len(), params::MAX_PARAMS);
                assert!(!ignore);
            }
            _ => panic!("expected csi sequence"),
        }
    }

    #[test]
    fn parse_csi_params_ignore_long_params() {
        // This will build a list of repeating '1;'s
        // The length is MAX_PARAMS because the last semicolon is interpreted
        // as an implicit zero, making the total number of parameters MAX_PARAMS + 1
        let params = "1;".repeat(params::MAX_PARAMS);
        let input = format!("\x1b[{}p", &params[..]).into_bytes();

        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, &input);

        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::Csi(params, _, ignore, _) => {
                assert_eq!(params.len(), params::MAX_PARAMS);
                assert!(ignore);
            }
            _ => panic!("expected csi sequence"),
        }
    }

    #[test]
    fn parse_csi_params_trailing_semicolon() {
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, b"\x1b[4;m");

        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::Csi(params, ..) => assert_eq!(params, &[[4], [0]]),
            _ => panic!("expected csi sequence"),
        }
    }

    #[test]
    fn parse_csi_params_leading_semicolon() {
        // Create dispatcher and check state
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, b"\x1b[;4m");

        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::Csi(params, ..) => assert_eq!(params, &[[0], [4]]),
            _ => panic!("expected csi sequence"),
        }
    }

    #[test]
    fn parse_long_csi_param() {
        // The important part is the parameter, which is (i64::MAX + 1)
        const INPUT: &[u8] = b"\x1b[9223372036854775808m";
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::Csi(params, ..) => assert_eq!(params, &[[u16::MAX]]),
            _ => panic!("expected csi sequence"),
        }
    }

    #[test]
    fn csi_reset() {
        const INPUT: &[u8] = b"\x1b[3;1\x1b[?1049h";
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::Csi(params, intermediates, ignore, _) => {
                assert_eq!(intermediates, b"?");
                assert_eq!(params, &[[1049]]);
                assert!(!ignore);
            }
            _ => panic!("expected csi sequence"),
        }
    }

    #[test]
    fn csi_subparameters() {
        const INPUT: &[u8] = b"\x1b[38:2:255:0:255;1m";
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::Csi(params, intermediates, ignore, _) => {
                assert_eq!(params, &[vec![38, 2, 255, 0, 255], vec![1]]);
                assert!(intermediates.is_empty());
                assert!(!ignore);
            }
            _ => panic!("expected csi sequence"),
        }
    }

    #[test]
    fn parse_dcs_max_params() {
        let params = "1;".repeat(params::MAX_PARAMS + 1);
        let input = format!("\x1bP{}p", &params[..]).into_bytes();
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, &input);

        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::DcsHook(params, _, ignore, _) => {
                assert_eq!(params.len(), params::MAX_PARAMS);
                assert!(params.iter().all(|param| param == &[1]));
                assert!(ignore);
            }
            _ => panic!("expected dcs sequence"),
        }
    }

    #[test]
    fn dcs_reset() {
        const INPUT: &[u8] = b"\x1b[3;1\x1bP1$tx\x1b\\";
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 4);
        match &dispatcher.dispatched[0] {
            Sequence::DcsHook(params, intermediates, ignore, _) => {
                assert_eq!(intermediates, b"$");
                assert_eq!(params, &[[1]]);
                assert!(!ignore);
            }
            _ => panic!("expected dcs sequence"),
        }
        assert_eq!(dispatcher.dispatched[1], Sequence::DcsPut(b'x'));
        assert_eq!(dispatcher.dispatched[2], Sequence::DcsUnhook);
        assert_eq!(
            dispatcher.dispatched[3],
            Sequence::Esc(vec![], false, b'\\')
        );
    }

    #[test]
    fn parse_dcs() {
        const INPUT: &[u8] = b"\x1bP0;1|17/ab\x1b\\";
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 8);
        match &dispatcher.dispatched[0] {
            Sequence::DcsHook(params, _, _, c) => {
                assert_eq!(params, &[[0], [1]]);
                assert_eq!(c, &'|');
            }
            _ => panic!("expected dcs sequence"),
        }
        for (i, byte) in b"17/ab".iter().enumerate() {
            assert_eq!(dispatcher.dispatched[1 + i], Sequence::DcsPut(*byte));
        }
        assert_eq!(dispatcher.dispatched[6], Sequence::DcsUnhook);
        assert_eq!(
            dispatcher.dispatched[7],
            Sequence::Esc(vec![], false, b'\\')
        );
    }

    #[test]
    fn intermediate_reset_on_dcs_exit() {
        const INPUT: &[u8] = b"\x1bP=1sZZZ\x1b+\x5c";
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 6);
        match &dispatcher.dispatched[5] {
            Sequence::Esc(intermediates, ..) => assert_eq!(intermediates, b"+"),
            _ => panic!("expected esc sequence"),
        }
    }

    #[test]
    fn esc_reset() {
        const INPUT: &[u8] = b"\x1b[3;1\x1b(A";
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::Esc(intermediates, ignore, byte) => {
                assert_eq!(intermediates, b"(");
                assert_eq!(*byte, b'A');
                assert!(!ignore);
            }
            _ => panic!("expected esc sequence"),
        }
    }

    #[test]
    fn esc_reset_intermediates() {
        const INPUT: &[u8] = b"\x1b[?2004l\x1b#8";
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 2);
        assert_eq!(
            dispatcher.dispatched[0],
            Sequence::Csi(vec![vec![2004]], vec![63], false, 'l')
        );
        assert_eq!(dispatcher.dispatched[1], Sequence::Esc(vec![35], false, 56));
    }

    #[test]
    fn params_buffer_filled_with_subparam() {
        const INPUT: &[u8] = b"\x1b[::::::::::::::::::::::::::::::::x\x1b";
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 1);
        match &dispatcher.dispatched[0] {
            Sequence::Csi(params, intermediates, ignore, c) => {
                assert!(intermediates.is_empty());
                assert_eq!(params, &[[0; 32]]);
                assert_eq!(c, &'x');
                assert!(ignore);
            }
            _ => panic!("expected csi sequence"),
        }
    }

    fn expect_opaque_sequence(
        input: &[u8],
        kind: OpaqueSequenceKind,
        expected_payload: &[u8],
        expected_trailer: &[Sequence],
    ) {
        let mut expected_dispatched: Vec<Sequence> = vec![Sequence::OpaqueStart(kind)];
        for byte in expected_payload {
            expected_dispatched.push(Sequence::OpaquePut(kind, *byte));
        }
        expected_dispatched.push(Sequence::OpaqueEnd(kind));
        for item in expected_trailer {
            expected_dispatched.push(item.clone());
        }

        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();
        parser.advance(&mut dispatcher, input);

        assert_eq!(dispatcher.dispatched, expected_dispatched);
    }

    #[test]
    fn sos_c0_st_terminated() {
        expect_opaque_sequence(
            b"\x1bXTest\x20\xFF;xyz\x1b\\",
            OpaqueSequenceKind::Sos,
            b"Test\x20;xyz",
            ST_ESC_SEQUENCE,
        );
    }

    #[test]
    fn sos_bell_is_payload_until_st() {
        expect_opaque_sequence(
            b"\x1bXTest\x20\xFF;xyz\x07\x1b\\",
            OpaqueSequenceKind::Sos,
            b"Test\x20;xyz\x07",
            ST_ESC_SEQUENCE,
        );
    }

    #[test]
    fn sos_empty() {
        expect_opaque_sequence(
            b"\x1bX\x1b\\",
            OpaqueSequenceKind::Sos,
            &[],
            ST_ESC_SEQUENCE,
        );
    }

    #[test]
    fn pm_c0_st_terminated() {
        expect_opaque_sequence(
            b"\x1b^Test\x20\xFF;xyz\x1b\\",
            OpaqueSequenceKind::Pm,
            b"Test\x20;xyz",
            ST_ESC_SEQUENCE,
        );
    }

    #[test]
    fn pm_bell_is_payload_until_st() {
        expect_opaque_sequence(
            b"\x1b^Test\x20\xFF;xyz\x07\x1b\\",
            OpaqueSequenceKind::Pm,
            b"Test\x20;xyz\x07",
            ST_ESC_SEQUENCE,
        );
    }

    #[test]
    fn pm_empty() {
        expect_opaque_sequence(
            b"\x1b^\x1b\\",
            OpaqueSequenceKind::Pm,
            &[],
            ST_ESC_SEQUENCE,
        );
    }

    #[test]
    fn parse_kitty_apc() {
        const INPUT: &[u8] = b"\x1b_Gf=24,s=10,v=20;Zm9v\x1b\\";
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        let expected = vec![
            Sequence::OpaqueStart(OpaqueSequenceKind::Apc),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'G'),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'f'),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'='),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'2'),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'4'),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b','),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b's'),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'='),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'1'),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'0'),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b','),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'v'),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'='),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'2'),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'0'),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b';'),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'Z'),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'm'),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'9'),
            Sequence::OpaquePut(OpaqueSequenceKind::Apc, b'v'),
            Sequence::OpaqueEnd(OpaqueSequenceKind::Apc),
            Sequence::Esc(vec![], false, b'\\'),
        ];

        assert_eq!(dispatcher.dispatched, expected)
    }

    #[test]
    fn parse_kitty_apc_dispatch_params() {
        // Test that commas in control data are NOT treated as param separators
        // Only semicolons should separate control data from payload
        const INPUT: &[u8] = b"\x1b_Gf=32,s=10,v=20;AQIDBA==\x1b\\";
        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        // Verify we got an APC dispatch
        let apc_dispatch = dispatcher
            .dispatched
            .iter()
            .find(|s| matches!(s, Sequence::OpaqueEnd(OpaqueSequenceKind::Apc)));
        assert!(apc_dispatch.is_some(), "Should have APC dispatch");

        // The test in performer::handler verifies the actual param parsing
        // Here we just ensure the sequence completes correctly
    }

    #[test]
    fn apc_c0_st_terminated() {
        expect_opaque_sequence(
            b"\x1b_Test\x20\xFF;xyz\x1b\\",
            OpaqueSequenceKind::Apc,
            b"Test\x20;xyz",
            ST_ESC_SEQUENCE,
        );
    }

    #[test]
    fn apc_bell_is_payload_until_st() {
        expect_opaque_sequence(
            b"\x1b_Test\x20\xFF;xyz\x07\x1b\\",
            OpaqueSequenceKind::Apc,
            b"Test\x20;xyz\x07",
            ST_ESC_SEQUENCE,
        );
    }

    #[test]
    fn apc_empty() {
        expect_opaque_sequence(
            b"\x1b_\x1b\\",
            OpaqueSequenceKind::Apc,
            &[],
            ST_ESC_SEQUENCE,
        );
    }

    #[test]
    fn unicode() {
        const INPUT: &[u8] =
            b"\xF0\x9F\x8E\x89_\xF0\x9F\xA6\x80\xF0\x9F\xA6\x80_\xF0\x9F\x8E\x89";

        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 6);
        assert_eq!(dispatcher.dispatched[0], Sequence::Print('🎉'));
        assert_eq!(dispatcher.dispatched[1], Sequence::Print('_'));
        assert_eq!(dispatcher.dispatched[2], Sequence::Print('🦀'));
        assert_eq!(dispatcher.dispatched[3], Sequence::Print('🦀'));
        assert_eq!(dispatcher.dispatched[4], Sequence::Print('_'));
        assert_eq!(dispatcher.dispatched[5], Sequence::Print('🎉'));
    }

    #[test]
    fn invalid_utf8() {
        const INPUT: &[u8] = b"a\xEF\xBCb";

        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 3);
        assert_eq!(dispatcher.dispatched[0], Sequence::Print('a'));
        assert_eq!(dispatcher.dispatched[1], Sequence::Print('�'));
        assert_eq!(dispatcher.dispatched[2], Sequence::Print('b'));
    }

    #[test]
    fn partial_utf8() {
        const INPUT: &[u8] = b"\xF0\x9F\x9A\x80";

        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, &INPUT[..1]);
        parser.advance(&mut dispatcher, &INPUT[1..2]);
        parser.advance(&mut dispatcher, &INPUT[2..3]);
        parser.advance(&mut dispatcher, &INPUT[3..]);

        assert_eq!(dispatcher.dispatched.len(), 1);
        assert_eq!(dispatcher.dispatched[0], Sequence::Print('🚀'));
    }

    #[test]
    fn partial_utf8_separating_utf8() {
        // This is different from the `partial_utf8` test since it has a multi-byte UTF8
        // character after the partial UTF8 state, causing a partial byte to be present
        // in the `partial_utf8` buffer after the 2-byte codepoint.

        // "ĸ🎉"
        const INPUT: &[u8] = b"\xC4\xB8\xF0\x9F\x8E\x89";

        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, &INPUT[..1]);
        parser.advance(&mut dispatcher, &INPUT[1..]);

        assert_eq!(dispatcher.dispatched.len(), 2);
        assert_eq!(dispatcher.dispatched[0], Sequence::Print('ĸ'));
        assert_eq!(dispatcher.dispatched[1], Sequence::Print('🎉'));
    }

    #[test]
    fn partial_utf8_preserves_ascii_before_the_next_partial_character() {
        for input in ["éC\u{301}", "ĸA界", "éAB\u{301}"] {
            let expected: Vec<_> = input.chars().map(Sequence::Print).collect();
            for split in 0..=input.len() {
                let mut dispatcher = Dispatcher::default();
                let mut parser = Parser::default();
                parser.advance(&mut dispatcher, &input.as_bytes()[..split]);
                parser.advance(&mut dispatcher, &input.as_bytes()[split..]);
                assert_eq!(dispatcher.dispatched, expected, "{input:?}, split {split}");
            }
        }
    }

    #[test]
    fn partial_invalid_utf8() {
        const INPUT: &[u8] = b"a\xEF\xBCb";

        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, &INPUT[..1]);
        parser.advance(&mut dispatcher, &INPUT[1..2]);
        parser.advance(&mut dispatcher, &INPUT[2..3]);
        parser.advance(&mut dispatcher, &INPUT[3..]);

        assert_eq!(dispatcher.dispatched.len(), 3);
        assert_eq!(dispatcher.dispatched[0], Sequence::Print('a'));
        assert_eq!(dispatcher.dispatched[1], Sequence::Print('�'));
        assert_eq!(dispatcher.dispatched[2], Sequence::Print('b'));
    }

    #[test]
    fn partial_invalid_utf8_split() {
        const INPUT: &[u8] = b"\xE4\xBF\x99\xB5";

        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, &INPUT[..2]);
        parser.advance(&mut dispatcher, &INPUT[2..]);

        assert_eq!(dispatcher.dispatched[0], Sequence::Print('俙'));
        assert_eq!(dispatcher.dispatched[1], Sequence::Print('�'));
    }

    #[test]
    fn partial_utf8_into_esc() {
        const INPUT: &[u8] = b"\xD8\x1b012";

        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 4);
        assert_eq!(dispatcher.dispatched[0], Sequence::Print('�'));
        assert_eq!(
            dispatcher.dispatched[1],
            Sequence::Esc(Vec::new(), false, b'0')
        );
        assert_eq!(dispatcher.dispatched[2], Sequence::Print('1'));
        assert_eq!(dispatcher.dispatched[3], Sequence::Print('2'));
    }

    #[test]
    fn c1s() {
        const INPUT: &[u8] = b"\x00\x1f\x80\x90\x98\x9b\x9c\x9d\x9e\x9fa";

        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 11);
        assert_eq!(dispatcher.dispatched[0], Sequence::Execute(0));
        assert_eq!(dispatcher.dispatched[1], Sequence::Execute(31));
        for sequence in &dispatcher.dispatched[2..10] {
            assert_eq!(*sequence, Sequence::Print('\u{fffd}'));
        }
        assert_eq!(dispatcher.dispatched[10], Sequence::Print('a'));
    }

    #[test]
    fn execute_anywhere() {
        const INPUT: &[u8] = b"\x18\x1a";

        let mut dispatcher = Dispatcher::default();
        let mut parser = Parser::default();

        parser.advance(&mut dispatcher, INPUT);

        assert_eq!(dispatcher.dispatched.len(), 2);
        assert_eq!(dispatcher.dispatched[0], Sequence::Execute(0x18));
        assert_eq!(dispatcher.dispatched[1], Sequence::Execute(0x1A));
    }
}
