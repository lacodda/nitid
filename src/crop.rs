//! The crop box: what is being framed, in the picture's own coordinates.
//!
//! Like [`crate::view`], this is plain geometry with no GPU or windowing type
//! in it, so the rules a person feels — a corner drag moves one corner, a
//! locked ratio gives way on the axis that has room, a box cannot be dragged
//! inside out — are tested without a device or a surface.
//!
//! The box lives in **image pixels**, not screen ones. That is what makes it
//! survive a zoom or a pan mid-crop: the framing is a statement about the
//! picture, and a box stored in screen coordinates would slide off the subject
//! the moment the view moved under it.

/// The proportions a crop can be held to.
///
/// A short list on purpose. These are the shapes a picture is actually cropped
/// to — a print, a phone screen, a square for a profile — and a dozen more
/// would make the common ones harder to reach rather than the rare ones
/// easier. Anything else is what `Free` is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Ratio {
    #[default]
    Free,
    /// The picture's own proportions, for trimming without reshaping.
    Original,
    Square,
    ThreeTwo,
    FourThree,
    SixteenNine,
    /// The same shapes turned on their side.
    TwoThree,
    ThreeFour,
    NineSixteen,
}

impl Ratio {
    /// Every ratio, in the order the interface offers them.
    pub const ALL: &'static [Ratio] = &[
        Ratio::Free,
        Ratio::Original,
        Ratio::Square,
        Ratio::ThreeTwo,
        Ratio::TwoThree,
        Ratio::FourThree,
        Ratio::ThreeFour,
        Ratio::SixteenNine,
        Ratio::NineSixteen,
    ];

    /// What this is called in the interface.
    pub fn label(self) -> &'static str {
        match self {
            Ratio::Free => "free",
            Ratio::Original => "original",
            Ratio::Square => "1:1",
            Ratio::ThreeTwo => "3:2",
            Ratio::TwoThree => "2:3",
            Ratio::FourThree => "4:3",
            Ratio::ThreeFour => "3:4",
            Ratio::SixteenNine => "16:9",
            Ratio::NineSixteen => "9:16",
        }
    }

    /// Width divided by height, or `None` when the shape is not fixed.
    ///
    /// `Original` needs the picture to answer, which is why this takes it.
    pub fn of(self, image: (u32, u32)) -> Option<f32> {
        let value = match self {
            Ratio::Free => return None,
            Ratio::Original => {
                if image.1 == 0 {
                    return None;
                }
                image.0 as f32 / image.1 as f32
            }
            Ratio::Square => 1.0,
            Ratio::ThreeTwo => 3.0 / 2.0,
            Ratio::TwoThree => 2.0 / 3.0,
            Ratio::FourThree => 4.0 / 3.0,
            Ratio::ThreeFour => 3.0 / 4.0,
            Ratio::SixteenNine => 16.0 / 9.0,
            Ratio::NineSixteen => 9.0 / 16.0,
        };
        (value.is_finite() && value > 0.0).then_some(value)
    }
}

/// Which part of the box a drag has hold of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handle {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Top,
    Bottom,
    Left,
    Right,
    /// The middle: the whole box moves, keeping its size.
    Inside,
}

impl Handle {
    /// The corner diagonally opposite this one, which a corner drag pivots on.
    fn opposite_corner(self) -> Option<(bool, bool)> {
        // `(right, bottom)` — which edges the anchor sits on.
        match self {
            Handle::TopLeft => Some((true, true)),
            Handle::TopRight => Some((false, true)),
            Handle::BottomLeft => Some((true, false)),
            Handle::BottomRight => Some((false, false)),
            _ => None,
        }
    }
}

/// A rectangle in image coordinates, held as edges.
///
/// Floating point rather than whole pixels because it is dragged: rounding at
/// every mouse move makes a slow drag stutter, and a box held to a ratio would
/// wander off it as the rounding accumulated. It becomes whole pixels once,
/// when the crop is taken.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Box2 {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Box2 {
    pub fn width(self) -> f32 {
        self.right - self.left
    }

    pub fn height(self) -> f32 {
        self.bottom - self.top
    }

    /// The box as whole pixels, for the crop itself.
    ///
    /// Rounded rather than truncated, and never to nothing: a box dragged
    /// thinner than a pixel is still a request to keep something.
    pub fn to_pixels(self, image: (u32, u32)) -> (u32, u32, u32, u32) {
        let left = self.left.round().clamp(0.0, image.0 as f32) as u32;
        let top = self.top.round().clamp(0.0, image.1 as f32) as u32;
        let right = self.right.round().clamp(0.0, image.0 as f32) as u32;
        let bottom = self.bottom.round().clamp(0.0, image.1 as f32) as u32;
        let width = right.saturating_sub(left).max(1).min(image.0.saturating_sub(left).max(1));
        let height = bottom.saturating_sub(top).max(1).min(image.1.saturating_sub(top).max(1));
        (left, top, width, height)
    }
}

/// How close to an edge, in screen pixels, counts as reaching for it.
///
/// Generous, because the alternative is a person hunting for a one-pixel line.
pub const GRAB: f32 = 12.0;

/// The smallest a box may be dragged to, in image pixels. Below this a crop is
/// almost certainly a misclick rather than an intention.
const MINIMUM: f32 = 8.0;

/// A crop in progress.
#[derive(Clone, Copy, Debug)]
pub struct Crop {
    /// The picture being cropped, in pixels.
    image: (u32, u32),
    area: Box2,
    pub ratio: Ratio,
    /// The handle a drag has hold of, while one is going on.
    holding: Option<Handle>,
    /// Where the pointer was when the drag began, in image coordinates, and
    /// the box as it was then.
    ///
    /// Both kept, so a move is computed against the start rather than against
    /// the previous frame: accumulating deltas drifts, and a box held to a
    /// ratio would drift off it.
    from: (f32, f32),
    started_as: Box2,
    /// The framing to fall back to when a drag turns out to be a click.
    ///
    /// Its own field rather than `started_as`, which a fresh drag has to set
    /// to the collapsed box so that the drag is drawn from the press point.
    /// One field carrying both meanings put a new box's corner at the old
    /// box's, which `drawing_a_fresh_box_starts_where_the_press_was` caught.
    fell_back_to: Box2,
}

impl Crop {
    /// Open a crop over the whole picture.
    ///
    /// The whole picture rather than an inset rectangle: it makes the first
    /// gesture a trim of the edge the person wants trimmed, instead of a
    /// correction of a box the viewer chose for them.
    pub fn new(image: (u32, u32)) -> Self {
        let area = Box2 {
            left: 0.0,
            top: 0.0,
            right: image.0.max(1) as f32,
            bottom: image.1.max(1) as f32,
        };
        Self {
            image: (image.0.max(1), image.1.max(1)),
            area,
            ratio: Ratio::Free,
            holding: None,
            from: (0.0, 0.0),
            started_as: area,
            fell_back_to: area,
        }
    }

    pub fn area(&self) -> Box2 {
        self.area
    }

    pub fn image(&self) -> (u32, u32) {
        self.image
    }

    pub fn dragging(&self) -> bool {
        self.holding.is_some()
    }

    /// Which handle is under a point of the picture, given how big a screen
    /// pixel is in image coordinates.
    ///
    /// The grab distance is converted rather than fixed, so the handles are
    /// the same size under the pointer whatever the zoom. A fixed distance in
    /// image pixels would be untouchable on a photograph fitted to the window
    /// and would swallow the whole box on one zoomed in.
    pub fn handle_at(&self, point: (f32, f32), image_pixels_per_screen_pixel: f32) -> Option<Handle> {
        let grab = (GRAB * image_pixels_per_screen_pixel).max(1.0);

        let near_left = (point.0 - self.area.left).abs() <= grab;
        let near_right = (point.0 - self.area.right).abs() <= grab;
        let near_top = (point.1 - self.area.top).abs() <= grab;
        let near_bottom = (point.1 - self.area.bottom).abs() <= grab;

        // Within the box's span on the other axis, so the corner handles do
        // not extend infinitely along their edges.
        let within_x = point.0 >= self.area.left - grab && point.0 <= self.area.right + grab;
        let within_y = point.1 >= self.area.top - grab && point.1 <= self.area.bottom + grab;

        // Corners first: at a corner both edges are near, and answering with
        // an edge there would make the corner unreachable.
        match (near_left, near_right, near_top, near_bottom) {
            (true, _, true, _) => return Some(Handle::TopLeft),
            (_, true, true, _) => return Some(Handle::TopRight),
            (true, _, _, true) => return Some(Handle::BottomLeft),
            (_, true, _, true) => return Some(Handle::BottomRight),
            _ => {}
        }
        if near_left && within_y {
            return Some(Handle::Left);
        }
        if near_right && within_y {
            return Some(Handle::Right);
        }
        if near_top && within_x {
            return Some(Handle::Top);
        }
        if near_bottom && within_x {
            return Some(Handle::Bottom);
        }

        let inside = point.0 > self.area.left && point.0 < self.area.right && point.1 > self.area.top && point.1 < self.area.bottom;
        inside.then_some(Handle::Inside)
    }

    /// Begin a drag on `handle`, from `point`.
    pub fn begin(&mut self, handle: Handle, point: (f32, f32)) {
        self.holding = Some(handle);
        self.from = point;
        self.started_as = self.area;
        self.fell_back_to = self.area;
    }

    /// Begin drawing a new box from nothing, at `point`.
    ///
    /// A press on the picture outside the box starts again rather than moving
    /// what is there: someone who clicks well away from their crop has changed
    /// their mind about where it goes, and dragging the old box over to them
    /// would carry its size along with it.
    pub fn begin_fresh(&mut self, point: (f32, f32)) {
        // The framing that was there is what `finish` puts back when the
        // "drag" turns out to be a click, so it is kept aside. `started_as`
        // becomes the collapsed box, because that is what the drag is drawn
        // from.
        self.fell_back_to = self.area;
        self.area = Box2 {
            left: point.0,
            top: point.1,
            right: point.0,
            bottom: point.1,
        };
        self.started_as = self.area;
        self.from = point;
        self.holding = Some(Handle::BottomRight);
    }

    pub fn finish(&mut self) {
        // A box dragged to nothing — a click with no movement, on the picture
        // outside the old box — is not a crop of nothing; it is a slip. The
        // previous framing is the better answer than an empty one.
        if self.area.width() < MINIMUM || self.area.height() < MINIMUM {
            self.area = self.fell_back_to;
        }
        self.holding = None;
        self.settle();
    }

    /// Carry a drag to `point`.
    pub fn drag_to(&mut self, point: (f32, f32)) {
        let Some(handle) = self.holding else {
            return;
        };

        match handle {
            Handle::Inside => self.move_box(point),
            _ => self.resize(handle, point),
        }
    }

    /// Slide the whole box, keeping its size, held inside the picture.
    fn move_box(&mut self, point: (f32, f32)) {
        let delta = (point.0 - self.from.0, point.1 - self.from.1);
        let width = self.started_as.width();
        let height = self.started_as.height();

        // Clamped on the position rather than on each edge: clamping the edges
        // separately would squash the box against the side of the picture
        // instead of stopping it there.
        let left = (self.started_as.left + delta.0).clamp(0.0, (self.image.0 as f32 - width).max(0.0));
        let top = (self.started_as.top + delta.1).clamp(0.0, (self.image.1 as f32 - height).max(0.0));

        self.area = Box2 {
            left,
            top,
            right: left + width,
            bottom: top + height,
        };
    }

    /// Move one edge or corner to `point`.
    fn resize(&mut self, handle: Handle, point: (f32, f32)) {
        let mut area = self.started_as;
        let point = (point.0.clamp(0.0, self.image.0 as f32), point.1.clamp(0.0, self.image.1 as f32));

        match handle {
            Handle::TopLeft => {
                area.left = point.0;
                area.top = point.1;
            }
            Handle::TopRight => {
                area.right = point.0;
                area.top = point.1;
            }
            Handle::BottomLeft => {
                area.left = point.0;
                area.bottom = point.1;
            }
            Handle::BottomRight => {
                area.right = point.0;
                area.bottom = point.1;
            }
            Handle::Left => area.left = point.0,
            Handle::Right => area.right = point.0,
            Handle::Top => area.top = point.1,
            Handle::Bottom => area.bottom = point.1,
            Handle::Inside => return,
        }

        // A drag past the far edge turns the box inside out. Swapping rather
        // than stopping is what a person expects: the corner follows the
        // pointer and the box is simply the other way round.
        if area.left > area.right {
            std::mem::swap(&mut area.left, &mut area.right);
        }
        if area.top > area.bottom {
            std::mem::swap(&mut area.top, &mut area.bottom);
        }

        self.area = area;
        if let Some(ratio) = self.ratio.of(self.image) {
            self.hold_to_ratio(handle, ratio);
        }
        self.clamp_to_image();
    }

    /// Reshape the box to `ratio`, moving the edges the drag is not holding.
    ///
    /// Which axis gives way is decided by the handle. A side handle can only
    /// have moved one axis, so the other is the one that follows; a corner
    /// takes whichever the drag made larger, so the box tracks the pointer
    /// instead of snapping away from it.
    fn hold_to_ratio(&mut self, handle: Handle, ratio: f32) {
        let width = self.area.width();
        let height = self.area.height();
        if width <= 0.0 && height <= 0.0 {
            return;
        }

        let (width, height) = match handle {
            // A vertical edge moved: width is what the person set.
            Handle::Left | Handle::Right => (width, width / ratio),
            // A horizontal edge moved: height is what they set.
            Handle::Top | Handle::Bottom => (height * ratio, height),
            // A corner: follow the larger of the two, so the box grows with
            // the pointer rather than being pulled back by the smaller axis.
            _ => {
                if width / ratio >= height {
                    (width, width / ratio)
                } else {
                    (height * ratio, height)
                }
            }
        };

        // The anchor is the corner the drag is not moving. For a side handle
        // it is the opposite edge, and the other axis grows about the centre —
        // which is what makes dragging the left edge of a locked box feel like
        // reshaping it rather than pivoting it around a corner.
        match handle {
            Handle::Left => {
                self.area.left = self.area.right - width;
                let centre = (self.started_as.top + self.started_as.bottom) / 2.0;
                self.area.top = centre - height / 2.0;
                self.area.bottom = centre + height / 2.0;
            }
            Handle::Right => {
                self.area.right = self.area.left + width;
                let centre = (self.started_as.top + self.started_as.bottom) / 2.0;
                self.area.top = centre - height / 2.0;
                self.area.bottom = centre + height / 2.0;
            }
            Handle::Top => {
                self.area.top = self.area.bottom - height;
                let centre = (self.started_as.left + self.started_as.right) / 2.0;
                self.area.left = centre - width / 2.0;
                self.area.right = centre + width / 2.0;
            }
            Handle::Bottom => {
                self.area.bottom = self.area.top + height;
                let centre = (self.started_as.left + self.started_as.right) / 2.0;
                self.area.left = centre - width / 2.0;
                self.area.right = centre + width / 2.0;
            }
            _ => {
                let Some((anchor_right, anchor_bottom)) = handle.opposite_corner() else {
                    return;
                };
                if anchor_right {
                    self.area.left = self.area.right - width;
                } else {
                    self.area.right = self.area.left + width;
                }
                if anchor_bottom {
                    self.area.top = self.area.bottom - height;
                } else {
                    self.area.bottom = self.area.top + height;
                }
            }
        }
    }

    /// Bring the box back inside the picture.
    ///
    /// A ratio-locked box is moved rather than trimmed where it can be: a
    /// trimmed one would no longer be the shape that was asked for, which is
    /// the one thing the lock exists to guarantee. Where it cannot fit at all
    /// it is scaled down, keeping its shape.
    fn clamp_to_image(&mut self) {
        let (width, height) = (self.image.0 as f32, self.image.1 as f32);

        if self.ratio.of(self.image).is_some() {
            let mut box_width = self.area.width().min(width);
            let mut box_height = self.area.height().min(height);
            // Scaling keeps the shape; trimming one axis would not.
            if let Some(ratio) = self.ratio.of(self.image) {
                if box_width / ratio > box_height {
                    box_width = box_height * ratio;
                } else {
                    box_height = box_width / ratio;
                }
            }
            let left = self.area.left.clamp(0.0, (width - box_width).max(0.0));
            let top = self.area.top.clamp(0.0, (height - box_height).max(0.0));
            self.area = Box2 {
                left,
                top,
                right: left + box_width,
                bottom: top + box_height,
            };
            return;
        }

        self.area.left = self.area.left.clamp(0.0, width);
        self.area.top = self.area.top.clamp(0.0, height);
        self.area.right = self.area.right.clamp(0.0, width);
        self.area.bottom = self.area.bottom.clamp(0.0, height);
    }

    /// Choose a ratio, reshaping what is on screen to it.
    ///
    /// The box changes shape under the choice rather than waiting for the next
    /// drag: a person picking 16:9 is asking to see 16:9, and a control that
    /// did nothing until touched again would read as broken.
    pub fn set_ratio(&mut self, ratio: Ratio) {
        self.ratio = ratio;
        let Some(value) = ratio.of(self.image) else {
            return;
        };

        // About the centre, which keeps the subject where it was.
        let centre = ((self.area.left + self.area.right) / 2.0, (self.area.top + self.area.bottom) / 2.0);
        let mut width = self.area.width();
        let mut height = self.area.height();
        if width / value > height {
            width = height * value;
        } else {
            height = width / value;
        }
        self.area = Box2 {
            left: centre.0 - width / 2.0,
            top: centre.1 - height / 2.0,
            right: centre.0 + width / 2.0,
            bottom: centre.1 + height / 2.0,
        };
        self.clamp_to_image();
    }

    /// Put the box back over the whole picture.
    pub fn reset(&mut self) {
        self.area = Box2 {
            left: 0.0,
            top: 0.0,
            right: self.image.0 as f32,
            bottom: self.image.1 as f32,
        };
        if self.ratio != Ratio::Free {
            self.set_ratio(self.ratio);
        }
    }

    /// Hold the box to the picture and to the minimum size, after a drag.
    fn settle(&mut self) {
        if self.area.width() < MINIMUM {
            self.area.right = self.area.left + MINIMUM;
        }
        if self.area.height() < MINIMUM {
            self.area.bottom = self.area.top + MINIMUM;
        }
        self.clamp_to_image();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGE: (u32, u32) = (1000, 800);

    fn a_crop() -> Crop {
        Crop::new(IMAGE)
    }

    #[test]
    fn a_new_crop_frames_the_whole_picture() {
        let crop = a_crop();
        assert_eq!(crop.area().to_pixels(IMAGE), (0, 0, 1000, 800), "a crop opened on less than the picture");
    }

    #[test]
    fn a_corner_drag_moves_that_corner_and_leaves_the_opposite_one() {
        let mut crop = a_crop();
        crop.begin(Handle::TopLeft, (0.0, 0.0));
        crop.drag_to((200.0, 100.0));
        crop.finish();

        let area = crop.area();
        assert_eq!((area.left, area.top), (200.0, 100.0), "the dragged corner did not follow the pointer");
        assert_eq!((area.right, area.bottom), (1000.0, 800.0), "the opposite corner moved");
    }

    #[test]
    fn a_side_drag_moves_only_its_own_axis() {
        let mut crop = a_crop();
        crop.begin(Handle::Left, (0.0, 0.0));
        crop.drag_to((300.0, 500.0));
        crop.finish();

        let area = crop.area();
        assert_eq!(area.left, 300.0, "the edge did not follow the pointer");
        assert_eq!((area.top, area.bottom), (0.0, 800.0), "a vertical edge moved the horizontal ones");
    }

    #[test]
    fn dragging_a_corner_past_the_far_side_turns_the_box_round_rather_than_inside_out() {
        let mut crop = a_crop();
        crop.begin(Handle::TopLeft, (0.0, 0.0));
        // Past the bottom-right corner on both axes.
        crop.drag_to((900.0, 700.0));
        let area = crop.area();
        assert!(area.right >= area.left && area.bottom >= area.top, "the box was left inside out: {area:?}");

        crop.drag_to((1000.0, 800.0));
        crop.finish();
        let area = crop.area();
        assert!(area.width() >= 0.0 && area.height() >= 0.0, "the box was left inside out: {area:?}");
    }

    #[test]
    fn a_drag_cannot_take_the_box_outside_the_picture() {
        let mut crop = a_crop();
        crop.begin(Handle::TopLeft, (0.0, 0.0));
        crop.drag_to((-500.0, -500.0));
        crop.finish();

        let area = crop.area();
        assert!(area.left >= 0.0 && area.top >= 0.0, "the box escaped the picture: {area:?}");

        let mut crop = a_crop();
        crop.begin(Handle::BottomRight, (1000.0, 800.0));
        crop.drag_to((5000.0, 5000.0));
        crop.finish();
        let area = crop.area();
        assert!(area.right <= 1000.0 && area.bottom <= 800.0, "the box escaped the picture: {area:?}");
    }

    #[test]
    fn moving_the_box_keeps_its_size_and_stops_at_the_edge() {
        let mut crop = a_crop();
        crop.begin(Handle::TopLeft, (0.0, 0.0));
        crop.drag_to((100.0, 100.0));
        crop.finish();
        let before = crop.area();

        crop.begin(Handle::Inside, (500.0, 400.0));
        crop.drag_to((-900.0, -900.0));
        crop.finish();
        let after = crop.area();

        assert!(
            (after.width() - before.width()).abs() < 0.01 && (after.height() - before.height()).abs() < 0.01,
            "moving the box changed its size: {before:?} -> {after:?}"
        );
        assert_eq!((after.left, after.top), (0.0, 0.0), "the box did not stop at the edge");
    }

    /// The promise of a locked ratio, and the one a test must not state as the
    /// implementation states it: whatever the drag, the shape is the shape.
    #[test]
    fn a_locked_ratio_survives_every_handle() {
        for handle in [
            Handle::TopLeft,
            Handle::TopRight,
            Handle::BottomLeft,
            Handle::BottomRight,
            Handle::Left,
            Handle::Right,
            Handle::Top,
            Handle::Bottom,
        ] {
            let mut crop = a_crop();
            crop.set_ratio(Ratio::SixteenNine);
            crop.begin(handle, (500.0, 400.0));
            crop.drag_to((320.0, 610.0));
            crop.finish();

            let area = crop.area();
            let shape = area.width() / area.height();
            assert!(
                (shape - 16.0 / 9.0).abs() < 0.02,
                "{handle:?} left the box at {shape:.4} rather than 16:9 ({area:?})"
            );
            assert!(
                area.left >= -0.01 && area.top >= -0.01 && area.right <= 1000.01 && area.bottom <= 800.01,
                "{handle:?} left the box outside the picture: {area:?}"
            );
        }
    }

    #[test]
    fn choosing_a_ratio_reshapes_the_box_at_once() {
        let mut crop = a_crop();
        crop.set_ratio(Ratio::Square);
        let area = crop.area();
        assert!(
            (area.width() - area.height()).abs() < 0.01,
            "choosing a square left the box at {}x{}",
            area.width(),
            area.height()
        );
    }

    #[test]
    fn the_original_ratio_is_the_pictures_own() {
        let mut crop = a_crop();
        crop.begin(Handle::TopLeft, (0.0, 0.0));
        crop.drag_to((400.0, 100.0));
        crop.finish();

        crop.set_ratio(Ratio::Original);
        let area = crop.area();
        let shape = area.width() / area.height();
        assert!((shape - 1000.0 / 800.0).abs() < 0.01, "the box is at {shape:.4} rather than the picture's 1.25");
    }

    #[test]
    fn a_click_with_no_drag_keeps_the_framing_rather_than_cropping_to_nothing() {
        let mut crop = a_crop();
        crop.begin(Handle::TopLeft, (0.0, 0.0));
        crop.drag_to((100.0, 100.0));
        crop.finish();
        let framed = crop.area();

        // A press on the picture away from the box, released without moving.
        crop.begin_fresh((600.0, 500.0));
        crop.finish();

        assert_eq!(crop.area(), framed, "a click threw the framing away");
    }

    /// The other half of `begin_fresh`, and the half the click test cannot
    /// see: a drag that does move must draw the box the pointer describes,
    /// from the press point, not from a corner of the framing it replaced.
    #[test]
    fn drawing_a_fresh_box_starts_where_the_press_was() {
        let mut crop = a_crop();
        crop.begin(Handle::TopLeft, (0.0, 0.0));
        crop.drag_to((50.0, 50.0));
        crop.finish();

        crop.begin_fresh((600.0, 500.0));
        crop.drag_to((800.0, 650.0));
        crop.finish();

        let area = crop.area();
        assert_eq!(
            (area.left, area.top, area.right, area.bottom),
            (600.0, 500.0, 800.0, 650.0),
            "a fresh box was not drawn from the press point"
        );
    }

    #[test]
    fn a_handle_is_the_same_size_under_the_pointer_at_any_zoom() {
        let crop = a_crop();
        // Zoomed in: one screen pixel is a fraction of an image pixel, so the
        // handle reaches a fraction of an image pixel out.
        assert_eq!(crop.handle_at((2.0, 2.0), 0.25), Some(Handle::TopLeft));
        assert_eq!(crop.handle_at((200.0, 200.0), 0.25), Some(Handle::Inside));
        // Fitted: one screen pixel is several image pixels, so the same
        // reach in screen terms covers more of the picture.
        assert_eq!(crop.handle_at((20.0, 20.0), 4.0), Some(Handle::TopLeft));
    }

    #[test]
    fn the_corners_win_over_the_edges_where_they_meet() {
        let crop = a_crop();
        // At the top-left the left edge and the top edge are both near; an
        // answer of `Left` there would make the corner unreachable.
        assert_eq!(crop.handle_at((1.0, 1.0), 1.0), Some(Handle::TopLeft));
        // Well down the left edge, only the edge is near.
        assert_eq!(crop.handle_at((1.0, 400.0), 1.0), Some(Handle::Left));
    }

    #[test]
    fn a_box_is_never_taken_as_nothing() {
        let mut crop = a_crop();
        crop.begin(Handle::Right, (1000.0, 400.0));
        crop.drag_to((0.0, 400.0));
        crop.finish();

        let (_, _, width, height) = crop.area().to_pixels(IMAGE);
        assert!(width >= 1 && height >= 1, "the crop came out as {width}x{height}");
    }

    #[test]
    fn resetting_frames_the_whole_picture_again() {
        let mut crop = a_crop();
        crop.begin(Handle::TopLeft, (0.0, 0.0));
        crop.drag_to((400.0, 300.0));
        crop.finish();
        crop.reset();
        assert_eq!(crop.area().to_pixels(IMAGE), (0, 0, 1000, 800), "reset left the box cropped");
    }
}
