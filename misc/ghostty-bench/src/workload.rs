// Adapted workload shapes from Ghostty src/benchmark at the revision in main.rs.
// Rio's storage and APIs are used throughout; this is not a Ghostty speed proxy.
use crate::{Input, Options, Result};
use rio_unicode::{
    grapheme::{grapheme_class, is_break, BreakState},
    UnicodeWidthChar,
};
use rio_vt::ansi::CursorShape;
use rio_vt::crosswords::formatter::FormatOptions;
use rio_vt::crosswords::grid::{row::Row, Dimensions, ExtrasTable, Scroll};
#[cfg(not(ghostty_bench_upstream_api))]
use rio_vt::crosswords::snapshot::{RowStyles, SnapshotExtras};
#[cfg(ghostty_bench_upstream_api)]
type RowStyles = Vec<rio_vt::crosswords::style::Style>;
#[cfg(ghostty_bench_upstream_api)]
type SnapshotExtras = rustc_hash::FxHashMap<u16, Extras>;
use rio_vt::crosswords::pos::Line;
use rio_vt::crosswords::square::{Extras, Hyperlink, Square};
use rio_vt::crosswords::{Crosswords, CrosswordsSize};
use rio_vt::event::{TerminalDamage, VoidListener, WindowId};
use rio_vt::performer::handler::Processor;
use rio_vt::performer::parser::{Params, Parser, Perform};
use std::hint::black_box;

type Terminal = Crosswords<VoidListener>;

fn terminal(opts: &Options) -> Terminal {
    Crosswords::new(
        CrosswordsSize::new(opts.cols, opts.rows),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        opts.scrollback,
    )
}

pub fn hash(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}

#[derive(Default)]
pub struct Sink {
    printed_bytes: u64,
    commands: u64,
    controls: u64,
    apc: Vec<u8>,
    collect_apc: bool,
}

impl Perform for Sink {
    fn print(&mut self, c: char) {
        black_box(c);
        self.printed_bytes += c.len_utf8() as u64;
    }
    fn print_str(&mut self, text: &str) {
        black_box(text);
        self.printed_bytes += text.len() as u64;
    }
    fn execute(&mut self, byte: u8) {
        black_box(byte);
        self.controls += 1;
    }
    fn csi_dispatch(&mut self, params: &Params, inter: &[u8], ignore: bool, c: char) {
        black_box((params, inter, ignore, c));
        self.commands += 1;
    }
    fn esc_dispatch(&mut self, inter: &[u8], ignore: bool, byte: u8) {
        black_box((inter, ignore, byte));
        self.commands += 1;
    }
    fn osc_dispatch(&mut self, params: &[&[u8]], bell: bool) {
        black_box((params, bell));
        self.commands += 1;
    }
    fn apc_start(&mut self) {
        self.apc.clear();
    }
    fn apc_put(&mut self, byte: u8) {
        if self.collect_apc {
            self.apc.push(byte);
        } else {
            black_box(byte);
        }
    }
    fn apc_put_slice(&mut self, bytes: &[u8]) {
        if self.collect_apc {
            self.apc.extend_from_slice(bytes);
        } else {
            black_box(bytes);
        }
    }
    fn apc_end(&mut self) {
        black_box(&self.apc);
        self.commands += 1;
    }
}

#[derive(Default)]
pub struct Snapshot {
    rows: Vec<Row<Square>>,
    styles: Vec<RowStyles>,
    extras: SnapshotExtras,
    #[cfg(ghostty_bench_deferred_styles)]
    deferred: rio_vt::crosswords::snapshot::DeferredStyles,
}

impl Snapshot {
    fn update(&mut self, term: &mut Terminal, damage: TerminalDamage) {
        #[cfg(not(ghostty_bench_deferred_styles))]
        term.snapshot_visible(
            &damage,
            &mut self.rows,
            &mut self.styles,
            &mut self.extras,
        );
        #[cfg(ghostty_bench_deferred_styles)]
        {
            // Include both phases in elapsed time. The separate lock-time
            // measurement lives in snapshot_style_capture_timings in rio-vt.
            term.snapshot_visible_deferred(
                &damage,
                &mut self.rows,
                &mut self.styles,
                &mut self.extras,
                &mut self.deferred,
            )
            .resolve();
        }
        black_box((&self.rows, &self.styles, &self.extras));
        term.reset_damage();
    }
    fn checksum(&self) -> u64 {
        // Resolve styles and extras rather than hashing recyclable IDs. This
        // detects changes in glyphs, combining marks, colors, and hyperlink URIs.
        let mut bytes = Vec::new();
        for (y, row) in self.rows.iter().enumerate() {
            for (x, cell) in row.inner.iter().enumerate() {
                bytes.extend_from_slice(&(cell.c() as u32).to_le_bytes());
                bytes.push(cell.wide() as u8);
                bytes.push(cell.cell_flags().bits());
                #[cfg(not(ghostty_bench_upstream_api))]
                let style = self.styles[y].at(x);
                #[cfg(ghostty_bench_upstream_api)]
                let style = self.styles[y].get(x).copied().unwrap_or_default();
                bytes.extend_from_slice(format!("{style:?}").as_bytes());
                if let Some(extras) = cell.extras_id().and_then(|id| self.extras.get(&id))
                {
                    for c in &extras.zerowidth {
                        bytes.extend_from_slice(&(*c as u32).to_le_bytes());
                    }
                    if let Some(link) = &extras.hyperlink {
                        // Implicit hyperlink IDs come from a process-wide counter;
                        // their URI, rather than allocation order, is observable.
                        bytes.extend_from_slice(link.uri().as_bytes());
                    }
                }
                bytes.push(0xff);
            }
        }
        hash(bytes)
    }
}

// The two large variants both contain parser state and differ only modestly
// in size. Keep that state inline rather than changing timed pointer access
// when a snapshot API adds scratch storage.
#[allow(clippy::large_enum_variant)]
pub enum Workload {
    Parser {
        parser: Parser,
        sink: Sink,
        osc_records: Vec<Vec<u8>>,
    },
    Unicode {
        decoder: Decoder,
        value: u64,
    },
    Screen {
        term: Box<Terminal>,
        processor: Processor,
        snapshot: Snapshot,
        output: String,
    },
    Links {
        table: ExtrasTable,
        cells: Vec<Square>,
        link: Extras,
    },
}

impl Workload {
    pub fn new(opts: &Options, data: &[u8]) -> Result<Self> {
        match opts.action.as_str() {
            "terminal-parser" | "apc-parser" | "osc-parser" => Ok(Self::Parser {
                parser: Parser::new(),
                sink: Sink {
                    collect_apc: opts.action == "apc-parser",
                    ..Sink::default()
                },
                osc_records: if opts.action == "osc-parser" {
                    osc_records(data)?
                } else {
                    Vec::new()
                },
            }),
            "codepoint-width" | "grapheme-break" => Ok(Self::Unicode {
                decoder: Decoder::default(),
                value: 0,
            }),
            "hyperlink-map" => {
                let count = (opts.entries * opts.load_percent / 100).max(1);
                let mut table = ExtrasTable::new();
                let link = Extras {
                    hyperlink: Some(Hyperlink::new(
                        Some("1"),
                        "https://example.com/benchmark",
                    )),
                    ..Extras::default()
                };
                let id = table.alloc(link.clone());
                let mut cell = Square::from_char('x');
                cell.set_extras_id(Some(id));
                Ok(Self::Links {
                    table,
                    cells: vec![cell; count],
                    link,
                })
            }
            _ => {
                let mut term = Box::new(terminal(opts));
                let mut processor = Processor::default();
                if opts.action == "terminal-resize" {
                    fill(&mut term, &mut processor, opts);
                }
                if opts.action != "terminal-stream" {
                    for chunk in data.chunks(opts.chunk_size) {
                        processor.advance(term.as_mut(), chunk);
                    }
                }
                let mut snapshot = Snapshot::default();
                snapshot.update(&mut term, TerminalDamage::Full);
                Ok(Self::Screen {
                    term,
                    processor,
                    snapshot,
                    output: String::new(),
                })
            }
        }
    }

    // Captured before the timer: reflow cost depends on retained history, not
    // just the requested viewport dimensions or the size of the input file.
    pub fn setup_metrics(&self) -> std::collections::BTreeMap<&'static str, usize> {
        let mut metrics = std::collections::BTreeMap::new();
        if let Self::Screen { term, .. } = self {
            metrics.insert("columns", term.columns());
            metrics.insert("rows", term.screen_lines());
            metrics.insert("history_lines", term.grid.history_size());
        }
        metrics
    }

    pub fn run(
        &mut self,
        opts: &Options,
        input: &mut Input,
        buffer: &mut [u8],
    ) -> Result<()> {
        for iteration in 0..opts.loops {
            match self {
                Self::Parser {
                    parser,
                    sink,
                    osc_records,
                } => {
                    if opts.action == "osc-parser" {
                        // Ghostty's OSC-only parser consumes payload records. Rio
                        // exposes the integrated VT parser, so framing is timed too.
                        for record in osc_records.iter() {
                            parser.advance(sink, b"\x1b]");
                            for chunk in record.chunks(buffer.len()) {
                                parser.advance(sink, chunk);
                            }
                            parser.advance(sink, b"\x07");
                        }
                    } else {
                        input.replay(buffer, |chunk| parser.advance(sink, chunk))?;
                    }
                }
                Self::Unicode { decoder, value } => {
                    // Decode inside the timed region, as Ghostty does. Preserve a
                    // trailing UTF-8 prefix between reads; never decode chunks alone.
                    // Select the workload once per replay, not once per
                    // decoded scalar. Otherwise option-string comparisons
                    // obscure the cost of the Unicode operations themselves.
                    if opts.mode == "noop" {
                        let mut accept = |c: char| {
                            *value = value.wrapping_add(black_box(c) as u64);
                        };
                        input.replay(buffer, |chunk| decoder.feed(chunk, &mut accept))?;
                        decoder.finish(&mut accept);
                    } else if opts.action == "codepoint-width"
                        && (opts.mode == "reference"
                            || !cfg!(ghostty_bench_unicode_tables))
                    {
                        let mut accept = |c: char| {
                            *value = value
                                .wrapping_add(black_box(c.width().unwrap_or(0)) as u64);
                        };
                        input.replay(buffer, |chunk| decoder.feed(chunk, &mut accept))?;
                        decoder.finish(&mut accept);
                    } else if opts.action == "codepoint-width" {
                        #[cfg(ghostty_bench_unicode_tables)]
                        {
                            let table = rio_vt::codepoint_width::width_table();
                            let mut accept = |c: char| {
                                let width =
                                    rio_vt::codepoint_width::width_in(table, c as u32);
                                *value = value
                                    .wrapping_add(black_box(width.unwrap_or(0)) as u64);
                            };
                            input.replay(buffer, |chunk| {
                                decoder.feed(chunk, &mut accept)
                            })?;
                            decoder.finish(&mut accept);
                        }
                    } else if opts.mode == "reference"
                        || !cfg!(ghostty_bench_unicode_tables)
                    {
                        // A synthetic Control guarantees the initial boundary
                        // without testing an Option on every later scalar.
                        let mut previous = rio_unicode::grapheme::GraphemeClass::Control;
                        let mut state = BreakState::start(previous);
                        let mut accept = |c: char| {
                            let next = grapheme_class(c);
                            let boundary = is_break(previous, next, &mut state);
                            previous = next;
                            *value = value.wrapping_add(black_box(boundary) as u64);
                        };
                        input.replay(buffer, |chunk| decoder.feed(chunk, &mut accept))?;
                        decoder.finish(&mut accept);
                    } else {
                        #[cfg(ghostty_bench_unicode_tables)]
                        {
                            // Measure the terminal's packed transition table.
                            // Reference mode retains the scalar rule machine.
                            let mut previous =
                                rio_unicode::grapheme::GraphemeClass::Control as u8;
                            let mut state = 0;
                            let mut accept = |c: char| {
                                use rio_vt::grapheme_lut::{class_of, is_break_lut};
                                let next = class_of(c);
                                let boundary = is_break_lut(previous, next, &mut state);
                                previous = next;
                                *value = value.wrapping_add(black_box(boundary) as u64);
                            };
                            input.replay(buffer, |chunk| {
                                decoder.feed(chunk, &mut accept)
                            })?;
                            decoder.finish(&mut accept);
                        }
                    }
                }
                Self::Links { table, cells, link } => {
                    for cell in cells.iter_mut() {
                        if opts.mode == "churn" {
                            cell.set_extras_id(None);
                            // Borrow reusable payloads; ownership is copied
                            // only for a genuinely new interned value.
                            #[cfg(ghostty_bench_borrowed_extras)]
                            let id = table.intern(black_box(link));
                            #[cfg(not(ghostty_bench_borrowed_extras))]
                            let id = table.alloc(black_box(link.clone()));
                            cell.set_extras_id(Some(id));
                        } else {
                            black_box(table.get(cell.extras_id().unwrap()).unwrap());
                        }
                    }
                }
                Self::Screen {
                    term,
                    processor,
                    snapshot,
                    output,
                } => match opts.action.as_str() {
                    "terminal-stream" => {
                        input.replay(buffer, |chunk| {
                            processor.advance(term.as_mut(), chunk)
                        })?;
                        black_box(&term);
                    }
                    "terminal-resize" => {
                        let cols = if opts.mode == "cols" || opts.mode == "both" {
                            opts.resize_cols
                        } else {
                            opts.cols
                        };
                        let rows = if opts.mode == "rows" || opts.mode == "both" {
                            opts.resize_rows
                        } else {
                            opts.rows
                        };
                        term.resize(CrosswordsSize::new(cols, rows));
                        if opts.mode != "noop" {
                            term.resize(CrosswordsSize::new(opts.cols, opts.rows));
                        }
                        black_box(&term);
                    }
                    "screen-clone" => match opts.mode.as_str() {
                        "clone" => {
                            snapshot.rows = black_box(term.visible_rows());
                        }
                        "render" => snapshot.update(term, TerminalDamage::Full),
                        "render-clean" => snapshot.update(term, TerminalDamage::Noop),
                        "render-partial" => {
                            // Match Ghostty's dirty-row probe: no parsing or
                            // character changes are included in this timer.
                            // render-output separately measures output + capture.
                            term.grid[Line((opts.rows / 2) as i32)].dirty = true;
                            snapshot.update(term, TerminalDamage::Partial);
                        }
                        "render-scroll" | "render-scroll-page" => {
                            // Match Ghostty's one-row or half-page steps and
                            // reversal intervals, rather than jumping to endpoints.
                            let (step, interval) = if opts.mode == "render-scroll-page" {
                                ((opts.rows / 2).max(1) as i32, 40)
                            } else {
                                (1, 2000)
                            };
                            let direction = if (iteration / interval) % 2 == 0 {
                                1
                            } else {
                                -1
                            };
                            term.scroll_display(Scroll::Delta(step * direction));
                            snapshot.update(term, TerminalDamage::Full);
                        }
                        "render-output" => {
                            processor.advance(term.as_mut(), b"hello\r\n");
                            snapshot.update(term, TerminalDamage::Full);
                        }
                        _ => unreachable!(),
                    },
                    "terminal-formatter" => {
                        if opts.mode == "noop" {
                            black_box(&output);
                            continue;
                        }
                        let options = if opts.emit == "plain" {
                            FormatOptions::plain()
                        } else {
                            FormatOptions::vt()
                        };
                        #[cfg(ghostty_bench_reusable_format)]
                        term.format_into(options, output);
                        #[cfg(not(ghostty_bench_reusable_format))]
                        {
                            *output = term.format(options);
                        }
                        black_box(&output);
                        if opts.mode == "roundtrip" {
                            let mut decoded = terminal(opts);
                            Processor::default().advance(&mut decoded, output.as_bytes());
                            black_box(decoded);
                        }
                    }
                    _ => unreachable!(),
                },
            }
        }
        Ok(())
    }

    pub fn verify(&mut self, opts: &Options) -> Result<()> {
        if opts.action == "screen-clone" {
            if let Self::Screen { term, snapshot, .. } = self {
                // The reference must never repair the retained measured output.
                let mut reference = Snapshot::default();
                reference.update(term, TerminalDamage::Full);
                if snapshot.checksum() != reference.checksum() {
                    return Err(
                        "measured snapshot differs from a fresh full snapshot".into()
                    );
                }
            }
        }
        Ok(())
    }

    pub fn checksum(&mut self, opts: &Options) -> u64 {
        match self {
            Self::Parser { sink, .. } => hash(
                sink.printed_bytes
                    .to_le_bytes()
                    .into_iter()
                    .chain(sink.commands.to_le_bytes())
                    .chain(sink.controls.to_le_bytes())
                    .chain(sink.apc.iter().copied()),
            ),
            Self::Unicode { value, .. } => *value,
            Self::Screen {
                term,
                snapshot,
                output,
                ..
            } => {
                if opts.action != "screen-clone" {
                    snapshot.update(term, TerminalDamage::Full);
                }
                snapshot.checksum()
                    ^ hash(output.bytes())
                    ^ hash(format!("{:?}{:?}", term.cursor(), term.colors()).bytes())
            }
            Self::Links { table, cells, .. } => hash(cells.iter().flat_map(|c| {
                table
                    .get(c.extras_id().unwrap())
                    .unwrap()
                    .hyperlink
                    .as_ref()
                    .unwrap()
                    .uri()
                    .bytes()
            })),
        }
    }
}

// Ghostty's parser corpus uses a little-endian native usize length. Pin to
// 64-bit files here for portable, unambiguous interchange with desktop Ghostty.
fn osc_records(mut data: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut records = Vec::new();
    while !data.is_empty() {
        if data.len() < 8 {
            return Err("truncated OSC record length".into());
        }
        let len = u64::from_le_bytes(data[..8].try_into().unwrap());
        if len > 4096 || len > (data.len() - 8) as u64 {
            return Err("invalid or truncated OSC record".into());
        }
        let end = 8 + len as usize;
        records.push(data[8..end].to_vec());
        data = &data[end..];
    }
    Ok(records)
}

fn fill(term: &mut Terminal, processor: &mut Processor, opts: &Options) {
    // Same stressors as TerminalResize.zig: deterministic varying line length,
    // blank rows, periodic backgrounds, spaces, and wide characters. This PRNG
    // is deliberately fixed, rather than relying on a library RNG version.
    let mut state = 42_u64;
    for i in 0..opts.fill_lines {
        if i % 64 == 0 {
            processor.advance(term, b"\x1b[48;2;20;40;60m");
        }
        if i % 64 == 32 {
            processor.advance(term, b"\x1b[m");
        }
        if i % 16 == 15 {
            processor.advance(term, b"\r\n");
            continue;
        }
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let min = (opts.cols / 4).max(1);
        let max = (opts.cols * 5 / 2).clamp(min + 1, 16384.max(min + 1));
        let len = min + state as usize % (max - min);
        let mut line = String::new();
        for j in 0..len {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            line.push(if i % 8 == 7 && j % 16 == 8 {
                '漢'
            } else if j % 8 == 7 {
                ' '
            } else {
                (b'a' + (state >> 32) as u8 % 26) as char
            });
        }
        line.push_str("\r\n");
        processor.advance(term, line.as_bytes());
    }
}

/// Incremental maximal-subpart UTF-8 decoding for the standalone Unicode probes.
/// Use the terminal's SIMD prefix decoder, with Unicode replacement semantics
/// for malformed input. Raw C1 bytes are replacements here, not VT controls.
#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
    #[cfg(ghostty_bench_unicode_simd)]
    decoded: Vec<u32>,
}

// Header metadata keeps the malformed fallback from branching on every
// possible lead-byte family. Zero length denotes an invalid leading byte.
const UTF8_HEADERS: [(u8, u8, u8, u8); 256] = {
    let mut headers = [(0, 0, 0, 0); 256];
    let mut byte = 0;
    while byte < headers.len() {
        headers[byte] = match byte {
            0xc2..=0xdf => (2, 0x1f, 0x80, 0xbf),
            0xe0 => (3, 0x0f, 0xa0, 0xbf),
            0xed => (3, 0x0f, 0x80, 0x9f),
            0xe1..=0xef => (3, 0x0f, 0x80, 0xbf),
            0xf0 => (4, 7, 0x90, 0xbf),
            0xf4 => (4, 7, 0x80, 0x8f),
            0xf1..=0xf3 => (4, 7, 0x80, 0xbf),
            _ => (0, 0, 0, 0),
        };
        byte += 1;
    }
    headers
};

impl Decoder {
    #[inline]
    pub fn feed(&mut self, bytes: &[u8], accept: &mut impl FnMut(char)) {
        let combined;
        let mut bytes = if self.pending.is_empty() {
            bytes
        } else {
            combined = [self.pending.as_slice(), bytes].concat();
            self.pending.clear();
            &combined
        };
        if bytes.is_ascii() {
            for &byte in bytes {
                accept(byte as char);
            }
            return;
        }
        #[cfg(ghostty_bench_unicode_simd)]
        {
            let result = rio_vt::simd_utf8::decode_to_utf32(bytes, &mut self.decoded);
            for &cp in &self.decoded {
                // SAFETY: the shared decoder produces only validated scalars.
                accept(unsafe { char::from_u32_unchecked(cp) });
            }
            match result {
                Ok(()) => return,
                Err(err) => bytes = &bytes[err.valid_up_to()..],
            }
        }
        // Once an error is found, stay scalar for this read. Retrying the
        // SIMD entry point per invalid byte overwhelms malformed workloads.
        while !bytes.is_empty() {
            let first = bytes[0];
            if first < 0x80 {
                accept(first as char);
                bytes = &bytes[1..];
                continue;
            }
            let (needed, mask, min_second, max_second) = UTF8_HEADERS[first as usize];
            if needed == 0 {
                accept('\u{fffd}');
                bytes = &bytes[1..];
                continue;
            }
            let Some(&second) = bytes.get(1) else {
                self.pending.extend_from_slice(bytes);
                return;
            };
            if !(min_second..=max_second).contains(&second) {
                accept('\u{fffd}');
                bytes = &bytes[1..];
                continue;
            }
            let mut cp = (u32::from(first & mask) << 6) | u32::from(second & 0x3f);
            let needed = needed as usize;
            let mut used = 2;
            while used < needed {
                let Some(&byte) = bytes.get(used) else {
                    self.pending.extend_from_slice(bytes);
                    return;
                };
                if !(0x80..=0xbf).contains(&byte) {
                    break;
                }
                cp = (cp << 6) | u32::from(byte & 0x3f);
                used += 1;
            }
            if used == needed {
                // SAFETY: lead and continuation bounds exclude overlong
                // encodings, surrogates, and scalars above U+10FFFF.
                accept(unsafe { char::from_u32_unchecked(cp) });
            } else {
                // Consume only the valid prefix of an invalid sequence.
                accept('\u{fffd}');
            }
            bytes = &bytes[used..];
        }
    }
    pub fn finish(&mut self, accept: &mut impl FnMut(char)) {
        if !self.pending.is_empty() {
            accept('\u{fffd}');
            self.pending.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_decoder_matches_lossy_utf8_across_chunk_boundaries() {
        for first in 0..=255u8 {
            for second in 0..=255u8 {
                let input = [b'x', 0xc3, 0xa9, first, second, 0xf0, 0x9f, 0x92];
                let expected = String::from_utf8_lossy(&input);
                for size in [1, 3, input.len()] {
                    let mut decoder = Decoder::default();
                    let mut actual = String::new();
                    let mut accept = |c| actual.push(c);
                    for chunk in input.chunks(size) {
                        decoder.feed(chunk, &mut accept);
                    }
                    decoder.finish(&mut accept);
                    assert_eq!(actual, expected, "{input:02x?} chunk={size}");
                }
            }
        }
    }

    #[test]
    fn stale_measured_snapshot_is_rejected_without_repairing_its_fingerprint() {
        let opts = Options::parse(
            [
                "screen-clone",
                "--mode",
                "render-clean",
                "--data",
                "unused",
                "--terminal-cols",
                "10",
                "--terminal-rows",
                "2",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap();
        let mut work = Workload::new(&opts, b"expected").unwrap();
        if let Workload::Screen { snapshot, .. } = &mut work {
            snapshot.rows[0].inner[0].set_c('!');
        }
        let broken = work.checksum(&opts);
        assert!(work.verify(&opts).is_err());
        assert_eq!(work.checksum(&opts), broken);
    }
}
