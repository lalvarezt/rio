//! Replay a Ghostty fuzz corpus and compare observable state across read boundaries.
//! The comparison covers active cells and history, cursors, styles, cell extras,
//! modes, replies, and titles. It does not cover inactive screen state, tab stops,
//! or margins.
//! Directory replay:
//! cargo run -p rio-vt --no-default-features --example audit_corpus --
//!   ../ghostty/test/fuzz-libghostty/corpus /tmp/rio-corpus-audit
//! A single input path runs the same comparison and writes a minimized failure.

use rio_vt::ansi::CursorShape;
use rio_vt::crosswords::grid::Dimensions;
use rio_vt::crosswords::pos::{Column, Cursor, Line};
use rio_vt::crosswords::square::Square;
use rio_vt::crosswords::style::Style;
use rio_vt::crosswords::{Crosswords, CrosswordsSize};
use rio_vt::event::{EventListener, RioEvent, WindowId};
use rio_vt::performer::handler::Processor;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Clone)]
struct Recorder(Rc<RefCell<Vec<String>>>);

impl EventListener for Recorder {
    fn send_event(&self, event: RioEvent, _: WindowId) {
        let event = match event {
            RioEvent::PtyWrite(_, text) => format!("reply:{text:?}"),
            RioEvent::Title(_, text) => format!("title:{text:?}"),
            _ => return,
        };
        self.0.borrow_mut().push(event);
    }
}

type ExtrasState = (Vec<char>, Option<(usize, String)>);
type Cell = (char, u8, bool, Style, Option<ExtrasState>);
type CursorState = (i32, usize, bool, Style, Option<ExtrasState>, String);

#[derive(Debug, PartialEq)]
struct State {
    dimensions: (usize, usize, usize),
    cells: Vec<Cell>,
    cursor: CursorState,
    saved: CursorState,
    modes: String,
    events: Vec<String>,
}

fn extras_state(
    term: &Crosswords<Recorder>,
    cell: Square,
    links: &mut HashMap<String, usize>,
) -> Option<ExtrasState> {
    let extras = cell
        .extras_id()
        .and_then(|id| term.grid.extras_table.get(id))?;
    let hyperlink = extras.hyperlink.as_ref().map(|link| {
        // Normalize hyperlink IDs while preserving grouping and destinations.
        // This comparison does not check literal explicit IDs.
        let next = links.len();
        let id = *links.entry(link.id().to_owned()).or_insert(next);
        (id, link.uri().to_owned())
    });
    Some((extras.zerowidth.clone(), hyperlink))
}

fn cursor_state(
    term: &Crosswords<Recorder>,
    cursor: &Cursor<Square>,
    links: &mut HashMap<String, usize>,
) -> CursorState {
    (
        cursor.pos.row.0,
        cursor.pos.col.0,
        cursor.should_wrap,
        cursor.pending_style,
        extras_state(term, cursor.template, links),
        format!("{:?}", cursor.charsets),
    )
}

fn difference(baseline: &State, actual: &State) -> String {
    let mut differences = Vec::new();
    if baseline.dimensions != actual.dimensions {
        differences.push("dimensions".to_owned());
    }
    if baseline.cursor != actual.cursor {
        differences.push(format!(
            "cursor {:?} != {:?}",
            baseline.cursor, actual.cursor
        ));
    }
    if baseline.saved != actual.saved {
        differences.push("saved cursor".to_owned());
    }
    if baseline.modes != actual.modes {
        differences.push("modes".to_owned());
    }
    if baseline.events != actual.events {
        differences.push("replies or titles".to_owned());
    }
    if baseline.cells != actual.cells {
        let index = baseline
            .cells
            .iter()
            .zip(&actual.cells)
            .position(|(a, b)| a != b);
        differences.push(match index {
            Some(i) => {
                format!("cell {i}: {:?} != {:?}", baseline.cells[i], actual.cells[i])
            }
            None => "cell count".to_owned(),
        });
    }
    differences.join("; ")
}

fn cell_state(
    term: &Crosswords<Recorder>,
    cell: Square,
    links: &mut HashMap<String, usize>,
) -> Cell {
    (
        cell.c(),
        cell.wide() as u8,
        cell.wrapline(),
        term.grid.style_of(&cell),
        extras_state(term, cell, links),
    )
}

fn replay(input: &[u8], chunk_size: usize) -> State {
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut term = Crosswords::new(
        CrosswordsSize::new(20, 8),
        CursorShape::Block,
        Recorder(events.clone()),
        WindowId::from(0),
        0,
        100,
    );
    let mut parser = Processor::default();
    for chunk in input.chunks(chunk_size.max(1)) {
        parser.advance(&mut term, chunk);
    }
    // Compare the final buffered content, not elapsed render-hold time. A
    // replayed buffer can begin another hold, so flush each remaining one.
    for _ in 0..input.len().saturating_add(1) {
        if parser.sync_timeout().sync_timeout().is_none() {
            break;
        }
        parser.stop_sync(&mut term);
    }
    let mut cells = Vec::new();
    let mut links = HashMap::new();
    for row in -(term.history_size() as i32)..term.grid.screen_lines() as i32 {
        for col in 0..term.grid.columns() {
            cells.push(cell_state(
                &term,
                term.grid[Line(row)][Column(col)],
                &mut links,
            ));
        }
    }
    let state = State {
        dimensions: (
            term.grid.columns(),
            term.grid.screen_lines(),
            term.history_size(),
        ),
        cells,
        cursor: cursor_state(&term, &term.grid.cursor, &mut links),
        saved: cursor_state(&term, &term.grid.saved_cursor, &mut links),
        modes: format!("{:?}", term.mode()),
        events: events.borrow().clone(),
    };
    state
}

fn failure(input: &[u8]) -> Option<String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let baseline = replay(input, input.len());
        for chunk_size in [1, 7, 64] {
            let actual = replay(input, chunk_size);
            if actual != baseline {
                return Some(format!(
                    "chunk {chunk_size}: {}",
                    difference(&baseline, &actual)
                ));
            }
        }
        None
    })) {
        Ok(reason) => reason,
        Err(panic) => Some(format!(
            "panic: {}",
            panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("unknown panic")
        )),
    }
}

fn minimize(mut input: Vec<u8>) -> Vec<u8> {
    let mut granularity = 2;
    let mut attempts = 0;
    while input.len() >= 2 && attempts < 10000 {
        let size = input.len().div_ceil(granularity);
        let mut reduced = false;
        for start in (0..input.len()).step_by(size) {
            let end = (start + size).min(input.len());
            let candidate = [&input[..start], &input[end..]].concat();
            attempts += 1;
            if failure(&candidate).is_some() {
                input = candidate;
                granularity = granularity.saturating_sub(1).max(2);
                reduced = true;
                break;
            }
        }
        if !reduced {
            if granularity >= input.len() {
                break;
            }
            granularity = (granularity * 2).min(input.len());
        }
    }
    eprintln!(
        "minimization: {attempts} candidates, {} bytes retained",
        input.len()
    );
    input
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let root = PathBuf::from(args.next().ok_or("missing corpus directory")?);
    let output = PathBuf::from(args.next().ok_or("missing report directory")?);
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }
    fs::create_dir_all(&output)?;
    if root.is_file() {
        let input = fs::read(&root)?;
        if failure(&input).is_none() {
            return Err("input no longer fails".into());
        }
        let input = minimize(input);
        fs::write(output.join("minimal.bin"), &input)?;
        fs::write(
            output.join("reason.txt"),
            failure(&input).ok_or("minimization lost failure")?,
        )?;
        println!("minimized reproduction: {} bytes", input.len());
        return Ok(());
    }
    let mut files = Vec::new();
    for family in [
        "osc-initial",
        "osc-cmin",
        "parser-initial",
        "parser-cmin",
        "stream-initial",
        "stream-cmin",
    ] {
        for entry in fs::read_dir(root.join(family))? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut seen = HashSet::new();
    let mut failures = Vec::new();
    let mut total_bytes = 0;
    for file in &files {
        let input = fs::read(file)?;
        if !seen.insert(input.clone()) {
            continue;
        }
        total_bytes += input.len();
        let reason = failure(&input);
        if let Some(reason) = reason {
            let number = failures.len();
            fs::write(output.join(format!("failure-{number:04}.bin")), &input)?;
            failures.push(format!("{}\t{reason}", file.strip_prefix(&root)?.display()));
        }
        if seen.len() % 250 == 0 {
            eprintln!("{} unique inputs; {} failures", seen.len(), failures.len());
        }
    }
    std::panic::set_hook(original_hook);
    fs::write(output.join("failures.tsv"), failures.join("\n"))?;
    let summary = format!(
        "{} paths; {} unique inputs; {total_bytes} bytes; {} failures\n",
        files.len(),
        seen.len(),
        failures.len()
    );
    fs::write(output.join("summary.txt"), &summary)?;
    print!("{summary}");
    if !failures.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}
