//! Table Layout › View Gridlines, as Word shows them: a light dashed line along each table cell
//! edge that has no border, so a borderless table can still be seen and edited. Edges with a
//! border show the border alone. On screen only, never printed or exported.

use std::collections::BTreeMap;

use wordcraft_layout::{Page, Placed};

/// Shortest bit of edge drawn, points: what a border stops short of is not a gap.
const MIN_PIECE: f32 = 0.5;
/// How far apart, points, a border and a cell edge can lie and still be the same line.
const SAME_LINE: f32 = 0.1;

/// One gridline, in page points: from `(x0, y0)` to `(x1, y1)`, horizontal or vertical.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gridline {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// Lines keyed by their position across (a y for horizontal ones, an x for vertical ones),
/// quantized so the same edge from two neighbouring cells lands on one key.
type Lines = BTreeMap<i64, (f32, Vec<(f32, f32)>)>;

fn key(v: f32) -> i64 {
    (v / SAME_LINE).round() as i64
}

fn push(lines: &mut Lines, at: f32, a: f32, b: f32) {
    if !(at.is_finite() && a.is_finite() && b.is_finite()) {
        return;
    }
    lines.entry(key(at)).or_insert((at, Vec::new())).1.push((a.min(b), a.max(b)));
}

/// `spans` sorted and joined where they touch or overlap.
fn union(mut spans: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f32, f32)> = Vec::with_capacity(spans.len());
    for (a, b) in spans {
        match out.last_mut() {
            Some(last) if a <= last.1 + 0.01 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// What is left of `edges` once `borders` (both joined) are taken out.
fn minus(edges: &[(f32, f32)], borders: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    for &(a, b) in edges {
        let mut from = a;
        for &(c, d) in borders {
            if d <= from || c >= b {
                continue;
            }
            if c > from {
                out.push((from, c));
            }
            from = from.max(d);
        }
        if from < b {
            out.push((from, b));
        }
    }
    out.retain(|(a, b)| b - a >= MIN_PIECE);
    out
}

/// The borders lying along the line at `at`: those keyed there or one key either side.
fn borders_at(borders: &Lines, at: f32) -> Vec<(f32, f32)> {
    let k = key(at);
    union(
        borders
            .range(k.saturating_sub(1)..=k.saturating_add(1))
            .filter(|(_, (pos, _))| (pos - at).abs() <= SAME_LINE)
            .flat_map(|(_, (_, spans))| spans.iter().copied())
            .collect(),
    )
}

/// The gridlines of a page's tables (the body's, the header's and the footer's): every edge of a
/// cell not covered by a merge, each shared edge once, less the stretches a border is drawn on.
pub fn gridlines(page: &Page) -> Vec<Gridline> {
    let (mut across, mut down) = (Lines::new(), Lines::new());
    let (mut rules_across, mut rules_down) = (Lines::new(), Lines::new());
    for it in page.items.iter().chain(&page.header).chain(&page.footer) {
        match it {
            Placed::Cell { rect, covered: false, .. } => {
                push(&mut across, rect.y, rect.x, rect.right());
                push(&mut across, rect.bottom(), rect.x, rect.right());
                push(&mut down, rect.x, rect.y, rect.bottom());
                push(&mut down, rect.right(), rect.y, rect.bottom());
            }
            Placed::Rule { x0, y0, x1, y1, .. } if (y1 - y0).abs() < 0.01 => push(&mut rules_across, *y0, *x0, *x1),
            Placed::Rule { x0, y0, x1, y1, .. } if (x1 - x0).abs() < 0.01 => push(&mut rules_down, *x0, *y0, *y1),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for (_, (y, spans)) in across {
        for (a, b) in minus(&union(spans), &borders_at(&rules_across, y)) {
            out.push(Gridline { x0: a, y0: y, x1: b, y1: y });
        }
    }
    for (_, (x, spans)) in down {
        for (a, b) in minus(&union(spans), &borders_at(&rules_down, x)) {
            out.push(Gridline { x0: x, y0: a, x1: x, y1: b });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wordcraft_doc::StoryRef;
    use wordcraft_doc::props::Border;

    fn cell(x: f32, y: f32, w: f32, h: f32, covered: bool) -> Placed {
        Placed::Cell {
            rect: wordcraft_geom::Rect::new(x, y, w, h),
            covered,
            table: Default::default(),
            row: 0,
            cell: 0,
            story: StoryRef::Body,
            clip: false,
        }
    }

    fn rule(x0: f32, y0: f32, x1: f32, y1: f32) -> Placed {
        Placed::Rule { x0, y0, x1, y1, border: Border::default() }
    }

    /// Two cells side by side, 100 x 20 each, from (0, 0).
    fn two_cells() -> Vec<Placed> {
        vec![cell(0.0, 0.0, 100.0, 20.0, false), cell(100.0, 0.0, 100.0, 20.0, false)]
    }

    #[test]
    fn a_borderless_table_shows_each_edge_once() {
        let page = Page { items: two_cells(), ..Default::default() };
        let g = gridlines(&page);
        // Top and bottom run the table's width; three verticals, the shared one once.
        assert_eq!(g.len(), 5, "{g:?}");
        assert!(g.contains(&Gridline { x0: 0.0, y0: 0.0, x1: 200.0, y1: 0.0 }));
        assert!(g.contains(&Gridline { x0: 100.0, y0: 0.0, x1: 100.0, y1: 20.0 }));
    }

    #[test]
    fn edges_with_a_border_show_no_gridline() {
        // Word's look for a "rules only" table: borders under each row, none down the sides.
        let mut items = two_cells();
        items.push(rule(-0.25, 20.0, 200.25, 20.0));
        let page = Page { items, ..Default::default() };
        let g = gridlines(&page);
        assert!(g.iter().all(|l| !(l.y0 == 20.0 && l.y1 == 20.0)), "bottom has a border: {g:?}");
        assert!(g.contains(&Gridline { x0: 0.0, y0: 0.0, x1: 200.0, y1: 0.0 }), "top has none");
        assert_eq!(g.iter().filter(|l| l.x0 == l.x1).count(), 3, "the sides have none");
    }

    #[test]
    fn a_border_on_part_of_an_edge_leaves_the_rest() {
        let mut items = two_cells();
        // Only the first cell's top has a border.
        items.push(rule(0.0, 0.0, 100.0, 0.0));
        let page = Page { items, ..Default::default() };
        let g = gridlines(&page);
        assert!(g.contains(&Gridline { x0: 100.0, y0: 0.0, x1: 200.0, y1: 0.0 }), "{g:?}");
        assert!(!g.iter().any(|l| l.y0 == 0.0 && l.y1 == 0.0 && l.x0 < 100.0));
    }

    #[test]
    fn merged_continuations_header_and_footer_tables() {
        let page = Page {
            items: vec![cell(0.0, 100.0, 10.0, 10.0, false), cell(0.0, 110.0, 10.0, 10.0, true)],
            header: vec![cell(0.0, 10.0, 10.0, 10.0, false)],
            footer: vec![cell(0.0, 200.0, 10.0, 10.0, false)],
            ..Default::default()
        };
        let mut ys: Vec<f32> = gridlines(&page).iter().filter(|l| l.y0 == l.y1).map(|l| l.y0).collect();
        ys.sort_by(f32::total_cmp);
        // The covered cell adds no edge of its own (#1628: header and footer tables count).
        assert_eq!(ys, [10.0, 20.0, 100.0, 110.0, 200.0, 210.0]);
    }

    #[test]
    fn hostile_coordinates_are_skipped() {
        let page = Page { items: vec![cell(f32::NAN, 0.0, 10.0, 10.0, false), rule(f32::INFINITY, 0.0, 1.0, 0.0)], ..Default::default() };
        let g = gridlines(&page);
        assert!(g.iter().all(|l| l.x0.is_finite() && l.y0.is_finite() && l.x1.is_finite() && l.y1.is_finite()), "{g:?}");
    }
}
