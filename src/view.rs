//! Where the image sits inside the window: zoom, pan, and the fit rules.
//!
//! The state here is deliberately free of any GPU or windowing type — it is
//! plain geometry, so the rules that users feel (zoom lands where the cursor
//! is, a small image is not blown up to fill the window) are testable without
//! a device or a surface.

/// Zoom limits. Below the floor an image is a speck; above the ceiling the
/// screen shows a handful of texels and further zoom carries no information.
const MIN_SCALE: f32 = 0.02;
const MAX_SCALE: f32 = 64.0;

/// One notch of the wheel multiplies the scale by this factor, which keeps
/// zooming geometric: every notch covers the same visual distance whether the
/// image is tiny or huge.
const WHEEL_STEP: f32 = 1.1;

/// How the image should be scaled when it is loaded or the window resizes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FitMode {
    /// Fit inside the window, never enlarging beyond 100%.
    #[default]
    Fit,
    /// One image pixel per logical screen pixel — "100%" as the user reads it.
    Actual,
    /// The user has zoomed or panned; the viewer leaves the framing alone.
    Free,
}

/// The placement of one image inside one window, in physical pixels.
#[derive(Clone, Copy, Debug)]
pub struct View {
    /// Size of the image after its EXIF orientation is applied.
    image: (f32, f32),
    /// Size of the drawing surface.
    window: (f32, f32),
    /// Image pixels per physical screen pixel.
    scale: f32,
    /// Physical pixels per logical pixel, as the display reports it.
    ///
    /// "100%" means one image pixel per *logical* pixel: on a 200% display an
    /// image shown at one texel per physical pixel is half the size the user
    /// sees it at everywhere else, which reads as a bug rather than fidelity.
    scale_factor: f32,
    /// Offset of the image centre from the window centre.
    offset: (f32, f32),
    mode: FitMode,
}

impl View {
    /// Frame a freshly loaded image: fit it to the window.
    ///
    /// `window` is in physical pixels; `scale_factor` is what the display
    /// reports, so 1.0 on a standard monitor and 2.0 at 200%.
    pub fn new(image: (u32, u32), window: (u32, u32), scale_factor: f32) -> Self {
        let mut view = Self {
            image: (image.0.max(1) as f32, image.1.max(1) as f32),
            window: (window.0.max(1) as f32, window.1.max(1) as f32),
            scale: 1.0,
            scale_factor: if scale_factor.is_finite() && scale_factor > 0.0 { scale_factor } else { 1.0 },
            offset: (0.0, 0.0),
            mode: FitMode::Fit,
        };
        view.fit();
        view
    }

    /// Zoom as the user reads it: 1.0 is one image pixel per logical pixel.
    pub fn scale(&self) -> f32 {
        self.scale / self.scale_factor
    }

    /// Image pixels per physical pixel.
    ///
    /// The renderer works from [`scaled_size`](Self::scaled_size), which folds
    /// this in already; the raw factor is what the geometry tests check.
    #[cfg(test)]
    pub fn physical_scale(&self) -> f32 {
        self.scale
    }

    pub fn mode(&self) -> FitMode {
        self.mode
    }

    /// Offset of the image centre from the window centre, in physical pixels.
    pub fn offset(&self) -> (f32, f32) {
        self.offset
    }

    /// The image size on screen at the current scale.
    pub fn scaled_size(&self) -> (f32, f32) {
        (self.image.0 * self.scale, self.image.1 * self.scale)
    }

    /// Carry this framing onto a different picture, for the zoom lock.
    ///
    /// Unlike [`rebase`](Self::rebase), the two images are not the same
    /// picture: they are neighbours in a folder, and may be any size. What
    /// carries over is what the user set — the zoom as they read it, and where
    /// in the frame they were looking, as a fraction of the image.
    ///
    /// The fraction is what makes a series comparable. Holding the pixel
    /// offset instead would drift across images of different sizes; holding
    /// the fraction puts the same part of each picture under the same part of
    /// the window, which is the whole point of stepping through frames of one
    /// scene.
    ///
    /// `Fit` is not carried: it is the default rather than a choice, and each
    /// image answers it for itself. `Actual` is a choice — "show me this
    /// pixel for pixel" — so it carries, which is how a series is walked at
    /// 100% to compare sharpness.
    pub fn carry_onto(&self, image: (u32, u32), window: (u32, u32), scale_factor: f32) -> Self {
        let mut next = Self::new(image, window, scale_factor);
        match self.mode {
            FitMode::Fit => return next,
            FitMode::Actual => {
                next.set_actual();
                return next;
            }
            FitMode::Free => {}
        }

        // The zoom the user reads, restated in the new view's physical terms.
        next.scale = (self.scale() * next.scale_factor).clamp(MIN_SCALE, MAX_SCALE);
        next.mode = FitMode::Free;

        // Where they were looking, as a fraction of the image, mapped onto
        // whatever the new image's overhang allows.
        let fraction = self.looking_at();
        let (width, height) = next.scaled_size();
        let limit_x = ((width - next.window.0) / 2.0).max(0.0);
        let limit_y = ((height - next.window.1) / 2.0).max(0.0);
        next.offset = (fraction.0 * limit_x, fraction.1 * limit_y);
        next.clamp_offset();
        next
    }

    /// Where in the frame the user is looking, as a fraction of the pan range.
    ///
    /// Zero is centred; ±1 is against an edge. Expressed this way it means the
    /// same thing on an image of any size, which is what the zoom lock needs.
    fn looking_at(&self) -> (f32, f32) {
        let (width, height) = self.scaled_size();
        let limit_x = ((width - self.window.0) / 2.0).max(0.0);
        let limit_y = ((height - self.window.1) / 2.0).max(0.0);
        (
            if limit_x > 0.0 { self.offset.0 / limit_x } else { 0.0 },
            if limit_y > 0.0 { self.offset.1 / limit_y } else { 0.0 },
        )
    }

    /// Swap in an image of the same picture at a different resolution,
    /// keeping the framing the user is looking at.
    ///
    /// This is the thumbnail-to-full-image handover. The two differ in pixel
    /// count but show the same thing, so the scale is rebased to keep the
    /// picture the same size on screen: a thumbnail at 8x becomes a full image
    /// at 1x without anything appearing to move.
    pub fn rebase(&mut self, image: (u32, u32)) {
        let width = image.0.max(1) as f32;
        let height = image.1.max(1) as f32;
        let ratio = self.image.0 / width;

        self.image = (width, height);
        // `Fit` and `Actual` are recomputed from the new size; only a framing
        // the user chose has to be carried across by hand.
        match self.mode {
            FitMode::Fit => self.fit(),
            FitMode::Actual => self.set_actual(),
            FitMode::Free => {
                self.scale = (self.scale * ratio).clamp(MIN_SCALE, MAX_SCALE);
                self.clamp_offset();
            }
        }
    }

    /// React to a resized window, preserving whatever framing is in force.
    ///
    /// A window dragged to another monitor changes both size and scale factor
    /// in one event, so both are taken here.
    pub fn resize(&mut self, window: (u32, u32), scale_factor: f32) {
        self.window = (window.0.max(1) as f32, window.1.max(1) as f32);
        if scale_factor.is_finite() && scale_factor > 0.0 {
            self.scale_factor = scale_factor;
        }
        match self.mode {
            FitMode::Fit => self.fit(),
            FitMode::Actual => self.set_actual(),
            // Free framing is the user's; only the clamp is re-applied.
            FitMode::Free => self.clamp_offset(),
        }
    }

    /// Scale the image to fit the window, never enlarging past 100%.
    ///
    /// Enlarging a thumbnail to fill a 4K window shows nothing but
    /// interpolation, so "fit" means "fit if it is too large".
    pub fn fit(&mut self) {
        let by_width = self.window.0 / self.image.0;
        let by_height = self.window.1 / self.image.1;
        // The ceiling is 100% as the user reads it, which on a scaled display
        // is more than one texel per physical pixel.
        self.scale = by_width.min(by_height).min(self.scale_factor).clamp(MIN_SCALE, MAX_SCALE);
        self.offset = (0.0, 0.0);
        self.mode = FitMode::Fit;
    }

    /// Show the image at one image pixel per logical pixel.
    pub fn set_actual(&mut self) {
        self.scale = self.scale_factor.clamp(MIN_SCALE, MAX_SCALE);
        self.offset = (0.0, 0.0);
        self.mode = FitMode::Actual;
        self.clamp_offset();
    }

    /// Toggle between fitting the window and 100%.
    pub fn toggle_fit_actual(&mut self) {
        if self.mode == FitMode::Actual {
            self.fit();
        } else {
            self.set_actual();
        }
    }

    /// Zoom by `notches` wheel steps, keeping the image point under the cursor
    /// stationary. `cursor` is in physical pixels from the window's top-left.
    ///
    /// Anchoring to the cursor is what makes zoom feel like moving a loupe
    /// rather than resizing a picture: without it every zoom is followed by a
    /// corrective pan.
    pub fn zoom_at(&mut self, notches: f32, cursor: (f32, f32)) {
        let target = (self.scale * WHEEL_STEP.powf(notches)).clamp(MIN_SCALE, MAX_SCALE);
        self.zoom_to_at(target, cursor);
    }

    /// Zoom to an absolute scale, keeping the point under `cursor` fixed.
    pub fn zoom_to_at(&mut self, scale: f32, cursor: (f32, f32)) {
        let target = scale.clamp(MIN_SCALE, MAX_SCALE);
        if (target - self.scale).abs() < f32::EPSILON {
            return;
        }

        // Cursor relative to the window centre, where the image is anchored.
        let from_centre = (cursor.0 - self.window.0 / 2.0, cursor.1 - self.window.1 / 2.0);
        // The image-space point under the cursor must not move, so the offset
        // absorbs the change in scale around that point.
        let ratio = target / self.scale;
        self.offset = (
            from_centre.0 - (from_centre.0 - self.offset.0) * ratio,
            from_centre.1 - (from_centre.1 - self.offset.1) * ratio,
        );
        self.scale = target;
        self.mode = FitMode::Free;
        self.clamp_offset();
    }

    /// Drag the image by a mouse delta in physical pixels.
    pub fn pan(&mut self, delta: (f32, f32)) {
        if delta.0 == 0.0 && delta.1 == 0.0 {
            return;
        }
        self.offset = (self.offset.0 + delta.0, self.offset.1 + delta.1);
        self.mode = FitMode::Free;
        self.clamp_offset();
    }

    /// Keep the image from being dragged off-screen.
    ///
    /// An axis smaller than the window stays centred on that axis; a larger one
    /// may be panned until its edge meets the window edge, and no further.
    fn clamp_offset(&mut self) {
        let (width, height) = self.scaled_size();
        let limit_x = ((width - self.window.0) / 2.0).max(0.0);
        let limit_y = ((height - self.window.1) / 2.0).max(0.0);
        self.offset = (self.offset.0.clamp(-limit_x, limit_x), self.offset.1.clamp(-limit_y, limit_y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn about(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    /// A view on an ordinary, unscaled display.
    fn view(image: (u32, u32), window: (u32, u32)) -> View {
        View::new(image, window, 1.0)
    }

    #[test]
    fn fit_shrinks_a_large_image_to_the_window() {
        let view = view((2000, 1000), (1000, 1000));
        assert!(about(view.scale(), 0.5));
        assert_eq!(view.offset(), (0.0, 0.0));
    }

    #[test]
    fn fit_never_enlarges_a_small_image() {
        let view = view((100, 80), (1000, 1000));
        assert!(about(view.scale(), 1.0));
        assert_eq!(view.mode(), FitMode::Fit);
    }

    #[test]
    fn actual_size_is_one_to_one() {
        let mut view = view((4000, 3000), (800, 600));
        view.set_actual();
        assert!(about(view.scale(), 1.0));
        assert_eq!(view.mode(), FitMode::Actual);
    }

    /// The bug this guards against was visible on a 200% display: an image
    /// smaller than the surface in physical pixels sat at a quarter of the
    /// window, because "do not enlarge" was measured in physical pixels.
    #[test]
    fn a_scaled_display_shows_an_image_at_the_size_the_user_expects() {
        // 1200x800 image, 1280x800 logical window at 200% -> 2560x1600.
        let view = View::new((1200, 800), (2560, 1600), 2.0);

        // Read as 100% by the user...
        assert!(about(view.scale(), 1.0));
        // ...which is two texels per logical pixel on this display.
        assert!(about(view.physical_scale(), 2.0));
        // The image covers most of the window rather than a quarter of it.
        let (width, _) = view.scaled_size();
        assert!(width > 2000.0, "the image spans only {width} physical pixels");
    }

    #[test]
    fn a_scaled_display_still_fits_an_image_too_large_for_the_window() {
        let view = View::new((8000, 6000), (2560, 1600), 2.0);
        assert!(view.physical_scale() < 1.0);
        // A pixel of slack: fitting divides and multiplies by the same size,
        // so the fitted axis lands a rounding step either side of exact.
        let (width, height) = view.scaled_size();
        assert!(width <= 2561.0 && height <= 1601.0, "the fitted image spans {width}x{height}");
    }

    #[test]
    fn toggling_returns_to_the_fitted_framing() {
        let mut view = view((4000, 3000), (800, 600));
        let fitted = view.scale();

        view.toggle_fit_actual();
        assert!(about(view.scale(), 1.0));
        view.toggle_fit_actual();
        assert!(about(view.scale(), fitted));
    }

    #[test]
    fn zoom_keeps_the_point_under_the_cursor_in_place() {
        let mut view = view((1000, 1000), (1000, 1000));
        let cursor = (250.0, 400.0);

        // The image point under the cursor before zooming...
        let before = image_point_under(&view, cursor);
        view.zoom_at(6.0, cursor);
        let after = image_point_under(&view, cursor);

        assert!(view.scale() > 1.0);
        assert!(about(before.0, after.0), "{before:?} vs {after:?}");
        assert!(about(before.1, after.1), "{before:?} vs {after:?}");
    }

    /// Which image-space pixel sits beneath a window position.
    fn image_point_under(view: &View, cursor: (f32, f32)) -> (f32, f32) {
        let (width, height) = view.scaled_size();
        let (offset_x, offset_y) = view.offset();
        let top_left = (view.window.0 / 2.0 + offset_x - width / 2.0, view.window.1 / 2.0 + offset_y - height / 2.0);
        ((cursor.0 - top_left.0) / view.physical_scale(), (cursor.1 - top_left.1) / view.physical_scale())
    }

    #[test]
    fn zoom_is_bounded_at_both_ends() {
        let mut view = view((1000, 1000), (1000, 1000));
        view.zoom_at(500.0, (500.0, 500.0));
        assert!(view.physical_scale() <= MAX_SCALE);

        view.zoom_at(-5000.0, (500.0, 500.0));
        assert!(view.physical_scale() >= MIN_SCALE);
    }

    #[test]
    fn an_image_smaller_than_the_window_stays_centred_when_panned() {
        let mut view = view((200, 200), (1000, 1000));
        view.pan((300.0, -200.0));
        assert_eq!(view.offset(), (0.0, 0.0));
    }

    #[test]
    fn a_zoomed_image_pans_no_further_than_its_edge() {
        let mut view = view((1000, 1000), (500, 500));
        view.set_actual();
        view.pan((10_000.0, 10_000.0));

        // Half the overhang on each axis: (1000 - 500) / 2.
        assert!(about(view.offset().0, 250.0));
        assert!(about(view.offset().1, 250.0));
    }

    #[test]
    fn resizing_reflows_a_fitted_image_but_leaves_a_free_one() {
        let mut fitted = view((2000, 2000), (1000, 1000));
        fitted.resize((500, 500), 1.0);
        assert!(about(fitted.scale(), 0.25));

        let mut free = view((2000, 2000), (1000, 1000));
        free.zoom_at(3.0, (500.0, 500.0));
        let scale = free.scale();
        free.resize((500, 500), 1.0);
        assert!(about(free.scale(), scale));
    }

    #[test]
    fn moving_to_another_monitor_rescales_a_fitted_image() {
        let mut view = View::new((400, 400), (1000, 1000), 1.0);
        assert!(about(view.physical_scale(), 1.0));

        // The same window dragged onto a 200% display.
        view.resize((2000, 2000), 2.0);
        assert!(about(view.scale(), 1.0));
        assert!(about(view.physical_scale(), 2.0));
    }

    /// The zoom lock's whole purpose: step through frames of one scene and
    /// each arrives at the same magnification over the same part of the
    /// picture, so the difference between them is what moves.
    #[test]
    fn the_lock_carries_the_zoom_and_the_place_onto_the_next_image() {
        let mut first = view((4000, 3000), (1000, 800));
        first.set_actual();
        // Look at the top-left corner rather than the middle.
        first.pan((10_000.0, 10_000.0));
        let zoom = first.scale();
        let place = first.looking_at();

        // The next frame of the same shoot: identical dimensions.
        let next = first.carry_onto((4000, 3000), (1000, 800), 1.0);

        assert!(about(next.scale(), zoom), "the zoom changed: {} vs {zoom}", next.scale());
        assert!(about(next.looking_at().0, place.0), "the horizontal place moved");
        assert!(about(next.looking_at().1, place.1), "the vertical place moved");
        // Same size, same framing: the offsets themselves match too.
        assert!(about(next.offset().0, first.offset().0));
        assert!(about(next.offset().1, first.offset().1));
    }

    /// The place is held as a fraction, so a neighbour of another size shows
    /// the corresponding part of itself rather than drifting by pixels.
    #[test]
    fn the_lock_holds_the_same_part_of_a_differently_sized_neighbour() {
        let mut first = view((4000, 3000), (1000, 800));
        first.zoom_to_at(2.0, (500.0, 400.0));
        first.pan((10_000.0, 0.0));
        assert!(about(first.looking_at().0, 1.0), "the fixture is not against the edge");

        // Half the size, same aspect: still against the same edge.
        let next = first.carry_onto((2000, 1500), (1000, 800), 1.0);
        assert!(about(next.scale(), first.scale()), "the zoom did not carry");
        assert!(about(next.looking_at().0, 1.0), "the place did not carry onto a smaller neighbour");
    }

    /// `Fit` is the default rather than a choice, so it is not carried: each
    /// image is fitted for itself. `Actual` *is* a choice — "show me this
    /// pixel for pixel" — and carries, which is how a series is walked at
    /// 100% to compare sharpness.
    #[test]
    fn the_lock_carries_a_choice_but_not_the_default() {
        let fitted = view((4000, 3000), (1000, 800));
        assert_eq!(fitted.mode(), FitMode::Fit);
        let next = fitted.carry_onto((800, 600), (1000, 800), 1.0);
        assert_eq!(next.mode(), FitMode::Fit, "a fitted framing was carried instead of recomputed");
        // A small neighbour is shown at 100%, not shrunk to the big one's fit.
        assert!(about(next.scale(), 1.0), "the small neighbour came out at {}", next.scale());

        let mut actual = view((4000, 3000), (1000, 800));
        actual.set_actual();
        let next = actual.carry_onto((800, 600), (1000, 800), 1.0);
        assert_eq!(next.mode(), FitMode::Actual, "100% did not carry to the next image");
        assert!(about(next.scale(), 1.0));
    }

    /// A neighbour that fits in the window has nothing to pan, so the carried
    /// place has nowhere to go — and must not push it off centre.
    #[test]
    fn the_lock_does_not_shift_a_neighbour_that_fits() {
        let mut first = view((4000, 3000), (1000, 800));
        first.set_actual();
        first.pan((10_000.0, 10_000.0));

        let next = first.carry_onto((200, 150), (1000, 800), 1.0);
        assert_eq!(next.offset(), (0.0, 0.0), "a neighbour smaller than the window was pushed off centre");
    }

    /// The zoom the user reads is what carries, not the physical scale: the
    /// same series stepped through on a 200% display must not double.
    #[test]
    fn the_lock_carries_the_zoom_the_user_reads() {
        let mut first = View::new((4000, 3000), (1000, 800), 1.0);
        first.zoom_to_at(2.0, (500.0, 400.0));

        let next = first.carry_onto((4000, 3000), (2000, 1600), 2.0);
        assert!(about(next.scale(), 2.0), "the zoom read {} on the scaled display", next.scale());
        assert!(about(next.physical_scale(), 4.0), "the physical scale did not follow the display");
    }

    #[test]
    fn a_degenerate_size_does_not_divide_by_zero() {
        let view = view((0, 0), (0, 0));
        assert!(view.scale().is_finite());
        assert!(view.scale() > 0.0);
    }

    #[test]
    fn a_nonsense_scale_factor_falls_back_to_one() {
        let view = View::new((100, 100), (1000, 1000), 0.0);
        assert!(about(view.scale(), view.physical_scale()));
    }
}
