use super::{GraphicError, MAX_DIMENSION, MAX_SIZE};
use std::io::Cursor;

/// Decode the first animation frame, or the image for a nonanimated PNG.
pub(super) fn decode(data: &[u8]) -> Result<(usize, usize, Vec<u8>), GraphicError> {
    let mut decoder =
        png::Decoder::new_with_limits(Cursor::new(data), png::Limits { bytes: MAX_SIZE });
    // Wuffs expands palettes and packed samples and keeps the high byte of
    // sixteen-bit samples rather than rescaling them with rounding.
    decoder.set_transformations(
        png::Transformations::EXPAND | png::Transformations::STRIP_16,
    );
    let mut reader = decoder.read_info().map_err(|_| GraphicError::InvalidData)?;
    // Wuffs rejects acTL on interlaced PNGs, including an included poster.
    if reader.info().interlaced && reader.info().animation_control.is_some() {
        return Err(GraphicError::InvalidData);
    }
    let (width, height) = (reader.info().width, reader.info().height);
    let rgba_size = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|&bytes| bytes <= MAX_SIZE)
        .ok_or(GraphicError::InvalidData)?;
    // An APNG poster without fcTL is not part of its animation.
    if reader.info().animation_control.is_some() && reader.info().frame_control.is_none()
    {
        reader
            .next_frame_info()
            .map_err(|_| GraphicError::InvalidData)?;
    }
    let size = reader
        .output_buffer_size()
        .ok_or(GraphicError::InvalidData)?;
    if size > MAX_SIZE {
        return Err(GraphicError::InvalidData);
    }
    let mut decoded = vec![0; size];
    let frame = reader
        .next_frame(&mut decoded)
        .map_err(|_| GraphicError::InvalidData)?;
    // Ghostty validates dimensions after decoding; corrupt data wins over
    // its dimension error, while the callback's byte budget still applies.
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(GraphicError::DimensionsTooLarge);
    }
    let (x, y) = reader
        .info()
        .frame_control
        .map_or((0, 0), |f| (f.x_offset, f.y_offset));
    let (width, height) = (width as usize, height as usize);
    let (x, y) = (x as usize, y as usize);
    let (frame_width, frame_height) = (frame.width as usize, frame.height as usize);
    if x + frame_width > width || y + frame_height > height {
        return Err(GraphicError::InvalidData);
    }
    if frame.color_type == png::ColorType::Rgba
        && x == 0
        && y == 0
        && frame_width == width
        && frame_height == height
    {
        decoded.truncate(frame.buffer_size());
        return Ok((width, height, decoded));
    }
    // A first subframe can cover only part of the canvas. The rest is
    // transparent, independent of the excluded poster's contents.
    let mut pixels = vec![0; rgba_size];
    let channels = frame.color_type.samples();
    for (row, source) in decoded[..frame.buffer_size()]
        .chunks_exact(frame.line_size)
        .enumerate()
    {
        let start = ((y + row) * width + x) * 4;
        for (target, source) in pixels[start..start + frame_width * 4]
            .chunks_exact_mut(4)
            .zip(source.chunks_exact(channels))
        {
            let rgba = match source {
                [gray] => [*gray, *gray, *gray, 255],
                [gray, alpha] => [*gray, *gray, *gray, *alpha],
                [red, green, blue] => [*red, *green, *blue, 255],
                [red, green, blue, alpha] => [*red, *green, *blue, *alpha],
                _ => return Err(GraphicError::InvalidData),
            };
            target.copy_from_slice(&rgba);
        }
    }
    Ok((width, height, pixels))
}
