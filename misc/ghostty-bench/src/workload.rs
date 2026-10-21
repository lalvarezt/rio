// Adapted workload shapes from Ghostty src/benchmark at the revision in main.rs.
// Rio's storage and APIs are used throughout; this is not a Ghostty speed proxy.
use crate::{Input, Options, Result};
use rio_unicode::{
    grapheme::{grapheme_class, is_break, BreakState},
    UnicodeWidthChar,
};
use rio_vt::ansi::CursorShape;
use rio_vt::crosswords::formatter::FormatOptions;
use rio_vt::crosswords::grid::{row::Row, ExtrasTable, Scroll};
#[cfg(not(ghostty_bench_upstream_api))]
use rio_vt::crosswords::snapshot::{RowStyles, SnapshotExtras};
#[cfg(ghostty_bench_upstream_api)]
type RowStyles = Vec<rio_vt::crosswords::style::Style>;
#[cfg(ghostty_bench_upstream_api)]
type SnapshotExtras = rustc_hash::FxHashMap<u16, Extras>;
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
                    let mut previous = None;
                    let mut accept = |c: char| {
                        if opts.action == "codepoint-width" {
                            *value = value.wrapping_add(if opts.mode == "noop" {
                                black_box(c) as u64
                            } else {
                                black_box(c.width().unwrap_or(0)) as u64
                            });
                        } else if opts.mode == "noop" {
                            *value = value.wrapping_add(black_box(c) as u64);
                        } else {
                            let next = grapheme_class(c);
                            let boundary = match &mut previous {
                                Some((prev, state)) => {
                                    let boundary = is_break(*prev, next, state);
                                    *prev = next;
                                    boundary
                                }
                                None => {
                                    previous = Some((next, BreakState::start(next)));
                                    true
                                }
                            };
                            *value = value.wrapping_add(black_box(boundary) as u64);
                        }
                    };
                    input.replay(buffer, |chunk| decoder.feed(chunk, &mut accept))?;
                    decoder.finish(&mut accept);
                }
                Self::Links { table, cells, link } => {
                    for cell in cells.iter_mut() {
                        if opts.mode == "churn" {
                            cell.set_extras_id(None);
                            cell.set_extras_id(Some(
                                table.alloc(black_box(link.clone())),
                            ));
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
                            processor.advance(
                                term.as_mut(),
                                if iteration % 2 == 0 {
                                    b"\x1b[1;1HX"
                                } else {
                                    b"\x1b[1;1HY"
                                },
                            );
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
                        *output = term.format(if opts.emit == "plain" {
                            FormatOptions::plain()
                        } else {
                            FormatOptions::vt()
                        });
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
/// This measures Rust's decoder plus Rio's tables, not Ghostty's decoder/SIMD.
#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
}

impl Decoder {
    pub fn feed(&mut self, bytes: &[u8], accept: &mut impl FnMut(char)) {
        let combined;
        let mut bytes = if self.pending.is_empty() {
            bytes
        } else {
            combined = [self.pending.as_slice(), bytes].concat();
            self.pending.clear();
            &combined
        };
        while !bytes.is_empty() {
            match std::str::from_utf8(bytes) {
                Ok(text) => {
                    text.chars().for_each(&mut *accept);
                    break;
                }
                Err(err) => {
                    std::str::from_utf8(&bytes[..err.valid_up_to()])
                        .unwrap()
                        .chars()
                        .for_each(&mut *accept);
                    bytes = &bytes[err.valid_up_to()..];
                    match err.error_len() {
                        Some(len) => {
                            accept('\u{fffd}');
                            bytes = &bytes[len..];
                        }
                        None => {
                            self.pending.extend_from_slice(bytes);
                            break;
                        }
                    }
                }
            }
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
