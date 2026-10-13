//! The remote screen as a shared pixel buffer plus the regions changed since the view last
//! looked.
//!
//! The session task writes decoded updates into it and sends [`crate::VncEvent::Frame`]
//! whenever damage appears while none is pending (the first change after a
//! [`Framebuffer::take_dirty`], a resize included); the view then takes the dirty rectangles and
//! copies just those regions out. Several updates between two frames of the view coalesce into
//! one notification.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Byte order of the 4-byte pixels. Alpha is always 255 (the desktop is opaque).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PixelOrder {
    /// R, G, B, A: what a canvas `ImageData` holds.
    #[default]
    Rgba,
    /// B, G, R, A: what GPU textures and GPUI's `RenderImage` take without a swizzle.
    Bgra,
}

/// A rectangle of the desktop, in desktop pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn right(&self) -> u32 {
        self.x + self.width
    }

    fn bottom(&self) -> u32 {
        self.y + self.height
    }

    fn union(&self, other: &Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect::new(
            x,
            y,
            self.right().max(other.right()) - x,
            self.bottom().max(other.bottom()) - y,
        )
    }

    /// The part of this rectangle inside a `width`×`height` desktop.
    pub fn clipped(&self, width: u32, height: u32) -> Option<Rect> {
        let x = self.x.min(width);
        let y = self.y.min(height);
        let right = self.right().min(width);
        let bottom = self.bottom().min(height);
        (right > x && bottom > y).then(|| Rect::new(x, y, right - x, bottom - y))
    }
}

/// Beyond this many separate dirty rectangles the view gets their bounding box instead.
const MAX_DIRTY_RECTS: usize = 64;

/// The pixels and the pending damage, behind the framebuffer's lock.
#[derive(Debug)]
pub struct FrameState {
    width: u32,
    height: u32,
    order: PixelOrder,
    pixels: Vec<u8>,
    dirty: Vec<Rect>,
}

impl FrameState {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Bytes per row.
    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }

    pub fn pixel_order(&self) -> PixelOrder {
        self.order
    }

    /// Row-major pixels, [`Self::stride`] bytes per row.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The bytes of one region, row by row.
    pub fn region(&self, rect: Rect) -> Vec<u8> {
        let Some(rect) = rect.clipped(self.width, self.height) else {
            return Vec::new();
        };
        let stride = self.stride();
        let row_bytes = rect.width as usize * 4;
        let mut out = Vec::with_capacity(row_bytes * rect.height as usize);
        for row in rect.y..rect.bottom() {
            let start = row as usize * stride + rect.x as usize * 4;
            out.extend_from_slice(&self.pixels[start..start + row_bytes]);
        }
        out
    }

    /// The pixel at (x, y) as R, G, B, A whatever the storage order.
    pub fn rgba_at(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = y as usize * self.stride() + x as usize * 4;
        let pixel = &self.pixels[at..at + 4];
        Some(match self.order {
            PixelOrder::Rgba => [pixel[0], pixel[1], pixel[2], pixel[3]],
            PixelOrder::Bgra => [pixel[2], pixel[1], pixel[0], pixel[3]],
        })
    }

    /// Resize, keeping the overlapping picture and filling new area with black; the whole
    /// desktop is dirty. Returns whether the view had taken all earlier damage (so it needs a
    /// new notification).
    pub(crate) fn resize(&mut self, width: u32, height: u32) -> bool {
        let first = self.dirty.is_empty();
        // Keep what overlaps, like a canvas resize that saves its image data first.
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        let keep_width = self.width.min(width) as usize * 4;
        for row in 0..self.height.min(height) as usize {
            let from = row * self.stride();
            let to = row * width as usize * 4;
            pixels[to..to + keep_width].copy_from_slice(&self.pixels[from..from + keep_width]);
        }
        self.width = width;
        self.height = height;
        self.pixels = pixels;
        self.dirty.clear();
        if width > 0 && height > 0 {
            self.dirty.push(Rect::new(0, 0, width, height));
        }
        first && width > 0 && height > 0
    }

    /// Record damage; returns whether this is the first since the view last took it.
    pub(crate) fn mark_dirty(&mut self, rect: Rect) -> bool {
        let Some(rect) = rect.clipped(self.width, self.height) else {
            return false;
        };
        let first = self.dirty.is_empty();
        if self.dirty.len() >= MAX_DIRTY_RECTS {
            let bounds = self
                .dirty
                .iter()
                .fold(rect, |bounds, other| bounds.union(other));
            self.dirty.clear();
            self.dirty.push(bounds);
        } else {
            self.dirty.push(rect);
        }
        first
    }

    fn put(&mut self, at: usize, rgb: [u8; 3]) {
        let pixel = &mut self.pixels[at..at + 4];
        match self.order {
            PixelOrder::Rgba => pixel.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]),
            PixelOrder::Bgra => pixel.copy_from_slice(&[rgb[2], rgb[1], rgb[0], 255]),
        }
    }

    /// Draw a `width`×`height` image at (x, y). `data` holds rows of R, G, B pixels with
    /// `bytes_per_pixel` bytes each (3, or 4 with a padding byte); what falls outside the
    /// desktop is dropped.
    pub(crate) fn blit(
        &mut self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        data: &[u8],
        bytes_per_pixel: usize,
    ) {
        let Some(rect) = Rect::new(x, y, width, height).clipped(self.width, self.height) else {
            return;
        };
        let stride = self.stride();
        let src_stride = width as usize * bytes_per_pixel;
        for row in 0..rect.height as usize {
            let src_row = (rect.y - y) as usize + row;
            for column in 0..rect.width as usize {
                let src_column = (rect.x - x) as usize + column;
                let from = src_row * src_stride + src_column * bytes_per_pixel;
                let Some(pixel) = data.get(from..from + 3) else {
                    return;
                };
                let at = (rect.y as usize + row) * stride + (rect.x as usize + column) * 4;
                self.put(at, [pixel[0], pixel[1], pixel[2]]);
            }
        }
    }

    /// Fill a rectangle with one colour.
    pub(crate) fn fill(&mut self, rect: Rect, rgb: [u8; 3]) {
        let Some(rect) = rect.clipped(self.width, self.height) else {
            return;
        };
        let stride = self.stride();
        for row in rect.y..rect.bottom() {
            for column in rect.x..rect.right() {
                self.put(row as usize * stride + column as usize * 4, rgb);
            }
        }
    }

    /// Copy the `rect`-sized region at (src_x, src_y) to `rect` (CopyRect), overlap included.
    pub(crate) fn copy_within(&mut self, src_x: u32, src_y: u32, rect: Rect) {
        let source = self.region(Rect::new(src_x, src_y, rect.width, rect.height));
        let Some(source_rect) =
            Rect::new(src_x, src_y, rect.width, rect.height).clipped(self.width, self.height)
        else {
            return;
        };
        let target = Rect::new(rect.x, rect.y, source_rect.width, source_rect.height);
        let Some(target) = target.clipped(self.width, self.height) else {
            return;
        };
        let stride = self.stride();
        let src_row_bytes = source_rect.width as usize * 4;
        let row_bytes = target.width as usize * 4;
        for row in 0..target.height as usize {
            let from = row * src_row_bytes;
            let to = (target.y as usize + row) * stride + target.x as usize * 4;
            self.pixels[to..to + row_bytes].copy_from_slice(&source[from..from + row_bytes]);
        }
    }
}

/// A shared handle on the remote screen. Cloning shares the same buffer.
#[derive(Debug, Clone)]
pub struct Framebuffer {
    inner: Arc<Mutex<FrameState>>,
}

impl Framebuffer {
    pub fn new(order: PixelOrder) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FrameState {
                width: 0,
                height: 0,
                order,
                pixels: Vec::new(),
                dirty: Vec::new(),
            })),
        }
    }

    /// Lock the pixels for reading. Hold the guard briefly: the session waits on it.
    pub fn lock(&self) -> MutexGuard<'_, FrameState> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The regions changed since the last call, in the order they changed.
    pub fn take_dirty(&self) -> Vec<Rect> {
        std::mem::take(&mut self.lock().dirty)
    }

    pub fn size(&self) -> (u32, u32) {
        let state = self.lock();
        (state.width, state.height)
    }

    pub fn pixel_order(&self) -> PixelOrder {
        self.lock().order
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_overlapping_picture_on_resize() {
        let framebuffer = Framebuffer::new(PixelOrder::Rgba);
        let mut state = framebuffer.lock();
        state.resize(2, 2);
        state.pixels[4..8].copy_from_slice(&[7, 8, 9, 255]);
        state.resize(3, 1);
        assert_eq!(state.rgba_at(1, 0), Some([7, 8, 9, 255]));
        assert_eq!(state.rgba_at(2, 0), Some([0, 0, 0, 255]));
        assert_eq!(state.region(Rect::new(0, 0, 3, 1)).len(), 12);
    }

    #[test]
    fn coalesces_damage_and_reports_the_first_change() {
        let framebuffer = Framebuffer::new(PixelOrder::Rgba);
        assert!(framebuffer.lock().resize(100, 50));
        assert!(!framebuffer.lock().resize(100, 50));
        assert_eq!(framebuffer.take_dirty(), [Rect::new(0, 0, 100, 50)]);
        let mut state = framebuffer.lock();
        assert!(state.mark_dirty(Rect::new(10, 10, 5, 5)));
        assert!(!state.mark_dirty(Rect::new(90, 40, 20, 20)));
        drop(state);
        assert_eq!(
            framebuffer.take_dirty(),
            [Rect::new(10, 10, 5, 5), Rect::new(90, 40, 10, 10)]
        );
        let mut state = framebuffer.lock();
        for i in 0..(MAX_DIRTY_RECTS as u32 + 1) {
            state.mark_dirty(Rect::new(i, 0, 1, 1));
        }
        drop(state);
        assert_eq!(framebuffer.take_dirty(), [Rect::new(0, 0, 65, 1)]);
    }

    #[test]
    fn draws_in_the_requested_order() {
        let framebuffer = Framebuffer::new(PixelOrder::Bgra);
        let mut state = framebuffer.lock();
        state.resize(4, 3);
        // A 2×2 RGBX image hanging off the right edge.
        let data = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0, 10, 11, 12, 0];
        state.blit(3, 1, 2, 2, &data, 4);
        assert_eq!(state.rgba_at(3, 1), Some([1, 2, 3, 255]));
        assert_eq!(state.rgba_at(3, 2), Some([7, 8, 9, 255]));
        assert_eq!(state.region(Rect::new(3, 1, 1, 1)), [3, 2, 1, 255]);
        state.fill(Rect::new(0, 0, 2, 1), [9, 9, 9]);
        assert_eq!(state.rgba_at(1, 0), Some([9, 9, 9, 255]));
        // Overlapping copy: shift the first row right by one.
        state.copy_within(0, 0, Rect::new(1, 0, 3, 1));
        assert_eq!(state.rgba_at(1, 0), Some([9, 9, 9, 255]));
        assert_eq!(state.rgba_at(2, 0), Some([9, 9, 9, 255]));
        assert_eq!(state.rgba_at(3, 0), Some([0, 0, 0, 255]));
    }
}
