//! Where a panel's header, tabs and content go, and what a point in one means.
//!
//! The geometry half of drawing the workspace, kept apart from the drawing so it can be tested
//! without a GPU — the same split that made [`crate::crop`] and [`crate::transform_box`] provable.
//! Painting is then a transcription of these rectangles with nothing to get wrong but colours.
//!
//! It is also the piece that turns a pointer into a [`crate::panel_drag::Target`]. That module
//! deliberately does not hit-test tabs itself, because tab widths depend on measured text and text
//! measurement belongs to whatever is drawing. So the measurement arrives as a function and the
//! arithmetic stays here.
//!
//! # Compact headers are a style, not an exception
//!
//! A tool rail has nothing worth captioning, but it still has to be grabbable — §1c is emphatic
//! that the tool rail is a panel like any other. So a header can render compact: a short grab bar
//! instead of a row of names. Any panel may use it; nothing here knows which ones do, and the
//! choice arrives as a parameter rather than being decided by looking at the panel.

use crate::layout::{Placed, Rect};
use crate::panel_drag::Target;
use crate::theme::Metrics;

/// How a panel's header is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderStyle {
    /// Names, one tab per panel.
    Named,
    /// A short grab bar and no names, for a panel whose contents speak for themselves.
    Compact,
}

/// One tab in a header.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    /// Index into the leaf's panels.
    pub index: usize,
    pub rect: Rect,
    pub active: bool,
}

/// A panel's parts, ready to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelChrome {
    /// The whole panel, inset from its layout slot by the gutter.
    pub outer: Rect,
    /// The header strip.
    pub header: Rect,
    /// What is left for the panel to draw in.
    pub content: Rect,
    /// Empty when the header is compact.
    pub tabs: Vec<Tab>,
    pub style: HeaderStyle,
}

/// Lay out one panel slot.
///
/// `measure` gives the width of a tab's label; the caller owns text, so it owns measurement.
///
/// The gutter is taken out of the *slot*, so panels sit apart and the ground shows between them.
/// Doing it here rather than in [`crate::layout`] is deliberate: a layout that left gaps would
/// have to answer where the pointer is when it lands in one, and "nowhere" is not an answer a hit
/// test can act on.
#[must_use]
pub fn panel(
    placed: &Placed,
    metrics: &Metrics,
    style: HeaderStyle,
    mut measure: impl FnMut(usize) -> f32,
) -> PanelChrome {
    let half = metrics.gutter / 2.0;
    let outer = Rect::new(
        placed.rect.x + half,
        placed.rect.y + half,
        (placed.rect.w - metrics.gutter).max(0.0),
        (placed.rect.h - metrics.gutter).max(0.0),
    );

    let bar = match style {
        HeaderStyle::Named => metrics.header,
        HeaderStyle::Compact => metrics.header_compact,
    }
    // A panel dragged down to a sliver keeps a header and loses its content, rather than the other
    // way round: the header is the only part that can be grabbed to undo the drag by hand.
    .min(outer.h);

    let header = Rect::new(outer.x, outer.y, outer.w, bar);
    let content = Rect::new(outer.x, outer.y + bar, outer.w, (outer.h - bar).max(0.0));

    let mut tabs = Vec::new();
    if style == HeaderStyle::Named {
        let mut x = outer.x;
        for index in 0..placed.tabs.len() {
            let w = measure(index) + metrics.tab_padding * 2.0;
            // Tabs run off the end rather than shrinking. A tab narrower than its label is a tab
            // you cannot read *and* cannot aim at; overflowing at least leaves the ones you can
            // see usable, and scrolling them is a later refinement with somewhere to go.
            tabs.push(Tab {
                index,
                rect: Rect::new(x, header.y, w.min((outer.x + outer.w - x).max(0.0)), bar),
                active: index == placed.active,
            });
            x += w;
        }
    }

    PanelChrome {
        outer,
        header,
        content,
        tabs,
        style,
    }
}

/// What a press at a point means.
///
/// Dividers are tested **before** panels, because a divider's grab width is wider than the gutter
/// it sits in and therefore overlaps the panels either side. Testing panels first would make a
/// divider unreachable everywhere except the hairline it draws — which is the bug that makes
/// splitters in other apps feel like they need to be hunted for.
#[must_use]
pub fn target_at(
    placed: &[Placed],
    splitters: &[crate::layout::Splitter],
    metrics: &Metrics,
    style_of: impl Fn(&Placed) -> HeaderStyle,
    mut measure: impl FnMut(&Placed, usize) -> f32,
    x: f32,
    y: f32,
) -> Target {
    for s in splitters {
        if s.rect.contains(x, y) {
            return Target::Splitter {
                path: s.path.clone(),
                index: s.index,
            };
        }
    }
    for p in placed {
        if !p.rect.contains(x, y) {
            continue;
        }
        let chrome = panel(p, metrics, style_of(p), |i| measure(p, i));
        if !chrome.header.contains(x, y) {
            return Target::Elsewhere;
        }
        // A compact header has no tabs, but it is still a grab surface — pressing it takes the
        // panel it belongs to. That is what makes the tool rail movable.
        let tab = chrome
            .tabs
            .iter()
            .find(|t| t.rect.contains(x, y))
            .map_or(p.active, |t| t.index);
        return Target::Tab {
            path: p.path.clone(),
            tab,
        };
    }
    Target::Elsewhere
}

/// The five drop regions of a panel, for drawing the overlay while a panel is in the air.
///
/// **They are trapezoids, not rectangles, and that is not a decoration.** [`Layout::zone_at`]
/// decides the drop by *nearest edge*, which makes the boundary between two edge zones the
/// diagonal — a mitre, like a picture frame. Drawing side columns and top and bottom bars instead
/// would light the left region in a corner where the drop actually goes to the top.
///
/// A test caught exactly that, and it is worth naming what the bug would have been: **the app
/// showing one thing and doing another**, with no way for the artist to tell which to believe.
/// That is the worst class of UI defect, and the only defence is that one piece of arithmetic
/// answers both questions.
///
/// The corner of the mitre sits where the two normalised distances are equal, which in pixels is
/// `EDGE_BAND` of the width across and `EDGE_BAND` of the height down — so a wide panel gets a
/// shallow diagonal and a tall one a steep one, matching what `zone_at` does.
#[must_use]
pub fn drop_zones(rect: Rect) -> [DropRegion; 5] {
    use crate::layout::{Zone, EDGE_BAND};
    let (x0, y0) = (rect.x, rect.y);
    let (x1, y1) = (rect.x + rect.w, rect.y + rect.h);
    let (bx, by) = (rect.w * EDGE_BAND, rect.h * EDGE_BAND);
    // The four inner corners, where the mitres meet.
    let (ix0, iy0) = (x0 + bx, y0 + by);
    let (ix1, iy1) = (x1 - bx, y1 - by);
    [
        DropRegion {
            zone: Zone::Left,
            points: [[x0, y0], [ix0, iy0], [ix0, iy1], [x0, y1]],
        },
        DropRegion {
            zone: Zone::Top,
            points: [[x0, y0], [x1, y0], [ix1, iy0], [ix0, iy0]],
        },
        DropRegion {
            zone: Zone::Right,
            points: [[x1, y0], [x1, y1], [ix1, iy1], [ix1, iy0]],
        },
        DropRegion {
            zone: Zone::Bottom,
            points: [[x0, y1], [ix0, iy1], [ix1, iy1], [x1, y1]],
        },
        DropRegion {
            zone: Zone::Center,
            points: [[ix0, iy0], [ix1, iy0], [ix1, iy1], [ix0, iy1]],
        },
    ]
}

/// One drop region: the zone it stands for, and the quad to fill.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DropRegion {
    pub zone: crate::layout::Zone,
    /// Four corners, in order around the shape.
    pub points: [[f32; 2]; 4],
}

impl DropRegion {
    /// Whether a point is inside the quad.
    ///
    /// By the sign of the cross product against each edge, which works for any convex shape and so
    /// does not care that four of the five are trapezoids and one is a rectangle.
    ///
    /// Read only by the test that holds these regions and `Layout::zone_at` to the same answer —
    /// the drop itself asks the layout, so there is exactly one decision at runtime.
    #[cfg(test)]
    #[must_use]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let mut positive = false;
        let mut negative = false;
        for i in 0..4 {
            let a = self.points[i];
            let b = self.points[(i + 1) % 4];
            let cross = (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
            if cross > 0.0 {
                positive = true;
            } else if cross < 0.0 {
                negative = true;
            }
        }
        !(positive && negative)
    }

    /// The area it covers, for checking the five tile the panel.
    #[cfg(test)]
    #[must_use]
    pub fn area(&self) -> f32 {
        let mut sum = 0.0;
        for i in 0..4 {
            let a = self.points[i];
            let b = self.points[(i + 1) % 4];
            sum += a[0].mul_add(b[1], -(b[0] * a[1]));
        }
        (sum / 2.0).abs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Layout, PanelId, Zone};
    use crate::theme::Theme;

    const CANVAS: PanelId = PanelId(1);
    const LAYERS: PanelId = PanelId(2);
    const HISTORY: PanelId = PanelId(3);

    fn area() -> Rect {
        Rect::new(0.0, 0.0, 1000.0, 800.0)
    }

    fn metrics() -> Metrics {
        Theme::default().metrics
    }

    fn workspace() -> Layout {
        let mut l = Layout::single(CANVAS);
        l.insert(&[], Zone::Right, LAYERS);
        l.insert(&[1], Zone::Center, HISTORY);
        l
    }

    /// Header plus content is the whole panel, with nothing lost between them.
    #[test]
    fn the_header_and_content_tile_the_panel() {
        let l = workspace();
        for placed in l.resolve(area()) {
            let c = panel(&placed, &metrics(), HeaderStyle::Named, |_| 40.0);
            assert!((c.header.y - c.outer.y).abs() < 0.001);
            assert!(
                (c.content.y - (c.header.y + c.header.h)).abs() < 0.001,
                "content must start where the header ends"
            );
            assert!(
                ((c.content.y + c.content.h) - (c.outer.y + c.outer.h)).abs() < 0.001,
                "and finish where the panel does"
            );
        }
    }

    /// The gutter comes out of the slot, so neighbours sit apart by a full gutter rather than half.
    #[test]
    fn neighbouring_panels_are_a_gutter_apart() {
        let l = workspace();
        let placed = l.resolve(area());
        let a = panel(&placed[0], &metrics(), HeaderStyle::Named, |_| 40.0);
        let b = panel(&placed[1], &metrics(), HeaderStyle::Named, |_| 40.0);
        let gap = b.outer.x - (a.outer.x + a.outer.w);
        assert!(
            (gap - metrics().gutter).abs() < 0.001,
            "expected a {} gap, got {gap}",
            metrics().gutter
        );
    }

    /// A panel squeezed to nothing keeps its header and loses its content — the header is the only
    /// part that can be grabbed to undo the squeeze by hand.
    #[test]
    fn a_squeezed_panel_keeps_its_header() {
        let placed = Placed {
            path: vec![],
            rect: Rect::new(0.0, 0.0, 200.0, 14.0),
            tabs: vec![LAYERS],
            active: 0,
        };
        let c = panel(&placed, &metrics(), HeaderStyle::Named, |_| 40.0);
        assert!(c.header.h > 0.0, "there is still something to grab");
        assert!(c.content.h >= 0.0, "and content never goes negative");
        assert!(
            (c.header.h + c.content.h - c.outer.h).abs() < 0.001,
            "and the two still sum to the panel"
        );
    }

    /// Tabs sit side by side, sized to their labels, in order.
    #[test]
    fn tabs_are_laid_out_in_order_and_sized_to_their_labels() {
        let l = workspace();
        let placed = l.resolve(area());
        let leaf = &placed[1];
        assert_eq!(leaf.tabs.len(), 2);

        let widths = [50.0_f32, 90.0];
        let c = panel(leaf, &metrics(), HeaderStyle::Named, |i| widths[i]);
        assert_eq!(c.tabs.len(), 2);
        assert!((c.tabs[0].rect.w - (50.0 + metrics().tab_padding * 2.0)).abs() < 0.001);
        assert!((c.tabs[1].rect.w - (90.0 + metrics().tab_padding * 2.0)).abs() < 0.001);
        assert!(
            (c.tabs[1].rect.x - (c.tabs[0].rect.x + c.tabs[0].rect.w)).abs() < 0.001,
            "tabs must meet, not overlap or gap"
        );
        assert!(c.tabs[1].active, "the shown panel is the marked tab");
    }

    /// A compact header has no tabs and takes less room, but is still a header.
    #[test]
    fn a_compact_header_is_shorter_and_nameless() {
        let l = Layout::single(CANVAS);
        let placed = l.resolve(area());
        let named = panel(&placed[0], &metrics(), HeaderStyle::Named, |_| 40.0);
        let compact = panel(&placed[0], &metrics(), HeaderStyle::Compact, |_| 40.0);

        assert!(compact.tabs.is_empty());
        assert!(compact.header.h < named.header.h);
        assert!(compact.header.h > 0.0, "it is still a grab surface");
        assert!(
            compact.content.h > named.content.h,
            "and gives back the room"
        );
    }

    /// **Dividers win over panels**, because a divider's grab width is wider than the gutter and
    /// so overlaps its neighbours. Test the panels first and a divider can only be caught on the
    /// hairline it draws, which is the thing that makes splitters elsewhere feel unhittable.
    #[test]
    fn a_divider_is_reachable_where_it_overlaps_a_panel() {
        let l = workspace();
        let placed = l.resolve(area());
        let splitters = l.splitters(area(), metrics().splitter_grab);
        let s = &splitters[0];
        // A point inside the grab width but well inside the left panel's own rectangle.
        let x = s.rect.x + 1.0;
        let y = 400.0;
        assert!(
            placed[0].rect.contains(x, y),
            "the sample must be inside a panel, or this proves nothing"
        );
        assert_eq!(
            target_at(
                &placed,
                &splitters,
                &metrics(),
                |_| HeaderStyle::Named,
                |_, _| 40.0,
                x,
                y
            ),
            Target::Splitter {
                path: s.path.clone(),
                index: s.index
            }
        );
    }

    /// A press on a header names the tab under it; a press below the header is the panel's own.
    #[test]
    fn a_press_finds_the_tab_or_the_content() {
        let l = workspace();
        let placed = l.resolve(area());
        let splitters = l.splitters(area(), metrics().splitter_grab);
        let leaf = placed[1].clone();
        let c = panel(&leaf, &metrics(), HeaderStyle::Named, |_| 40.0);

        let hit = |x: f32, y: f32| {
            target_at(
                &placed,
                &splitters,
                &metrics(),
                |_| HeaderStyle::Named,
                |_, _| 40.0,
                x,
                y,
            )
        };
        // Aimed at the label rather than at the tab's extreme left edge, because the divider's
        // grab width is centred on the boundary and so claims a margin of the panel beside it.
        // See `a_divider_takes_a_margin_from_its_neighbour`.
        let aim = metrics().tab_padding + 2.0;
        assert_eq!(
            hit(c.tabs[0].rect.x + aim, c.tabs[0].rect.y + 5.0),
            Target::Tab {
                path: leaf.path.clone(),
                tab: 0
            }
        );
        assert_eq!(
            hit(c.tabs[1].rect.x + aim, c.tabs[1].rect.y + 5.0),
            Target::Tab {
                path: leaf.path.clone(),
                tab: 1
            }
        );
        assert_eq!(
            hit(c.content.x + 20.0, c.content.y + 40.0),
            Target::Elsewhere
        );
    }

    /// A divider claims a margin of the panel beside it, and that margin is bounded.
    ///
    /// **A known consequence, stated rather than discovered.** The grab width is centred on the
    /// boundary, so widening it to something a pen can catch — which it had to be — necessarily
    /// takes half of that width from each neighbour. The trade is worth making: a divider is two
    /// units of visible line and needs the margin to be usable at all, while a tab is dozens of
    /// units wide and can spare its leading edge.
    ///
    /// What must not happen is the margin swallowing something whole, so that is what is checked.
    #[test]
    fn a_divider_takes_a_margin_from_its_neighbour() {
        let l = workspace();
        let placed = l.resolve(area());
        let leaf = placed[1].clone();
        let c = panel(&leaf, &metrics(), HeaderStyle::Named, |_| 40.0);
        let first = c.tabs[0].rect;

        let margin = metrics().splitter_grab / 2.0;
        assert!(
            margin < first.w * 0.5,
            "the divider claims {margin} of a {} tab, which is most of it",
            first.w
        );
        // And the label itself is clear of it, which is where anyone actually aims.
        assert!(
            metrics().tab_padding + 2.0 > margin,
            "a tab's label starts inside the divider's margin"
        );
    }

    /// A compact header is still grabbable, which is what makes a tool rail movable — the whole
    /// point of §1c's "no exceptions".
    #[test]
    fn a_compact_header_can_still_be_grabbed() {
        let l = Layout::single(CANVAS);
        let placed = l.resolve(area());
        let target = target_at(
            &placed,
            &[],
            &metrics(),
            |_| HeaderStyle::Compact,
            |_, _| 40.0,
            500.0,
            2.0,
        );
        assert_eq!(
            target,
            Target::Tab {
                path: vec![],
                tab: 0
            },
            "pressing a nameless header still picks up its panel"
        );
    }

    /// Pressing past the last tab, but still on the header, picks up the panel on show rather than
    /// declining. The header is one grab surface; the tabs are labels on it.
    #[test]
    fn the_empty_part_of_a_header_grabs_the_shown_panel() {
        let l = workspace();
        let placed = l.resolve(area());
        let leaf = placed[1].clone();
        let c = panel(&leaf, &metrics(), HeaderStyle::Named, |_| 30.0);
        let past = c.tabs.last().expect("tabs").rect;

        assert_eq!(
            target_at(
                &placed,
                &[],
                &metrics(),
                |_| HeaderStyle::Named,
                |_, _| 30.0,
                past.x + past.w + 15.0,
                past.y + 5.0
            ),
            Target::Tab {
                path: leaf.path,
                tab: leaf.active
            }
        );
    }

    /// The drawn regions agree with `Layout::zone_at` **everywhere**, not just at their centres.
    ///
    /// This is the highest-stakes agreement in the whole UI: the region that lights up under the
    /// pointer has to be the drop that happens when you let go. If the two ever disagree, the app
    /// shows one thing and does another — and the artist has no way to tell which to believe.
    ///
    /// The first version sampled each region's midpoint, and a sabotage that widened the drawn
    /// band from a quarter to two fifths **passed it**: the midpoints still landed in the right
    /// zones and only the boundaries had moved. Sampling a grid instead then found something
    /// worse than the sabotage — the regions were rectangles while the drop mitres at the corners,
    /// so they disagreed in all four corners of every panel from the start.
    #[test]
    fn the_drawn_zones_agree_with_the_drop_everywhere() {
        let r = Rect::new(100.0, 50.0, 400.0, 300.0);
        let zones = drop_zones(r);

        let covered: f32 = zones.iter().map(DropRegion::area).sum();
        assert!(
            (covered - r.w * r.h).abs() < 0.01,
            "the regions cover {covered} of {}",
            r.w * r.h
        );

        for i in 0..60_u16 {
            for j in 0..45_u16 {
                let x = r.x + (f32::from(i) + 0.5) * r.w / 60.0;
                let y = r.y + (f32::from(j) + 0.5) * r.h / 45.0;
                let drawn = zones.iter().find(|z| z.contains(x, y)).map(|z| z.zone);
                assert_eq!(
                    drawn,
                    Some(Layout::zone_at(r, x, y)),
                    "at ({x:.1}, {y:.1}) the lit region and the drop disagree"
                );
            }
        }
    }
}
