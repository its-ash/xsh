//! Binary split tree that places panes inside a window.

use serde::{Deserialize, Serialize};

use crate::PaneId;

const MIN_RATIO: u16 = 10;
const MAX_RATIO: u16 = 90;

/// A screen rectangle in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Rect {
    /// Left column.
    pub x: u16,
    /// Top row.
    pub y: u16,
    /// Width in columns.
    pub width: u16,
    /// Height in rows.
    pub height: u16,
}

impl Rect {
    /// Builds a rectangle.
    #[must_use]
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The area inside a one-cell border.
    #[must_use]
    pub fn inner(self) -> Self {
        Self::new(
            self.x.saturating_add(1),
            self.y.saturating_add(1),
            self.width.saturating_sub(2).max(1),
            self.height.saturating_sub(2).max(1),
        )
    }

    fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }
}

/// How a split divides its area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    /// Side by side (`Ctrl-b %`).
    Horizontal,
    /// Stacked (`Ctrl-b "`).
    Vertical,
}

/// A direction for focus moves and resizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dir {
    /// Toward column 0.
    Left,
    /// Toward the last column.
    Right,
    /// Toward row 0.
    Up,
    /// Toward the last row.
    Down,
}

impl Dir {
    fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::Horizontal,
            Self::Up | Self::Down => Axis::Vertical,
        }
    }
}

/// Layout tree node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Layout {
    /// A single pane.
    Pane(PaneId),
    /// Two children sharing an area.
    Split {
        /// Split direction.
        axis: Axis,
        /// Share of the area given to `first`, in percent.
        ratio: u16,
        /// Left or top child.
        first: Box<Layout>,
        /// Right or bottom child.
        second: Box<Layout>,
    },
}

impl Layout {
    /// Pane rectangles for `area`, in tree order.
    ///
    /// ```
    /// use rushy_pty::{Axis, Layout, PaneId, Rect};
    /// let mut l = Layout::Pane(PaneId(1));
    /// l.split(PaneId(1), Axis::Horizontal, PaneId(2));
    /// let r = l.rects(Rect::new(0, 0, 80, 24));
    /// assert_eq!(r[1], (PaneId(2), Rect::new(40, 0, 40, 24)));
    /// ```
    #[must_use]
    pub fn rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        self.collect(area, &mut out);
        out
    }

    fn collect(&self, area: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            Self::Pane(id) => out.push((*id, area)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (a, b) = divide(area, *axis, *ratio);
                first.collect(a, out);
                second.collect(b, out);
            }
        }
    }

    /// Pane ids in tree order.
    #[must_use]
    pub fn panes(&self) -> Vec<PaneId> {
        self.rects(Rect::new(0, 0, 0, 0))
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    /// Whether the tree contains `id`.
    #[must_use]
    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            Self::Pane(p) => *p == id,
            Self::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    /// Splits pane `target`, placing `new` right of or below it.
    pub fn split(&mut self, target: PaneId, axis: Axis, new: PaneId) -> bool {
        match self {
            Self::Pane(p) if *p == target => {
                *self = Self::Split {
                    axis,
                    ratio: 50,
                    first: Box::new(Self::Pane(target)),
                    second: Box::new(Self::Pane(new)),
                };
                true
            }
            Self::Pane(_) => false,
            Self::Split { first, second, .. } => {
                first.split(target, axis, new) || second.split(target, axis, new)
            }
        }
    }

    /// Removes `target`; its sibling takes its parent's place. Returns `None`
    /// when the tree held only `target`.
    #[must_use]
    pub fn remove(self, target: PaneId) -> Option<Self> {
        match self {
            Self::Pane(p) if p == target => None,
            Self::Pane(_) => Some(self),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.remove(target), second.remove(target)) {
                (Some(a), Some(b)) => Some(Self::Split {
                    axis,
                    ratio,
                    first: Box::new(a),
                    second: Box::new(b),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }

    /// Moves the nearest divider next to `target` along `dir` by `step` percent.
    pub fn resize(&mut self, target: PaneId, dir: Dir, step: u16) -> bool {
        let Self::Split {
            axis,
            ratio,
            first,
            second,
        } = self
        else {
            return false;
        };
        let inner = if first.contains(target) {
            first.resize(target, dir, step)
        } else if second.contains(target) {
            second.resize(target, dir, step)
        } else {
            return false;
        };
        if inner || *axis != dir.axis() {
            return inner;
        }
        *ratio = match dir {
            Dir::Right | Dir::Down => ratio.saturating_add(step),
            Dir::Left | Dir::Up => ratio.saturating_sub(step),
        }
        .clamp(MIN_RATIO, MAX_RATIO);
        true
    }
}

fn divide(area: Rect, axis: Axis, ratio: u16) -> (Rect, Rect) {
    let share = |len: u16| {
        let n = u32::from(len) * u32::from(ratio) / 100;
        u16::try_from(n).unwrap_or(len).clamp(1.min(len), len)
    };
    match axis {
        Axis::Horizontal => {
            let w = share(area.width);
            (
                Rect::new(area.x, area.y, w, area.height),
                Rect::new(area.x + w, area.y, area.width - w, area.height),
            )
        }
        Axis::Vertical => {
            let h = share(area.height);
            (
                Rect::new(area.x, area.y, area.width, h),
                Rect::new(area.x, area.y + h, area.width, area.height - h),
            )
        }
    }
}

/// The pane adjacent to `from` in direction `dir`, if any.
#[must_use]
pub fn neighbor(rects: &[(PaneId, Rect)], from: PaneId, dir: Dir) -> Option<PaneId> {
    let (_, cur) = rects.iter().find(|(id, _)| *id == from)?;
    let overlap = |a0: u16, a1: u16, b0: u16, b1: u16| a1.min(b1).saturating_sub(a0.max(b0));
    rects
        .iter()
        .filter(|(id, _)| *id != from)
        .filter_map(|(id, r)| {
            let (gap, ov) = match dir {
                Dir::Right if r.x >= cur.right() => (
                    r.x - cur.right(),
                    overlap(cur.y, cur.bottom(), r.y, r.bottom()),
                ),
                Dir::Left if r.right() <= cur.x => (
                    cur.x - r.right(),
                    overlap(cur.y, cur.bottom(), r.y, r.bottom()),
                ),
                Dir::Down if r.y >= cur.bottom() => (
                    r.y - cur.bottom(),
                    overlap(cur.x, cur.right(), r.x, r.right()),
                ),
                Dir::Up if r.bottom() <= cur.y => (
                    cur.y - r.bottom(),
                    overlap(cur.x, cur.right(), r.x, r.right()),
                ),
                _ => return None,
            };
            (ov > 0).then_some((gap, std::cmp::Reverse(ov), *id))
        })
        .min()
        .map(|(_, _, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn three() -> Layout {
        let mut l = Layout::Pane(PaneId(1));
        l.split(PaneId(1), Axis::Horizontal, PaneId(2));
        l.split(PaneId(2), Axis::Vertical, PaneId(3));
        l
    }

    #[test]
    fn split_places_panes() {
        let r = three().rects(Rect::new(0, 0, 80, 24));
        assert_eq!(
            r,
            [
                (PaneId(1), Rect::new(0, 0, 40, 24)),
                (PaneId(2), Rect::new(40, 0, 40, 12)),
                (PaneId(3), Rect::new(40, 12, 40, 12)),
            ]
        );
    }

    #[test]
    fn neighbors_follow_geometry() {
        let r = three().rects(Rect::new(0, 0, 80, 24));
        assert_eq!(neighbor(&r, PaneId(1), Dir::Right), Some(PaneId(2)));
        assert_eq!(neighbor(&r, PaneId(3), Dir::Up), Some(PaneId(2)));
        assert_eq!(neighbor(&r, PaneId(3), Dir::Left), Some(PaneId(1)));
        assert_eq!(neighbor(&r, PaneId(1), Dir::Left), None);
    }

    #[test]
    fn remove_collapses_parent() {
        let l = three().remove(PaneId(2));
        let r = l.map(|l| l.rects(Rect::new(0, 0, 80, 24)));
        assert_eq!(
            r,
            Some(vec![
                (PaneId(1), Rect::new(0, 0, 40, 24)),
                (PaneId(3), Rect::new(40, 0, 40, 24)),
            ])
        );
        assert_eq!(Layout::Pane(PaneId(1)).remove(PaneId(1)), None);
    }

    #[test]
    fn resize_moves_matching_divider() {
        let mut l = three();
        assert!(l.resize(PaneId(3), Dir::Right, 10));
        assert_eq!(l.rects(Rect::new(0, 0, 100, 20))[0].1.width, 60);
        assert!(l.resize(PaneId(3), Dir::Up, 20));
        assert_eq!(l.rects(Rect::new(0, 0, 100, 20))[1].1.height, 6);
    }
}
