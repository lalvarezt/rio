// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use rio_backend::sugarloaf::font::{FontData, FontLibrary};

mod catalog;
mod logos;
mod neovim;
pub(super) use catalog::Library;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Glyph(char),
    Logo(&'static logos::Logo),
    Neovim,
}

pub const DEFAULT_ICON: Icon = Icon::Glyph('\u{ea85}');
pub const SSH_ICON: Icon = Icon::Glyph('\u{f08c0}');

pub fn load_font(library: &FontLibrary) -> Option<usize> {
    #[cfg(feature = "profiling")]
    let _timing = crate::profiling::span("header.load_app_font");
    if let Some(id) = library.font_id_for_postscript_name("SymbolsNFM") {
        return Some(id);
    }
    let font = FontData::from_static_slice(rio_fonts::SYMBOLS_NERD_FONT).ok()?;
    let mut fonts = library.inner.write();
    let id = fonts.len();
    fonts.insert(font);
    Some(id)
}

#[allow(clippy::too_many_arguments)]
pub fn draw(
    sugarloaf: &mut rio_backend::sugarloaf::Sugarloaf,
    icon: Icon,
    cx: f32,
    cy: f32,
    size: f32,
    color: [f32; 4],
    font_id: Option<usize>,
    order: u8,
    occlusion: Option<[f32; 2]>,
) -> bool {
    match icon {
        Icon::Neovim => {
            neovim::draw(sugarloaf, cx, cy, size, color[3], order, occlusion);
            true
        }
        Icon::Logo(logo) => {
            logos::draw(sugarloaf, logo, cx, cy, size, color, order, occlusion);
            true
        }
        Icon::Glyph(glyph) => font_id.is_some_and(|id| {
            let mut utf8 = [0; 4];
            super::windows_chrome::draw_font_icon(
                sugarloaf,
                glyph.encode_utf8(&mut utf8),
                cx,
                cy,
                size,
                color,
                id,
            )
        }),
    }
}
