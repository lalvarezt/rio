# Windows branch review

The review covers all 43 changed files in the 14 commits between local `main`
(`59203ccd5f`) and `b2a4578a4d`, including OSC status handling, shell integration,
Windows chrome, input geometry, text caching, GPU initialization, and profiling.
Independent standards and behavior reviews found focused problems rather than
a reason to rewrite the branch's architecture.

## Standards

No hard documented-standard violations were found. Four findings warranted
investigation:

- Bash kept its preexec guard armed throughout a command. A 50-iteration loop
  repeatedly reported `for` and `:` and launched encoding pipelines. Reporting
  now disarms after the first eligible command and rearms at the prompt.
- Windows tab measurement, ordinary drawing, and floating-tab drawing repeated
  prefix geometry. They now use one title-padding calculation and one icon
  drawing method. SSH-to-app and app-to-title gaps are both 8 logical pixels,
  instead of 4 and 8. Width measurement and truncation reserve the same space.
  A screenshot also exposed fractional-width cancellation: the measured
  `cmd.exe` title exceeded its computed budget by roughly 0.000008 logical px.
  Preferred widths now round upward to device pixels, preserving the full title
  and avoiding the unnecessary truncation path.
- The tab selection fade duplicated the header's fade implementation. Tabs now
  call the existing header helper.
- The colored Neovim mark has a custom raster cache and emits rectangles for
  covered pixel runs. This remains intentional: existing polygon and image
  helpers hardcode order zero, which puts the mark under a floating tab, and
  the CPU renderer does not support arbitrary polygons correctly. A replacement
  requires an ordered drawing interface across backends. Removing the raster
  without that work would change rendering behavior. This review does not claim
  a performance improvement for Neovim drawing.

The bounded OSC parser/store, focused-split status routing, immutable shared
shaped text, and per-backend Windows GPU-instance cache remain in place.

## Spec

The behavior review confirmed the repeated Bash-reporting bug against the
requirement for maximum performance. Follow-up runtime checks also found that
the fallback integration overwrote an existing DEBUG handler while claiming
to preserve it. Bash hides that handler while a script is sourced.

The fallback now installs once from the prompt, where it can capture the
caller's restored handler. It preserves prompt-array order, command exit
status, and the user's functrace setting. Integration internals do not become
application names. Bash, Zsh, and Fish also encode the SSH boolean directly;
Bash and Zsh remove Base64 line breaks without launching `tr`.

Existing preexec/precmd frameworks remain supported. No new dependency or
automatic global shell configuration is introduced.

## Validation

Matched interactive Bash captures of a 50-iteration loop drop from a median
of 250 ms and 103 application reports to 9 ms and 3 reports. Those counts
include initial shell and prompt reporting; the loop itself reports once.
Existing DEBUG handlers, including handlers that print output, work with
functrace enabled and disabled. Two existing prompt-array callbacks remain
ahead of Rio's callbacks, and a failing command's exit status remains 1.

Bash, Zsh, and Fish syntax checks pass. Zsh and Fish runtime captures preserve
local and SSH application metadata. Geometry checks cover five icon sizes and
five display scales, with equal logical gaps in every case.

Both Windows release builds pass. Five baseline, five intermediate, and five
final captures each complete 600 full-text frames with eight cmd tabs, for
9000 measured frames in total. The table compares baseline and final builds.
Every capture passes readiness and frame-count checks, with matching acquisition,
submission, and presentation counts, including startup frames. Builds finish
before timing begins. The normal build also exits successfully without creating
profiling output when profiling environment variables are present.

| Median across five runs, ms | Baseline | Updated |
| --- | ---: | ---: |
| Tab render scope | 0.0740 | 0.0713 |
| Full screen scope, including presentation | 1.3258 | 1.2984 |
| Frame interval | 16.6744 | 16.6787 |
| Frame interval p95 | 18.8021 | 18.6636 |

The tab-render timing ranges overlap, and frame cadence remains about 60 Hz.
The full-screen scope includes driver waits; these batches do not establish a
GPU throughput improvement. This local cmd workload does not exercise the remote
Neovim prefix. The measured performance gain is in shell reporting.

The preserved baseline executable is `target/release/rio-profile-review-baseline.exe`.
Capture series are under `target/profiles/review-baseline-eight` and
`target/profiles/review-final-eight`, with copies in this checkout. Their
executable SHA-256 prefixes are `6e58ff08de866f85` and `7425be65e030e09e`.
The Windows header captures are `target/profiles/review-before.png` and
`target/profiles/review-after.png`; they use reported SSH and Neovim metadata.
The final capture confirms balanced prefix gaps and a complete `cmd.exe` title.
The runtime changes in this review remove 18 lines overall.

Standards: four findings, with repeated Bash work the largest confirmed cost;
three are fixed and the Neovim rendering constraint is documented. Spec: two
confirmed shell-behavior issues, both fixed. The requested icon-spacing fix
also removes the duplicated geometry that caused it.
