// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a
// copy of the MPL was not distributed with this file, You can obtain one at
// https://mozilla.org/MPL/2.0/. Adapted from noVNC (Copyright The noVNC authors).

//! Framebuffer update encodings: Raw, CopyRect, RRE, Hextile, Zlib, Tight, TightPNG, ZRLE and
//! JPEG.
//!
//! Port of noVNC's `core/decoders/*` (MPL-2.0). noVNC decodes from a receive queue and resumes
//! when more data arrives; here each decoder reads what it needs from the socket with awaits,
//! so there is no resumption state left, only the compression streams that live across
//! rectangles. Pixels arrive in the format the session asks for (32 bits, little endian, red
//! in the low byte), so on the wire a pixel is R, G, B, padding.

use std::io::Cursor;

use flate2::{Decompress, FlushDecompress, Status};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::framebuffer::{Framebuffer, Rect};

pub(crate) const ENCODING_RAW: i32 = 0;
pub(crate) const ENCODING_COPY_RECT: i32 = 1;
pub(crate) const ENCODING_RRE: i32 = 2;
pub(crate) const ENCODING_HEXTILE: i32 = 5;
pub(crate) const ENCODING_ZLIB: i32 = 6;
pub(crate) const ENCODING_TIGHT: i32 = 7;
pub(crate) const ENCODING_ZRLE: i32 = 16;
pub(crate) const ENCODING_TIGHT_PNG: i32 = -260;
pub(crate) const ENCODING_JPEG: i32 = 21;

/// Rectangles larger than this are not a desktop update but a broken or hostile server.
const MAX_RECT_PIXELS: u64 = 1 << 26;
/// The largest compressed block accepted.
const MAX_DATA_BYTES: usize = 1 << 28;

pub(crate) type DecodeResult<T> = Result<T, String>;

fn io_error(err: std::io::Error) -> String {
    format!("Connection closed ({err})")
}

/// Raw output of one zlib stream that lives across rectangles.
struct Inflater {
    stream: Decompress,
}

impl Inflater {
    fn new() -> Self {
        Self {
            stream: Decompress::new(true),
        }
    }

    fn reset(&mut self) {
        self.stream.reset(true);
    }

    /// Everything the input inflates to (servers sync-flush at the end of each rectangle);
    /// with `expected`, exactly that many bytes or an error.
    fn inflate(&mut self, input: &[u8], expected: Option<usize>) -> DecodeResult<Vec<u8>> {
        let mut out = Vec::with_capacity(expected.unwrap_or(input.len() * 4).max(64));
        let mut offset = 0;
        loop {
            if out.len() == out.capacity() {
                if expected.is_some_and(|expected| out.len() >= expected) {
                    break;
                }
                if out.len() >= MAX_DATA_BYTES {
                    return Err("zlib output too large".into());
                }
                out.reserve(out.capacity().max(4096));
            }
            let (before_in, before_out) = (self.stream.total_in(), self.stream.total_out());
            let status = self
                .stream
                .decompress_vec(&input[offset..], &mut out, FlushDecompress::Sync)
                .map_err(|err| format!("zlib: {err}"))?;
            offset += (self.stream.total_in() - before_in) as usize;
            let produced = self.stream.total_out() - before_out;
            if status == Status::StreamEnd
                || (offset >= input.len() && produced == 0)
                || (produced == 0 && self.stream.total_in() == before_in)
            {
                break;
            }
        }
        if let Some(expected) = expected {
            if out.len() < expected {
                return Err(format!(
                    "Incomplete zlib block (got {} of {expected} bytes)",
                    out.len()
                ));
            }
            out.truncate(expected);
        }
        Ok(out)
    }
}

/// Decoder state that survives between rectangles.
pub(crate) struct Decoders {
    zlib: Inflater,
    tight: [Inflater; 4],
    zrle: Inflater,
    hextile_background: [u8; 3],
    hextile_foreground: [u8; 3],
    jpeg_quant_tables: Vec<Vec<u8>>,
    jpeg_huffman_tables: Vec<Vec<u8>>,
}

impl Decoders {
    pub(crate) fn new() -> Self {
        Self {
            zlib: Inflater::new(),
            tight: [
                Inflater::new(),
                Inflater::new(),
                Inflater::new(),
                Inflater::new(),
            ],
            zrle: Inflater::new(),
            hextile_background: [0; 3],
            hextile_foreground: [0; 3],
            jpeg_quant_tables: Vec::new(),
            jpeg_huffman_tables: Vec::new(),
        }
    }

    pub(crate) fn supports(encoding: i32) -> bool {
        matches!(
            encoding,
            ENCODING_RAW
                | ENCODING_COPY_RECT
                | ENCODING_RRE
                | ENCODING_HEXTILE
                | ENCODING_ZLIB
                | ENCODING_TIGHT
                | ENCODING_TIGHT_PNG
                | ENCODING_ZRLE
                | ENCODING_JPEG
        )
    }

    /// Decode one rectangle of `encoding` into the framebuffer.
    pub(crate) async fn decode<R: AsyncRead + Unpin>(
        &mut self,
        encoding: i32,
        rect: Rect,
        depth: u8,
        reader: &mut R,
        framebuffer: &Framebuffer,
    ) -> DecodeResult<()> {
        if u64::from(rect.width) * u64::from(rect.height) > MAX_RECT_PIXELS {
            return Err(format!(
                "rectangle too large ({}x{})",
                rect.width, rect.height
            ));
        }
        match encoding {
            ENCODING_RAW => raw(rect, depth, reader, framebuffer).await,
            ENCODING_COPY_RECT => copy_rect(rect, reader, framebuffer).await,
            ENCODING_RRE => rre(rect, reader, framebuffer).await,
            ENCODING_HEXTILE => self.hextile(rect, reader, framebuffer).await,
            ENCODING_ZLIB => self.zlib(rect, reader, framebuffer).await,
            ENCODING_TIGHT => self.tight(rect, reader, framebuffer, false).await,
            ENCODING_TIGHT_PNG => self.tight(rect, reader, framebuffer, true).await,
            ENCODING_ZRLE => self.zrle(rect, reader, framebuffer).await,
            ENCODING_JPEG => self.jpeg(rect, reader, framebuffer).await,
            other => Err(format!("Unsupported encoding (encoding: {other})")),
        }
    }

    async fn hextile<R: AsyncRead + Unpin>(
        &mut self,
        rect: Rect,
        reader: &mut R,
        framebuffer: &Framebuffer,
    ) -> DecodeResult<()> {
        let tiles_x = rect.width.div_ceil(16);
        let tiles_y = rect.height.div_ceil(16);
        let mut last_subencoding = 0u8;
        let mut tile = vec![0u8; 16 * 16 * 3];
        for tile_index in 0..tiles_x * tiles_y {
            let subencoding = reader.read_u8().await.map_err(io_error)?;
            if subencoding > 30 {
                return Err(format!(
                    "Illegal hextile subencoding (subencoding: {subencoding})"
                ));
            }
            let tx = rect.x + (tile_index % tiles_x) * 16;
            let ty = rect.y + (tile_index / tiles_x) * 16;
            let tw = 16.min(rect.x + rect.width - tx);
            let th = 16.min(rect.y + rect.height - ty);
            let tile_rect = Rect::new(tx, ty, tw, th);
            if subencoding == 0 {
                // Blanks after a raw tile are ignored (servers send them as filler).
                if last_subencoding & 0x01 == 0 {
                    framebuffer.lock().fill(tile_rect, self.hextile_background);
                }
            } else if subencoding & 0x01 != 0 {
                let mut data = vec![0u8; (tw * th * 4) as usize];
                reader.read_exact(&mut data).await.map_err(io_error)?;
                framebuffer.lock().blit(tx, ty, tw, th, &data, 4);
            } else {
                if subencoding & 0x02 != 0 {
                    self.hextile_background = read_pixel(reader).await?;
                }
                if subencoding & 0x04 != 0 {
                    self.hextile_foreground = read_pixel(reader).await?;
                }
                for pixel in tile.chunks_mut(3).take((tw * th) as usize) {
                    pixel.copy_from_slice(&self.hextile_background);
                }
                if subencoding & 0x08 != 0 {
                    let subrects = reader.read_u8().await.map_err(io_error)?;
                    for _ in 0..subrects {
                        let color = if subencoding & 0x10 != 0 {
                            read_pixel(reader).await?
                        } else {
                            self.hextile_foreground
                        };
                        let xy = reader.read_u8().await.map_err(io_error)?;
                        let wh = reader.read_u8().await.map_err(io_error)?;
                        let (sx, sy) = (u32::from(xy >> 4), u32::from(xy & 0x0f));
                        let (sw, sh) = (u32::from(wh >> 4) + 1, u32::from(wh & 0x0f) + 1);
                        for row in sy..(sy + sh).min(th) {
                            for column in sx..(sx + sw).min(tw) {
                                let at = ((row * tw + column) * 3) as usize;
                                tile[at..at + 3].copy_from_slice(&color);
                            }
                        }
                    }
                }
                framebuffer
                    .lock()
                    .blit(tx, ty, tw, th, &tile[..(tw * th * 3) as usize], 3);
            }
            last_subencoding = subencoding;
        }
        Ok(())
    }

    async fn zlib<R: AsyncRead + Unpin>(
        &mut self,
        rect: Rect,
        reader: &mut R,
        framebuffer: &Framebuffer,
    ) -> DecodeResult<()> {
        if rect.width == 0 || rect.height == 0 {
            return Ok(());
        }
        let data = read_length_prefixed(reader).await?;
        let pixels = self
            .zlib
            .inflate(&data, Some((rect.width * rect.height * 4) as usize))?;
        framebuffer
            .lock()
            .blit(rect.x, rect.y, rect.width, rect.height, &pixels, 4);
        Ok(())
    }

    async fn zrle<R: AsyncRead + Unpin>(
        &mut self,
        rect: Rect,
        reader: &mut R,
        framebuffer: &Framebuffer,
    ) -> DecodeResult<()> {
        let data = read_length_prefixed(reader).await?;
        let inflated = self.zrle.inflate(&data, None)?;
        let mut input = ByteReader::new(&inflated);
        const TILE: u32 = 64;
        let mut tile = vec![0u8; (TILE * TILE * 3) as usize];
        let mut ty = rect.y;
        while ty < rect.y + rect.height {
            let th = TILE.min(rect.y + rect.height - ty);
            let mut tx = rect.x;
            while tx < rect.x + rect.width {
                let tw = TILE.min(rect.x + rect.width - tx);
                let size = (tw * th) as usize;
                let subencoding = input.u8()?;
                match subencoding {
                    0 => {
                        let data = input.take(size * 3)?;
                        framebuffer.lock().blit(tx, ty, tw, th, data, 3);
                    }
                    1 => {
                        let color = input.pixel()?;
                        framebuffer.lock().fill(Rect::new(tx, ty, tw, th), color);
                    }
                    2..=16 => {
                        let palette = input.palette(usize::from(subencoding))?;
                        let bits = match subencoding {
                            2 => 1,
                            3..=4 => 2,
                            _ => 4,
                        };
                        let mask = (1u8 << bits) - 1;
                        let mut offset = 0;
                        for _ in 0..th {
                            let mut shift: i32 = 8 - bits;
                            let mut encoded = input.u8()?;
                            for _ in 0..tw {
                                if shift < 0 {
                                    shift = 8 - bits;
                                    encoded = input.u8()?;
                                }
                                let index = usize::from((encoded >> shift) & mask);
                                let color = palette
                                    .get(index)
                                    .ok_or_else(|| format!("Too big index in palette: {index}"))?;
                                tile[offset..offset + 3].copy_from_slice(color);
                                offset += 3;
                                shift -= bits;
                            }
                        }
                        framebuffer
                            .lock()
                            .blit(tx, ty, tw, th, &tile[..size * 3], 3);
                    }
                    128 => {
                        let mut index = 0;
                        while index < size {
                            let color = input.pixel()?;
                            let length = input.rle_length()?;
                            if index + length > size {
                                return Err(format!("Too big rle length: {length}"));
                            }
                            for _ in 0..length {
                                tile[index * 3..index * 3 + 3].copy_from_slice(&color);
                                index += 1;
                            }
                        }
                        framebuffer
                            .lock()
                            .blit(tx, ty, tw, th, &tile[..size * 3], 3);
                    }
                    130..=255 => {
                        let palette_size = usize::from(subencoding - 128);
                        let palette = input.palette(palette_size)?;
                        let mut index = 0;
                        while index < size {
                            let mut entry = usize::from(input.u8()?);
                            let mut length = 1;
                            if entry >= 128 {
                                entry -= 128;
                                length = input.rle_length()?;
                            }
                            let color = palette.get(entry).ok_or_else(|| {
                                format!(
                                    "Too big index in palette: {entry}, palette size: {palette_size}"
                                )
                            })?;
                            if index + length > size {
                                return Err(format!(
                                    "Too big rle length in palette mode: {length}, allowed length is: {}",
                                    size - index
                                ));
                            }
                            for _ in 0..length {
                                tile[index * 3..index * 3 + 3].copy_from_slice(color);
                                index += 1;
                            }
                        }
                        framebuffer
                            .lock()
                            .blit(tx, ty, tw, th, &tile[..size * 3], 3);
                    }
                    other => return Err(format!("Unknown subencoding: {other}")),
                }
                tx += TILE;
            }
            ty += TILE;
        }
        Ok(())
    }

    async fn tight<R: AsyncRead + Unpin>(
        &mut self,
        rect: Rect,
        reader: &mut R,
        framebuffer: &Framebuffer,
        png: bool,
    ) -> DecodeResult<()> {
        let control = reader.read_u8().await.map_err(io_error)?;
        // Reset streams if the server requests it.
        for (index, stream) in self.tight.iter_mut().enumerate() {
            if (control >> index) & 1 != 0 {
                stream.reset();
            }
        }
        let compression = control >> 4;
        let (x, y, width, height) = (rect.x, rect.y, rect.width, rect.height);
        match compression {
            0x08 => {
                let color = read_tpixel(reader).await?;
                framebuffer.lock().fill(rect, color);
            }
            0x09 => {
                let data = read_compact(reader).await?;
                draw_image(framebuffer, x, y, &decode_jpeg(&data)?);
            }
            0x0a if png => {
                let data = read_compact(reader).await?;
                draw_image(framebuffer, x, y, &decode_png(&data)?);
            }
            0x0a => return Err("PNG received in standard Tight rect".into()),
            basic if basic & 0x08 == 0 => {
                if png {
                    return Err("BasicCompression received in TightPNG rect".into());
                }
                let filter = if basic & 0x04 != 0 {
                    reader.read_u8().await.map_err(io_error)?
                } else {
                    // Implicit CopyFilter.
                    0
                };
                let stream = usize::from(basic & 0x03);
                match filter {
                    0 => {
                        let size = (width * height * 3) as usize;
                        let data = self.tight_data(reader, stream, size).await?;
                        framebuffer.lock().blit(x, y, width, height, &data, 3);
                    }
                    1 => {
                        let colors = usize::from(reader.read_u8().await.map_err(io_error)?) + 1;
                        let mut palette = vec![0u8; colors * 3];
                        reader.read_exact(&mut palette).await.map_err(io_error)?;
                        let bits_per_pixel = if colors <= 2 { 1 } else { 8 };
                        let row_size = (width as usize * bits_per_pixel).div_ceil(8);
                        let size = row_size * height as usize;
                        let data = self.tight_data(reader, stream, size).await?;
                        let mut rgb = vec![0u8; (width * height * 3) as usize];
                        for row in 0..height as usize {
                            for column in 0..width as usize {
                                let index = if colors == 2 {
                                    let byte = data[row * row_size + column / 8];
                                    usize::from((byte >> (7 - column % 8)) & 1)
                                } else {
                                    usize::from(data[row * width as usize + column])
                                };
                                let at = (row * width as usize + column) * 3;
                                if let Some(color) = palette.get(index * 3..index * 3 + 3) {
                                    rgb[at..at + 3].copy_from_slice(color);
                                }
                            }
                        }
                        framebuffer.lock().blit(x, y, width, height, &rgb, 3);
                    }
                    2 => {
                        let size = (width * height * 3) as usize;
                        let data = self.tight_data(reader, stream, size).await?;
                        let rgb = gradient(&data, width as usize, height as usize);
                        framebuffer.lock().blit(x, y, width, height, &rgb, 3);
                    }
                    other => {
                        return Err(format!("Illegal tight filter received (ctl: {other})"));
                    }
                }
            }
            other => {
                return Err(format!("Illegal tight compression received (ctl: {other})"));
            }
        }
        Ok(())
    }

    /// Tight's filtered data: raw when under 12 bytes, otherwise zlib on `stream`.
    async fn tight_data<R: AsyncRead + Unpin>(
        &mut self,
        reader: &mut R,
        stream: usize,
        size: usize,
    ) -> DecodeResult<Vec<u8>> {
        if size == 0 {
            return Ok(Vec::new());
        }
        if size < 12 {
            let mut data = vec![0u8; size];
            reader.read_exact(&mut data).await.map_err(io_error)?;
            return Ok(data);
        }
        let compressed = read_compact(reader).await?;
        self.tight[stream].inflate(&compressed, Some(size))
    }

    async fn jpeg<R: AsyncRead + Unpin>(
        &mut self,
        rect: Rect,
        reader: &mut R,
        framebuffer: &Framebuffer,
    ) -> DecodeResult<()> {
        // A JPEG rectangle is simply a JPEG file, whose tables RealVNC reuses between frames.
        let mut segments: Vec<Vec<u8>> = Vec::new();
        while !segments.last().is_some_and(|segment| segment[1] == 0xd9) {
            segments.extend(read_jpeg_segments(reader).await?);
        }
        let huffman: Vec<Vec<u8>> = segments.iter().filter(|s| s[1] == 0xc4).cloned().collect();
        let quant: Vec<Vec<u8>> = segments.iter().filter(|s| s[1] == 0xdb).cloned().collect();
        let sof = segments
            .iter()
            .position(|s| s[1] == 0xc0 || s[1] == 0xc2)
            .ok_or("Illegal JPEG image without SOF")?;
        if quant.is_empty() {
            for (offset, table) in self.jpeg_quant_tables.iter().enumerate() {
                segments.insert(sof + 1 + offset, table.clone());
            }
        }
        if huffman.is_empty() {
            for (offset, table) in self.jpeg_huffman_tables.iter().enumerate() {
                segments.insert(sof + 1 + offset, table.clone());
            }
        }
        let data: Vec<u8> = segments.concat();
        draw_image(framebuffer, rect.x, rect.y, &decode_jpeg(&data)?);
        if !huffman.is_empty() {
            self.jpeg_huffman_tables = huffman;
        }
        if !quant.is_empty() {
            self.jpeg_quant_tables = quant;
        }
        Ok(())
    }
}

/// An image decoded to RGB rows.
struct Image {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

fn draw_image(framebuffer: &Framebuffer, x: u32, y: u32, image: &Image) {
    framebuffer
        .lock()
        .blit(x, y, image.width, image.height, &image.rgb, 3);
}

fn decode_jpeg(data: &[u8]) -> DecodeResult<Image> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(Cursor::new(data), options);
    let rgb = decoder.decode().map_err(|err| format!("JPEG: {err:?}"))?;
    let info = decoder.info().ok_or("JPEG: no image information")?;
    Ok(Image {
        width: u32::from(info.width),
        height: u32::from(info.height),
        rgb,
    })
}

fn decode_png(data: &[u8]) -> DecodeResult<Image> {
    let mut decoder = png::Decoder::new(Cursor::new(data));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|err| format!("PNG: {err}"))?;
    let size = reader.output_buffer_size().ok_or("PNG: image too large")?;
    let mut buffer = vec![0u8; size];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|err| format!("PNG: {err}"))?;
    let channels = info.color_type.samples();
    let pixels = (info.width * info.height) as usize;
    let mut rgb = Vec::with_capacity(pixels * 3);
    for pixel in buffer[..info.buffer_size()].chunks(channels).take(pixels) {
        match channels {
            1 | 2 => rgb.extend([pixel[0]; 3]),
            _ => rgb.extend_from_slice(&pixel[..3]),
        }
    }
    Ok(Image {
        width: info.width,
        height: info.height,
        rgb,
    })
}

/// Tight's gradient filter: each pixel predicted from left + above - above-left.
fn gradient(data: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut rgb = vec![0u8; width * height * 3];
    for y in 0..height {
        for x in 0..width {
            for c in 0..3 {
                let at = (y * width + x) * 3 + c;
                let left = if x > 0 { i32::from(rgb[at - 3]) } else { 0 };
                let (upper, upper_left) = if y > 0 {
                    let above = at - width * 3;
                    (
                        i32::from(rgb[above]),
                        if x > 0 { i32::from(rgb[above - 3]) } else { 0 },
                    )
                } else {
                    (0, 0)
                };
                let prediction = if y == 0 {
                    left
                } else {
                    (left + upper - upper_left).clamp(0, 255)
                };
                rgb[at] = data[at].wrapping_add(prediction as u8);
            }
        }
    }
    rgb
}

async fn raw<R: AsyncRead + Unpin>(
    rect: Rect,
    depth: u8,
    reader: &mut R,
    framebuffer: &Framebuffer,
) -> DecodeResult<()> {
    if rect.width == 0 || rect.height == 0 {
        return Ok(());
    }
    let pixel_size = if depth == 8 { 1 } else { 4 };
    let mut line = vec![0u8; rect.width as usize * pixel_size];
    for row in 0..rect.height {
        reader.read_exact(&mut line).await.map_err(io_error)?;
        if depth == 8 {
            // Low colour mode: 2 bits per channel, red in the lowest.
            let scale = |value: u8| ((u32::from(value & 0x3) * 255) / 3) as u8;
            let rgb: Vec<u8> = line
                .iter()
                .flat_map(|&v| [scale(v), scale(v >> 2), scale(v >> 4)])
                .collect();
            framebuffer
                .lock()
                .blit(rect.x, rect.y + row, rect.width, 1, &rgb, 3);
        } else {
            framebuffer
                .lock()
                .blit(rect.x, rect.y + row, rect.width, 1, &line, 4);
        }
    }
    Ok(())
}

async fn copy_rect<R: AsyncRead + Unpin>(
    rect: Rect,
    reader: &mut R,
    framebuffer: &Framebuffer,
) -> DecodeResult<()> {
    let src_x = reader.read_u16().await.map_err(io_error)?;
    let src_y = reader.read_u16().await.map_err(io_error)?;
    if rect.width == 0 || rect.height == 0 {
        return Ok(());
    }
    framebuffer
        .lock()
        .copy_within(u32::from(src_x), u32::from(src_y), rect);
    Ok(())
}

async fn rre<R: AsyncRead + Unpin>(
    rect: Rect,
    reader: &mut R,
    framebuffer: &Framebuffer,
) -> DecodeResult<()> {
    let subrects = reader.read_u32().await.map_err(io_error)?;
    let background = read_pixel(reader).await?;
    framebuffer.lock().fill(rect, background);
    for _ in 0..subrects {
        let color = read_pixel(reader).await?;
        let sx = u32::from(reader.read_u16().await.map_err(io_error)?);
        let sy = u32::from(reader.read_u16().await.map_err(io_error)?);
        let sw = u32::from(reader.read_u16().await.map_err(io_error)?);
        let sh = u32::from(reader.read_u16().await.map_err(io_error)?);
        framebuffer
            .lock()
            .fill(Rect::new(rect.x + sx, rect.y + sy, sw, sh), color);
    }
    Ok(())
}

/// A 4-byte pixel (R, G, B, padding).
async fn read_pixel<R: AsyncRead + Unpin>(reader: &mut R) -> DecodeResult<[u8; 3]> {
    let mut pixel = [0u8; 4];
    reader.read_exact(&mut pixel).await.map_err(io_error)?;
    Ok([pixel[0], pixel[1], pixel[2]])
}

/// Tight's 3-byte TPIXEL.
async fn read_tpixel<R: AsyncRead + Unpin>(reader: &mut R) -> DecodeResult<[u8; 3]> {
    let mut pixel = [0u8; 3];
    reader.read_exact(&mut pixel).await.map_err(io_error)?;
    Ok(pixel)
}

async fn read_length_prefixed<R: AsyncRead + Unpin>(reader: &mut R) -> DecodeResult<Vec<u8>> {
    let length = reader.read_u32().await.map_err(io_error)? as usize;
    if length > MAX_DATA_BYTES {
        return Err(format!("compressed block too large ({length} bytes)"));
    }
    let mut data = vec![0u8; length];
    reader.read_exact(&mut data).await.map_err(io_error)?;
    Ok(data)
}

/// Tight's compact length (7 bits per byte, up to three bytes), then the data.
async fn read_compact<R: AsyncRead + Unpin>(reader: &mut R) -> DecodeResult<Vec<u8>> {
    let mut byte = reader.read_u8().await.map_err(io_error)?;
    let mut length = usize::from(byte & 0x7f);
    if byte & 0x80 != 0 {
        byte = reader.read_u8().await.map_err(io_error)?;
        length |= usize::from(byte & 0x7f) << 7;
        if byte & 0x80 != 0 {
            byte = reader.read_u8().await.map_err(io_error)?;
            length |= usize::from(byte) << 14;
        }
    }
    let mut data = vec![0u8; length];
    reader.read_exact(&mut data).await.map_err(io_error)?;
    Ok(data)
}

/// The next JPEG marker segment, entropy-coded data included for start-of-scan. Finding the
/// end of a scan means reading the following marker, so that segment comes along too.
async fn read_jpeg_segments<R: AsyncRead + Unpin>(reader: &mut R) -> DecodeResult<Vec<Vec<u8>>> {
    let marker = reader.read_u8().await.map_err(io_error)?;
    if marker != 0xff {
        return Err(format!("Illegal JPEG marker received (byte: {marker})"));
    }
    let kind = reader.read_u8().await.map_err(io_error)?;
    let segment = read_jpeg_segment_body(reader, kind).await?;
    if kind != 0xda {
        return Ok(vec![segment]);
    }
    // Start of scan: the entropy-coded data runs to the next marker that is neither a stuffed
    // 0xFF00 nor a restart marker.
    let mut scan = segment;
    loop {
        let byte = reader.read_u8().await.map_err(io_error)?;
        if byte != 0xff {
            scan.push(byte);
            continue;
        }
        let next = reader.read_u8().await.map_err(io_error)?;
        if next == 0x00 || (0xd0..=0xd7).contains(&next) {
            scan.extend([byte, next]);
            continue;
        }
        let following = read_jpeg_segment_body(reader, next).await?;
        return Ok(vec![scan, following]);
    }
}

/// The rest of a segment whose marker byte `kind` was read.
async fn read_jpeg_segment_body<R: AsyncRead + Unpin>(
    reader: &mut R,
    kind: u8,
) -> DecodeResult<Vec<u8>> {
    if (0xd0..=0xd9).contains(&kind) || kind == 0x01 {
        // No length after the marker.
        return Ok(vec![0xff, kind]);
    }
    let length = reader.read_u16().await.map_err(io_error)?;
    if length < 2 {
        return Err(format!("Illegal JPEG length received (length: {length})"));
    }
    let mut segment = vec![0xff, kind];
    segment.extend(length.to_be_bytes());
    let mut body = vec![0u8; usize::from(length) - 2];
    reader.read_exact(&mut body).await.map_err(io_error)?;
    segment.extend(body);
    Ok(segment)
}

/// Reads from an inflated ZRLE block.
struct ByteReader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> ByteReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    fn take(&mut self, len: usize) -> DecodeResult<&'a [u8]> {
        let slice = self
            .data
            .get(self.at..self.at + len)
            .ok_or("ZRLE data ended early")?;
        self.at += len;
        Ok(slice)
    }

    fn u8(&mut self) -> DecodeResult<u8> {
        Ok(self.take(1)?[0])
    }

    /// A CPIXEL: three bytes at depth 24.
    fn pixel(&mut self) -> DecodeResult<[u8; 3]> {
        let bytes = self.take(3)?;
        Ok([bytes[0], bytes[1], bytes[2]])
    }

    fn palette(&mut self, size: usize) -> DecodeResult<Vec<[u8; 3]>> {
        (0..size).map(|_| self.pixel()).collect()
    }

    fn rle_length(&mut self) -> DecodeResult<usize> {
        let mut length = 0usize;
        loop {
            let byte = self.u8()?;
            length += usize::from(byte);
            if byte != 255 {
                return Ok(length + 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuffer::PixelOrder;
    use flate2::{Compress, Compression, FlushCompress};

    fn framebuffer(width: u32, height: u32) -> Framebuffer {
        let framebuffer = Framebuffer::new(PixelOrder::Rgba);
        framebuffer.lock().resize(width, height);
        framebuffer
    }

    fn deflate(stream: &mut Compress, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 64);
        stream
            .compress_vec(data, &mut out, FlushCompress::Sync)
            .unwrap();
        out
    }

    async fn decode(
        decoders: &mut Decoders,
        encoding: i32,
        rect: Rect,
        wire: &[u8],
        fb: &Framebuffer,
    ) {
        let mut reader = wire;
        decoders
            .decode(encoding, rect, 24, &mut reader, fb)
            .await
            .unwrap();
        assert!(reader.is_empty(), "{} bytes left over", reader.len());
    }

    #[tokio::test]
    async fn raw_copyrect_and_rre() {
        let fb = framebuffer(4, 4);
        let mut decoders = Decoders::new();
        let wire: Vec<u8> = [[10, 20, 30, 0], [40, 50, 60, 0]].concat();
        decode(
            &mut decoders,
            ENCODING_RAW,
            Rect::new(0, 0, 2, 1),
            &wire,
            &fb,
        )
        .await;
        assert_eq!(fb.lock().rgba_at(1, 0), Some([40, 50, 60, 255]));
        decode(
            &mut decoders,
            ENCODING_COPY_RECT,
            Rect::new(2, 3, 2, 1),
            &[0, 0, 0, 0],
            &fb,
        )
        .await;
        assert_eq!(fb.lock().rgba_at(3, 3), Some([40, 50, 60, 255]));
        let mut rre = 1u32.to_be_bytes().to_vec();
        rre.extend([1, 1, 1, 0, 9, 9, 9, 0, 0, 1, 0, 1, 0, 1, 0, 1]);
        decode(
            &mut decoders,
            ENCODING_RRE,
            Rect::new(0, 0, 3, 3),
            &rre,
            &fb,
        )
        .await;
        assert_eq!(fb.lock().rgba_at(0, 0), Some([1, 1, 1, 255]));
        assert_eq!(fb.lock().rgba_at(1, 1), Some([9, 9, 9, 255]));
    }

    #[tokio::test]
    async fn hextile_tiles() {
        let fb = framebuffer(20, 4);
        let mut decoders = Decoders::new();
        // Tile 1: background + coloured subrect; tile 2 (4 wide): raw.
        let mut wire = vec![0x02 | 0x08 | 0x10, 5, 5, 5, 0, 1, 0, 0, 0, 0, 0x11, 0x11];
        wire.push(0x01);
        wire.extend([7u8, 7, 7, 0].repeat(4 * 4));
        decode(
            &mut decoders,
            ENCODING_HEXTILE,
            Rect::new(0, 0, 20, 4),
            &wire,
            &fb,
        )
        .await;
        let state = fb.lock();
        assert_eq!(state.rgba_at(3, 3), Some([5, 5, 5, 255]));
        assert_eq!(state.rgba_at(1, 1), Some([0, 0, 0, 255]));
        assert_eq!(state.rgba_at(17, 2), Some([7, 7, 7, 255]));
    }

    #[tokio::test]
    async fn zlib_streams_survive_between_rectangles() {
        let fb = framebuffer(2, 2);
        let mut decoders = Decoders::new();
        let mut stream = Compress::new(Compression::default(), true);
        for color in [3u8, 4] {
            let compressed = deflate(&mut stream, &[color, color, color, 0].repeat(4));
            let mut wire = (compressed.len() as u32).to_be_bytes().to_vec();
            wire.extend(compressed);
            decode(
                &mut decoders,
                ENCODING_ZLIB,
                Rect::new(0, 0, 2, 2),
                &wire,
                &fb,
            )
            .await;
            assert_eq!(fb.lock().rgba_at(1, 1), Some([color, color, color, 255]));
        }
    }

    #[tokio::test]
    async fn tight_fill_copy_palette_and_gradient() {
        let fb = framebuffer(8, 8);
        let mut decoders = Decoders::new();
        decode(
            &mut decoders,
            ENCODING_TIGHT,
            Rect::new(0, 0, 8, 8),
            &[0x80, 1, 2, 3],
            &fb,
        )
        .await;
        assert_eq!(fb.lock().rgba_at(7, 7), Some([1, 2, 3, 255]));

        // Copy filter, small enough to be sent raw.
        decode(
            &mut decoders,
            ENCODING_TIGHT,
            Rect::new(0, 0, 3, 1),
            &[0x00, 9, 8, 7, 6, 5, 4, 3, 2, 1],
            &fb,
        )
        .await;
        assert_eq!(fb.lock().rgba_at(2, 0), Some([3, 2, 1, 255]));

        // Copy filter through zlib stream 1.
        let mut stream = Compress::new(Compression::default(), true);
        let pixels = [50u8, 60, 70].repeat(16);
        let compressed = deflate(&mut stream, &pixels);
        let mut wire = vec![0x10];
        wire.push(compressed.len() as u8);
        wire.extend(&compressed);
        decode(
            &mut decoders,
            ENCODING_TIGHT,
            Rect::new(4, 4, 4, 4),
            &wire,
            &fb,
        )
        .await;
        assert_eq!(fb.lock().rgba_at(5, 6), Some([50, 60, 70, 255]));

        // Two-colour palette: one bit per pixel, rows padded to bytes.
        let wire = [0x40, 1, 1, 0, 0, 0, 255, 255, 255, 0b1010_0000];
        decode(
            &mut decoders,
            ENCODING_TIGHT,
            Rect::new(0, 7, 3, 1),
            &wire,
            &fb,
        )
        .await;
        assert_eq!(fb.lock().rgba_at(0, 7), Some([255, 255, 255, 255]));
        assert_eq!(fb.lock().rgba_at(1, 7), Some([0, 0, 0, 255]));

        // Gradient filter: data are differences from the prediction.
        let wire = [0x40, 2, 10, 10, 10, 5, 5, 5];
        decode(
            &mut decoders,
            ENCODING_TIGHT,
            Rect::new(0, 0, 2, 1),
            &wire,
            &fb,
        )
        .await;
        assert_eq!(fb.lock().rgba_at(1, 0), Some([15, 15, 15, 255]));
    }

    #[tokio::test]
    async fn zrle_solid_raw_palette_and_rle_tiles() {
        let fb = framebuffer(4, 2);
        let mut decoders = Decoders::new();
        let mut stream = Compress::new(Compression::default(), true);
        let send = |stream: &mut Compress, data: &[u8]| {
            let compressed = deflate(stream, data);
            let mut wire = (compressed.len() as u32).to_be_bytes().to_vec();
            wire.extend(compressed);
            wire
        };
        let wire = send(&mut stream, &[1, 9, 8, 7]);
        decode(
            &mut decoders,
            ENCODING_ZRLE,
            Rect::new(0, 0, 4, 2),
            &wire,
            &fb,
        )
        .await;
        assert_eq!(fb.lock().rgba_at(3, 1), Some([9, 8, 7, 255]));
        // Palette of two, one bit per pixel: 1010 / 0101.
        let wire = send(
            &mut stream,
            &[2, 0, 0, 0, 255, 0, 0, 0b1010_0000, 0b0101_0000],
        );
        decode(
            &mut decoders,
            ENCODING_ZRLE,
            Rect::new(0, 0, 4, 2),
            &wire,
            &fb,
        )
        .await;
        assert_eq!(fb.lock().rgba_at(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(fb.lock().rgba_at(0, 1), Some([0, 0, 0, 255]));
        // Plain RLE: 5 of one colour, 3 of another.
        let wire = send(&mut stream, &[128, 1, 1, 1, 4, 2, 2, 2, 2]);
        decode(
            &mut decoders,
            ENCODING_ZRLE,
            Rect::new(0, 0, 4, 2),
            &wire,
            &fb,
        )
        .await;
        assert_eq!(fb.lock().rgba_at(0, 1), Some([1, 1, 1, 255]));
        assert_eq!(fb.lock().rgba_at(1, 1), Some([2, 2, 2, 255]));
        // Palette RLE: entry 1 run of 7, entry 0 single.
        let wire = send(&mut stream, &[130, 5, 5, 5, 6, 6, 6, 0x81, 6, 0]);
        decode(
            &mut decoders,
            ENCODING_ZRLE,
            Rect::new(0, 0, 4, 2),
            &wire,
            &fb,
        )
        .await;
        assert_eq!(fb.lock().rgba_at(2, 1), Some([6, 6, 6, 255]));
        assert_eq!(fb.lock().rgba_at(3, 1), Some([5, 5, 5, 255]));
    }

    #[tokio::test]
    async fn rejects_unknown_encodings_and_huge_rectangles() {
        let fb = framebuffer(1, 1);
        let mut decoders = Decoders::new();
        let mut empty: &[u8] = &[];
        assert_eq!(
            decoders
                .decode(99, Rect::new(0, 0, 1, 1), 24, &mut empty, &fb)
                .await,
            Err("Unsupported encoding (encoding: 99)".into())
        );
        assert!(
            decoders
                .decode(
                    ENCODING_RAW,
                    Rect::new(0, 0, 65535, 65535),
                    24,
                    &mut empty,
                    &fb
                )
                .await
                .is_err()
        );
    }
}
