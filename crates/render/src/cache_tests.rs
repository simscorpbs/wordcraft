//! Decoded-media cache: hits need the same allocation, and the byte budget bounds what is kept.

use super::*;

#[test]
fn media_cache_only_hits_the_same_allocation() {
    let mut c: MediaCache<u32> = MediaCache { map: HashMap::new(), bytes: 0 };
    let a = Arc::new(vec![1u8, 2, 3]);
    let b = Arc::new(vec![1u8, 2, 3]);
    c.insert("pic", &a, 7, 3);
    assert_eq!(c.get("pic", &a), Some(7));
    assert_eq!(c.get("pic", &b), None, "equal bytes in another allocation are another picture");
}

#[test]
fn media_cache_is_bounded_by_bytes() {
    let mut c: MediaCache<u32> = MediaCache { map: HashMap::new(), bytes: 0 };
    let src = Arc::new(vec![0u8]);
    let big = CACHE_BYTES / 2 + 1;
    c.insert("a", &src, 1, big);
    c.insert("b", &src, 2, big);
    assert_eq!(c.get("a", &src), None, "the first entry is evicted to make room");
    assert_eq!(c.get("b", &src), Some(2));
    assert!(c.bytes <= CACHE_BYTES);
    c.insert("huge", &src, 3, CACHE_BYTES + 1);
    assert_eq!(c.get("huge", &src), None, "a value over the whole budget is not kept");
}

#[test]
fn media_cache_counts_the_source_bytes_it_holds() {
    let mut c: MediaCache<Option<u32>> = MediaCache { map: HashMap::new(), bytes: 0 };
    let src = Arc::new(vec![0u8; 1000]);
    c.insert("broken", &src, None, 0);
    assert_eq!(c.bytes, 1000, "an entry that decoded to nothing still holds its source");
    c.insert("pic", &src, Some(1), 24);
    assert_eq!(c.bytes, 2024);
    // A source over the whole budget is not kept, whatever it decoded to (calloc'd, so cheap).
    let huge = Arc::new(vec![0u8; CACHE_BYTES + 1]);
    c.insert("huge", &huge, None, 0);
    assert!(!c.map.contains_key("huge"));
    assert_eq!(CACHE_BYTES * 2, 256 * 1024 * 1024, "the image and vector caches share about 256 MB");
}

/// A wave stroke starting around 1e20 never advanced (`x + step == x`), pushing curves until memory ran out.
#[test]
fn wave_stroke_at_a_huge_coordinate_ends() {
    assert!(wave_path(1e20, 0.0, 2e20, 0.0, 1.0).elements().len() < 3);
    assert!(wave_path(0.0, 0.0, 1e20, 0.0, 1.0).elements().len() < 20_100);
    assert!(wave_path(0.0, 0.0, 100.0, 0.0, 1.0).elements().len() > 10);
    assert!(wave_path(0.0, 0.0, 0.0, 100.0, 1.0).elements().len() > 10, "vertical wave strokes produce elements");
}

/// A freeform's joined subpaths (one DrawingML `a:path`) are filled even-odd: an outline inside
/// another is a hole even when both run the same way, as a letter's counter in a vector logo.
#[test]
fn even_odd_paths_leave_inner_outlines_empty() {
    use wordcraft_doc::graphic::PathSeg;
    let square = |a: f32, b: f32| vec![PathSeg::Move(a, a), PathSeg::Line(b, a), PathSeg::Line(b, b), PathSeg::Line(a, b), PathSeg::Close];
    let segs: Vec<PathSeg> = square(0.0, 40.0).into_iter().chain(square(10.0, 30.0)).collect();
    let alpha_at = |even_odd: bool, x: usize, y: usize| {
        let p = Draw::Path {
            segs: segs.clone(),
            fill: Some(Rgb(0x20, 0x2E, 0x55)),
            stroke: None,
            stroke_width: 0.0,
            even_odd,
            fill_alpha: 1.0,
            dash: Vec::new(),
        };
        let r = render_items(&Document::new(), &[p], 0.0, 0.0, 40.0, 40.0, 1.0, &RenderOptions::default());
        r.pixels.get((y * r.width as usize + x) * 4 + 3).copied()
    };
    assert_eq!(alpha_at(true, 20, 20), Some(0), "the inner square is a hole");
    assert_eq!(alpha_at(true, 5, 20), Some(255), "the ring between them is filled");
    assert_eq!(alpha_at(false, 20, 20), Some(255), "non-zero fills the inner square");
}

/// A shape's fill transparency: a 30%-opaque fill lets the page show through.
#[test]
fn a_translucent_shape_fill_is_drawn_see_through() {
    let shape = |fill_alpha: f32| Draw::Shape {
        rect: wordcraft_geom::Rect::new(0.0, 0.0, 20.0, 20.0),
        kind: ShapeKind::Rectangle,
        fill: Some(Rgb(0xF5, 0xF8, 0xFF)),
        stroke: None,
        stroke_width: 0.0,
        effects: Default::default(),
        corner: None,
        fill_alpha,
        dash: Vec::new(),
    };
    let alpha = |d: Draw| {
        let r = render_items(&Document::new(), &[d], 0.0, 0.0, 20.0, 20.0, 1.0, &RenderOptions::default());
        r.pixels.get((10 * r.width as usize + 10) * 4 + 3).copied().unwrap_or(0)
    };
    assert!((alpha(shape(0.3)) as i32 - 77).abs() <= 1, "{}", alpha(shape(0.3)));
    assert_eq!(alpha(shape(1.0)), 255);
}

/// A faded shape (in a dimmed header) is one layer: its outline over its fill doesn't add up.
#[test]
fn a_faded_shape_is_drawn_as_one_layer() {
    let shape = Draw::Shape {
        rect: wordcraft_geom::Rect::new(0.0, 0.0, 20.0, 20.0),
        kind: ShapeKind::Rectangle,
        fill: Some(Rgb::BLACK),
        stroke: Some(Rgb::BLACK),
        stroke_width: 4.0,
        effects: Default::default(),
        corner: None,
        fill_alpha: 1.0,
        dash: Vec::new(),
    };
    let faded = Draw::Faded { alpha: 0.573, items: vec![shape] };
    let r = render_items(&Document::new(), &[faded], 0.0, 0.0, 20.0, 20.0, 1.0, &RenderOptions::default());
    let at = |x: usize, y: usize| r.pixels.get((y * r.width as usize + x) * 4 + 3).copied().unwrap_or(0) as i32;
    assert!((at(10, 10) - 146).abs() <= 1, "{}", at(10, 10));
    assert_eq!(at(1, 10), at(10, 10), "the outline over the fill is as faded as the fill");
}
