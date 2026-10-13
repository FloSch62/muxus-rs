//! The remote screen as a shared pixel buffer plus the regions changed since the view last
//! looked.
//!
//! The session task writes decoded updates into it and sends [`crate::RdpEvent::Frame`]
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

    /// Copy `rect` from a source image laid out like this buffer (same order, `src_stride`
    /// bytes per row, `rect` in the same coordinates), forcing alpha to opaque.
    pub(crate) fn copy_from_image(&mut self, rect: Rect, src: &[u8], src_stride: usize) {
        let Some(rect) = rect.clipped(self.width, self.height) else {
            return;
        };
        let stride = self.stride();
        let row_bytes = rect.width as usize * 4;
        for row in rect.y..rect.bottom() {
            let from = row as usize * src_stride + rect.x as usize * 4;
            let to = row as usize * stride + rect.x as usize * 4;
            let (Some(src_row), Some(dst_row)) = (
                src.get(from..from + row_bytes),
                self.pixels.get_mut(to..to + row_bytes),
            ) else {
                continue;
            };
            dst_row.copy_from_slice(src_row);
            for pixel in dst_row.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
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
    fn copies_regions_opaque() {
        let framebuffer = Framebuffer::new(PixelOrder::Bgra);
        let mut state = framebuffer.lock();
        state.resize(4, 2);
        let mut src = vec![0u8; 4 * 2 * 4];
        src[(4 + 2) * 4..(4 + 2) * 4 + 4].copy_from_slice(&[1, 2, 3, 0]);
        state.copy_from_image(Rect::new(1, 1, 2, 1), &src, 16);
        assert_eq!(state.rgba_at(2, 1), Some([3, 2, 1, 255]));
        assert_eq!(state.region(Rect::new(2, 1, 1, 1)), [1, 2, 3, 255]);
    }
}
