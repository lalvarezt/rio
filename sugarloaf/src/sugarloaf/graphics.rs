// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

// The graphic value types (GraphicData, GraphicId, ColorType, Resize*,
// GraphicOverlay, the image-key hashers) now live in the leaf crate
// `rio-graphics` so the terminal core can model images without pulling
// the renderer. They are re-exported here so `sugarloaf::graphics::*` and
// `sugarloaf::GraphicData` keep resolving. The render entry owns a
// GPU-uploadable Handle and stays in sugarloaf.

use crate::sugarloaf::Handle;

pub use rio_graphics::{
    atlas_image_key, image_key_route, kitty_image_key, route_image_key, ColorType,
    Graphic, GraphicData, GraphicId, GraphicOverlay, ResizeCommand, ResizeParameter,
    MAX_GRAPHIC_DIMENSIONS,
};

pub struct GraphicDataEntry {
    pub handle: Handle,
    pub transmit_time: std::time::Instant,
}

impl GraphicDataEntry {
    /// Create from a GraphicData, taking ownership of pixel data.
    pub fn from_graphic_data(data: GraphicData) -> Self {
        Self {
            handle: Handle::from_pixels(
                data.width as u32,
                data.height as u32,
                data.pixels,
            ),
            transmit_time: data.transmit_time,
        }
    }
}
