//! The remote desktop as GPU images, updated from dirty rectangles.
//!
//! The browser kept one canvas and IronRDP/noVNC wrote changed regions into it. GPUI has no
//! partial texture upload, and a new image per frame would copy and upload the whole desktop
//! for every changed cursor blink. So the picture is cut into tiles of [`TILE`] pixels, each
//! its own image: a change rebuilds (copies and uploads) only the tiles it touches.
//!
//! Each tile image carries a one-pixel border of its neighbours' pixels. The tile is drawn
//! clipped to its own area, so the GPU's bilinear filtering at the seams samples the right
//! neighbours instead of whatever else sits next to it in the texture atlas; scaled pictures
//! show no tile edges.

use std::sync::Arc;

use muxus_session::desktop::{DirtyRect, FramePixels, ViewRect};
use muxus_ui::gpui_kit::{Bounds, Corners, Pixels, RenderImage, Window, point, px, size};

/// Tile edge in desktop pixels.
pub const TILE: u32 = 256;

/// How the desktop is cut into tiles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TileGrid {
    pub width: u32,
    pub height: u32,
    pub columns: u32,
    pub rows: u32,
}

impl TileGrid {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            columns: width.div_ceil(TILE),
            rows: height.div_ceil(TILE),
        }
    }

    pub fn len(&self) -> usize {
        self.columns as usize * self.rows as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The pixels tile `index` shows.
    pub fn area(&self, index: usize) -> DirtyRect {
        let column = index as u32 % self.columns.max(1);
        let row = index as u32 / self.columns.max(1);
        let x = column * TILE;
        let y = row * TILE;
        DirtyRect {
            x,
            y,
            width: TILE.min(self.width - x),
            height: TILE.min(self.height - y),
        }
    }

    /// The pixels tile `index` holds: its area plus a one-pixel border inside the desktop.
    pub fn padded(&self, index: usize) -> DirtyRect {
        let area = self.area(index);
        let x = area.x.saturating_sub(1);
        let y = area.y.saturating_sub(1);
        let right = (area.x + area.width + 1).min(self.width);
        let bottom = (area.y + area.height + 1).min(self.height);
        DirtyRect {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }

    /// The tiles whose pixels (border included) a change touches, in order.
    pub fn touched(&self, dirty: &[DirtyRect]) -> Vec<usize> {
        let mut tiles = vec![false; self.len()];
        for rect in dirty {
            if rect.width == 0 || rect.height == 0 || self.is_empty() {
                continue;
            }
            // Widen by one pixel: the neighbours' borders show these pixels too.
            let left = rect.x.saturating_sub(1).min(self.width - 1);
            let top = rect.y.saturating_sub(1).min(self.height - 1);
            let right = (rect.x + rect.width).min(self.width - 1);
            let bottom = (rect.y + rect.height).min(self.height - 1);
            for row in top / TILE..=bottom / TILE {
                for column in left / TILE..=right / TILE {
                    tiles[(row * self.columns + column) as usize] = true;
                }
            }
        }
        tiles
            .iter()
            .enumerate()
            .filter_map(|(index, touched)| touched.then_some(index))
            .collect()
    }
}

/// Copy `rect` out of the framebuffer, row by row, as an opaque BGRA image.
fn tile_image(frame: &FramePixels<'_>, rect: DirtyRect) -> Option<Arc<RenderImage>> {
    let row_bytes = rect.width as usize * 4;
    let mut bytes = Vec::with_capacity(row_bytes * rect.height as usize);
    for row in rect.y..rect.y + rect.height {
        let start = row as usize * frame.stride + rect.x as usize * 4;
        bytes.extend_from_slice(frame.pixels.get(start..start + row_bytes)?);
    }
    let buffer = image::RgbaImage::from_raw(rect.width, rect.height, bytes)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// The desktop's tile images.
#[derive(Default)]
pub struct Picture {
    grid: TileGrid,
    tiles: Vec<Option<Arc<RenderImage>>>,
    /// Replaced images, removed from the atlas once the frame that drew them is gone.
    retired: Vec<Arc<RenderImage>>,
}

impl Picture {
    pub fn grid(&self) -> TileGrid {
        self.grid
    }

    /// The desktop has a new size: every tile goes (the next frame redraws all of it).
    pub fn resize(&mut self, width: u32, height: u32) {
        if self.grid.width == width && self.grid.height == height {
            return;
        }
        self.retired.extend(self.tiles.drain(..).flatten());
        self.grid = TileGrid::new(width, height);
        self.tiles = vec![None; self.grid.len()];
    }

    /// Rebuild the tiles `dirty` touches from the framebuffer (BGRA). Returns how many.
    pub fn update(&mut self, frame: FramePixels<'_>, dirty: &[DirtyRect]) -> usize {
        if frame.width != self.grid.width || frame.height != self.grid.height {
            self.resize(frame.width, frame.height);
        }
        let touched = self.grid.touched(dirty);
        for &index in &touched {
            let image = tile_image(&frame, self.grid.padded(index));
            if let Some(old) = std::mem::replace(&mut self.tiles[index], image) {
                self.retired.push(old);
            }
        }
        touched.len()
    }

    /// Images replaced since the last call; drop them from the atlas.
    pub fn take_retired(&mut self) -> Vec<Arc<RenderImage>> {
        std::mem::take(&mut self.retired)
    }

    /// Everything, for dropping when the view goes away.
    pub fn clear(&mut self) -> Vec<Arc<RenderImage>> {
        let mut all = self.take_retired();
        all.extend(self.tiles.drain(..).flatten());
        self.grid = TileGrid::default();
        all
    }

    /// What to paint: the grid and the current images.
    pub fn snapshot(&self) -> PictureSnapshot {
        PictureSnapshot {
            grid: self.grid,
            tiles: self.tiles.clone(),
        }
    }
}

/// The picture as one frame paints it.
#[derive(Clone, Default)]
pub struct PictureSnapshot {
    grid: TileGrid,
    tiles: Vec<Option<Arc<RenderImage>>>,
}

impl PictureSnapshot {
    /// Paint the tiles into `view` (the fitted desktop box, in window coordinates).
    pub fn paint(&self, view: ViewRect, window: &mut Window) {
        let grid = self.grid;
        if grid.is_empty() || view.width <= 0.0 || view.height <= 0.0 {
            return;
        }
        // Every edge comes from the same function, so neighbouring tiles share it exactly and
        // snap to the same device pixel.
        let x_at =
            |x: u32| px((view.left + f64::from(x) * view.width / f64::from(grid.width)) as f32);
        let y_at =
            |y: u32| px((view.top + f64::from(y) * view.height / f64::from(grid.height)) as f32);
        let bounds = |rect: DirtyRect| -> Bounds<Pixels> {
            let (left, top) = (x_at(rect.x), y_at(rect.y));
            Bounds {
                origin: point(left, top),
                size: size(
                    x_at(rect.x + rect.width) - left,
                    y_at(rect.y + rect.height) - top,
                ),
            }
        };
        for (index, tile) in self.tiles.iter().enumerate() {
            let Some(image) = tile else {
                continue;
            };
            let _ = window.paint_image(
                bounds(grid.area(index)),
                bounds(grid.padded(index)),
                Corners::default(),
                image.clone(),
                0,
                false,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u32, y: u32, width: u32, height: u32) -> DirtyRect {
        DirtyRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn cuts_the_desktop_into_tiles() {
        let grid = TileGrid::new(1132, 786);
        assert_eq!((grid.columns, grid.rows), (5, 4));
        assert_eq!(grid.area(0), rect(0, 0, 256, 256));
        assert_eq!(grid.area(4), rect(1024, 0, 108, 256));
        assert_eq!(grid.area(19), rect(1024, 768, 108, 18));
        assert_eq!(grid.padded(0), rect(0, 0, 257, 257));
        assert_eq!(grid.padded(6), rect(255, 255, 258, 258));
        assert_eq!(grid.padded(19), rect(1023, 767, 109, 19));
        assert!(TileGrid::new(0, 0).is_empty());
    }

    #[test]
    fn rebuilds_only_the_tiles_a_change_touches() {
        let grid = TileGrid::new(1132, 786);
        assert_eq!(grid.touched(&[rect(10, 10, 20, 20)]), [0]);
        // A pixel next to a seam shows in the neighbour's border too.
        assert_eq!(grid.touched(&[rect(255, 10, 1, 1)]), [0, 1]);
        assert_eq!(grid.touched(&[rect(256, 10, 1, 1)]), [0, 1]);
        assert_eq!(grid.touched(&[rect(300, 300, 1, 1)]), [6]);
        assert_eq!(grid.touched(&[rect(0, 0, 1132, 786)]).len(), 20);
        assert!(grid.touched(&[rect(5, 5, 0, 3)]).is_empty());
        // Damage reported past the edge is clipped.
        assert_eq!(grid.touched(&[rect(1130, 780, 50, 50)]), [19]);
    }

    #[test]
    fn copies_tiles_with_their_border() {
        let (width, height) = (300u32, 2u32);
        let pixels: Vec<u8> = (0..width * height)
            .flat_map(|i| [(i % 256) as u8, (i / 256) as u8, 7, 255])
            .collect();
        let frame = FramePixels {
            width,
            height,
            stride: width as usize * 4,
            pixels: &pixels,
        };
        let mut picture = Picture::default();
        assert_eq!(picture.update(frame, &[rect(0, 0, width, height)]), 2);
        assert_eq!(picture.grid().columns, 2);
        let snapshot = picture.snapshot();
        let second = snapshot.tiles[1].as_ref().unwrap();
        // Columns 255..300: a one-pixel border on the left.
        assert_eq!(second.size(0).width.0, 45);
        let bytes = second.as_bytes(0).unwrap();
        assert_eq!(&bytes[..4], &[255, 0, 7, 255]);
        assert_eq!(&bytes[4..8], &[0, 1, 7, 255]);
        // A small change replaces one tile and retires its old image.
        assert_eq!(picture.update(frame, &[rect(280, 1, 2, 1)]), 1);
        assert_eq!(picture.take_retired().len(), 1);
        assert!(picture.take_retired().is_empty());
        picture.resize(10, 10);
        assert_eq!(picture.take_retired().len(), 2);
    }
}
