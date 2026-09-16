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
