// Workloads adapted from Ghostty 48dde97e0413a27064e4c6182db3df6ee999db7d.
// See README.md for timing boundaries and LICENSE.ghostty for attribution.
mod workload;

use serde::Serialize;
use std::collections::BTreeMap;
use std::error::Error;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Instant;
use workload::Workload;

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const GHOSTTY_REVISION: &str = "48dde97e0413a27064e4c6182db3df6ee999db7d";
const HELP: &str = "ghostty-bench <action> [options]
Actions and modes:
  terminal-parser                     VT parser, no terminal mutation
  terminal-stream                     VT parser + terminal mutation
  osc-parser                          length-prefixed OSC records, framed for Rio
  apc-parser                          APC framing and accumulation, no image decode
  codepoint-width     --mode table|noop
  grapheme-break      --mode table|noop
  terminal-resize     --mode cols|rows|both|noop
  screen-clone        --mode clone|render|render-clean|render-partial|render-scroll|render-scroll-page|render-output
  terminal-formatter  --mode format|roundtrip|noop --emit vt|plain
  hyperlink-map      --mode lookup|churn
Options:
  --data PATH            Existing corpus file, or - for stdin
  --io file|memory       Timed file reads (default), or preloaded memory
  --terminal-cols N      120 by default, minimum 2
  --terminal-rows N      80 by default
  --resize-cols N        Half the initial width by default
  --resize-rows N        Half the initial height by default
  --fill-lines N         Deterministic resize setup lines, default 10000
  --scrollback-lines N   Rio history limit, default 10000
  --entries N            Hyperlink cell count, default 4096
  --load-percent N       Hyperlink occupancy, default 100
  --chunk-size N         Stream input chunk size, default 65536
  --loops N              Operations per sample, default 1
  --samples N            Measured samples, default 5
  --warmups N            Unmeasured samples, default 2
Outputs one JSON object. Setup, verification, and result hashing are untimed.
Use release builds. See misc/ghostty-bench/README.md for migration differences.";

#[derive(Clone, Debug, Serialize)]
pub struct Options {
    action: String,
    mode: String,
    emit: String,
    data: Option<PathBuf>,
    io: String,
    cols: usize,
    rows: usize,
    resize_cols: usize,
    resize_rows: usize,
    fill_lines: usize,
    scrollback: usize,
    entries: usize,
    load_percent: usize,
    chunk_size: usize,
    loops: usize,
    samples: usize,
    warmups: usize,
}

impl Options {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut args = args.into_iter();
        let action = args.next().ok_or("missing action; use --help")?;
        let mut values = BTreeMap::new();
        while let Some(arg) = args.next() {
            let (key, value) = match arg.split_once('=') {
                Some((key, value)) => (key.to_owned(), value.to_owned()),
                None => (arg, args.next().ok_or("missing option value")?),
            };
            if !key.starts_with("--") || values.insert(key, value).is_some() {
                return Err("unexpected or duplicate option".into());
            }
        }
        let mut take = |key: &str, default: &str| {
            values.remove(key).unwrap_or_else(|| default.to_owned())
        };
        let cols = take("--terminal-cols", "120").parse()?;
        let rows = take("--terminal-rows", "80").parse()?;
        let default_mode = match action.as_str() {
            "codepoint-width" | "grapheme-break" => "table",
            "terminal-resize" => "cols",
            "screen-clone" => "clone",
            "terminal-formatter" => "format",
            "hyperlink-map" => "churn",
            _ => "default",
        };
        let opts = Self {
            action,
            mode: take("--mode", default_mode),
            emit: take("--emit", "vt"),
            data: match take("--data", "").as_str() {
                "" => None,
                path => Some(PathBuf::from(path)),
            },
            io: take("--io", "file"),
            cols,
            rows,
            resize_cols: take("--resize-cols", &(cols / 2).max(2).to_string()).parse()?,
            resize_rows: take("--resize-rows", &(rows / 2).max(1).to_string()).parse()?,
            fill_lines: take("--fill-lines", "10000").parse()?,
            scrollback: take("--scrollback-lines", "10000").parse()?,
            entries: take("--entries", "4096").parse()?,
            load_percent: take("--load-percent", "100").parse()?,
            chunk_size: take("--chunk-size", "65536").parse()?,
            loops: take("--loops", "1").parse()?,
            samples: take("--samples", "5").parse()?,
            warmups: take("--warmups", "2").parse()?,
        };
        if !values.is_empty() {
            return Err(format!("unknown options: {:?}", values.keys()).into());
        }
        opts.validate()?;
        Ok(opts)
    }

    fn validate(&self) -> Result<()> {
        let modes: &[&str] = match self.action.as_str() {
            "terminal-parser" | "terminal-stream" | "osc-parser" | "apc-parser" => {
                &["default"]
            }
            "codepoint-width" | "grapheme-break" => &["table", "noop"],
            "terminal-resize" => &["cols", "rows", "both", "noop"],
            "screen-clone" => &[
                "clone",
                "render",
                "render-clean",
                "render-partial",
                "render-scroll",
                "render-scroll-page",
                "render-output",
            ],
            "terminal-formatter" => &["format", "roundtrip", "noop"],
            "hyperlink-map" => &["lookup", "churn"],
            _ => {
                return Err(
                    format!("unsupported action {}; use --help", self.action).into()
                )
            }
        };
        if !modes.contains(&self.mode.as_str()) {
            return Err(
                format!("unsupported mode {} for {}", self.mode, self.action).into(),
            );
        }
        if !["vt", "plain"].contains(&self.emit.as_str())
            || !["file", "memory"].contains(&self.io.as_str())
            || (self.mode == "roundtrip" && self.emit != "vt")
        {
            return Err("invalid emit/io option; roundtrip requires --emit vt".into());
        }
        if self.cols < 2
            || self.rows == 0
            || self.resize_cols < 2
            || self.resize_rows == 0
            || self.cols > 32767
            || self.rows > 32767
            || self.resize_cols > 32767
            || self.resize_rows > 32767
            || self.chunk_size == 0
            || self.loops == 0
            || self.samples == 0
            || self.entries == 0
            || self.entries >= u16::MAX as usize
            || self.load_percent > 100
        {
            return Err("dimensions, counts, or occupancy are out of range".into());
        }
        if self.data.is_none()
            && !["terminal-resize", "hyperlink-map"].contains(&self.action.as_str())
        {
            return Err(
                "--data is required; generate a corpus before benchmarking".into()
            );
        }
        Ok(())
    }
}

enum Input {
    Memory(Vec<u8>),
    File(File),
}

impl Input {
    fn open(opts: &Options) -> Result<Self> {
        let Some(path) = &opts.data else {
            return Ok(Self::Memory(Vec::new()));
        };
        if path.as_os_str() == "-" {
            if opts.io != "memory" {
                return Err(
                    "stdin requires --io memory so samples can replay identical bytes"
                        .into(),
                );
            }
            let mut data = Vec::new();
            io::stdin().read_to_end(&mut data)?;
            return Ok(Self::Memory(data));
        }
        if opts.io == "memory" {
            Ok(Self::Memory(std::fs::read(path)?))
        } else {
            Ok(Self::File(File::open(path)?))
        }
    }

    fn read_all(&mut self) -> Result<Vec<u8>> {
        match self {
            Self::Memory(data) => Ok(data.clone()),
            Self::File(file) => {
                file.rewind()?;
                let mut data = Vec::new();
                file.read_to_end(&mut data)?;
                Ok(data)
            }
        }
    }

    fn replay(&mut self, buf: &mut [u8], mut consume: impl FnMut(&[u8])) -> Result<()> {
        match self {
            Self::Memory(data) => data.chunks(buf.len()).for_each(consume),
            Self::File(file) => {
                file.seek(SeekFrom::Start(0))?;
                loop {
                    let n = match file.read(buf) {
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        result => result?,
                    };
                    if n == 0 {
                        break;
                    }
                    consume(&buf[..n]);
                }
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct Report<'a> {
    suite: &'static str,
    version: u32,
    ghostty_revision: &'static str,
    optimized_build: bool,
    snapshot_api: &'static str,
    rio_revision: &'static str,
    rio_dirty: Option<bool>,
    rustc_version: &'static str,
    options: &'a Options,
    input_bytes: usize,
    checksum: String,
    sample_ns: Vec<u128>,
    median_ns_per_operation: f64,
}

fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|v| v == "--help" || v == "-h") {
        println!("{HELP}");
        return Ok(());
    }
    let opts = Options::parse(args)?;
    let mut input = Input::open(&opts)?;
    let data = input.read_all()?;
    if data.is_empty() && opts.data.is_some() {
        return Err("empty input corpus".into());
    }
    let mut times = Vec::with_capacity(opts.samples);
    let mut checksum = None;
    for sample in 0..opts.warmups + opts.samples {
        // A fresh workload per sample prevents warmups changing scrollback size,
        // reflow contents, or cache state for the first measured operation.
        let mut work = Workload::new(&opts, &data)?;
        let mut buffer = vec![0; opts.chunk_size];
        let start = Instant::now();
        work.run(&opts, &mut input, &mut buffer)?;
        let elapsed = start.elapsed().as_nanos();
        work.verify(&opts)?;
        let current = work.checksum(&opts);
        if checksum.is_some_and(|previous| previous != current) {
            return Err("workload output changed between samples".into());
        }
        checksum = Some(current);
        if sample >= opts.warmups {
            times.push(elapsed);
        }
    }
    let mut sorted = times.clone();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    let median = if sorted.len() % 2 == 0 {
        (sorted[middle - 1] as f64 + sorted[middle] as f64) / 2.0
    } else {
        sorted[middle] as f64
    };
    println!(
        "{}",
        serde_json::to_string(&Report {
            suite: "ghostty-bench",
            version: 1,
            ghostty_revision: GHOSTTY_REVISION,
            optimized_build: !cfg!(debug_assertions),
            snapshot_api: if cfg!(ghostty_bench_upstream_api) {
                "upstream-vec"
            } else {
                "retained-rows"
            },
            rio_revision: env!("GHOSTTY_BENCH_RIO_REVISION"),
            rio_dirty: match env!("GHOSTTY_BENCH_RIO_DIRTY") {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            },
            rustc_version: env!("GHOSTTY_BENCH_RUSTC_VERSION"),
            options: &opts,
            input_bytes: data.len(),
            checksum: format!("{:016x}", checksum.unwrap()),
            sample_ns: times,
            median_ns_per_operation: median / opts.loops as f64,
        })?
    );
    Ok(())
}

fn main() {
    if let Err(err) = run() {
        eprintln!("ghostty-bench: {err}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests;
