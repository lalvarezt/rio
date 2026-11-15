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
`.local-support/merge-upstream-2026-11-15/`. Validation results and final decisions
will be added here after integration.
