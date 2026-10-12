//! Bubble chart layout: one circle per item, its area proportional to the
//! item's size, packed tightly with the biggest in the middle.
//!
//! Packing uses the front-chain method of Wang et al., "Visualization of
//! large hierarchical data by circle packing" (CHI 2006), in the form
//! d3-hierarchy uses: each new circle is placed tangent to two neighbours on
//! the outer chain, next to the pair closest to the centre.

use crate::tree::{NodeId, SizeMode, SortKey, Tree};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Circle {
    pub x: f32,
    pub y: f32,
    pub r: f32,
}

#[derive(Clone, Copy, Debug)]
struct C {
    x: f64,
    y: f64,
    r: f64,
}

/// Places circles of the given radii, largest first works best, so none
/// overlap and they sit tightly around the origin. Returns their centres in
/// input order.
pub fn pack(radii: &[f64]) -> Vec<(f64, f64)> {
    let n = radii.len();
    let mut c: Vec<C> = radii.iter().map(|&r| C { x: 0.0, y: 0.0, r }).collect();
    if n >= 2 {
        c[0].x = -c[1].r;
        c[1].x = c[0].r;
    }
    if n >= 3 {
        let (x, y) = place(c[1], c[0], c[2].r);
        c[2].x = x;
        c[2].y = y;
        // The front chain: the circles on the outside, as a ring.
        let mut next = vec![0usize; n];
        let mut prev = vec![0usize; n];
        let (mut a, mut b) = (0usize, 1usize);
        next[0] = 1;
        prev[2] = 1;
        next[1] = 2;
        prev[0] = 2;
        next[2] = 0;
        prev[1] = 0;
        let mut i = 3;
        'pack: while i < n {
            let (x, y) = place(c[a], c[b], c[i].r);
            c[i].x = x;
            c[i].y = y;
            // The nearest circle on the chain that the new one overlaps, if
            // any, measured along the chain in both directions.
            let (mut j, mut k) = (next[b], prev[a]);
            let (mut sj, mut sk) = (c[b].r, c[a].r);
            loop {
                if sj <= sk {
                    if intersects(c[j], c[i]) {
                        b = j;
                        next[a] = b;
                        prev[b] = a;
                        continue 'pack;
                    }
                    sj += c[j].r;
                    j = next[j];
                } else {
                    if intersects(c[k], c[i]) {
                        a = k;
                        next[a] = b;
                        prev[b] = a;
                        continue 'pack;
                    }
                    sk += c[k].r;
                    k = prev[k];
                }
                if j == next[k] {
                    break;
                }
            }
            // Fits: insert it between a and b.
            prev[i] = a;
            next[i] = b;
            next[a] = i;
            prev[b] = i;
            b = i;
            // Continue from the pair closest to the centre.
            let mut best = score(&c, &next, a);
            let mut node = next[i];
            while node != b {
                let s = score(&c, &next, node);
                if s < best {
                    a = node;
                    best = s;
                }
                node = next[node];
            }
            b = next[a];
            i += 1;
        }
    }
    c.iter().map(|c| (c.x, c.y)).collect()
}

/// Where a circle of radius `r` touches both `b` and `a`.
fn place(b: C, a: C, r: f64) -> (f64, f64) {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let d2 = dx * dx + dy * dy;
    if d2 == 0.0 {
        return (a.x + r, a.y);
    }
    let a2 = (a.r + r).powi(2);
    let b2 = (b.r + r).powi(2);
    if a2 > b2 {
        let x = (d2 + b2 - a2) / (2.0 * d2);
        let y = (b2 / d2 - x * x).max(0.0).sqrt();
        (b.x - x * dx - y * dy, b.y - x * dy + y * dx)
    } else {
        let x = (d2 + a2 - b2) / (2.0 * d2);
        let y = (a2 / d2 - x * x).max(0.0).sqrt();
        (a.x + x * dx - y * dy, a.y + x * dy + y * dx)
    }
}

fn intersects(a: C, b: C) -> bool {
    let dr = a.r + b.r - 1e-6;
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    dr > 0.0 && dr * dr > dx * dx + dy * dy
}

/// Squared distance from the origin to where `node` and the next circle on
/// the chain touch.
fn score(c: &[C], next: &[usize], node: usize) -> f64 {
    let (a, b) = (c[node], c[next[node]]);
    let ab = a.r + b.r;
    let dx = (a.x * b.r + b.x * a.r) / ab;
    let dy = (a.y * b.r + b.y * a.r) / ab;
    dx * dx + dy * dy
}

/// A circle around all of `circles`, close to the smallest one: starts at
/// their bounding box's centre and walks towards the farthest circle in
/// shrinking steps (Bădoiu and Clarkson).
fn enclose(circles: &[C]) -> C {
    let farthest = |x: f64, y: f64| {
        circles
            .iter()
            .map(|c| ((c.x - x).hypot(c.y - y) + c.r, c))
            .fold(
                (0.0f64, None),
                |best, (d, c)| {
                    if d > best.0 { (d, Some(c)) } else { best }
                },
            )
    };
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for c in circles {
        x0 = x0.min(c.x - c.r);
        y0 = y0.min(c.y - c.r);
        x1 = x1.max(c.x + c.r);
        y1 = y1.max(c.y + c.r);
    }
    let (mut x, mut y) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    for step in 1..=200 {
        let (d, Some(far)) = farthest(x, y) else {
            break;
        };
        // The point of the farthest circle farthest from the centre.
        let to = (far.x - x).hypot(far.y - y);
        let (px, py) = if to > 0.0 {
            (
                far.x + (far.x - x) / to * far.r,
                far.y + (far.y - y) / to * far.r,
            )
        } else {
            (far.x + far.r, far.y)
        };
        let t = 1.0 / (step as f64 + 1.0);
        let (nx, ny) = (x + (px - x) * t, y + (py - y) * t);
        // Keep the step only if it helps.
        if farthest(nx, ny).0 < d {
            x = nx;
            y = ny;
        }
    }
    C {
        x,
        y,
        r: farthest(x, y).0,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BubbleOptions {
    /// Most bubbles drawn for the focused folder; the smallest items beyond
    /// this share one "other" bubble.
    pub max_items: usize,
    /// Items smaller than this share of the folder join the "other" bubble.
    pub min_share: f64,
    /// Folders drawn with at least this radius show their contents inside.
    pub nested_min_radius: f32,
    /// Most bubbles drawn inside a folder's bubble.
    pub max_nested: usize,
    /// Space between the view's edge and the bubbles, and between a bubble
    /// and what's drawn inside it (as a share of its radius).
    pub margin: f32,
    pub inner_margin: f32,
}

impl Default for BubbleOptions {
    fn default() -> Self {
        Self {
            max_items: 40,
            min_share: 0.004,
            nested_min_radius: 44.0,
            max_nested: 16,
            margin: 6.0,
            inner_margin: 0.12,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bubble {
    /// The item, or `None` for the bubble standing for the smaller items.
    pub node: Option<NodeId>,
    pub circle: Circle,
    /// 0 for the focused folder's items, 1 for items drawn inside them.
    pub depth: u8,
    /// Index of the top-level bubble it belongs to, for coloring.
    pub group: u16,
    /// Bytes.
    pub size: u64,
    /// How many items the "other" bubble stands for; 1 otherwise.
    pub count: u32,
}

/// Lays out the contents of `focus` in a `width` × `height` view. Each
/// top-level bubble is followed by the bubbles drawn inside it.
pub fn layout(
    tree: &Tree,
    focus: NodeId,
    mode: SizeMode,
    width: f32,
    height: f32,
    options: &BubbleOptions,
) -> Vec<Bubble> {
    let mut out = Vec::new();
    let sizes = biggest(tree, focus, mode, options.max_items, options.min_share);
    let circles = fit(
        &sizes.iter().map(|&(_, size, _)| size).collect::<Vec<_>>(),
        width,
        height,
        options.margin,
    );

    for (group, (&(node, size, count), &circle)) in sizes.iter().zip(&circles).enumerate() {
        let group = group.min(u16::MAX as usize) as u16;
        out.push(Bubble {
            node,
            circle,
            depth: 0,
            group,
            size,
            count,
        });
        let Some(node) = node else {
            continue;
        };
        if circle.r < options.nested_min_radius || !tree.node(node).is_dir() {
            continue;
        }
        let inner_sizes = biggest(tree, node, mode, options.max_nested, options.min_share);
        // A folder holding one thing would just draw a second circle.
        if inner_sizes.len() < 2 {
            continue;
        }
        let Some(inner_placed) = pack_sizes(&inner_sizes) else {
            continue;
        };
        let around = enclose(&inner_placed);
        let fit = f64::from(circle.r * (1.0 - options.inner_margin)) / around.r;
        for (&(node, size, count), c) in inner_sizes.iter().zip(&inner_placed) {
            out.push(Bubble {
                node,
                circle: Circle {
                    x: circle.x + ((c.x - around.x) * fit) as f32,
                    y: circle.y + ((c.y - around.y) * fit) as f32,
                    r: (c.r * fit) as f32,
                },
                depth: 1,
                group,
                size,
                count,
            });
        }
    }
    out
}

/// One bubble to draw: the item (`None` for "other"), its size and how many
/// items it stands for.
type Entry = (Option<NodeId>, u64, u32);

/// Circles whose areas follow `sizes`, packed and fitted into a `width` ×
/// `height` view with `margin` around them, in input order.
pub fn fit(sizes: &[u64], width: f32, height: f32, margin: f32) -> Vec<Circle> {
    let entries: Vec<Entry> = sizes.iter().map(|&size| (None, size, 1)).collect();
    let Some(placed) = pack_sizes(&entries) else {
        return Vec::new();
    };
    // Fit the packed group's bounding box into the view.
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for c in &placed {
        x0 = x0.min(c.x - c.r);
        y0 = y0.min(c.y - c.r);
        x1 = x1.max(c.x + c.r);
        y1 = y1.max(c.y + c.r);
    }
    let avail_w = f64::from((width - 2.0 * margin).max(1.0));
    let avail_h = f64::from((height - 2.0 * margin).max(1.0));
    let scale = (avail_w / (x1 - x0)).min(avail_h / (y1 - y0));
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    placed
        .iter()
        .map(|c| Circle {
            x: (f64::from(width) / 2.0 + (c.x - cx) * scale) as f32,
            y: (f64::from(height) / 2.0 + (c.y - cy) * scale) as f32,
            r: (c.r * scale) as f32,
        })
        .collect()
}

/// The biggest children of `dir`, then one "other" entry for the rest.
fn biggest(tree: &Tree, dir: NodeId, mode: SizeMode, max: usize, min_share: f64) -> Vec<Entry> {
    let children: Vec<(NodeId, u64)> = tree
        .children_sorted(dir, SortKey::Size(mode))
        .into_iter()
        .map(|c| (c, tree.node(c).size_in(mode)))
        .take_while(|&(_, size)| size > 0)
        .collect();
    let total: u64 = children.iter().map(|&(_, size)| size).sum();
    let mut keep = children
        .iter()
        .take(max.max(1))
        .take_while(|&&(_, size)| size as f64 >= total as f64 * min_share)
        .count()
        .max(children.len().min(1));
    // One leftover is drawn as itself.
    if children.len() == keep + 1 {
        keep += 1;
    }
    let rest = &children[keep..];
    let mut out: Vec<Entry> = children[..keep]
        .iter()
        .map(|&(id, size)| (Some(id), size, 1))
        .collect();
    if !rest.is_empty() {
        out.push((
            None,
            rest.iter().map(|&(_, size)| size).sum(),
            rest.len().min(u32::MAX as usize) as u32,
        ));
    }
    out
}

/// Packs circles whose areas follow `sizes`.
fn pack_sizes(sizes: &[Entry]) -> Option<Vec<C>> {
    if sizes.is_empty() {
        return None;
    }
    let radii: Vec<f64> = sizes
        .iter()
        .map(|&(_, size, _)| (size as f64).sqrt().max(1e-9))
        .collect();
    let centres = pack(&radii);
    Some(
        centres
            .into_iter()
            .zip(radii)
            .map(|((x, y), r)| C { x, y, r })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{ScanOptions, ScanProgress, scan};

    fn overlaps(a: (f64, f64, f64), b: (f64, f64, f64)) -> bool {
        (a.0 - b.0).hypot(a.1 - b.1) < a.2 + b.2 - 1e-6
    }

    #[test]
    fn packed_circles_never_overlap() {
        for n in [1, 2, 3, 5, 12, 40] {
            let radii: Vec<f64> = (0..n).map(|i| 50.0 / (1.0 + i as f64).sqrt()).collect();
            let centres = pack(&radii);
            let circles: Vec<_> = centres
                .iter()
                .zip(&radii)
                .map(|(&(x, y), &r)| (x, y, r))
                .collect();
            for i in 0..n {
                for j in i + 1..n {
                    assert!(!overlaps(circles[i], circles[j]), "{n}: {i} and {j}");
                }
            }
            // Tight: the bounding box isn't much bigger than the circles.
            let area: f64 = radii.iter().map(|r| std::f64::consts::PI * r * r).sum();
            let (x0, x1) = circles.iter().fold((f64::MAX, f64::MIN), |(a, b), c| {
                (a.min(c.0 - c.2), b.max(c.0 + c.2))
            });
            let (y0, y1) = circles.iter().fold((f64::MAX, f64::MIN), |(a, b), c| {
                (a.min(c.1 - c.2), b.max(c.1 + c.2))
            });
            assert!(
                (x1 - x0) * (y1 - y0) < area * 2.6,
                "{n} circles packed loosely"
            );
        }
    }

    #[test]
    fn enclosing_circle_holds_everything() {
        let radii = [30.0, 20.0, 20.0, 10.0, 5.0, 5.0, 3.0];
        let circles: Vec<C> = pack(&radii)
            .into_iter()
            .zip(radii)
            .map(|((x, y), r)| C { x, y, r })
            .collect();
        let around = enclose(&circles);
        for c in &circles {
            assert!((c.x - around.x).hypot(c.y - around.y) + c.r <= around.r + 1e-6);
        }
        // Not much bigger than needed: the two biggest circles alone need 50.
        assert!(around.r < 70.0, "{}", around.r);
    }

    #[test]
    fn layout_fits_the_view_and_folds_small_items() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |path: &str, bytes: usize| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![1u8; bytes]).unwrap();
        };
        write("games/a.pak", 400_000);
        write("games/b.pak", 300_000);
        write("games/c.pak", 200_000);
        write("videos/a.mp4", 300_000);
        for i in 0..30 {
            write(&format!("tiny/{i}.txt"), 10);
            write(&format!("small{i}.txt"), 100);
        }
        let tree = scan(root, &ScanOptions::default(), &ScanProgress::new())
            .unwrap()
            .tree;
        let options = BubbleOptions {
            nested_min_radius: 40.0,
            ..BubbleOptions::default()
        };
        let bubbles = layout(
            &tree,
            tree.root(),
            SizeMode::Logical,
            400.0,
            300.0,
            &options,
        );
        let top: Vec<&Bubble> = bubbles.iter().filter(|b| b.depth == 0).collect();
        // games, videos, then 31 small items folded into one bubble.
        assert_eq!(top.len(), 3);
        let other = top.iter().find(|b| b.node.is_none()).unwrap();
        assert_eq!(other.count, 31);
        assert_eq!(other.size, 30 * 100 + 30 * 10);
        for b in &top {
            let c = b.circle;
            assert!(c.x - c.r >= 5.9 && c.x + c.r <= 394.1, "{c:?}");
            assert!(c.y - c.r >= 5.9 && c.y + c.r <= 294.1, "{c:?}");
        }
        // The biggest folder shows its three files inside it.
        let games = tree.child_by_name(tree.root(), "games".as_ref()).unwrap();
        let parent = bubbles
            .iter()
            .find(|b| b.node == Some(games))
            .unwrap()
            .circle;
        let inner: Vec<&Bubble> = bubbles
            .iter()
            .filter(|b| b.depth == 1 && b.group == 0)
            .collect();
        assert_eq!(inner.len(), 3);
        for b in inner {
            let c = b.circle;
            assert!((c.x - parent.x).hypot(c.y - parent.y) + c.r <= parent.r + 0.01);
        }
        // Areas follow sizes.
        let videos = tree.child_by_name(tree.root(), "videos".as_ref()).unwrap();
        let v = bubbles
            .iter()
            .find(|b| b.node == Some(videos))
            .unwrap()
            .circle;
        let ratio = (parent.r / v.r).powi(2);
        assert!((ratio - 3.0).abs() < 0.05, "{ratio}");
    }
}
