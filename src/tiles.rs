//! Cutting an image into pieces a GPU will accept.
//!
//! A texture has a side limit — 16384 on the hardware this was measured on,
//! and as little as 2048 on the downlevel floor nitid targets. A panorama or a
//! scanned map goes past it, and the failure is the worst kind: `create_texture`
//! has no `Result` to reject it with, so wgpu reports the oversize extent
//! through the device's error handler, whose default is a panic. Measured: a
//! 20000-pixel-wide file decoded in full and then took the viewer down as it
//! was about to appear. See `docs/adr/0015-large-images-are-tiled.md`.
//!
//! The cut itself is arithmetic, so it lives here rather than in `gpu.rs` and
//! is tested without a graphics device.

/// A single piece of the image, in source pixels.
///
/// `x`/`y`/`width`/`height` are the region the tile is responsible for
/// drawing. `padded_*` is the region it actually holds, which is one pixel
/// wider on each edge that touches a neighbour — see [`Grid::OVERLAP`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Tile {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub padded_x: u32,
    pub padded_y: u32,
    pub padded_width: u32,
    pub padded_height: u32,
}

impl Tile {
    /// Where this tile's drawn region sits in the whole image, as fractions
    /// of the image's width and height.
    ///
    /// This is what places the tile's quad: the renderer draws the whole
    /// image's rectangle scaled down to this sub-rectangle of it.
    pub fn span(&self, image: (u32, u32)) -> Span {
        let width = image.0.max(1) as f32;
        let height = image.1.max(1) as f32;
        Span {
            left: self.x as f32 / width,
            top: self.y as f32 / height,
            right: (self.x + self.width) as f32 / width,
            bottom: (self.y + self.height) as f32 / height,
        }
    }

    /// Where the drawn region sits inside this tile's own texture, as texture
    /// coordinates.
    ///
    /// The padding is sampled but never drawn: it exists so the filter has a
    /// real neighbouring texel to interpolate towards at the seam, instead of
    /// the repeat that `ClampToEdge` would give it.
    pub fn inner_uv(&self) -> Span {
        let width = self.padded_width.max(1) as f32;
        let height = self.padded_height.max(1) as f32;
        Span {
            left: (self.x - self.padded_x) as f32 / width,
            top: (self.y - self.padded_y) as f32 / height,
            right: (self.x - self.padded_x + self.width) as f32 / width,
            bottom: (self.y - self.padded_y + self.height) as f32 / height,
        }
    }

    /// Bytes this tile's padded region occupies at `bytes_per_pixel`.
    pub fn byte_len(&self, bytes_per_pixel: usize) -> usize {
        self.padded_width as usize * self.padded_height as usize * bytes_per_pixel
    }
}

/// A rectangle in fractions of a whole, as `0.0..=1.0` on each axis.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Span {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Span {
    /// Width and height of the span.
    pub fn size(&self) -> (f32, f32) {
        (self.right - self.left, self.bottom - self.top)
    }

    /// Centre of the span, in the same fractions.
    pub fn centre(&self) -> (f32, f32) {
        ((self.left + self.right) / 2.0, (self.top + self.bottom) / 2.0)
    }
}

/// How an image is cut up for the device that has to hold it.
#[derive(Clone, Debug)]
pub struct Grid {
    tiles: Vec<Tile>,
    #[cfg_attr(not(test), allow(dead_code))]
    columns: u32,
    #[cfg_attr(not(test), allow(dead_code))]
    rows: u32,
}

impl Grid {
    /// Source pixels each tile keeps beyond its own region, on every edge
    /// that touches a neighbour.
    ///
    /// One is enough, and one is what a bilinear filter needs: it mixes a
    /// texel with the one next to it, so only the immediately neighbouring
    /// column or row is ever read across a seam. Measured — without it a
    /// magnified gradient shows a flat step at the join, eight values out of
    /// 255 away from the same gradient drawn whole.
    pub const OVERLAP: u32 = 1;

    /// Cut `image` into tiles no larger than `limit` on either side.
    ///
    /// `limit` is the device's `max_texture_dimension_2d`. The padding is
    /// included in that budget, so a tile never exceeds the limit even at the
    /// seams — the region is cut to `limit - 2 * OVERLAP` where an overlap is
    /// possible at all.
    pub fn new(image: (u32, u32), limit: u32) -> Self {
        let width = image.0.max(1);
        let height = image.1.max(1);
        let limit = limit.max(1);
        // The padding has to fit inside the limit along with the region, and
        // an interior tile is padded on both sides. A limit too small to hold
        // a region either side of that padding cannot afford the padding at
        // all: it degrades to unpadded tiles — a seam on a device this small
        // beats an oversize texture, which is the failure being fixed here.
        let overlap = if limit > 2 * Self::OVERLAP { Self::OVERLAP } else { 0 };
        let step = limit.saturating_sub(2 * overlap).max(1);

        let columns = width.div_ceil(step);
        let rows = height.div_ceil(step);
        let mut tiles = Vec::with_capacity((columns as usize) * (rows as usize));

        for row in 0..rows {
            let y = row * step;
            let tile_height = step.min(height - y);
            for column in 0..columns {
                let x = column * step;
                let tile_width = step.min(width - x);

                // Pad only towards a neighbour that exists; the image's outer
                // edges have nothing to bleed from and are left alone.
                let padded_x = if column > 0 { x - overlap } else { x };
                let padded_y = if row > 0 { y - overlap } else { y };
                let right = if column + 1 < columns {
                    (x + tile_width + overlap).min(width)
                } else {
                    x + tile_width
                };
                let bottom = if row + 1 < rows {
                    (y + tile_height + overlap).min(height)
                } else {
                    y + tile_height
                };

                tiles.push(Tile {
                    x,
                    y,
                    width: tile_width,
                    height: tile_height,
                    padded_x,
                    padded_y,
                    padded_width: right - padded_x,
                    padded_height: bottom - padded_y,
                });
            }
        }

        Self { tiles, columns, rows }
    }

    pub fn tiles(&self) -> &[Tile] {
        &self.tiles
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    /// Whether the image needed cutting at all.
    ///
    /// The single-tile case is the overwhelmingly common one, and it costs
    /// exactly what the untiled renderer cost: one texture, one draw call.
    pub fn is_single(&self) -> bool {
        self.tiles.len() == 1
    }

    /// Tiles across and down. The renderer does not need the shape, only
    /// the list, but a wrong shape is the fastest way to a wrong picture and
    /// the tests check it.
    #[cfg(test)]
    pub fn shape(&self) -> (u32, u32) {
        (self.columns, self.rows)
    }
}

/// Copy one tile's padded region out of a tightly packed RGBA image.
///
/// `pixels` is `width * height * bytes_per_pixel`, row-major and unpadded, as
/// every decoder in the viewer produces. The result is the same layout for
/// the tile alone, which is what `write_texture` wants.
///
/// Returns `None` when `pixels` is shorter than the image claims — a decoder
/// contract violation rather than something a file can cause, but the copy
/// reads by index and must not be handed a bad length.
pub fn extract(pixels: &[u8], image: (u32, u32), tile: &Tile, bytes_per_pixel: usize) -> Option<Vec<u8>> {
    let stride = image.0 as usize * bytes_per_pixel;
    if pixels.len() < stride * image.1 as usize {
        return None;
    }

    let row_bytes = tile.padded_width as usize * bytes_per_pixel;
    let mut out = Vec::with_capacity(tile.byte_len(bytes_per_pixel));
    for row in 0..tile.padded_height as usize {
        let start = (tile.padded_y as usize + row) * stride + tile.padded_x as usize * bytes_per_pixel;
        out.extend_from_slice(&pixels[start..start + row_bytes]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn about(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn an_image_within_the_limit_is_one_tile() {
        let grid = Grid::new((4000, 3000), 16384);
        assert!(grid.is_single());
        let tile = grid.tiles()[0];
        assert_eq!((tile.width, tile.height), (4000, 3000));
        // Nothing to bleed from, so nothing is padded.
        assert_eq!((tile.padded_width, tile.padded_height), (4000, 3000));
        assert_eq!((tile.padded_x, tile.padded_y), (0, 0));
    }

    /// The case the version exists for: a panorama past the side limit.
    #[test]
    fn an_image_past_the_limit_is_cut_into_columns() {
        let grid = Grid::new((30000, 4000), 16384);
        assert_eq!(grid.shape(), (2, 1));
        assert_eq!(grid.len(), 2);
    }

    #[test]
    fn every_tile_fits_the_limit_including_its_padding() {
        // A limit small enough to need many tiles in both directions.
        for limit in [2048, 4096, 16384] {
            for image in [(30000, 20000), (17000, 17000), (16385, 100), (100, 16385)] {
                let grid = Grid::new(image, limit);
                for tile in grid.tiles() {
                    assert!(
                        tile.padded_width <= limit && tile.padded_height <= limit,
                        "{image:?} at limit {limit}: tile {tile:?} exceeds it",
                    );
                }
            }
        }
    }

    /// Every pixel of the image is drawn by exactly one tile: the regions
    /// tile the image without gaps or double coverage. The padding overlaps
    /// on purpose and is excluded from this — it is sampled, never drawn.
    #[test]
    fn the_regions_cover_the_image_exactly_once() {
        let image = (5000u32, 3000u32);
        let grid = Grid::new(image, 2048);
        let mut covered = vec![0u8; (image.0 as usize) * (image.1 as usize)];
        for tile in grid.tiles() {
            for y in tile.y..tile.y + tile.height {
                for x in tile.x..tile.x + tile.width {
                    covered[y as usize * image.0 as usize + x as usize] += 1;
                }
            }
        }
        assert!(covered.iter().all(|&n| n == 1), "the regions do not tile the image exactly once");
    }

    #[test]
    fn interior_tiles_are_padded_towards_their_neighbours_and_edges_are_not() {
        let image = (5000u32, 3000u32);
        let grid = Grid::new(image, 2048);
        for tile in grid.tiles() {
            // Left edge of the image: no padding on the left.
            if tile.x == 0 {
                assert_eq!(tile.padded_x, 0);
            } else {
                assert_eq!(tile.padded_x, tile.x - Grid::OVERLAP);
            }
            // Right edge of the image: no padding on the right.
            let right = tile.padded_x + tile.padded_width;
            if tile.x + tile.width == image.0 {
                assert_eq!(right, image.0);
            } else {
                assert_eq!(right, tile.x + tile.width + Grid::OVERLAP);
            }
        }
    }

    #[test]
    fn a_single_tile_spans_the_whole_image_and_all_of_its_texture() {
        let grid = Grid::new((4000, 3000), 16384);
        let tile = grid.tiles()[0];
        let span = tile.span((4000, 3000));
        assert!(about(span.left, 0.0) && about(span.top, 0.0));
        assert!(about(span.right, 1.0) && about(span.bottom, 1.0));

        let uv = tile.inner_uv();
        assert!(about(uv.left, 0.0) && about(uv.top, 0.0));
        assert!(about(uv.right, 1.0) && about(uv.bottom, 1.0));
    }

    /// The two spans have to agree: the fraction of the image a tile draws
    /// must be the fraction of its texture that gets sampled, or the picture
    /// is stretched piecewise.
    #[test]
    fn a_tiles_screen_span_and_its_texture_span_describe_the_same_pixels() {
        let image = (5000u32, 3000u32);
        let grid = Grid::new(image, 2048);
        for tile in grid.tiles() {
            let span = tile.span(image);
            let uv = tile.inner_uv();

            // Pixels per unit of span, both ways round, must match the image.
            let (span_w, span_h) = span.size();
            let (uv_w, uv_h) = uv.size();
            let from_span = span_w * image.0 as f32;
            let from_uv = uv_w * tile.padded_width as f32;
            assert!((from_span - from_uv).abs() < 0.01, "{tile:?}: {from_span} vs {from_uv} columns");

            let from_span = span_h * image.1 as f32;
            let from_uv = uv_h * tile.padded_height as f32;
            assert!((from_span - from_uv).abs() < 0.01, "{tile:?}: {from_span} vs {from_uv} rows");
        }
    }

    #[test]
    fn extract_takes_the_padded_region_row_by_row() {
        // A 4x3 image whose every pixel is its own index, one byte per pixel.
        let image = (4u32, 3u32);
        let pixels: Vec<u8> = (0..12).collect();
        let tile = Tile {
            x: 2,
            y: 1,
            width: 2,
            height: 2,
            padded_x: 1,
            padded_y: 0,
            padded_width: 3,
            padded_height: 3,
        };
        let out = extract(&pixels, image, &tile, 1).unwrap();
        assert_eq!(out, vec![1, 2, 3, 5, 6, 7, 9, 10, 11]);
    }

    #[test]
    fn extract_refuses_pixels_shorter_than_the_image() {
        let grid = Grid::new((4, 3), 16384);
        assert!(extract(&[0u8; 8], (4, 3), &grid.tiles()[0], 1).is_none());
    }

    /// A tiled extract must be able to rebuild the image: taking each tile's
    /// drawn region back out of its padded copy has to give the original.
    #[test]
    fn the_tiles_reassemble_into_the_original_image() {
        let image = (37u32, 23u32);
        let pixels: Vec<u8> = (0..(37 * 23)).map(|i| (i % 251) as u8).collect();
        // A limit that forces several tiles on both axes.
        let grid = Grid::new(image, 10);
        let mut rebuilt = vec![0u8; pixels.len()];

        for tile in grid.tiles() {
            let data = extract(&pixels, image, tile, 1).unwrap();
            let inset_x = (tile.x - tile.padded_x) as usize;
            let inset_y = (tile.y - tile.padded_y) as usize;
            for row in 0..tile.height as usize {
                for column in 0..tile.width as usize {
                    let from = (inset_y + row) * tile.padded_width as usize + inset_x + column;
                    let to = (tile.y as usize + row) * image.0 as usize + tile.x as usize + column;
                    rebuilt[to] = data[from];
                }
            }
        }
        assert_eq!(rebuilt, pixels);
    }

    /// A degenerate limit must still produce a usable grid rather than an
    /// empty one or a division by zero.
    #[test]
    fn a_limit_too_small_for_padding_still_cuts_the_image() {
        let grid = Grid::new((10, 10), 1);
        assert_eq!(grid.len(), 100);
        for tile in grid.tiles() {
            assert_eq!((tile.width, tile.height), (1, 1));
            assert!(tile.padded_width <= 1 && tile.padded_height <= 1);
        }
    }

    #[test]
    fn a_zero_sized_image_is_still_one_tile() {
        let grid = Grid::new((0, 0), 16384);
        assert!(grid.is_single());
        assert_eq!((grid.tiles()[0].width, grid.tiles()[0].height), (1, 1));
    }
}
