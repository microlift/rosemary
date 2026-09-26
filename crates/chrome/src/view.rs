use crate::geometry::{Point, Rect};
use crate::paint::Canvas;
use crate::theme::Theme;

/// A piece of chrome: laid out into a rectangle, painted, and hit-tested
/// against the same layout.
///
/// Stateless between calls: whatever a view shows (hover, a pressed button,
/// an open menu) is in the value itself, so the owner rebuilds or mutates it
/// and repaints. Layout is separate from painting so that a hit test answers
/// about exactly what was last drawn.
pub trait View {
    /// What a point can land on.
    type Part: Copy + PartialEq;
    type Layout;

    fn layout(&self, bounds: Rect, theme: &Theme) -> Self::Layout;

    fn paint(&self, canvas: &mut Canvas, layout: &Self::Layout, theme: &Theme);

    fn hit(&self, layout: &Self::Layout, point: Point) -> Option<Self::Part>;
}
