# Missing tab status rings after the upstream merge

Merge `3d75d2551e` left OSC 7501 disabled in live terminal contexts.
The tab renderer stayed unchanged, but received no native program status to draw.

Upstream commit `1e48195e43` makes program status an explicit embedder opt-in.
`Crosswords::new` defaults to disabled. Reports and capability queries return
without updating status or replying until the embedder enables support.

The merge added `set_program_status_enabled(true)` to `create_dead_context`,
which existing frontend tests use. It omitted that call from
`ContextManager::create_context`, which creates live tabs and splits.
That difference let the existing ring and status tests pass while live tabs
silently ignored OSC 7501.

The fix enables program status immediately after constructing the live terminal.
The regression test creates a real PTY context, feeds a valid working report
through `Processor`, and checks the resulting tab rim color and progress.
It fails with `OSC 7501 must produce a status rim in a live tab` before the fix
and passes afterward.

Validation on Linux:

- `cargo test -p rioterm --bin rio program_status_rim_is_enabled_for_live_contexts`
  reproduces the failure before the fix and passes after it.
- `cargo test -p rioterm --bin rio` passes 287 tests, with 2 ignored.
- `cargo test -p rio-vt --lib program_status` passes 17 tests.
- `rustfmt --check --edition 2021 frontends/rioterm/src/renderer/island.rs frontends/rioterm/src/context/mod.rs`
  and `git diff --check` pass.

The live-context regression test runs on Unix. The fix is in shared context
initialization, but this investigation did not run Windows or visual checks.
Installed binaries need a rebuild and restart to receive the fix.
