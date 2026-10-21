"""Translate shared cases to native CLI operations, with explicit cost caveats."""
import suite


# Ghostty multiplies --loops internally. compare.py checks these source contracts
# before building; otherwise a changed HEAD could silently corrupt ns/op ratios.
FRAMES = {
    "clone": 1000, "render": 50000, "render-clean": 3000000,
    "render-partial": 2000000, "render-scroll": 50000,
    "render-scroll-page": 50000, "render-output": 50000,
}
RESIZES = {"cols": 25, "rows": 2500000, "both": 25, "noop": 50000000}


def cases(short_data, long_data):
    for name, action, profile, flags, _ in suite.cases():
        mode = dict(zip(flags[::2], flags[1::2])).get("--mode")
        native = ["+" + action]
        rio = [action, "--samples", "1", "--warmups", "0", "--io", "file"]
        native_flags = list(flags)
        operations = 1
        long_input = action in (
            "terminal-parser", "terminal-stream", "codepoint-width",
            "grapheme-break", "osc-parser", "apc-parser",
        )
        data = (long_data if long_input else short_data) / f"{profile}.bin"
        chunk = 4096 if action in (
            "terminal-parser", "codepoint-width", "grapheme-break",
        ) else 65536
        rio += ["--chunk-size", str(chunk), "--scrollback-lines", "10000"]
        if action == "screen-clone":
            operations = FRAMES[mode]
            native_flags += ["--loops", "1"]
            note = "Native snapshot representations differ; capture and style/extras resolution are timed."
            if mode == "clone":
                note = "Ghostty clones a complete Screen object and retains all 1000 copies; Rio replaces visible-row snapshots. Different work, not an equivalent clone speedup."
            if mode == "render-clean":
                note = "3000000 idle updates after a full initial snapshot; no terminal mutations. Rio checks before/after visible-state fingerprints outside the timer."
            if mode == "render-partial":
                note = "2000000 one-row dirty updates at the middle of the active viewport. Both mark the row without parsing or changing cell contents."
        elif action == "terminal-resize":
            operations = RESIZES[mode]
            native_flags += ["--loops", "1", "--fill-lines", "0",
                             "--resize-cols", "60", "--resize-rows", "40",
                             "--scrollback-bytes", "50000000"]
            rio += ["--fill-lines", "0", "--resize-cols", "60", "--resize-rows", "40"]
            note = "One shrink-and-restore cycle per operation (two resizes). Same corpus and 10000-line history limits; actual setup dimensions and history are checked across engines."
        elif action == "terminal-formatter":
            operations = 1 if mode == "roundtrip" else 1000
            native_flags += ["--loops", str(operations), "--formatter", "screen",
                             "--region", "active"]
            note = "Different serializer allocation and roundtrip verification."
        elif action == "hyperlink-map":
            operations = 1000
            native_flags += ["--loops", str(operations), "--entries", "4096"]
            rio += ["--entries", "4096"]
            note = "Different cell-map versus shared-extras representation."
        else:
            note = {
                "terminal-parser": "Rio batches ASCII/APC spans; Ghostty calls Parser.next for each byte and discards actions. Same replay bytes, different parser work; not terminal throughput.",
                "terminal-stream": "Initially empty terminals; 10000 history lines, supported operations differ.",
                "codepoint-width": "Width rules below U+0100 differ; Ghostty noop returns without decoding.",
                "grapheme-break": "First-boundary accounting differs; Rio noop also accumulates decoded values.",
                "osc-parser": "Ghostty dedicated parser includes record IO; Rio preloads and runs full VT framing.",
                "apc-parser": "Ghostty parses and disposes APC results; Rio collects raw payloads.",
            }[action]
        if action in ("terminal-stream", "screen-clone", "terminal-resize", "terminal-formatter"):
            native_flags += ["--terminal-cols", "120", "--terminal-rows", "80"]
            rio += ["--terminal-cols", "120", "--terminal-rows", "80"]
        if profile is not None:
            native_flags += ["--data", str(data)]
            rio += ["--data", str(data)]
        native += [f"{k}={v}" for k, v in zip(native_flags[::2], native_flags[1::2])]
        rio += list(flags) + ["--loops", str(operations)]
        yield dict(case=name, action=action, profile=profile, mode=mode,
                   operations=operations, ghostty_args=native, rio_args=rio,
                   notes=note, control=mode == "noop")
