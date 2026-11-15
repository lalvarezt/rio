# Upstream integration, 2026-11-15

## Revisions and decision

- Task start and pre-merge fork: `0e6e7df5c5a0d9c1683e878dd89ecfd75b068210`.
- Upstream: `54f7f9257ba84af84cccfc087fe4be50da6f1ded`, Rio 0.5.32.
- Merge base: `6dc89d10ead6d8662212c4bce513c4c8b247ca5e`, Rio 0.5.29.
- The working tree was clean at task start.
- Prefer upstream implementations for overlapping features. Retain an alternative
  implementation only with a demonstrated performance benefit and matching behavior.
- Preserve fork features outside those overlaps.

## Investigation

The upstream range contains 17 commits and changes 58 files. The fork range
contains 15 commits and changes 365 files. Counts include merge commits.
Reproduce with `git rev-list --count BASE..TIP` and
`git diff --name-only BASE TIP`, using the revisions above.

Upstream adds glyph protocol 1.12 layout, OSC 7501 program status,
command-finish notifications, whole OSC payload handling, parser and image
resource limits, modern bundled ConPTY, image placement APIs, PTY close-on-exec,
macOS numpad Enter, desktop launch flags, and release metadata.

Overlapping fork customizations include status events and UI, whole OSC payloads,
terminal resource limits, shell integration, hyperlink hints, glyph decoding,
grid rendering, and Windows PTY loading and packaging. The remaining fork changes
include terminal and rendering optimizations, desktop chrome, SSH sessions,
window behavior, fonts, clipboard handling, and profiling tools.

## Validation plan

No repository regression profile exists. Select checks from
`.github/workflows/test.yml`, `.github/workflows/librio.yml`, `Makefile`, and
the available profiling tools.

- Run workspace tests with the `wgpu` feature before and after integration.
- Run formatting, clippy with all targets and features, and the C API tests.
- Review clean merges and changed callers, as well as conflicts.
- Compare parser and rendering workloads against the pinned pre-merge fork.
- Record raw commands, samples, identities, timing boundaries, and coverage gaps.

Local raw diffs, logs, and measurements are retained under
`.local-support/merge-upstream-2026-11-15/`. Final decisions, validation results,
and coverage limits follow.

## Integration decisions

- Use upstream OSC 7501 parsing, status types, opt-in, query replies, and OSC 133
  lifecycle events. Keep the bounded pane store and tab display as a UI adapter.
  The frontend explicitly opts in, including dead-pane test fixtures.
- Use upstream whole-OSC payload handling, hyperlink rules, OSC 9 dispatch,
  XTGETTCAP limits, APC and OSC accumulation limits, and combining-mark caps.
  Adapt combining marks to the fork's wide-cell representation.
- Keep the 64 KiB Kitty clipboard packet budget and abort callback. Upstream
  has no Kitty clipboard implementation to replace this fork feature.
- Keep SetUserVar because app icons and SSH integration read it. Bound names to
  128 bytes, decoded values to 4096 bytes, and the store to 64 entries.
- Use upstream glyph protocol 1.12 layout, decoding, rasterization, and atlas
  identity. Keep the existing row, style, shaping, and cross-pane bitmap caches.
- Use upstream ConPTY loading, pipe workers, exit watcher, package version
  1.25.261109001, signature verification, ZIP layout, and MSI components.
  Restore its parking_lot dependency, removed by the old fork pipe implementation.
- Remove the old local ConPTY package and preparation script. Adapt install.ps1
  to copy architecture subdirectories and the upstream license. Upstream packages
  x64 and ARM64. The former x86 preparation option is removed. Preparation now
  requires network access. The fetch retry loop also supports Windows PowerShell
  5.1, used by the existing local installer.
- Keep the fork's pipe-before-backend teardown order and resize error propagation.
  These contracts remain in the fork PTY interface.
- Use upstream command-finish notifications, launch overrides, fixed titles,
  hold-after-exit behavior, macOS numpad Enter, PTY close-on-exec, image APIs,
  release metadata, and dependency updates. Keep failed-config reload recovery,
  terminal-resource cleanup, SSH sessions, and desktop chrome.
- Use upstream zsh prompt and command marks. Keep the SSH wrapper and app reports,
  removing the fork's duplicate prompt-start mark.

## Parser exception

The retained parser handles valid UTF-8 across read boundaries. Replacing it
wholesale with upstream fails the existing
`partial_utf8_preserves_ascii_before_the_next_partial_character` test. For
`éĆ` split after the first byte, upstream drops `C`.
The upstream-parser comparison passes 53 tests and fails 12 fork regression
checks, including this text-loss case and C1 and opaque-string handling.

Keep the boundary scanners, UTF-8 corrections, and string-state handling.
The OSC buffer, verbatim dispatch, and limits use upstream's design. The
same-caller comparison also measures a graphics benefit, described below.

## Fork coverage

The initial fork range changes 365 files. Of these, 324 remain byte-for-byte
unchanged. Forty-one change during integration, including six outside the
35-file overlap with upstream. The local `fork-audit.json` records each file.

| Affected files | Decision |
| --- | --- |
| .gitignore, .goreleaser.yaml, Cargo.toml, Cargo.lock | Upstream versions and package layout, with fork dependency patches retained. |
| extra/man/rio.5.scd, rio-backend/src/config/mod.rs | Add hold and command-finish configuration beside fork UI options. |
| rio-backend/src/config/hints.rs | Upstream explicit-URI documentation. |
| application.rs, bindings/mod.rs, cli.rs, context/mod.rs, context/title.rs, main.rs, screen/mod.rs in frontends/rioterm/src | Upstream launch, hold, notifications, and keypad behavior with fork UI, SSH, and cleanup. |
| shell_integration/mod.rs and rio-integration.zsh | Upstream command lifecycle with fork SSH and app reports. |
| librio/src/capi.rs, lib.rs, render_state.rs | Upstream status delegate and image APIs. Keep paste encoding, snapshot optimizations, and geometry casts. |
| misc/windows/install.ps1, prepare-runtime.ps1, rio.wxs | Install and package upstream runtime layout. |
| misc/windows/prepare-conpty.ps1, vendor/windows-console/SHA256SUMS, and its old NuGet archive | Remove the replaced preparation path and binary. |
| rio-graphics/src/glyph/glyf_decode.rs, rio-grid/src/lib.rs, sugarloaf/src/lib.rs | Upstream glyph layout and decode with fork rendering optimizations. |
| rio-vt/src/ansi/kitty_graphics_protocol.rs | Upstream nonblocking shared-memory checks. Keep the bounded single upload needed by fork animation and image identity. Port resource tests to that representation. |
| rio-vt/src/crosswords/mod.rs, event/mod.rs, performer/handler.rs, performer/mod.rs, performer/osc.rs, performer/parser/mod.rs, program_status.rs | Upstream protocols and limits with the correctness and UI adaptations above. |
| teletypewriter/src/unix/mod.rs | Upstream close-on-exec on both spawn paths. |
| teletypewriter/src/windows/child.rs, conpty.rs, pipes.rs | Upstream implementation with fork resize result contract. |
| teletypewriter/Cargo.toml | Restore parking_lot for upstream Windows pipe workers. |

The unchanged fork files include the hint implementation, whose literal-URI
behavior already matches upstream's change, and the other desktop, SSH, font,
clipboard, and rendering customizations outside this integration.

## Performance

Compare the pinned task-start fork, the integrated candidate, and the same
candidate with upstream's parser. The parser comparison changes that module and
compatibility adapters. All use the same probe and resolved probe dependencies.

The probe times file reads, terminal processing, and a visible snapshot after each
64 KiB of input. Fixtures use a 120 by 80 terminal, 10,000 history lines, and
1 KiB or 64 KiB reads. Setup and final state hashes are outside timing. Builds
use Rust 1.99.1, optimization level 3, no LTO, and 16 codegen units. Runs alternate
on one pinned CPU with builds idle. Allocation passes run separately.

- Four 512 by 512 RGBA uploads confirm the retained parser takes 4.6% and 6.3%
  less time than upstream's parser at the two read sizes. Medians are 3.38 ms
  and 2.94 ms over 33 samples. These are CPU caller timings, not interactive gains.
- Ordinary text and short protocols show no consistent parser advantage.
- Whole OSC title and hyperlink workloads take about 9.5% less time than the
  pre-merge fork and allocate 69,906 fewer times per 4 MiB fixture.
- Status handling costs about 2–3% more and makes 80,660 additional allocations
  per 4 MiB fixture. It emits the upstream status event and updates the fork UI
  mirror. Both versions handle reports.
- Combining marks cost about 2% more with the per-cell bound. This keeps the
  upstream protection against quadratic accumulation.
- Styled text with 64 KiB reads costs about 2% more, roughly 0.3 ms per 4 MiB
  fixture. This small difference remains in the recorded results.
- Large unsupported APC input costs about 1.6 to 2.4 times the old fork's time,
  with roughly 7 MiB more peak process memory. Upstream accumulates it under its
  64 MiB cap. The old fork stopped at a much smaller budget.
- Common fixtures preserve visible text, cursor, and history hashes. Those hashes
  do not establish image-pixel or complete protocol equivalence.

Upstream also changes accepted input sizes. Ordinary OSC and OSC 7501 accumulation
can exceed the old 64 KiB and 4 KiB limits, respectively, though OSC 7501 reports
still validate against the protocol limit. Single image and APC payloads now stop
at upstream's 64 MiB wire cap. Retained OSC capacity rises to 1 MiB. These are
intentional upstream resource-policy tradeoffs.

Raw samples, fixture hashes, binary hashes, commands, CPU details, source trees,
and allocation and memory results are retained under
`.local-support/merge-upstream-2026-11-15/`, including `evidence.json` and the
adjacent scripts and logs. The probe is `misc/render-profile/stream-probe.rs`.
Allocation bytes sum requested allocation and reallocation sizes. Memory uses
whole-process Linux VmHWM, not interval-only live bytes.

Preliminary runs with candidate status handling disabled are excluded. Final
runs explicitly opt in, matching the frontend caller.

## Validation status

- Exact task-start Linux and Windows workspace baselines pass.
- Integrated Linux workspace tests with wgpu pass, including metadata boundary
  coverage. Clippy with all targets and features passes with warnings.
- The C API smoke test compiles and passes on Linux. The Makefile entry is macOS
  specific, so Linux links the static library with m, dl, pthread, and stdc++.
- Native Wayland ASCII and Unicode smoke runs each present 90 frames. This checks execution,
  not image quality or before-and-after rendering performance.
- Integrated Windows workspace tests pass after restoring the pipe dependency.
- The signed upstream ConPTY package passes its hash and Authenticode checks.
  Three isolated tests verify bundle selection, resize failure reporting, and
  twenty cycles of failed spawn, resize, and teardown without leaked handles.
  A terminal integration test preserves final output through the bundled host.
- The local installer runs under Windows PowerShell 5.1, installs all six expected
  files, and passes the terminal lifecycle test from the installed layout. The
  explicit file list avoids copying stale files from the old runtime layout.
- Existing platform and graphics tests remain ignored where their fixtures are
  unavailable. The new bundle-only test is ignored in normal runs and was run
  explicitly with the signed bundle. Real zsh and fish are installed on Linux.
- macOS execution, Windows performance, image-pixel comparison, and a full desktop
  interaction matrix remain outside completed coverage.

The Windows checkout remains on its original branch and revision. Validation
uses extracted snapshots under its .git/info directory. Runtime installation
targets an isolated test directory, leaving the user's installed Rio unchanged.
