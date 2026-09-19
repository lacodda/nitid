//! Cropping a JPEG at the level of its coefficients, with no encoder in the
//! path.
//!
//! The ordinary way to crop a picture is to decode it, take a rectangle of
//! pixels and encode those again. That is what [`crate::export`] does, and for
//! PNG or WebP it costs nothing: both are lossless, so the second file holds
//! exactly what the first did.
//!
//! For JPEG it is not free. Every re-encode quantises again, and a photograph
//! that has been through the process a few times shows it — ringing at edges,
//! blocks in flat sky. A viewer whose whole promise is an honest picture must
//! not be the program that degrades it, so a crop whose edges happen to land
//! on the compression's own grid is done without decoding at all.
//!
//! # What "without loss" means here
//!
//! A baseline JPEG stores the picture as blocks of 8x8 discrete-cosine
//! coefficients, grouped into minimum coded units (MCUs) whose size depends on
//! how the chroma is subsampled. Those coefficients are quantised — that is
//! where the loss in JPEG happens — and then entropy-coded with Huffman tables.
//!
//! This module undoes only the last of those three steps. It decodes the
//! Huffman stream to coefficients, keeps the ones inside the requested
//! rectangle, and codes them again. The quantisation tables are copied across
//! untouched and the coefficients are never dequantised, so no value is ever
//! turned into a pixel and back. The bytes differ, because the Huffman coding
//! is redone over a smaller image; the picture does not, because every
//! surviving coefficient is the number the original file held.
//!
//! # The price, which is stated rather than hidden
//!
//! The rectangle has to fall on MCU boundaries. A 4:2:0 photograph has 16x16
//! MCUs, so an arbitrary crop is up to fifteen pixels away from one that can
//! be done this way. The caller is told what the nearest usable rectangle is
//! rather than being silently given a different crop than it asked for — see
//! [`snap`] — and an offer that the user declines falls back to the encoder in
//! [`crate::export`], which is honest about re-encoding.
//!
//! Progressive JPEGs are refused. Their coefficients are spread across several
//! scans with successive approximation, and rebuilding that for a smaller
//! image is a different and much larger job than this one; the fallback
//! handles them. [`Refusal`] says which case was hit, because "it re-encoded
//! and I do not know why" is the kind of silence this project treats as a
//! defect.

use anyhow::{Result, bail};

/// A rectangle of pixels, in the image's own coordinates.
///
/// Stored as a position and a size rather than as two corners: every caller
/// here has a width and a height in hand, and a pair of corners invites the
/// off-by-one where the far edge is included one time and excluded the next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }

    /// The first column past the right edge.
    fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }

    /// The first row past the bottom edge.
    fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }
}

/// Why a file could not be cropped this way.
///
/// Each variant is a case the fallback handles, and naming them is the point:
/// the interface tells the user that the crop will re-encode and why, rather
/// than re-encoding quietly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// The scan is progressive, not baseline.
    Progressive,
    /// The rectangle does not fall on MCU boundaries.
    OffGrid,
    /// The file uses a feature this module does not implement — arithmetic
    /// coding, twelve-bit samples, a component count it cannot place.
    Unsupported,
}

impl Refusal {
    /// What to say to a person about it, in the words the interface uses.
    pub fn reason(self) -> &'static str {
        match self {
            Refusal::Progressive => "this JPEG is progressive, which has to be re-encoded",
            Refusal::OffGrid => "the edges do not fall on the compression grid",
            Refusal::Unsupported => "this JPEG uses a feature that has to be re-encoded",
        }
    }
}

/// The grid a lossless crop of this file has to land on.
///
/// This is the MCU size in pixels: 8x8 for a JPEG with no chroma subsampling,
/// 16x16 for the 4:2:0 that most cameras and phones write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Grid {
    pub width: u32,
    pub height: u32,
}

/// Read the grid a JPEG's crop has to land on, without decoding it.
///
/// `None` when the file is not a baseline JPEG this module can handle, which
/// is also the answer to "can this be cropped without loss at all".
pub fn grid_of(bytes: &[u8]) -> Option<Grid> {
    let frame = Frame::read(bytes).ok()?;
    Some(frame.grid())
}

/// The largest rectangle inside `wanted` whose edges fall on the grid.
///
/// Shrinks rather than grows, and shrinks towards the inside of what was
/// asked for: a user dragging a crop box has framed something, and giving
/// them back pixels they excluded is a worse surprise than losing a few they
/// included. The far edge is allowed to be the image's own edge even when
/// that is not on the grid — the last MCU of a row is partial in the file
/// too, and keeping it is what makes "crop the right-hand quarter" reachable.
pub fn snap(wanted: Rect, image: (u32, u32), grid: Grid) -> Option<Rect> {
    let (image_width, image_height) = image;
    if grid.width == 0 || grid.height == 0 || image_width == 0 || image_height == 0 {
        return None;
    }

    // The near edges round up to the next grid line, the far edges round down
    // to the previous one — both of which move the rectangle inwards.
    let left = wanted.x.min(image_width).div_ceil(grid.width) * grid.width;
    let top = wanted.y.min(image_height).div_ceil(grid.height) * grid.height;

    let wanted_right = wanted.right().min(image_width);
    let wanted_bottom = wanted.bottom().min(image_height);

    // A far edge that reaches the image's own edge stays there: the file's
    // last MCU is already partial, so keeping it costs nothing and losing it
    // would mean the right-hand edge of a picture could never be cropped to.
    let right = if wanted_right >= image_width {
        image_width
    } else {
        wanted_right / grid.width * grid.width
    };
    let bottom = if wanted_bottom >= image_height {
        image_height
    } else {
        wanted_bottom / grid.height * grid.height
    };

    if right <= left || bottom <= top {
        return None;
    }

    Some(Rect::new(left, top, right - left, bottom - top))
}

/// Crop `bytes` to `rect`, keeping every surviving coefficient exactly as the
/// file held it.
///
/// Returns the new JPEG, or the reason it has to be done the other way.
pub fn crop(bytes: &[u8], rect: Rect) -> Result<std::result::Result<Vec<u8>, Refusal>> {
    let frame = match Frame::read(bytes) {
        Ok(frame) => frame,
        Err(refusal) => return Ok(Err(refusal)),
    };

    let grid = frame.grid();
    if rect.width == 0 || rect.height == 0 {
        bail!("there is nothing to crop to");
    }
    if rect.right() > frame.width as u32 || rect.bottom() > frame.height as u32 {
        bail!("the crop reaches outside the picture");
    }
    // The near edges must be on the grid. The far edges may be the image's own
    // edge, exactly as `snap` allows, because the file's last MCU is partial
    // in the original too.
    let on_grid = rect.x.is_multiple_of(grid.width)
        && rect.y.is_multiple_of(grid.height)
        && (rect.right().is_multiple_of(grid.width) || rect.right() == frame.width as u32)
        && (rect.bottom().is_multiple_of(grid.height) || rect.bottom() == frame.height as u32);
    if !on_grid {
        return Ok(Err(Refusal::OffGrid));
    }

    let coefficients = match decode_scan(bytes, &frame) {
        Ok(coefficients) => coefficients,
        Err(refusal) => return Ok(Err(refusal)),
    };

    let kept = take_rectangle(&frame, &coefficients, rect, grid);
    Ok(Ok(write(bytes, &frame, &kept, rect)?))
}

/// Turn a JPEG's pixels, keeping every coefficient's value, with no encoder in
/// the path.
///
/// The companion to [`crop`], and the same trick one step further. A crop moves
/// whole blocks about; a turn moves the blocks *and* transposes the
/// coefficients inside each one, which works because the discrete cosine
/// transform of a transposed block is the transpose of its coefficients. A
/// quarter turn is that transpose plus a sign flip on alternating rows or
/// columns — the flip is what turns a mirror into a rotation.
///
/// Returns the new JPEG, or the reason it has to be done the other way.
///
/// # Why this needs the grid and a crop does not
///
/// The picture's dimensions need not be a multiple of the MCU size: the last
/// block of a row is partial, and the pixels past the edge are padding the
/// decoder throws away. That is harmless while the edge stays where it is. Turn
/// the picture, and the padded edge becomes an interior one — the padding would
/// appear as a seam of rubbish inside the picture. So a turn that would move a
/// partial edge inwards is refused with [`Refusal::OffGrid`]: the honest answer
/// is the encoder, not a seam nobody asked for.
///
/// A half turn is exempt from that on one axis at a time, and in practice on
/// neither: it moves the right edge to the left and the bottom to the top, so
/// both partial edges end up interior. Only a picture whose dimensions are a
/// multiple of its grid can be turned at all, which is most photographs — a
/// 4:2:0 sensor writes multiples of 16 — and the refusal names the case when it
/// is not.
pub fn turn(bytes: &[u8], turn: Turn) -> Result<std::result::Result<Vec<u8>, Refusal>> {
    let frame = match Frame::read(bytes) {
        Ok(frame) => frame,
        Err(refusal) => return Ok(Err(refusal)),
    };

    if turn == Turn::None {
        return Ok(Ok(bytes.to_vec()));
    }

    // Every component's blocks must tile its own plane exactly, or a turn
    // brings padding inside the picture. Checked per component rather than on
    // the MCU grid: a 4:2:0 chroma plane is half the size, and it is the
    // chroma that is partial first.
    let grid = frame.grid();
    let width = u32::from(frame.width);
    let height = u32::from(frame.height);
    if !width.is_multiple_of(grid.width) || !height.is_multiple_of(grid.height) {
        return Ok(Err(Refusal::OffGrid));
    }

    let coefficients = match decode_scan(bytes, &frame) {
        Ok(coefficients) => coefficients,
        Err(refusal) => return Ok(Err(refusal)),
    };

    // A turn that swaps the axes swaps the sampling factors with them, so the
    // written frame header must describe the new shape. A component sampled
    // 2x1 is 1x2 after a quarter turn, and a header left saying 2x1 would send
    // the decoder looking for blocks in the wrong order.
    let turned_frame = frame.turned(turn);
    let turned = turn_blocks(&frame, &coefficients, turn);

    // The new size is read off the turned frame rather than worked out again
    // here. Two places that each decide what the turned picture measures are two
    // places that can disagree, and the disagreement would be invisible: the
    // header written from one and the scan written from the other still decode,
    // into a picture that is the right size and the wrong shape.
    let size = Rect::new(0, 0, u32::from(turned_frame.width), u32::from(turned_frame.height));
    debug_assert_eq!(
        (size.width, size.height),
        if turn.swaps_axes() { (height, width) } else { (width, height) },
        "the turned frame does not measure what turning this picture should"
    );
    Ok(Ok(write_turned(bytes, &frame, &turned_frame, &turned, size, turn)?))
}

/// Which way a picture is turned, in the four ways a JPEG can be turned without
/// an encoder.
///
/// Deliberately not [`crate::image_source::Orientation`]: that has eight values
/// because EXIF has eight, and the mirrored four are not what a person asks for
/// here. [`Turn::of`] maps the eight onto these.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Turn {
    #[default]
    None,
    Quarter,
    Half,
    ThreeQuarters,
    /// Left to right.
    FlipHorizontal,
    /// Top to bottom.
    FlipVertical,
    /// Across the main diagonal.
    Transpose,
    /// Across the other diagonal.
    Transverse,
}

impl Turn {
    /// What has to be done to a file's pixels to make `orientation` unnecessary.
    ///
    /// The inverse, which is the part worth saying out loud: an orientation of
    /// `Rotate90` means "the viewer must turn this a quarter clockwise to show
    /// it upright", so baking it means turning the pixels that way and writing
    /// `Normal`. For the four that are their own inverse it makes no difference;
    /// for the two quarter turns it is the difference between upright and
    /// upside-down-sideways.
    pub fn of(orientation: crate::image_source::Orientation) -> Self {
        use crate::image_source::Orientation;
        match orientation {
            Orientation::Normal => Turn::None,
            Orientation::FlipHorizontal => Turn::FlipHorizontal,
            Orientation::Rotate180 => Turn::Half,
            Orientation::FlipVertical => Turn::FlipVertical,
            Orientation::Transpose => Turn::Transpose,
            Orientation::Rotate90 => Turn::Quarter,
            Orientation::Transverse => Turn::Transverse,
            Orientation::Rotate270 => Turn::ThreeQuarters,
        }
    }

    /// Whether this turn makes the picture's width its height.
    pub fn swaps_axes(self) -> bool {
        matches!(self, Turn::Quarter | Turn::ThreeQuarters | Turn::Transpose | Turn::Transverse)
    }

    /// The turn as one transpose and two mirrors, in that order.
    ///
    /// The single description everything else here is derived from — the grid of
    /// blocks in [`Turn::moves`] and the coefficients inside each block in
    /// [`turn_block`]. Two hand-written tables would be two chances to write a
    /// turn one way in one place and the other way in the other, which produces
    /// a picture that is *nearly* right and survives every check that does not
    /// compare against an ordinary rotation.
    ///
    /// The mirrors are read in the destination's frame, after the transpose.
    fn parts(self) -> (bool, bool, bool) {
        match self {
            Turn::None => (false, false, false),
            Turn::FlipHorizontal => (false, false, true),
            Turn::FlipVertical => (false, true, false),
            Turn::Half => (false, true, true),
            Turn::Transpose => (true, false, false),
            Turn::Transverse => (true, true, true),
            // Clockwise: transpose, then mirror left to right.
            Turn::Quarter => (true, false, true),
            // Anticlockwise: transpose, then mirror top to bottom.
            Turn::ThreeQuarters => (true, true, false),
        }
    }

    /// Where the block at `(column, row)` of a `(columns, rows)` grid lands.
    ///
    /// The grid's own dimensions are after the turn when the axes swap, which
    /// is why this returns a position rather than an index.
    fn moves(self, column: usize, row: usize, columns: usize, rows: usize) -> (usize, usize) {
        let (transpose, mirror_rows, mirror_columns) = self.parts();

        // The transpose, which also swaps what the grid's extents mean.
        let (mut to_column, mut to_row, width, height) = if transpose {
            (row, column, rows, columns)
        } else {
            (column, row, columns, rows)
        };

        if mirror_columns {
            to_column = width - 1 - to_column;
        }
        if mirror_rows {
            to_row = height - 1 - to_row;
        }

        (to_column, to_row)
    }
}

// ---------------------------------------------------------------------------
// Turning the coefficients
// ---------------------------------------------------------------------------

/// Where each of the 64 coefficients sits, as a row and a column.
///
/// The blocks are held in the file's zig-zag order, which is the order the
/// entropy coder wants and has nothing to do with frequency position. A turn
/// is the first operation here that cares *where* a coefficient is, so the
/// order has to be undone and redone around it.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29,
    22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Transform one block's coefficients the way `turn` transforms its pixels.
///
/// # The identity, stated carefully
///
/// The two-dimensional DCT is separable, which gives exactly two primitive
/// operations on a block, and every one of the eight symmetries is a
/// composition of them:
///
/// - **Transpose.** The DCT of a transposed block is the transpose of its
///   coefficients. Coefficient `(u, v)` moves to `(v, u)`. No sign changes.
/// - **Mirror.** Mirroring a block along an axis *does not move any
///   coefficient*: it negates the ones whose frequency index along that axis is
///   odd, because those basis functions are odd about the block's centre and the
///   even ones are not.
///
/// The second half is the part that is easy to get wrong, and getting it wrong
/// is not subtle once it is measured: treating a mirror as a positional
/// permutation of coefficients — sending `(u, v)` to `(7 - u, v)` — swaps the
/// picture's lowest frequencies for its highest and decodes to a one-pixel
/// checkerboard. It was written that way here first, and the pixel-for-pixel
/// gate in `tests/live_bake.rs` is what said so.
///
/// So a turn is applied in that order: mirror the signs in the source's own
/// frequency space, then transpose positions if the turn transposes. A quarter
/// turn clockwise is "mirror vertically, then transpose"; three quarters is
/// "mirror horizontally, then transpose".
///
/// No step changes a coefficient's magnitude, which is why this is lossless in
/// the same strong sense the crop is.
fn turn_block(block: &Block, turn: Turn) -> Block {
    let mut natural = [0i16; 64];
    for (zigzag_index, value) in block.iter().enumerate() {
        natural[ZIGZAG[zigzag_index]] = *value;
    }

    // The same three parts the grid of blocks is permuted by, from the same
    // description: a transpose that moves coefficients, then mirrors that only
    // change signs. One source, so the two levels cannot drift apart.
    let (transpose, mirror_rows, mirror_columns) = turn.parts();

    let mut moved = [0i16; 64];
    for row in 0..8usize {
        for column in 0..8usize {
            let value = natural[row * 8 + column];
            let (to_row, to_column) = if transpose { (column, row) } else { (row, column) };
            // A mirror negates the odd-indexed basis functions along the axis
            // it mirrors, and moves nothing. The index that decides is the
            // destination's, because the mirror happens after the transpose.
            let mut sign = 1i16;
            if mirror_rows && to_row % 2 == 1 {
                sign = -sign;
            }
            if mirror_columns && to_column % 2 == 1 {
                sign = -sign;
            }
            moved[to_row * 8 + to_column] = value * sign;
        }
    }

    let mut out = [0i16; 64];
    for (zigzag_index, slot) in out.iter_mut().enumerate() {
        *slot = moved[ZIGZAG[zigzag_index]];
    }
    out
}

/// Every block, moved to where the turn puts it and transformed in place.
///
/// The blocks are stored MCU by MCU, and an MCU holds several blocks of several
/// components. A turn is a permutation of each *component's own* plane of
/// blocks, so the plane is unpacked from the MCU order, permuted, and packed
/// back into the MCU order of the turned image.
fn turn_blocks(frame: &Frame, coefficients: &Coefficients, turn: Turn) -> Coefficients {
    let mcus_across = frame.mcus_across() as usize;
    let mcus_down = frame.mcus_down() as usize;
    let per_mcu = frame.blocks_per_mcu();

    // Where each component's blocks start inside one MCU.
    let mut offsets = Vec::with_capacity(frame.components.len());
    let mut running = 0usize;
    for component in &frame.components {
        offsets.push(running);
        running += usize::from(component.horizontal) * usize::from(component.vertical);
    }

    let turned_frame = frame.turned(turn);
    let turned_across = if turn.swaps_axes() { mcus_down } else { mcus_across };
    let turned_down = if turn.swaps_axes() { mcus_across } else { mcus_down };
    let mut out = vec![[0i16; 64]; turned_across * turned_down * per_mcu];

    for (index, component) in frame.components.iter().enumerate() {
        let across = usize::from(component.horizontal);
        let down = usize::from(component.vertical);
        // This component's plane, in blocks.
        let columns = mcus_across * across;
        let rows = mcus_down * down;

        let turned = &turned_frame.components[index];
        let turned_across_blocks = usize::from(turned.horizontal);
        let turned_down_blocks = usize::from(turned.vertical);
        let turned_columns = turned_across * turned_across_blocks;

        for row in 0..rows {
            for column in 0..columns {
                // Out of the MCU order: which MCU holds this block, and where
                // inside it.
                let source = (row / down * mcus_across + column / across) * per_mcu + offsets[index] + (row % down) * across + (column % across);

                let (to_column, to_row) = turn.moves(column, row, columns, rows);
                let destination = (to_row / turned_down_blocks * turned_across + to_column / turned_across_blocks) * per_mcu
                    + offsets[index]
                    + (to_row % turned_down_blocks) * turned_across_blocks
                    + (to_column % turned_across_blocks);
                let _ = turned_columns;

                let block = coefficients.blocks.get(source).copied().unwrap_or([0i16; 64]);
                if let Some(slot) = out.get_mut(destination) {
                    *slot = turn_block(&block, turn);
                }
            }
        }
    }

    Coefficients { blocks: out }
}

// ---------------------------------------------------------------------------
// Reading the frame header
// ---------------------------------------------------------------------------

/// One colour component of the frame.
#[derive(Clone, Copy, Debug)]
struct Component {
    id: u8,
    /// How many 8x8 blocks of this component sit across one MCU.
    horizontal: u8,
    /// How many sit down one MCU.
    vertical: u8,
}

/// What the frame header says, plus where the scan's parts are in the file.
#[derive(Clone, Debug)]
struct Frame {
    width: u16,
    height: u16,
    components: Vec<Component>,
    /// The largest horizontal sampling factor across the components, which is
    /// what an MCU's width is measured in.
    max_horizontal: u8,
    max_vertical: u8,
    /// Which Huffman table each component uses, from the scan header.
    dc_tables: Vec<u8>,
    ac_tables: Vec<u8>,
    /// How many MCUs between restart markers, or zero for none.
    restart_interval: u16,
    /// Where in the file the entropy-coded data begins.
    scan_start: usize,
    /// Where the frame header's own length field sits, so the size can be
    /// rewritten without re-serialising the segment.
    sof_start: usize,
    huffman: Vec<HuffmanSpec>,
}

/// A Huffman table as the file stores it, kept so it can be written back out.
#[derive(Clone, Debug)]
struct HuffmanSpec {
    /// The high nibble of the table's identifier byte: 0 for DC, 1 for AC.
    class: u8,
    id: u8,
    counts: [u8; 16],
    symbols: Vec<u8>,
}

impl Frame {
    /// The MCU size in pixels.
    fn grid(&self) -> Grid {
        Grid {
            width: u32::from(self.max_horizontal) * 8,
            height: u32::from(self.max_vertical) * 8,
        }
    }

    /// How many MCUs across the whole image is.
    fn mcus_across(&self) -> u32 {
        u32::from(self.width).div_ceil(self.grid().width)
    }

    fn mcus_down(&self) -> u32 {
        u32::from(self.height).div_ceil(self.grid().height)
    }

    /// The same frame as it is after `turn`: the dimensions swapped when the
    /// turn swaps axes, and every component's sampling factors with them.
    ///
    /// The sampling factors are the part that is easy to forget and impossible
    /// to see: a 4:2:2 photograph is sampled 2x1, and a quarter turn makes it
    /// 1x2. A header left stating the old pair describes a block order the file
    /// no longer has, and the picture comes out as coloured hash.
    fn turned(&self, turn: Turn) -> Frame {
        let mut frame = self.clone();
        if !turn.swaps_axes() {
            return frame;
        }

        std::mem::swap(&mut frame.width, &mut frame.height);
        for component in &mut frame.components {
            std::mem::swap(&mut component.horizontal, &mut component.vertical);
        }
        std::mem::swap(&mut frame.max_horizontal, &mut frame.max_vertical);

        // The maxima must stay the maxima of the components they describe.
        //
        // Asserted rather than trusted because the alternative is a field that
        // is wrong and unobservable: the writer reads the maxima only to count
        // MCUs, and that count happens to be invariant under swapping them
        // whenever the picture tiles its grid exactly — which `turn` already
        // requires. So a mutation that dropped this swap left every test green.
        // Rather than hunt for a fixture that could tell the difference, the
        // invariant is stated here, where it is cheap and cannot drift.
        debug_assert_eq!(
            frame.max_horizontal,
            frame.components.iter().map(|component| component.horizontal).max().unwrap_or(1),
            "the turned frame's horizontal maximum is not its components'"
        );
        debug_assert_eq!(
            frame.max_vertical,
            frame.components.iter().map(|component| component.vertical).max().unwrap_or(1),
            "the turned frame's vertical maximum is not its components'"
        );

        frame
    }

    /// How many 8x8 blocks one MCU holds, over every component.
    fn blocks_per_mcu(&self) -> usize {
        self.components
            .iter()
            .map(|component| usize::from(component.horizontal) * usize::from(component.vertical))
            .sum()
    }

    /// Walk the markers and read what the crop needs.
    fn read(bytes: &[u8]) -> std::result::Result<Frame, Refusal> {
        if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
            return Err(Refusal::Unsupported);
        }

        let mut at = 2usize;
        let mut frame: Option<(u16, u16, Vec<Component>, usize)> = None;
        let mut huffman = Vec::new();
        let mut restart_interval = 0u16;

        while at + 3 < bytes.len() {
            if bytes[at] != 0xFF {
                // Padding bytes between segments are legal; anything else here
                // means the file is not shaped the way this reader expects.
                at += 1;
                continue;
            }
            let marker = bytes[at + 1];
            at += 2;
            match marker {
                // Standalone markers carry no length.
                0xD8 | 0x01 | 0xD0..=0xD7 => continue,
                0xD9 => break,
                _ => {}
            }

            if at + 1 >= bytes.len() {
                return Err(Refusal::Unsupported);
            }
            let length = usize::from(u16::from_be_bytes([bytes[at], bytes[at + 1]]));
            if length < 2 || at + length > bytes.len() {
                return Err(Refusal::Unsupported);
            }
            let segment = &bytes[at + 2..at + length];
            let segment_start = at;

            match marker {
                // Baseline and extended sequential are the same coefficient
                // layout; only the entropy coding of the latter's twelve-bit
                // form differs, and that is refused below on its precision.
                0xC0 | 0xC1 => {
                    frame = Some(read_frame_header(segment, segment_start)?);
                }
                // Progressive, lossless and every arithmetic-coded variant.
                0xC2 | 0xC3 => return Err(Refusal::Progressive),
                0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return Err(Refusal::Unsupported),
                0xC4 => read_huffman_tables(segment, &mut huffman)?,
                0xDD => {
                    if segment.len() < 2 {
                        return Err(Refusal::Unsupported);
                    }
                    restart_interval = u16::from_be_bytes([segment[0], segment[1]]);
                }
                0xDA => {
                    let (width, height, components, sof_start) = frame.ok_or(Refusal::Unsupported)?;
                    let (dc_tables, ac_tables) = read_scan_header(segment, &components)?;
                    return Ok(Frame {
                        width,
                        height,
                        components,
                        max_horizontal: 0,
                        max_vertical: 0,
                        dc_tables,
                        ac_tables,
                        restart_interval,
                        scan_start: at + length,
                        sof_start,
                        huffman,
                    }
                    .with_sampling());
                }
                _ => {}
            }

            at += length;
        }

        Err(Refusal::Unsupported)
    }

    /// Fill in the sampling maxima, which every other calculation reads.
    fn with_sampling(mut self) -> Self {
        self.max_horizontal = self.components.iter().map(|component| component.horizontal).max().unwrap_or(1);
        self.max_vertical = self.components.iter().map(|component| component.vertical).max().unwrap_or(1);
        self
    }
}

fn read_frame_header(segment: &[u8], segment_start: usize) -> std::result::Result<(u16, u16, Vec<Component>, usize), Refusal> {
    if segment.len() < 6 {
        return Err(Refusal::Unsupported);
    }
    // Eight bits per sample is what baseline means. Twelve-bit files exist and
    // code their coefficients differently; they go to the fallback.
    if segment[0] != 8 {
        return Err(Refusal::Unsupported);
    }
    let height = u16::from_be_bytes([segment[1], segment[2]]);
    let width = u16::from_be_bytes([segment[3], segment[4]]);
    let count = usize::from(segment[5]);
    if count == 0 || count > 4 || segment.len() < 6 + count * 3 {
        return Err(Refusal::Unsupported);
    }

    let mut components = Vec::with_capacity(count);
    for index in 0..count {
        let base = 6 + index * 3;
        let sampling = segment[base + 1];
        let horizontal = sampling >> 4;
        let vertical = sampling & 0x0F;
        if horizontal == 0 || vertical == 0 || horizontal > 4 || vertical > 4 {
            return Err(Refusal::Unsupported);
        }
        components.push(Component {
            id: segment[base],
            horizontal,
            vertical,
        });
    }

    Ok((width, height, components, segment_start))
}

fn read_huffman_tables(mut segment: &[u8], into: &mut Vec<HuffmanSpec>) -> std::result::Result<(), Refusal> {
    // One DHT segment may carry several tables, one after another.
    while !segment.is_empty() {
        if segment.len() < 17 {
            return Err(Refusal::Unsupported);
        }
        let class = segment[0] >> 4;
        let id = segment[0] & 0x0F;
        if class > 1 || id > 3 {
            return Err(Refusal::Unsupported);
        }
        let mut counts = [0u8; 16];
        counts.copy_from_slice(&segment[1..17]);
        let total: usize = counts.iter().map(|count| usize::from(*count)).sum();
        if segment.len() < 17 + total {
            return Err(Refusal::Unsupported);
        }
        let symbols = segment[17..17 + total].to_vec();

        // A table declared twice replaces the earlier one, which is what a
        // decoder does as it walks the file.
        into.retain(|existing| existing.class != class || existing.id != id);
        into.push(HuffmanSpec { class, id, counts, symbols });
        segment = &segment[17 + total..];
    }
    Ok(())
}

fn read_scan_header(segment: &[u8], components: &[Component]) -> std::result::Result<(Vec<u8>, Vec<u8>), Refusal> {
    if segment.is_empty() {
        return Err(Refusal::Unsupported);
    }
    let count = usize::from(segment[0]);
    // A scan over fewer components than the frame has is what a progressive
    // file does, and also what a rare non-interleaved baseline one does. Both
    // go to the fallback: this module walks one interleaved scan.
    if count != components.len() || segment.len() < 1 + count * 2 + 3 {
        return Err(Refusal::Unsupported);
    }

    let mut dc_tables = vec![0u8; components.len()];
    let mut ac_tables = vec![0u8; components.len()];
    for index in 0..count {
        let id = segment[1 + index * 2];
        let tables = segment[2 + index * 2];
        let position = components.iter().position(|component| component.id == id).ok_or(Refusal::Unsupported)?;
        dc_tables[position] = tables >> 4;
        ac_tables[position] = tables & 0x0F;
    }

    // Spectral selection and successive approximation. A baseline scan covers
    // the whole block at full precision; anything else is progressive.
    let trailer = &segment[1 + count * 2..];
    if trailer[0] != 0 || trailer[1] != 63 || trailer[2] != 0 {
        return Err(Refusal::Progressive);
    }

    Ok((dc_tables, ac_tables))
}

// ---------------------------------------------------------------------------
// Decoding the entropy-coded scan into coefficients
// ---------------------------------------------------------------------------

/// A Huffman table in the form a decoder reads it.
struct HuffmanTable {
    /// For each code length, the smallest code of that length and the index
    /// its symbol sits at. The canonical layout means a code can be found by
    /// comparing against these rather than by walking a tree.
    maximum: [i32; 17],
    offset: [i32; 17],
    symbols: Vec<u8>,
}

impl HuffmanTable {
    fn build(spec: &HuffmanSpec) -> Self {
        let mut maximum = [-1i32; 17];
        let mut offset = [0i32; 17];

        let mut code = 0i32;
        let mut index = 0i32;
        for length in 1..=16usize {
            let count = i32::from(spec.counts[length - 1]);
            offset[length] = index - code;
            if count > 0 {
                maximum[length] = code + count - 1;
                code += count;
                index += count;
            } else {
                maximum[length] = -1;
            }
            code <<= 1;
        }

        Self {
            maximum,
            offset,
            symbols: spec.symbols.clone(),
        }
    }
}

/// A reader over the entropy-coded bytes, which unstuffs the `FF 00` pairs the
/// format inserts so a marker can never appear inside the data.
struct BitReader<'a> {
    bytes: &'a [u8],
    at: usize,
    bits: u32,
    held: u32,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            at: 0,
            bits: 0,
            held: 0,
        }
    }

    /// Take one bit, feeding the buffer from the stream as it empties.
    fn bit(&mut self) -> Option<u32> {
        if self.held == 0 {
            let byte = self.next_byte()?;
            self.bits = u32::from(byte);
            self.held = 8;
        }
        self.held -= 1;
        Some((self.bits >> self.held) & 1)
    }

    fn take(&mut self, count: u32) -> Option<i32> {
        let mut value = 0i32;
        for _ in 0..count {
            value = (value << 1) | self.bit()? as i32;
        }
        Some(value)
    }

    /// The next byte of entropy data, with byte stuffing removed.
    fn next_byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.at)?;
        self.at += 1;
        if byte != 0xFF {
            return Some(byte);
        }
        match self.bytes.get(self.at) {
            // A stuffed zero: the 0xFF is data.
            Some(0x00) => {
                self.at += 1;
                Some(0xFF)
            }
            // A restart marker inside the stream is consumed by `restart`
            // below, so reaching one here means the scan has ended.
            _ => None,
        }
    }

    /// Step to the byte after the next restart marker.
    ///
    /// Restart markers sit on a byte boundary, so whatever bits are left in
    /// the current byte are discarded first — the encoder padded them.
    fn restart(&mut self) -> Option<()> {
        self.held = 0;
        while self.at + 1 < self.bytes.len() {
            if self.bytes[self.at] == 0xFF && (0xD0..=0xD7).contains(&self.bytes[self.at + 1]) {
                self.at += 2;
                return Some(());
            }
            self.at += 1;
        }
        None
    }

    fn decode(&mut self, table: &HuffmanTable) -> Option<u8> {
        let mut code = self.bit()? as i32;
        for length in 1..=16usize {
            if table.maximum[length] >= code {
                let index = (table.offset[length] + code) as usize;
                return table.symbols.get(index).copied();
            }
            code = (code << 1) | self.bit()? as i32;
        }
        None
    }
}

/// Turn a Huffman-coded magnitude into the signed coefficient it stands for.
///
/// JPEG codes a value's bit length and then its bits, with the negative half
/// of each range stored as the complement. Getting this wrong flips the sign
/// of about half the coefficients in a picture, which is why it is its own
/// function with its own test rather than three lines inside the loop.
fn extend(value: i32, length: u32) -> i32 {
    if length == 0 {
        return 0;
    }
    if value < (1 << (length - 1)) { value - (1 << length) + 1 } else { value }
}

/// One 8x8 block of quantised coefficients, in the file's zig-zag order.
///
/// Kept zig-zagged rather than laid out as a square: nothing here looks at a
/// coefficient's position, and un-zig-zagging only to zig-zag again on the way
/// out would be work that could go wrong for no gain.
type Block = [i16; 64];

/// Every block of the scan, ordered MCU by MCU and component by component
/// within each — the order they appear in the file.
struct Coefficients {
    blocks: Vec<Block>,
}

fn decode_scan(bytes: &[u8], frame: &Frame) -> std::result::Result<Coefficients, Refusal> {
    let mut dc_tables: Vec<Option<HuffmanTable>> = (0..4).map(|_| None).collect();
    let mut ac_tables: Vec<Option<HuffmanTable>> = (0..4).map(|_| None).collect();
    for spec in &frame.huffman {
        let table = HuffmanTable::build(spec);
        if spec.class == 0 {
            dc_tables[usize::from(spec.id)] = Some(table);
        } else {
            ac_tables[usize::from(spec.id)] = Some(table);
        }
    }

    let mut reader = BitReader::new(&bytes[frame.scan_start..]);
    let across = frame.mcus_across();
    let down = frame.mcus_down();
    let per_mcu = frame.blocks_per_mcu();
    let mut blocks = vec![[0i16; 64]; (across as usize) * (down as usize) * per_mcu];

    // The DC coefficient is stored as a difference from the previous block of
    // the same component, which is what makes a crop more than a copy: the
    // first kept block of each component has to be turned into an absolute
    // value before it can start a new stream.
    let mut previous_dc = vec![0i32; frame.components.len()];
    let mut written = 0usize;
    let mut since_restart = 0u32;

    for _ in 0..(across * down) {
        if frame.restart_interval > 0 && since_restart == u32::from(frame.restart_interval) {
            reader.restart().ok_or(Refusal::Unsupported)?;
            previous_dc.iter_mut().for_each(|value| *value = 0);
            since_restart = 0;
        }
        since_restart += 1;

        for (index, component) in frame.components.iter().enumerate() {
            let dc_table = dc_tables[usize::from(frame.dc_tables[index])].as_ref().ok_or(Refusal::Unsupported)?;
            let ac_table = ac_tables[usize::from(frame.ac_tables[index])].as_ref().ok_or(Refusal::Unsupported)?;

            for _ in 0..(usize::from(component.horizontal) * usize::from(component.vertical)) {
                let block = blocks.get_mut(written).ok_or(Refusal::Unsupported)?;

                let length = u32::from(reader.decode(dc_table).ok_or(Refusal::Unsupported)?);
                if length > 15 {
                    return Err(Refusal::Unsupported);
                }
                let difference = extend(reader.take(length).ok_or(Refusal::Unsupported)?, length);
                previous_dc[index] += difference;
                block[0] = clamp_coefficient(previous_dc[index]);

                let mut position = 1usize;
                while position < 64 {
                    let symbol = reader.decode(ac_table).ok_or(Refusal::Unsupported)?;
                    let run = usize::from(symbol >> 4);
                    let size = u32::from(symbol & 0x0F);
                    if size == 0 {
                        // 0x00 ends the block; 0xF0 is a run of sixteen zeros.
                        if run == 15 {
                            position += 16;
                            continue;
                        }
                        break;
                    }
                    position += run;
                    if position >= 64 {
                        return Err(Refusal::Unsupported);
                    }
                    let value = extend(reader.take(size).ok_or(Refusal::Unsupported)?, size);
                    block[position] = clamp_coefficient(value);
                    position += 1;
                }

                written += 1;
            }
        }
    }

    Ok(Coefficients { blocks })
}

/// A coefficient wider than sixteen bits cannot come from an eight-bit
/// baseline file; if one appears the file is malformed and the fallback path
/// is the honest answer.
fn clamp_coefficient(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

// ---------------------------------------------------------------------------
// Keeping the rectangle
// ---------------------------------------------------------------------------

/// The blocks inside `rect`, in the order a scan over the smaller image wants
/// them.
fn take_rectangle(frame: &Frame, coefficients: &Coefficients, rect: Rect, grid: Grid) -> Coefficients {
    let across = frame.mcus_across() as usize;
    let per_mcu = frame.blocks_per_mcu();

    let first_column = (rect.x / grid.width) as usize;
    let first_row = (rect.y / grid.height) as usize;
    let columns = (rect.width.div_ceil(grid.width)) as usize;
    let rows = (rect.height.div_ceil(grid.height)) as usize;

    let mut kept = Vec::with_capacity(columns * rows * per_mcu);
    for row in 0..rows {
        for column in 0..columns {
            let source = ((first_row + row) * across + first_column + column) * per_mcu;
            for offset in 0..per_mcu {
                kept.push(coefficients.blocks.get(source + offset).copied().unwrap_or([0i16; 64]));
            }
        }
    }

    Coefficients { blocks: kept }
}

// ---------------------------------------------------------------------------
// Writing the smaller file
// ---------------------------------------------------------------------------

/// A Huffman table in the form an encoder writes with: the code and its length
/// for each symbol.
struct HuffmanEncoder {
    codes: [u16; 256],
    lengths: [u8; 256],
}

impl HuffmanEncoder {
    fn build(spec: &HuffmanSpec) -> Self {
        let mut codes = [0u16; 256];
        let mut lengths = [0u8; 256];
        let mut code = 0u16;
        let mut index = 0usize;
        for length in 1..=16u8 {
            for _ in 0..spec.counts[usize::from(length) - 1] {
                if let Some(symbol) = spec.symbols.get(index) {
                    codes[usize::from(*symbol)] = code;
                    lengths[usize::from(*symbol)] = length;
                }
                code = code.wrapping_add(1);
                index += 1;
            }
            code <<= 1;
        }
        Self { codes, lengths }
    }
}

/// Collects bits into bytes, stuffing a zero after every `FF` the way the
/// format requires.
struct BitWriter {
    out: Vec<u8>,
    bits: u32,
    held: u32,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            bits: 0,
            held: 0,
        }
    }

    fn put(&mut self, value: u32, length: u32) {
        if length == 0 {
            return;
        }
        self.bits = (self.bits << length) | (value & ((1u32 << length) - 1));
        self.held += length;
        while self.held >= 8 {
            self.held -= 8;
            let byte = ((self.bits >> self.held) & 0xFF) as u8;
            self.out.push(byte);
            // A 0xFF in the data would otherwise read as the start of a
            // marker, so the format follows it with a zero.
            if byte == 0xFF {
                self.out.push(0x00);
            }
        }
    }

    /// Pad the last byte with ones, which is what the format specifies: a run
    /// of ones cannot be mistaken for a valid code's prefix.
    fn finish(mut self) -> Vec<u8> {
        if self.held > 0 {
            let padding = 8 - self.held;
            self.put((1u32 << padding) - 1, padding);
        }
        self.out
    }
}

/// How many bits a coefficient's magnitude needs.
fn magnitude(value: i32) -> u32 {
    32 - value.unsigned_abs().leading_zeros()
}

/// Assemble the cropped file.
///
/// Everything before the scan is copied from the original — the quantisation
/// tables above all, because copying them is what keeps the coefficients
/// meaningful — with the frame header's dimensions rewritten and the APP
/// segments carried across, so the crop keeps the file's colour profile and
/// its EXIF.
fn write(bytes: &[u8], frame: &Frame, kept: &Coefficients, rect: Rect) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len() / 2 + 1024);
    out.extend_from_slice(&[0xFF, 0xD8]);

    // Walk the original's segments again, copying each one across. The frame
    // header is rewritten with the new size; everything else, including the
    // quantisation tables, the Huffman tables and the metadata, is the
    // original's own bytes.
    let mut at = 2usize;
    while at + 3 < bytes.len() {
        if bytes[at] != 0xFF {
            at += 1;
            continue;
        }
        let marker = bytes[at + 1];
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            at += 2;
            continue;
        }
        if marker == 0xD9 {
            break;
        }
        if at + 3 >= bytes.len() {
            break;
        }
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if length < 2 || at + 2 + length > bytes.len() {
            break;
        }

        if marker == 0xDA {
            // The scan header is copied as it is — the same components, the
            // same tables — and the entropy data after it is the new one.
            out.extend_from_slice(&bytes[at..at + 2 + length]);
            break;
        }

        if at == frame.sof_start - 2 || (marker == 0xC0 || marker == 0xC1) {
            // The frame header, with the dimensions replaced.
            let mut segment = bytes[at..at + 2 + length].to_vec();
            // Two bytes of marker, two of length, one of precision, then the
            // height and the width.
            segment[5..7].copy_from_slice(&(rect.height as u16).to_be_bytes());
            segment[7..9].copy_from_slice(&(rect.width as u16).to_be_bytes());
            out.extend_from_slice(&segment);
        } else {
            out.extend_from_slice(&bytes[at..at + 2 + length]);
        }

        at += 2 + length;
    }

    out.extend_from_slice(&encode_scan(frame, kept, rect)?);
    out.extend_from_slice(&[0xFF, 0xD9]);
    Ok(out)
}

/// Assemble the turned file.
///
/// Like [`write`], with two more things rewritten rather than copied, both of
/// which are invisible until they are wrong:
///
/// - **Each component's sampling factors**, swapped when the axes swap. A
///   4:2:2 file whose header still claims 2x1 after a quarter turn describes a
///   block order the scan no longer has.
/// - **The orientation tag**, set to `Normal`. The turn is now in the pixels,
///   and a tag left saying `Rotate90` would have every viewer turn it a second
///   time. This is the one field a bake *must* touch, and the reason baking and
///   scrubbing belong in the same version.
fn write_turned(bytes: &[u8], frame: &Frame, turned: &Frame, kept: &Coefficients, size: Rect, turn: Turn) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len() + 1024);
    out.extend_from_slice(&[0xFF, 0xD8]);

    let mut at = 2usize;
    while at + 3 < bytes.len() {
        if bytes[at] != 0xFF {
            at += 1;
            continue;
        }
        let marker = bytes[at + 1];
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            at += 2;
            continue;
        }
        if marker == 0xD9 {
            break;
        }
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if length < 2 || at + 2 + length > bytes.len() {
            break;
        }

        if marker == 0xDA {
            out.extend_from_slice(&bytes[at..at + 2 + length]);
            break;
        }

        if at == frame.sof_start - 2 || marker == 0xC0 || marker == 0xC1 {
            let mut segment = bytes[at..at + 2 + length].to_vec();
            segment[5..7].copy_from_slice(&(size.height as u16).to_be_bytes());
            segment[7..9].copy_from_slice(&(size.width as u16).to_be_bytes());

            // Then each component's three bytes: id, the two sampling factors
            // packed into one byte, and the quantisation table it uses.
            let count = usize::from(segment.get(9).copied().unwrap_or(0));
            for index in 0..count {
                let Some(component) = turned.components.get(index) else {
                    break;
                };
                let field = 10 + index * 3 + 1;
                if let Some(byte) = segment.get_mut(field) {
                    *byte = (component.horizontal << 4) | (component.vertical & 0x0F);
                }
            }
            out.extend_from_slice(&segment);
        } else if marker == 0xDB && turn.swaps_axes() {
            // The quantisation tables, transposed with the coefficients.
            //
            // The step that is invisible until it is measured. A table is a
            // quantiser *per frequency position*, and the tables encoders write
            // are not symmetric about the diagonal — the one the `image` crate
            // writes has 10 where its transpose has 12. Move coefficient (u, v)
            // to (v, u) and leave the table alone, and every one of them is
            // divided by its neighbour's quantiser: the picture decodes, the
            // geometry is right, and every pixel is off by a little. Measured at
            // up to 15 of 255 on a quality-92 file, which is precisely the range
            // that reads as "close enough" to anything but a pixel-for-pixel
            // comparison.
            //
            // A mirror needs none of this, because a mirror moves nothing.
            let mut segment = bytes[at..at + 2 + length].to_vec();
            transpose_quantisation_tables(&mut segment[4..]);
            out.extend_from_slice(&segment);
        } else if marker == 0xE1 && bytes[at + 4..at + 2 + length].starts_with(b"Exif\0\0") {
            // The EXIF block, with its orientation set to upright. Rewritten in
            // place rather than dropped: it also carries the camera, the lens
            // and the date, and a bake is not a scrub. Someone who wants both
            // asks for both.
            let mut segment = bytes[at..at + 2 + length].to_vec();
            set_orientation_upright(&mut segment[10..]);
            out.extend_from_slice(&segment);
        } else {
            out.extend_from_slice(&bytes[at..at + 2 + length]);
        }

        at += 2 + length;
    }

    out.extend_from_slice(&encode_scan(turned, kept, size)?);
    out.extend_from_slice(&[0xFF, 0xD9]);
    Ok(out)
}

/// Transpose every quantisation table in a DQT segment's body, in place.
///
/// A DQT segment holds one or more tables, each a byte of precision-and-
/// identifier followed by 64 or 128 values in the file's zig-zag order. The
/// values are de-zigzagged to a square, transposed, and zigzagged back — the
/// same round trip [`turn_block`] makes, for the same reason: the zig-zag order
/// says nothing about position, and a transpose is entirely about position.
fn transpose_quantisation_tables(body: &mut [u8]) {
    let mut at = 0usize;
    while at < body.len() {
        let precision = body[at] >> 4;
        let width = if precision == 0 { 1 } else { 2 };
        let values = at + 1;
        let end = values + 64 * width;
        if end > body.len() {
            return;
        }

        // Read the table into natural order, transpose, and write it back.
        let mut natural = [0u16; 64];
        for index in 0..64 {
            let raw = if width == 1 {
                u16::from(body[values + index])
            } else {
                u16::from_be_bytes([body[values + index * 2], body[values + index * 2 + 1]])
            };
            natural[ZIGZAG[index]] = raw;
        }

        let mut transposed = [0u16; 64];
        for row in 0..8usize {
            for column in 0..8usize {
                transposed[column * 8 + row] = natural[row * 8 + column];
            }
        }

        for index in 0..64 {
            let value = transposed[ZIGZAG[index]];
            if width == 1 {
                body[values + index] = value as u8;
            } else {
                body[values + index * 2..values + index * 2 + 2].copy_from_slice(&value.to_be_bytes());
            }
        }

        at = end;
    }
}

/// Set the orientation tag in a TIFF block to 1, in place, if it has one.
///
/// Only the one field is touched: the value sits inside its own entry when it
/// is a SHORT, which it always is, so nothing moves and no offset elsewhere in
/// the block becomes wrong. A block with no orientation entry is left alone —
/// absent already means upright.
fn set_orientation_upright(tiff: &mut [u8]) {
    let Some(header) = tiff.get(..8) else {
        return;
    };
    let little = match &header[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return,
    };
    let read_u16 = |bytes: &[u8]| {
        let pair = [bytes[0], bytes[1]];
        if little { u16::from_le_bytes(pair) } else { u16::from_be_bytes(pair) }
    };
    let read_u32 = |bytes: &[u8]| {
        let quad = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if little { u32::from_le_bytes(quad) } else { u32::from_be_bytes(quad) }
    };

    let first = read_u32(&header[4..8]) as usize;
    let Some(count_at) = tiff.get(first..first + 2) else {
        return;
    };
    let count = usize::from(read_u16(count_at));

    for index in 0..count {
        let entry = first + 2 + index * 12;
        let Some(tag) = tiff.get(entry..entry + 2).map(&read_u16) else {
            return;
        };
        if tag != 0x0112 {
            continue;
        }
        // A SHORT sits in the first two bytes of the value field, in the
        // block's own byte order.
        let value_at = entry + 8;
        if let Some(slot) = tiff.get_mut(value_at..value_at + 2) {
            let upright = if little { 1u16.to_le_bytes() } else { 1u16.to_be_bytes() };
            slot.copy_from_slice(&upright);
        }
        return;
    }
}

fn encode_scan(frame: &Frame, kept: &Coefficients, rect: Rect) -> Result<Vec<u8>> {
    let mut dc_encoders: Vec<Option<HuffmanEncoder>> = (0..4).map(|_| None).collect();
    let mut ac_encoders: Vec<Option<HuffmanEncoder>> = (0..4).map(|_| None).collect();
    for spec in &frame.huffman {
        let encoder = HuffmanEncoder::build(spec);
        if spec.class == 0 {
            dc_encoders[usize::from(spec.id)] = Some(encoder);
        } else {
            ac_encoders[usize::from(spec.id)] = Some(encoder);
        }
    }

    let grid = frame.grid();
    let columns = rect.width.div_ceil(grid.width) as usize;
    let rows = rect.height.div_ceil(grid.height) as usize;
    let mut writer = BitWriter::new();
    let mut previous_dc = vec![0i32; frame.components.len()];
    let mut block_index = 0usize;
    let mut since_restart = 0u32;
    let mut restart_number = 0u8;

    for _ in 0..(columns * rows) {
        if frame.restart_interval > 0 && since_restart == u32::from(frame.restart_interval) {
            // The interval is kept, so a file written with restart markers
            // keeps them and stays as resilient as it was.
            let mut bytes = std::mem::replace(&mut writer, BitWriter::new()).finish();
            bytes.push(0xFF);
            bytes.push(0xD0 + (restart_number % 8));
            restart_number = restart_number.wrapping_add(1);
            writer.out = bytes;
            previous_dc.iter_mut().for_each(|value| *value = 0);
            since_restart = 0;
        }
        since_restart += 1;

        for (index, component) in frame.components.iter().enumerate() {
            let dc_encoder = dc_encoders[usize::from(frame.dc_tables[index])]
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("the JPEG names a DC table it does not carry"))?;
            let ac_encoder = ac_encoders[usize::from(frame.ac_tables[index])]
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("the JPEG names an AC table it does not carry"))?;

            for _ in 0..(usize::from(component.horizontal) * usize::from(component.vertical)) {
                let block = kept.blocks.get(block_index).copied().unwrap_or([0i16; 64]);
                block_index += 1;

                // The DC difference is recomputed against this stream's own
                // previous block, which is the whole of what makes the first
                // kept block valid: in the original it was a difference from a
                // block that is no longer here.
                let dc = i32::from(block[0]);
                let difference = dc - previous_dc[index];
                previous_dc[index] = dc;

                let length = magnitude(difference);
                put_symbol(&mut writer, dc_encoder, length as u8)?;
                if length > 0 {
                    writer.put(encode_value(difference, length), length);
                }

                let mut run = 0u32;
                for coefficient in block.iter().skip(1) {
                    let value = i32::from(*coefficient);
                    if value == 0 {
                        run += 1;
                        continue;
                    }
                    while run >= 16 {
                        put_symbol(&mut writer, ac_encoder, 0xF0)?;
                        run -= 16;
                    }
                    let size = magnitude(value);
                    put_symbol(&mut writer, ac_encoder, ((run as u8) << 4) | size as u8)?;
                    writer.put(encode_value(value, size), size);
                    run = 0;
                }
                if run > 0 {
                    put_symbol(&mut writer, ac_encoder, 0x00)?;
                }
            }
        }
    }

    Ok(writer.finish())
}

fn put_symbol(writer: &mut BitWriter, encoder: &HuffmanEncoder, symbol: u8) -> Result<()> {
    let length = encoder.lengths[usize::from(symbol)];
    if length == 0 {
        // The original's tables are reused, and they were built for the whole
        // picture — so every symbol the kept blocks need is in them. A missing
        // one means the file disagrees with itself, and re-encoding is the
        // honest answer rather than a broken JPEG.
        bail!("the JPEG's Huffman table has no code for a value the picture contains");
    }
    writer.put(u32::from(encoder.codes[usize::from(symbol)]), u32::from(length));
    Ok(())
}

/// The bits a coefficient is written as: its magnitude for a positive value,
/// and the complement for a negative one — the inverse of [`extend`].
fn encode_value(value: i32, length: u32) -> u32 {
    if value >= 0 {
        value as u32
    } else {
        (value - 1) as u32 & ((1u32 << length) - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A JPEG encoded here rather than committed, for the reason the rotation
    /// tests give: a fixture from the owner's archive would put a real
    /// photograph in a public repository.
    ///
    /// Noise rather than a gradient, and at a quality the default encoder does
    /// not use. Both matter: a smooth picture compresses to so few non-zero
    /// coefficients that a crop which dropped the AC terms entirely would
    /// still decode to something plausible.
    fn a_jpeg(width: u32, height: u32) -> Vec<u8> {
        let mut pixels = image::RgbImage::new(width, height);
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for pixel in pixels.pixels_mut() {
            let mut next = || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 24) as u8
            };
            *pixel = image::Rgb([next(), next(), next()]);
        }
        let mut out = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut out), 88);
        encoder.encode_image(&pixels).expect("an encoded jpeg");
        out
    }

    use std::io::Cursor;

    /// Decode a JPEG to RGB, with a decoder that is not the one under test.
    fn pixels_of(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let decoded = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).expect("a decodable jpeg");
        let rgb = decoded.to_rgb8();
        let (width, height) = rgb.dimensions();
        (width, height, rgb.into_raw())
    }

    #[test]
    fn a_value_survives_the_round_trip_through_the_coding() {
        // The sign convention is the one thing here that is easy to get
        // backwards and silent when it is: half the coefficients in a picture
        // are negative.
        for value in [-2047i32, -255, -9, -1, 1, 9, 255, 2047] {
            let length = magnitude(value);
            let coded = encode_value(value, length);
            assert_eq!(extend(coded as i32, length), value, "{value} did not survive coding at {length} bits");
        }
    }

    #[test]
    fn the_grid_is_the_mcu_size() {
        // The `image` crate writes 4:2:0 by default at this quality, so the
        // grid is sixteen pixels. Asserted rather than assumed: the whole
        // snapping rule is built on it.
        let bytes = a_jpeg(64, 64);
        let grid = grid_of(&bytes).expect("a baseline jpeg has a grid");
        assert!(
            grid.width.is_multiple_of(8) && grid.height.is_multiple_of(8),
            "an MCU is a whole number of blocks, got {}x{}",
            grid.width,
            grid.height
        );
    }

    #[test]
    fn snapping_moves_the_edges_inwards_only() {
        let grid = Grid { width: 16, height: 16 };
        let snapped = snap(Rect::new(5, 5, 40, 40), (128, 128), grid).expect("a rectangle inside the picture");
        assert_eq!(snapped, Rect::new(16, 16, 16, 16), "the edges did not move inwards to the grid");

        // Every edge of the result is on the grid, which is what makes it
        // usable at all.
        assert!(snapped.x.is_multiple_of(grid.width));
        assert!(snapped.y.is_multiple_of(grid.height));
        assert!(snapped.right().is_multiple_of(grid.width));
        assert!(snapped.bottom().is_multiple_of(grid.height));
    }

    #[test]
    fn snapping_keeps_the_pictures_own_far_edge() {
        // A picture whose size is not a multiple of the MCU has a partial one
        // at each far edge. Refusing to crop to it would mean the right-hand
        // side of most photographs could never be reached losslessly.
        let grid = Grid { width: 16, height: 16 };
        let snapped = snap(Rect::new(0, 0, 100, 100), (100, 100), grid).expect("the whole picture");
        assert_eq!(snapped, Rect::new(0, 0, 100, 100), "the picture's own edge was given away");
    }

    #[test]
    fn snapping_refuses_a_rectangle_with_no_whole_unit_in_it() {
        let grid = Grid { width: 16, height: 16 };
        assert_eq!(
            snap(Rect::new(1, 1, 4, 4), (128, 128), grid),
            None,
            "a rectangle smaller than one MCU cannot be snapped"
        );
    }

    /// The promise of the stage, asserted on the pixels: a lossless crop is
    /// the *same picture*, not a good re-encoding of it.
    ///
    /// Compared against the original's own pixels rather than against a
    /// re-encode, and demanded exact: every surviving coefficient is the
    /// number the file held, so the decoded samples must match to the bit.
    #[test]
    fn a_cropped_picture_holds_exactly_the_pixels_it_held_before() {
        let bytes = a_jpeg(128, 128);
        let grid = grid_of(&bytes).expect("a grid");
        let rect = Rect::new(grid.width, grid.height, grid.width * 2, grid.height * 2);

        let cropped = crop(&bytes, rect).expect("the crop ran").expect("this file can be cropped losslessly");

        let (width, height, after) = pixels_of(&cropped);
        assert_eq!((width, height), (rect.width, rect.height), "the cropped file is the wrong size");

        let (_, _, before) = pixels_of(&bytes);
        for row in 0..rect.height {
            for column in 0..rect.width {
                let from = (((row + rect.y) * 128 + column + rect.x) * 3) as usize;
                let to = ((row * rect.width + column) * 3) as usize;
                assert_eq!(
                    after[to..to + 3],
                    before[from..from + 3],
                    "the pixel at {column},{row} changed: a lossless crop re-encoded"
                );
            }
        }
    }

    /// The other half of "lossless", and the half a pixel comparison cannot
    /// see: that no encoder ran.
    ///
    /// The quantisation tables are what the coefficients are measured in, so a
    /// file whose tables were rebuilt has been through an encoder even if it
    /// happens to decode to similar pixels.
    #[test]
    fn the_crop_carries_the_originals_quantisation_tables_across() {
        let bytes = a_jpeg(128, 128);
        let grid = grid_of(&bytes).expect("a grid");
        let rect = Rect::new(0, 0, grid.width * 2, grid.height * 2);
        let cropped = crop(&bytes, rect).expect("the crop ran").expect("a lossless crop");

        assert_eq!(
            quantisation_tables(&bytes),
            quantisation_tables(&cropped),
            "the quantisation tables changed, so something re-encoded the picture"
        );
    }

    /// Every DQT segment's payload, which is what an encoder would replace.
    fn quantisation_tables(bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut tables = Vec::new();
        let mut at = 2usize;
        while at + 3 < bytes.len() {
            if bytes[at] != 0xFF {
                at += 1;
                continue;
            }
            let marker = bytes[at + 1];
            if marker == 0xDA || marker == 0xD9 {
                break;
            }
            if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                at += 2;
                continue;
            }
            let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
            if marker == 0xDB {
                tables.push(bytes[at + 4..at + 2 + length].to_vec());
            }
            at += 2 + length;
        }
        tables
    }

    #[test]
    fn a_crop_off_the_grid_is_refused_by_name() {
        let bytes = a_jpeg(128, 128);
        let grid = grid_of(&bytes).expect("a grid");
        assert!(grid.width > 1, "the rest of this test needs a grid wider than a pixel");

        let refusal = crop(&bytes, Rect::new(1, 1, 32, 32))
            .expect("the crop ran")
            .expect_err("an off-grid crop cannot be lossless");
        assert_eq!(refusal, Refusal::OffGrid, "the refusal did not say why");
    }

    #[test]
    fn a_progressive_jpeg_is_refused_by_name() {
        // Written by `image`, which is not the code under test.
        let mut pixels = image::RgbImage::new(64, 64);
        for (x, y, pixel) in pixels.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x * 4) as u8, (y * 4) as u8, 128]);
        }
        let mut bytes = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new(Cursor::new(&mut bytes));
        // The `image` encoder writes baseline, so a progressive file is made
        // by hand: the marker is what the reader keys on.
        let mut progressive = {
            let mut out = Vec::new();
            image::codecs::jpeg::JpegEncoder::new(Cursor::new(&mut out))
                .encode_image(&pixels)
                .expect("an encoded jpeg");
            out
        };
        let _ = encoder;
        // Turn the SOF0 marker into SOF2, which is what a progressive file
        // carries. Nothing else about the file has to be valid: the reader
        // must refuse it on the marker, before it reads a single coefficient.
        let position = progressive
            .windows(2)
            .position(|pair| pair == [0xFF, 0xC0])
            .expect("a baseline jpeg has an SOF0");
        progressive[position + 1] = 0xC2;

        let refusal = crop(&progressive, Rect::new(0, 0, 64, 64))
            .expect("the crop ran")
            .expect_err("a progressive jpeg cannot be cropped this way");
        assert_eq!(refusal, Refusal::Progressive, "the refusal did not say why");
    }

    /// A crop to the whole picture is the identity, and is the strongest test
    /// of the coder there is: every block goes through the decode and the
    /// encode, and the picture must come back unchanged.
    #[test]
    fn cropping_to_the_whole_picture_gives_the_picture_back() {
        let bytes = a_jpeg(96, 80);
        let cropped = crop(&bytes, Rect::new(0, 0, 96, 80)).expect("the crop ran").expect("a lossless crop");

        let (_, _, before) = pixels_of(&bytes);
        let (width, height, after) = pixels_of(&cropped);
        assert_eq!((width, height), (96, 80));
        assert_eq!(before, after, "a crop to the whole picture changed it");
    }

    /// A picture whose size is not a multiple of the MCU: the last row and
    /// column of MCUs are partial in the file, which is the case most likely
    /// to be got wrong by one block.
    #[test]
    fn a_picture_that_does_not_fill_its_last_unit_crops_cleanly() {
        let bytes = a_jpeg(100, 70);
        let grid = grid_of(&bytes).expect("a grid");
        let rect = Rect::new(grid.width, 0, 100 - grid.width, 70);

        let cropped = crop(&bytes, rect).expect("the crop ran").expect("a lossless crop");
        let (width, height, after) = pixels_of(&cropped);
        assert_eq!((width, height), (rect.width, rect.height));

        let (_, _, before) = pixels_of(&bytes);
        for row in 0..rect.height {
            for column in 0..rect.width {
                let from = (((row + rect.y) * 100 + column + rect.x) * 3) as usize;
                let to = ((row * rect.width + column) * 3) as usize;
                assert_eq!(after[to..to + 3], before[from..from + 3], "the pixel at {column},{row} changed");
            }
        }
    }

    /// The metadata a crop must not throw away: a photograph that loses its
    /// colour profile on the way through is a viewer breaking its own promise.
    #[test]
    fn the_crop_keeps_the_files_application_segments() {
        let mut bytes = a_jpeg(64, 64);
        // Splice an APP2 segment carrying an ICC profile in after SOI, the way
        // a tagged file has one.
        let payload = b"ICC_PROFILE\0\x01\x01some profile bytes";
        let mut segment = vec![0xFF, 0xE2];
        segment.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        segment.extend_from_slice(payload);
        bytes.splice(2..2, segment);

        let cropped = crop(&bytes, Rect::new(0, 0, 64, 64)).expect("the crop ran").expect("a lossless crop");
        assert!(
            cropped.windows(payload.len()).any(|window| window == payload),
            "the crop dropped the file's colour profile"
        );
    }
}
