//! How a picture meets a frame of another shape: the desktop, a printed page.
//!
//! One answer for both, because it is one question. A picture either covers
//! the frame and loses what overhangs, or shows whole with the frame round it;
//! the desktop and the page differ only in which they start from.

/// How a picture meets a frame of another shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fit {
    /// Cover the whole frame, cutting off what overhangs.
    Fill,
    /// Show the whole picture, with the frame round it.
    Fit,
}

impl Fit {
    /// How much a picture of `size` is scaled to meet a frame of `frame`, in
    /// whatever unit both are given in.
    ///
    /// Filling, the side that has further to go decides; fitting, the side
    /// that arrives first. A picture or a frame with no size is answered with
    /// no scale rather than with an infinity that would travel into a layout.
    pub fn scale(self, size: (f64, f64), frame: (f64, f64)) -> f64 {
        if size.0 <= 0.0 || size.1 <= 0.0 || frame.0 <= 0.0 || frame.1 <= 0.0 {
            return 0.0;
        }
        let across = frame.0 / size.0;
        let down = frame.1 / size.1;
        match self {
            Fit::Fill => across.max(down),
            Fit::Fit => across.min(down),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filling_covers_and_fitting_shows_all() {
        // A 3:2 picture against a square frame.
        let fill = Fit::Fill.scale((300.0, 200.0), (100.0, 100.0));
        let fit = Fit::Fit.scale((300.0, 200.0), (100.0, 100.0));
        assert_eq!(fill, 0.5, "filling lets the long side overhang");
        assert!((fit - 1.0 / 3.0).abs() < 1e-12, "fitting brings the long side inside");
        assert!(300.0 * fit <= 100.0 && 200.0 * fit <= 100.0);
        assert!(300.0 * fill >= 100.0 && 200.0 * fill >= 100.0);
    }

    #[test]
    fn nothing_to_scale_is_no_scale() {
        assert_eq!(Fit::Fill.scale((0.0, 200.0), (100.0, 100.0)), 0.0);
        assert_eq!(Fit::Fit.scale((300.0, 200.0), (100.0, 0.0)), 0.0);
    }
}
