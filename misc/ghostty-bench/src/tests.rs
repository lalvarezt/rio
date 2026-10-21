use super::*;
use workload::Decoder;

fn options(action: &str, flags: &[&str]) -> Options {
    Options::parse(
        [
            action,
            "--data",
            "unused",
            "--terminal-cols",
            "20",
            "--terminal-rows",
            "6",
            "--fill-lines",
            "20",
        ]
        .into_iter()
        .chain(flags.iter().copied())
        .map(str::to_owned),
    )
    .unwrap()
}

fn checksum(mut opts: Options, data: &[u8], chunk: usize) -> u64 {
    opts.chunk_size = chunk;
    let mut work = Workload::new(&opts, data).unwrap();
    work.run(
        &opts,
        &mut Input::Memory(data.to_vec()),
        &mut vec![0; chunk],
    )
    .unwrap();
    work.verify(&opts).unwrap();
    work.checksum(&opts)
}

#[test]
fn terminal_and_parser_replays_are_independent_of_read_boundaries() {
    let data = b"a\xe6\xbc\xa2e\xcc\x81\x1b[31mred\x1b[0m\r\n\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\\x1b_Ga=q;AAAA\x1b\\\xe2\x28\xa1end";
    for action in ["terminal-parser", "terminal-stream", "apc-parser"] {
        let opts = options(action, &[]);
        let expected = checksum(opts.clone(), data, data.len());
        for chunk in 1..=data.len() {
            assert_eq!(
                checksum(opts.clone(), data, chunk),
                expected,
                "{action}, chunk {chunk}"
            );
        }
    }
}

#[test]
fn unicode_decoder_matches_maximal_subpart_replacement_at_every_split() {
    for bytes in [
        b"ascii".as_slice(),
        "漢e\u{301}👩\u{200d}💻".as_bytes(),
        b"\xe2\x82",
        b"\xf0\x90\x80",
        b"\xf4\x90\x80\x80",
        b"\xed\xa0\x80",
        b"\xe2(\xa1",
    ] {
        for chunk in 1..=bytes.len() {
            let mut decoder = Decoder::default();
            let mut text = String::new();
            let mut accept = |c| text.push(c);
            for bytes in bytes.chunks(chunk) {
                decoder.feed(bytes, &mut accept);
            }
            decoder.finish(&mut accept);
            assert_eq!(text, String::from_utf8_lossy(bytes));
        }
    }
}

#[test]
fn unicode_probes_keep_widths_and_graphemes_across_reads() {
    let data = "a漢e\u{301}👩\u{200d}💻🇪🇸".as_bytes();
    for action in ["codepoint-width", "grapheme-break"] {
        let opts = options(action, &[]);
        let expected = checksum(opts.clone(), data, data.len());
        for chunk in 1..=data.len() {
            assert_eq!(checksum(opts.clone(), data, chunk), expected);
            assert_eq!(
                checksum(options(action, &["--mode", "reference"]), data, chunk),
                expected
            );
        }
    }
    assert_eq!(checksum(options("grapheme-break", &[]), data, 1), 5);
}

#[test]
fn production_and_reference_unicode_agree_on_malformed_and_initial_boundaries() {
    for data in [
        b"\r\na\xff\xed\xa0\x80\xf4\x90\x80\x80\xe2(\xa1".as_slice(),
        "\u{301}\u{600}e\u{301}\u{110bd}\u{20000}".as_bytes(),
    ] {
        for action in ["codepoint-width", "grapheme-break"] {
            let expected =
                checksum(options(action, &["--mode", "reference"]), data, data.len());
            for chunk in 1..=data.len() {
                assert_eq!(checksum(options(action, &[]), data, chunk), expected);
            }
        }
    }
}

#[test]
fn surrogate_encoding_replay_is_independent_of_read_boundaries() {
    for action in ["terminal-parser", "terminal-stream"] {
        let opts = options(action, &[]);
        assert_eq!(
            checksum(opts.clone(), b"\xed\xa0\x80", 1),
            checksum(opts, b"\xed\xa0\x80", 65536)
        );
    }
}

#[test]
fn osc_records_reject_truncation_and_oversize() {
    for data in [
        vec![0; 7],
        [100_u64.to_le_bytes().as_slice(), b"0;x"].concat(),
        [4097_u64.to_le_bytes().as_slice(), &vec![b'x'; 4097]].concat(),
    ] {
        assert!(Workload::new(&options("osc-parser", &[]), &data).is_err());
    }
    let body = b"0;title";
    let data = [(body.len() as u64).to_le_bytes().as_slice(), body].concat();
    assert_eq!(
        checksum(options("osc-parser", &[]), &data, 1),
        checksum(options("osc-parser", &[]), &data, 65536)
    );
}

#[test]
fn clean_and_full_snapshots_agree_without_mutations() {
    let data = "\x1b[31m漢e\u{301}\x1b[0m\x1b]8;;https://example.com\x07link".as_bytes();
    let clean = checksum(
        options("screen-clone", &["--mode", "render-clean"]),
        data,
        7,
    );
    let full = checksum(options("screen-clone", &["--mode", "render"]), data, 7);
    assert_eq!(clean, full);
}

#[test]
fn hyperlink_churn_preserves_lookup_results() {
    assert_eq!(
        checksum(options("hyperlink-map", &["--mode", "lookup"]), b"", 10),
        checksum(options("hyperlink-map", &["--mode", "churn"]), b"", 10)
    );
}

#[test]
fn resize_cycles_and_formatter_modes_produce_repeatable_results() {
    for (action, mode) in [
        ("terminal-resize", "cols"),
        ("terminal-resize", "rows"),
        ("terminal-resize", "both"),
        ("terminal-formatter", "format"),
        ("terminal-formatter", "roundtrip"),
    ] {
        let opts = options(action, &["--mode", mode]);
        assert_eq!(
            checksum(opts.clone(), b"\x1b[31mexample text\r\n", 3),
            checksum(opts, b"\x1b[31mexample text\r\n", 65536)
        );
    }
}

#[test]
fn invalid_cli_inputs_do_not_silently_run_another_workload() {
    for args in [
        vec!["terminal-parser"],
        vec!["page-compression"],
        vec!["terminal-resize", "--terminal-cols", "1"],
        vec!["terminal-resize", "--mode", "cols-no-reflow"],
        vec!["terminal-resize", "--chunk-size", "0"],
        vec!["terminal-resize", "--loops", "0"],
        vec!["terminal-resize", "--mdoe", "cols"],
        vec![
            "terminal-formatter",
            "--data",
            "unused",
            "--mode",
            "roundtrip",
            "--emit",
            "plain",
        ],
    ] {
        assert!(Options::parse(args.into_iter().map(str::to_owned)).is_err());
    }
}
