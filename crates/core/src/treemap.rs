//! Squarified treemap layout (Bruls, Huizing & van Wijk, 2000).

use crate::tree::{NodeId, SizeMode, SortKey, Tree};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn area(&self) -> f32 {
        self.w * self.h
    }
}

/// Splits `bounds` into one rectangle per value, each with an area
/// proportional to its value, keeping the rectangles close to square.
///
/// `values` must be sorted largest first. Zero values get empty rectangles.
pub fn squarify(values: &[f64], bounds: Rect) -> Vec<Rect> {
    let mut out = vec![Rect::default(); values.len()];
    let total: f64 = values.iter().sum();
    if total <= 0.0 || bounds.w <= 0.0 || bounds.h <= 0.0 {
        return out;
    }
    let scale = f64::from(bounds.w) * f64::from(bounds.h) / total;
    let areas: Vec<f64> = values.iter().map(|v| v * scale).collect();

    let (mut x, mut y) = (f64::from(bounds.x), f64::from(bounds.y));
    let (mut w, mut h) = (f64::from(bounds.w), f64::from(bounds.h));
    let mut start = 0;
    while start < areas.len() && areas[start] > 0.0 {
        let side = w.min(h);
        // Grow the row while doing so makes its worst aspect ratio better.
        let mut end = start + 1;
        let mut best = worst_ratio(&areas[start..end], side);
        while end < areas.len() && areas[end] > 0.0 {
            let candidate = worst_ratio(&areas[start..=end], side);
            if candidate > best {
                break;
            }
            best = candidate;
            end += 1;
        }

        let row = &areas[start..end];
        let row_sum: f64 = row.iter().sum();
        let last = end == areas.len() || areas[end] <= 0.0;
        if w >= h {
            // Lay the row out as a column on the left.
            let thickness = if last { w } else { row_sum / h };
            let mut cy = y;
            for (i, a) in row.iter().enumerate() {
                let ih = if i + 1 == row.len() {
                    y + h - cy
                } else {
                    a / thickness
                };
                out[start + i] = rect(x, cy, thickness, ih);
                cy += ih;
            }
            x += thickness;
            w = (w - thickness).max(0.0);
        } else {
            // Lay the row out as a strip along the top.
            let thickness = if last { h } else { row_sum / w };
            let mut cx = x;
            for (i, a) in row.iter().enumerate() {
                let iw = if i + 1 == row.len() {
                    x + w - cx
                } else {
                    a / thickness
                };
                out[start + i] = rect(cx, y, iw, thickness);
                cx += iw;
            }
            y += thickness;
            h = (h - thickness).max(0.0);
        }
        start = end;
    }
    out
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect::new(x as f32, y as f32, w.max(0.0) as f32, h.max(0.0) as f32)
}

/// Worst aspect ratio in a row of `areas` laid along a side of length `side`.
fn worst_ratio(areas: &[f64], side: f64) -> f64 {
    let sum: f64 = areas.iter().sum();
    let max = areas.iter().copied().fold(f64::MIN, f64::max);
    let min = areas.iter().copied().fold(f64::MAX, f64::min);
    let side2 = side * side;
    let sum2 = sum * sum;
    (side2 * max / sum2).max(sum2 / (side2 * min))
}

#[derive(Clone, Copy, Debug)]
pub struct TreemapOptions {
    /// Directory levels to draw below the focused directory.
    pub max_depth: u8,
    /// Rectangles narrower or shorter than this are dropped.
    pub min_side: f32,
    /// Height reserved for a directory's label above its contents.
    pub header: f32,
    /// Gap between a directory's border and its contents.
    pub padding: f32,
    /// Upper bound on rectangles, to keep rendering cheap.
    pub max_items: usize,
}

impl Default for TreemapOptions {
    fn default() -> Self {
        Self {
            max_depth: 3,
            min_side: 3.0,
            header: 16.0,
            padding: 2.0,
            max_items: 1500,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TreemapItem {
    pub node: NodeId,
    pub rect: Rect,
    /// 0 for children of the focused directory, 1 for grandchildren, …
    pub depth: u8,
    /// Index of the focused directory's child this item belongs to, for coloring.
    pub group: u16,
    /// The item is a directory whose children are drawn inside it, below a label.
    pub nested: bool,
}

/// Lays out the contents of `focus` inside `bounds`. Parents come before
/// their children in the result, so drawing in order paints children on top.
pub fn layout(
    tree: &Tree,
    focus: NodeId,
    mode: SizeMode,
    bounds: Rect,
    options: &TreemapOptions,
) -> Vec<TreemapItem> {
    let mut out = Vec::new();
    layout_dir(tree, focus, mode, bounds, 0, None, options, &mut out);
    // A folder with a single child would come out in one color; group by
    // the grandchildren instead. Items are in depth-first order.
    if out.iter().all(|i| i.group == 0) && out.iter().any(|i| i.depth == 1) {
        let mut next = 0u16;
        let mut current = 0u16;
        for item in &mut out {
            if item.depth == 1 {
                current = next;
                next = next.saturating_add(1);
            }
            item.group = if item.depth == 0 { 0 } else { current };
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn layout_dir(
    tree: &Tree,
    dir: NodeId,
    mode: SizeMode,
    bounds: Rect,
    depth: u8,
    group: Option<u16>,
    options: &TreemapOptions,
    out: &mut Vec<TreemapItem>,
) {
    let children: Vec<NodeId> = tree
        .children_sorted(dir, SortKey::Size(mode))
        .into_iter()
        .take_while(|&c| tree.node(c).size_in(mode) > 0)
        .collect();
    let sizes: Vec<f64> = children
        .iter()
        .map(|&c| tree.node(c).size_in(mode) as f64)
        .collect();
    let rects = squarify(&sizes, bounds);

    for (i, (&child, rect)) in children.iter().zip(rects).enumerate() {
        if out.len() >= options.max_items {
            return;
        }
        if rect.w < options.min_side || rect.h < options.min_side {
            continue;
        }
        let group = group.unwrap_or(i.min(u16::MAX as usize) as u16);
        let node = tree.node(child);
        let inner = Rect::new(
            rect.x + options.padding,
            rect.y + options.header,
            rect.w - 2.0 * options.padding,
            rect.h - options.header - options.padding,
        );
        let nested = node.is_dir()
            && node.child_count > 0
            && depth + 1 < options.max_depth
            && inner.w >= 4.0 * options.min_side
            && inner.h >= 4.0 * options.min_side;
        out.push(TreemapItem {
            node: child,
            rect,
            depth,
            group,
            nested,
        });
        if nested {
            layout_dir(
                tree,
                child,
                mode,
                inner,
                depth + 1,
                Some(group),
                options,
                out,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlaps(a: &Rect, b: &Rect) -> bool {
        let eps = 1e-3;
        a.x + eps < b.x + b.w
            && b.x + eps < a.x + a.w
            && a.y + eps < b.y + b.h
            && b.y + eps < a.y + a.h
    }

    #[test]
    fn squarify_preserves_proportions_and_bounds() {
        let values = [600.0, 300.0, 200.0, 100.0, 60.0, 30.0, 6.0, 4.0];
        let bounds = Rect::new(10.0, 20.0, 600.0, 400.0);
        let rects = squarify(&values, bounds);
        let total: f64 = values.iter().sum();

        for (v, r) in values.iter().zip(&rects) {
            let expected = (v / total) as f32 * bounds.area();
            assert!(
                (r.area() - expected).abs() < expected * 0.01 + 0.5,
                "{r:?} vs {expected}"
            );
            assert!(r.x >= bounds.x - 1e-3 && r.y >= bounds.y - 1e-3);
            assert!(r.x + r.w <= bounds.x + bounds.w + 1e-2);
            assert!(r.y + r.h <= bounds.y + bounds.h + 1e-2);
        }
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                assert!(!overlaps(&rects[i], &rects[j]), "{i} and {j} overlap");
            }
        }
        let covered: f32 = rects.iter().map(Rect::area).sum();
        assert!((covered - bounds.area()).abs() < 1.0);
    }

    #[test]
    fn squarify_keeps_rects_reasonably_square() {
        let values = vec![1.0; 16];
        let rects = squarify(&values, Rect::new(0.0, 0.0, 400.0, 400.0));
        for r in rects {
            let ratio = r.w.max(r.h) / r.w.min(r.h);
            assert!(ratio < 2.0, "{r:?}");
        }
    }

    #[test]
    fn squarify_handles_zeros_and_empty_input() {
        assert!(squarify(&[], Rect::new(0.0, 0.0, 10.0, 10.0)).is_empty());
        let rects = squarify(&[5.0, 0.0], Rect::new(0.0, 0.0, 10.0, 10.0));
        assert!((rects[0].area() - 100.0).abs() < 1e-3);
        assert_eq!(rects[1].area(), 0.0);
        let rects = squarify(&[0.0, 0.0], Rect::new(0.0, 0.0, 10.0, 10.0));
        assert!(rects.iter().all(|r| r.area() == 0.0));
    }
}
