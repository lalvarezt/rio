use std::path::PathBuf;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    println!(
        "cargo:rustc-env=GHOSTTY_BENCH_RIO_REVISION={}",
        git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into())
    );
    let dirty = git(&["status", "--porcelain", "--untracked-files=normal"])
        .map(|status| (!status.is_empty()).to_string())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=GHOSTTY_BENCH_RIO_DIRTY={dirty}");
    let rustc = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=GHOSTTY_BENCH_RUSTC_VERSION={rustc}");

    // Saved executables keep the revision they were built from. Watch Git's
    // real paths too, so a branch switch/commit refreshes cached provenance in
    // ordinary checkouts and worktrees. Source archives report unknown.
    for name in ["HEAD".to_owned(), "index".to_owned()]
        .into_iter()
        .chain(git(&["symbolic-ref", "HEAD"]))
    {
        if let Some(path) =
            git(&["rev-parse", "--path-format=absolute", "--git-path", &name])
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let root =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    // Only the snapshot containers differ at the pinned upstream revision.
    // Select their native API without changing the timed operations or core.
    println!("cargo:rustc-check-cfg=cfg(ghostty_bench_upstream_api)");
    if !root.join("rio-vt/src/crosswords/snapshot.rs").exists() {
        println!("cargo:rustc-cfg=ghostty_bench_upstream_api");
    }
    println!("cargo:rustc-check-cfg=cfg(ghostty_bench_deferred_styles)");
    println!("cargo:rustc-check-cfg=cfg(ghostty_bench_unicode_simd)");
    println!("cargo:rustc-check-cfg=cfg(ghostty_bench_reusable_format)");
    println!("cargo:rustc-check-cfg=cfg(ghostty_bench_borrowed_extras)");
    if std::fs::read_to_string(root.join("rio-vt/src/crosswords/grid/mod.rs"))
        .is_ok_and(|source| source.contains("pub fn intern("))
    {
        println!("cargo:rustc-cfg=ghostty_bench_borrowed_extras");
    }
    if std::fs::read_to_string(root.join("rio-vt/src/crosswords/formatter.rs"))
        .is_ok_and(|source| source.contains("pub fn format_into"))
    {
        println!("cargo:rustc-cfg=ghostty_bench_reusable_format");
    }
    if std::fs::read_to_string(root.join("rio-vt/src/simd_utf8.rs"))
        .is_ok_and(|source| source.contains("pub fn decode_to_utf32"))
    {
        println!("cargo:rustc-cfg=ghostty_bench_unicode_simd");
    }
    println!("cargo:rustc-check-cfg=cfg(ghostty_bench_unicode_tables)");
    if root.join("rio-vt/src/grapheme_lut.rs").exists() {
        println!("cargo:rustc-cfg=ghostty_bench_unicode_tables");
    }
    if std::fs::read_to_string(root.join("rio-vt/src/crosswords/mod.rs"))
        .is_ok_and(|source| source.contains("pub fn snapshot_visible_deferred"))
    {
        println!("cargo:rustc-cfg=ghostty_bench_deferred_styles");
    }
    for path in [
        "misc/ghostty-bench",
        "rio-vt/src",
        "rio-unicode/src",
        "rio-graphics/src",
        "rio-grapheme-width/src",
        "Cargo.toml",
        "Cargo.lock",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
}
