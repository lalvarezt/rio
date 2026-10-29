//! Bounded full-canvas Kitty frames. The displayed frame's buffer lives in
//! GraphicData; its entry here is empty. Selecting a frame swaps ownership.
use crate::time::{Duration, Instant};
use rio_graphics::GraphicData;

pub const MAX_FRAMES: usize = 1024;
pub const DEFAULT_GAP_MS: u32 = 40;

#[derive(Debug)]
pub struct FrameUpload {
    pub data: GraphicData,
    pub number: u32,
    pub base: u32,
    pub x: u32,
    pub y: u32,
    pub gap: i32,
    pub overwrite: bool,
    pub background: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Control {
    pub frame: u32,
    pub gap: i32,
    pub current: u32,
    pub state: u8,
    pub loops: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Composition {
    pub source: u32,
    pub destination: u32,
    pub source_x: u32,
    pub source_y: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub overwrite: bool,
}

#[derive(Debug)]
pub enum Command {
    Frame(FrameUpload),
    Control(Control),
    Compose(Composition),
}

#[derive(Debug)]
pub struct Request {
    pub target: Option<ImageIdentity>,
    pub image_id: u32,
    pub image_number: u32,
    pub placement_id: u32,
    pub quiet: u8,
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageIdentity {
    pub image_id: u32,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Reply {
    pub image_id: u32,
    pub image_number: u32,
    pub placement_id: u32,
    pub quiet: u8,
    pub frame: Option<u32>,
    pub silent_success: bool,
}

#[derive(Debug)]
pub struct Failure {
    pub message: &'static str,
    /// Resolved frame number, once frame dimensions have been validated.
    pub frame: Option<u32>,
}

impl From<&'static str> for Failure {
    fn from(message: &'static str) -> Self {
        Self {
            message,
            frame: None,
        }
    }
}

impl Request {
    pub fn reply(&self) -> Reply {
        Reply {
            image_id: self.image_id,
            image_number: self.image_number,
            placement_id: self.placement_id,
            quiet: self.quiet,
            frame: None,
            silent_success: matches!(&self.command, Command::Control(_)),
        }
    }
}

#[derive(Debug)]
struct Frame {
    pixels: Vec<u8>,
    gap: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Stopped,
    Loading,
    Running,
}

#[derive(Debug)]
pub struct Animation {
    frames: Vec<Frame>,
    current: usize,
    state: State,
    max_loops: u32,
    completed_loops: u32,
    shown_at: Option<Instant>,
    paused_at: Option<Instant>,
}

impl Default for Animation {
    fn default() -> Self {
        Self {
            frames: vec![Frame {
                pixels: Vec::new(),
                gap: 0,
            }],
            current: 0,
            state: State::Stopped,
            max_loops: 0,
            completed_loops: 0,
            shown_at: None,
            paused_at: None,
        }
    }
}

impl Animation {
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }
    pub fn extra_bytes(&self) -> usize {
        self.frames.iter().map(|f| f.pixels.len()).sum()
    }
    pub fn current_frame(&self) -> u32 {
        self.current as u32 + 1
    }

    pub fn pixels<'a>(&'a self, displayed: &'a [u8], number: u32) -> Option<&'a [u8]> {
        let index = number.checked_sub(1)? as usize;
        let frame = self.frames.get(index)?;
        Some(if index == self.current {
            displayed
        } else {
            &frame.pixels
        })
    }

    fn pixels_mut<'a>(
        &'a mut self,
        displayed: &'a mut [u8],
        index: usize,
    ) -> &'a mut [u8] {
        if index == self.current {
            displayed
        } else {
            &mut self.frames[index].pixels
        }
    }

    pub(crate) fn resolve_upload_frame(
        &self,
        image: &GraphicData,
        upload: &FrameUpload,
    ) -> Result<u32, &'static str> {
        if upload.data.width > image.width || upload.data.height > image.height {
            return Err("EINVAL: frame dimensions exceed image");
        }
        let count = self.frames.len() as u32;
        Ok(if upload.number == 0 || upload.number > count + 1 {
            count + 1
        } else {
            upload.number
        })
    }

    /// Validate before the caller reserves bytes or changes the root format.
    pub fn validate_upload(
        &self,
        image: &GraphicData,
        upload: &FrameUpload,
    ) -> Result<(u32, usize), &'static str> {
        let number = self.resolve_upload_frame(image, upload)?;
        let count = self.frames.len() as u32;
        if number == count + 1 {
            if self.frames.len() >= MAX_FRAMES {
                return Err("ENOSPC:animation frame count limit");
            }
            if upload.base > count {
                return Err("EINVAL: base frame not found");
            }
            let bytes = image
                .width
                .checked_mul(image.height)
                .and_then(|n| n.checked_mul(4))
                .ok_or("ENOSPC:animation frame storage full")?;
            Ok((number, bytes))
        } else {
            Ok((number, 0))
        }
    }

    /// Caller supplies decoded RGBA pixels and has reserved the returned bytes.
    pub fn upload(
        &mut self,
        image: &mut GraphicData,
        upload: FrameUpload,
    ) -> Result<(u32, bool), &'static str> {
        let (number, additional) = self.validate_upload(image, &upload)?;
        let index = number as usize - 1;
        if additional != 0 {
            let mut canvas = if upload.base != 0 {
                self.pixels(&image.pixels, upload.base)
                    .ok_or("EINVAL: base frame not found")?
                    .to_vec()
            } else {
                let mut canvas = vec![0; additional];
                for pixel in canvas.chunks_exact_mut(4) {
                    pixel.copy_from_slice(&upload.background.to_be_bytes());
                }
                canvas
            };
            compose_patch(
                &mut canvas,
                image.width,
                image.height,
                &upload.data,
                upload.x as usize,
                upload.y as usize,
                upload.overwrite,
            );
            self.frames.push(Frame {
                pixels: canvas,
                gap: if upload.gap == 0 {
                    DEFAULT_GAP_MS
                } else {
                    upload.gap.max(0) as u32
                },
            });
            Ok((number, false))
        } else {
            let width = image.width;
            let height = image.height;
            compose_patch(
                self.pixels_mut(&mut image.pixels, index),
                width,
                height,
                &upload.data,
                upload.x as usize,
                upload.y as usize,
                upload.overwrite,
            );
            if upload.gap != 0 {
                self.frames[index].gap = upload.gap.max(0) as u32;
            }
            let changed = index == self.current;
            if changed {
                self.shown_at = None;
            }
            Ok((number, changed))
        }
    }

    pub fn control(&mut self, image: &mut GraphicData, control: Control) -> bool {
        if control.frame > 0
            && control.frame as usize <= self.frames.len()
            && control.gap != 0
        {
            self.frames[control.frame as usize - 1].gap = control.gap.max(0) as u32;
        }
        let changed = control.current > 0
            && control.current as usize <= self.frames.len()
            && self.select(image, control.current as usize - 1);
        if let Some(state) = match control.state {
            1 => Some(State::Stopped),
            2 => Some(State::Loading),
            3 => Some(State::Running),
            _ => None,
        } {
            if self.state == State::Stopped && state != State::Stopped {
                self.shown_at = None;
            }
            self.state = state;
            self.completed_loops = 0;
        }
        if control.loops != 0 {
            self.max_loops = control.loops - 1;
        }
        changed
    }

    fn select(&mut self, image: &mut GraphicData, index: usize) -> bool {
        if index == self.current {
            return false;
        }
        std::mem::swap(&mut image.pixels, &mut self.frames[self.current].pixels);
        std::mem::swap(&mut image.pixels, &mut self.frames[index].pixels);
        self.current = index;
        self.shown_at = None;
        true
    }

    pub fn validate_composition(
        &self,
        image: &GraphicData,
        composition: Composition,
    ) -> Result<(usize, usize), &'static str> {
        if self.pixels(&image.pixels, composition.source).is_none() {
            return Err("ENOENT:source frame not found");
        }
        if self
            .pixels(&image.pixels, composition.destination)
            .is_none()
        {
            return Err("ENOENT:destination frame not found");
        }
        let width = if composition.width == 0 {
            image.width
        } else {
            composition.width as usize
        };
        let height = if composition.height == 0 {
            image.height
        } else {
            composition.height as usize
        };
        let fits = |x: u32, y: u32| {
            (x as u64 + width as u64 <= image.width as u64)
                && (y as u64 + height as u64 <= image.height as u64)
        };
        if !fits(composition.x, composition.y) {
            return Err("EINVAL:destination rectangle out of bounds");
        }
        if !fits(composition.source_x, composition.source_y) {
            return Err("EINVAL:source rectangle out of bounds");
        }
        if composition.source == composition.destination
            && u64::from(composition.source_x.max(composition.x))
                < u64::from(composition.source_x.min(composition.x)) + width as u64
            && u64::from(composition.source_y.max(composition.y))
                < u64::from(composition.source_y.min(composition.y)) + height as u64
        {
            return Err("EINVAL:source and destination rectangles overlap");
        }
        Ok((width, height))
    }

    pub fn compose(
        &mut self,
        image: &mut GraphicData,
        composition: Composition,
    ) -> Result<bool, &'static str> {
        let (width, height) = self.validate_composition(image, composition)?;
        let source = self.pixels(&image.pixels, composition.source).unwrap();
        // One bounded rectangle is staged so same-frame disjoint composition
        // uses safe borrows. No image-sized scratch is retained after the call.
        let mut rectangle = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            let offset = ((composition.source_y as usize + y) * image.width
                + composition.source_x as usize)
                * 4;
            rectangle.extend_from_slice(&source[offset..offset + width * 4]);
        }
        let image_width = image.width;
        let destination =
            self.pixels_mut(&mut image.pixels, composition.destination as usize - 1);
        for y in 0..height {
            let offset =
                ((composition.y as usize + y) * image_width + composition.x as usize) * 4;
            blend_row(
                &mut destination[offset..offset + width * 4],
                &rectangle[y * width * 4..(y + 1) * width * 4],
                composition.overwrite,
            );
        }
        let changed = composition.destination == self.current_frame();
        if changed {
            self.shown_at = None;
        }
        Ok(changed)
    }

    pub fn pause(&mut self, now: Instant) {
        self.paused_at.get_or_insert(now);
    }

    /// At most one displayed frame advances per tick. Hidden time is excluded
    /// from the current frame's delay when a paused animation resumes.
    pub fn tick(
        &mut self,
        image: &mut GraphicData,
        now: Instant,
    ) -> (bool, Option<Duration>) {
        if let Some(paused) = self.paused_at.take() {
            if let Some(at) = self.shown_at {
                self.shown_at = at.checked_add(now.saturating_duration_since(paused));
            }
        }
        if self.state == State::Stopped
            || self.frames.len() < 2
            || self.frames.iter().all(|f| f.gap == 0)
            || (self.max_loops != 0 && self.completed_loops >= self.max_loops)
        {
            return (false, None);
        }
        let shown = self.shown_at.get_or_insert(now);
        if *shown > now {
            *shown = now;
        }
        let gap = Duration::from_millis(self.frames[self.current].gap as u64);
        let remaining = gap.saturating_sub(now.saturating_duration_since(*shown));
        if !remaining.is_zero() {
            return (false, Some(remaining));
        }
        let mut next = self.current;
        for _ in 0..self.frames.len() {
            let index = (next + 1) % self.frames.len();
            if index == 0 {
                if self.state == State::Loading {
                    return (false, None);
                }
                self.completed_loops = self.completed_loops.saturating_add(1);
                if self.max_loops != 0 && self.completed_loops >= self.max_loops {
                    return (false, None);
                }
            }
            next = index;
            if self.frames[next].gap != 0 {
                break;
            }
        }
        let changed = self.select(image, next);
        self.shown_at = Some(now);
        (
            changed,
            Some(Duration::from_millis(self.frames[next].gap as u64)),
        )
    }

    /// The root is promoted naturally when frame one is removed. Return the
    /// released pixel bytes and whether the displayed frame was deleted.
    pub fn delete_frame(
        &mut self,
        image: &mut GraphicData,
        number: u32,
    ) -> (usize, bool) {
        if self.frames.len() < 2 {
            return (0, false);
        }
        let index = number.max(1).min(self.frames.len() as u32) as usize - 1;
        let changed = self.current == index;
        std::mem::swap(&mut image.pixels, &mut self.frames[self.current].pixels);
        let removed = self.frames.remove(index);
        self.current = if index < self.current {
            self.current - 1
        } else {
            self.current.min(self.frames.len() - 1)
        };
        std::mem::swap(&mut image.pixels, &mut self.frames[self.current].pixels);
        if changed {
            self.shown_at = None;
        }
        (removed.pixels.len(), changed)
    }
}

fn compose_patch(
    destination: &mut [u8],
    width: usize,
    height: usize,
    patch: &GraphicData,
    x: usize,
    y: usize,
    overwrite: bool,
) {
    if x >= width || y >= height {
        return;
    }
    let columns = patch.width.min(width - x);
    for row in 0..patch.height.min(height - y) {
        let target = ((y + row) * width + x) * 4;
        let source = row * patch.width * 4;
        blend_row(
            &mut destination[target..target + columns * 4],
            &patch.pixels[source..source + columns * 4],
            overwrite,
        );
    }
}

fn blend_row(destination: &mut [u8], source: &[u8], overwrite: bool) {
    if overwrite {
        destination.copy_from_slice(source);
        return;
    }
    for (dst, src) in destination.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
        // Match Ghostty's Wuffs straight-alpha composition, including its
        // 16-bit premultiplication and truncation at each division.
        if dst[3] == 0 || src[3] == 255 {
            dst.copy_from_slice(src);
            continue;
        }
        let alpha = u32::from(src[3]) * 257;
        let old_alpha = u32::from(dst[3]) * 257;
        let inverse = 65535 - alpha;
        let combined = alpha + old_alpha * inverse / 65535;
        for channel in 0..3 {
            let old_color = u32::from(dst[channel]) * 257 * old_alpha / 65535;
            let color =
                (u32::from(src[channel]) * 257 * alpha + old_color * inverse) / 65535;
            dst[channel] = ((color * 65535 / combined) >> 8) as u8;
        }
        dst[3] = (combined >> 8) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rio_graphics::{ColorType, GraphicId};

    #[test]
    fn blending_matches_ghostty_for_transparent_and_low_alpha_pixels() {
        let mut destination = [
            0, 0, 0, 0, 241, 17, 33, 0, 241, 17, 33, 1, 241, 17, 33, 1, 241, 17, 33, 63,
        ];
        let source = [
            31, 59, 239, 0, 19, 99, 199, 0, 19, 99, 199, 0, 19, 99, 199, 127, 19, 99,
            199, 254,
        ];
        blend_row(&mut destination, &source, false);
        assert_eq!(
            destination,
            [
                31, 59, 239, 0, 19, 99, 199, 0, 241, 16, 32, 1, 19, 99, 199, 128, 19, 99,
                199, 255
            ]
        );
    }

    fn image(pixels: &[u8]) -> GraphicData {
        GraphicData {
            id: GraphicId::new(1),
            width: pixels.len() / 4,
            height: 1,
            color_type: ColorType::Rgba,
            pixels: pixels.to_vec(),
            is_opaque: false,
            resize: None,
            display_width: None,
            display_height: None,
            transmit_time: Instant::now(),
        }
    }

    fn upload(pixels: &[u8]) -> FrameUpload {
        FrameUpload {
            data: image(pixels),
            number: 0,
            base: 0,
            x: 0,
            y: 0,
            gap: 0,
            overwrite: false,
            background: 0,
        }
    }

    #[test]
    fn frame_patches_blend_and_swap_without_a_playback_copy() {
        let mut root = image(&[0, 0, 255, 255, 0, 0, 255, 255]);
        let root_pointer = root.pixels.as_ptr();
        let mut animation = Animation::default();
        let mut patch = upload(&[255, 0, 0, 128]);
        patch.base = 1;
        patch.x = 1;
        assert_eq!(animation.upload(&mut root, patch).unwrap(), (2, false));
        assert_eq!(animation.extra_bytes(), 8);
        assert_eq!(root.pixels, [0, 0, 255, 255, 0, 0, 255, 255]);
        assert!(animation.control(
            &mut root,
            Control {
                current: 2,
                ..Default::default()
            }
        ));
        assert_eq!(root.pixels, [0, 0, 255, 255, 128, 0, 127, 255]);
        assert_eq!(
            animation.pixels(&root.pixels, 1).unwrap().as_ptr(),
            root_pointer
        );
        assert_eq!(animation.extra_bytes(), 8);
        let mut patch = upload(&[10, 20, 30, 0]);
        patch.number = 2;
        assert!(animation.upload(&mut root, patch).unwrap().1);
        assert_eq!(&root.pixels[..4], [0, 0, 255, 255]);
    }

    #[test]
    fn composition_validates_before_editing_and_supports_disjoint_self_copy() {
        let mut root = image(&[1, 2, 3, 255, 4, 5, 6, 255]);
        let mut animation = Animation::default();
        let overlap = Composition {
            source: 1,
            destination: 1,
            width: 1,
            height: 1,
            ..Default::default()
        };
        assert!(animation.compose(&mut root, overlap).is_err());
        assert_eq!(root.pixels, [1, 2, 3, 255, 4, 5, 6, 255]);
        assert!(animation
            .compose(&mut root, Composition { x: 1, ..overlap })
            .unwrap());
        assert_eq!(root.pixels, [1, 2, 3, 255, 1, 2, 3, 255]);
        assert!(animation
            .compose(
                &mut root,
                Composition {
                    x: u32::MAX,
                    ..overlap
                }
            )
            .is_err());
        assert!(animation
            .compose(
                &mut root,
                Composition {
                    source: 99,
                    ..overlap
                }
            )
            .is_err());
    }

    #[test]
    fn clipping_background_edits_and_frame_limits() {
        let mut root = image(&[0; 8]);
        let mut animation = Animation::default();
        let mut patch = upload(&[7, 8, 9, 255, 10, 11, 12, 255]);
        patch.background = 0x01020304;
        patch.x = 1;
        assert_eq!(animation.upload(&mut root, patch).unwrap().0, 2);
        assert_eq!(
            animation.pixels(&root.pixels, 2).unwrap(),
            [1, 2, 3, 4, 7, 8, 9, 255]
        );
        let mut invalid = upload(&[1, 2, 3, 4]);
        invalid.base = 99;
        assert!(animation.upload(&mut root, invalid).is_err());
        assert_eq!(animation.frame_count(), 2);
        for _ in 2..MAX_FRAMES {
            animation.upload(&mut root, upload(&[0, 0, 0, 0])).unwrap();
        }
        assert!(animation.upload(&mut root, upload(&[0, 0, 0, 0])).is_err());
        assert_eq!(animation.frame_count(), MAX_FRAMES);
    }

    #[test]
    fn playback_preserves_pause_delay_and_honors_finite_loops() {
        let now = Instant::now();
        let mut root = image(&[1, 2, 3, 255]);
        let mut animation = Animation::default();
        let mut patch = upload(&[4, 5, 6, 255]);
        patch.gap = 20;
        animation.upload(&mut root, patch).unwrap();
        animation.control(
            &mut root,
            Control {
                frame: 1,
                gap: 100,
                state: 3,
                loops: 2,
                ..Default::default()
            },
        );
        assert_eq!(
            animation.tick(&mut root, now),
            (false, Some(Duration::from_millis(100)))
        );
        animation.pause(now + Duration::from_millis(30));
        assert_eq!(
            animation.tick(&mut root, now + Duration::from_millis(1030)),
            (false, Some(Duration::from_millis(70)))
        );
        assert_eq!(
            animation.tick(&mut root, now + Duration::from_millis(1100)),
            (true, Some(Duration::from_millis(20)))
        );
        assert_eq!(root.pixels, [4, 5, 6, 255]);
        assert_eq!(
            animation.tick(&mut root, now + Duration::from_millis(1120)),
            (false, None)
        );
        assert_eq!(animation.current_frame(), 2);
    }

    #[test]
    fn loading_parks_until_a_frame_arrives_and_gapless_animations_do_not_spin() {
        let now = Instant::now();
        let mut root = image(&[1, 2, 3, 255]);
        let mut animation = Animation::default();
        let mut patch = upload(&[4, 5, 6, 255]);
        patch.gap = -1;
        animation.upload(&mut root, patch).unwrap();
        animation.control(
            &mut root,
            Control {
                state: 2,
                ..Default::default()
            },
        );
        assert_eq!(animation.tick(&mut root, now), (false, None));
        animation.control(
            &mut root,
            Control {
                frame: 2,
                gap: 40,
                ..Default::default()
            },
        );
        assert!(animation.tick(&mut root, now).0);
        assert_eq!(
            animation.tick(&mut root, now + Duration::from_millis(40)),
            (false, None)
        );
        animation
            .upload(&mut root, upload(&[7, 8, 9, 255]))
            .unwrap();
        assert!(
            animation
                .tick(&mut root, now + Duration::from_millis(1000))
                .0
        );
        assert_eq!(root.pixels, [7, 8, 9, 255]);
        let (released, changed) = animation.delete_frame(&mut root, 3);
        assert_eq!((released, changed), (4, true));
        assert_eq!(root.pixels, [4, 5, 6, 255]);
        assert_eq!(animation.delete_frame(&mut root, 1), (4, false));
        assert_eq!(animation.current_frame(), 1);
        assert_eq!(animation.frame_count(), 1);
        assert_eq!(animation.extra_bytes(), 0);
    }
}
