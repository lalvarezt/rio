// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use rio_backend::program_status::{ProgramState, ProgramStatusRecord};
use rio_backend::sugarloaf::font::{FontData, FontLibrary};

pub const DEFAULT_ICON: &str = "\u{ea85}";

pub fn load_font(library: &FontLibrary) -> Option<usize> {
    if let Some(id) = library.font_id_for_postscript_name("SymbolsNFM") {
        return Some(id);
    }
    let font = FontData::from_static_slice(rio_fonts::SYMBOLS_NERD_FONT).ok()?;
    let mut fonts = library.inner.write();
    let id = fonts.len();
    fonts.insert(font);
    Some(id)
}

fn lookup(program: &str) -> Option<&'static str> {
    let executable = program
        .trim()
        .trim_matches(['\'', '"'])
        .rsplit(['/', '\\'])
        .next()?
        .to_ascii_lowercase();
    let executable = executable.strip_suffix(".exe").unwrap_or(&executable);
    match executable {
        // This bundled font has the Vim icon, but predates the Neovim icon.
        "nvim" | "neovim" | "vim" | "gvim" | "vimdiff" => Some("\u{e7c5}"),
        "emacs" | "emacsclient" => Some("\u{e632}"),
        "git" | "lazygit" => Some("\u{e702}"),
        "python" | "python2" | "python3" | "ipython" | "pip" | "pip3" | "uv" => {
            Some("\u{e73c}")
        }
        "cargo" | "rustc" | "rustup" => Some("\u{e7a8}"),
        "node" | "nodejs" | "npm" | "npx" | "pnpm" | "yarn" | "bun" | "deno" => {
            Some("\u{e718}")
        }
        "docker" | "podman" => Some("\u{e7b0}"),
        "go" => Some("\u{e724}"),
        "pwsh" | "powershell" => Some("\u{ebc7}"),
        "bash" | "zsh" | "fish" | "sh" => Some("\u{ebca}"),
        "ssh" | "mosh" => Some("\u{f08a9}"),
        "wsl" | "wslhost" => Some("\u{e712}"),
        "code" | "code-insiders" => Some("\u{e70c}"),
        "cmd" => Some(DEFAULT_ICON),
        _ => None,
    }
}

pub fn for_program(
    program: &str,
    terminal_title: &str,
    status: Option<&ProgramStatusRecord>,
) -> &'static str {
    if let Some(record) = status {
        if matches!(record.state, ProgramState::Working | ProgramState::Blocked) {
            if let Some(app) = record.app.as_deref() {
                return lookup(app).unwrap_or(DEFAULT_ICON);
            }
        }
    }

    // Titles such as "NVIM file.rs" and "file.rs - NVIM" identify the app.
    // A path alone is a working directory, not an application hint.
    let title = terminal_title.trim();
    let hint = title.rsplit(" - ").next().unwrap_or(title);
    if let Some(word) = hint.split_whitespace().next() {
        if !word.contains(['/', '\\']) || word.to_ascii_lowercase().ends_with(".exe") {
            if let Some(icon) = lookup(word.trim_matches(['[', ']'])) {
                return icon;
            }
        }
    }
    lookup(program).unwrap_or(DEFAULT_ICON)
}
