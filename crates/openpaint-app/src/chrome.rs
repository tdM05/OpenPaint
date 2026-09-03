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
    ///
    /// Not where controls go: the canvas fills this exactly, and padding it would leave a border
    /// of ground around the artwork.
    pub content: Rect,
    /// Where a panel's *controls* go: the content, inset by the padding.
    ///
    /// **The one place the padding is applied**, because it was previously applied twice --- once
    /// where the panel's egui area was built and once inside the renderer --- and the result was a
    /// menu strip with sixteen units of nothing above its own text. Naming the padded rectangle
    /// separately means the renderer takes a rectangle rather than a rectangle-and-a-convention,
    /// and there is nowhere left for a second helping to hide.
    pub controls: Controls,
    /// Empty when the header is compact.
    pub tabs: Vec<Tab>,
    pub style: HeaderStyle,
}

/// A panel's controls rectangle, which only [`panel`] can make.
///
/// A newtype for one reason: the padding used to be applied here *and* again where the panel's
/// drawing area was built, and the result was a menu strip mostly made of empty space. A plain
/// `Rect` invites a second helping and looks perfectly reasonable doing it; this does not
/// type-check unless somebody deliberately takes the rectangle out first, which is the difference
/// between a mistake and a decision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Controls(Rect);

impl Controls {
    /// The rectangle itself, for whoever is actually going to draw in it.
    #[must_use]
    pub fn rect(self) -> Rect {
        self.0
    }
}

/// Which edges of a rectangle a point takes hold of.
///
/// One number per axis rather than a list of eight named edges and corners: `-1` is the near side,
/// `1` the far side, `0` neither. A corner is simply both at once, so there is no corner case to
/// write, nothing to keep in step with a table of names, and the resize that follows is the same
/// two lines of arithmetic whichever of the eight the artist grabbed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pull {
    pub x: i8,
    pub y: i8,
}

/// Which edges of `rect` a point takes hold of, or `None` if it is not near the boundary.
///
/// **The band straddles the boundary, exactly as a divider's does.** Half of `grab` lies outside
/// and half inside, for the same reason and at the same width: the line you see is a hairline and
/// the thing you aim at has to be a target. That is why a window's touchable extent reaches a
/// little past its own rectangle.
///
/// **Except that the inside half gives way on a small window.** Two opposite borders 13 units deep
/// leave nothing between them on a window 28 units tall -- and what is between them is the header,
/// which is how the window is *moved*. A window that could only be resized would be one that had
/// eaten its own handle. So the inner reach shrinks to keep `least` units of interior, reaching
/// zero exactly at the smallest a window is allowed to be, where the border is entirely outside
/// it. `least` is the same floor the resize itself stops at, asked once.
///
/// **And the top edge reaches outward only, whatever the size.** That rule above is about the
/// *window* keeping a handle; this is about the handle keeping itself. A window's top edge is its
/// header, and the header is 28 units tall -- so an inner reach of 13 made the upper half of every
/// tab a resize border. Grabbing a tab a little high resized the window instead of moving it,
/// which is the same complaint the panels drew before ("only that tiny little tab is the button"),
/// arrived at from the other direction. Resizing from the top is still there, from just outside
/// the window, which is where a frame is.
#[must_use]
pub fn edge_at(rect: Rect, grab: f32, least: f32, x: f32, y: f32) -> Option<Pull> {
    let outer = grab / 2.0;
    let near = |v: f32, low: f32, size: f32, inner_low: f32| -> Option<i8> {
        let high = low + size;
        let inner = ((size - least) / 2.0).clamp(0.0, outer);
        let inner_low = inner.min(inner_low);
        if v >= low - outer && v <= low + inner_low {
            Some(-1)
        } else if v >= high - inner && v <= high + outer {
            Some(1)
        } else if v > low && v < high {
            Some(0)
        } else {
            // Past the boundary and not within reach of it: not this rectangle at all.
            None
        }
    };
    let px = near(x, rect.x, rect.w, outer)?;
    // Zero, and only here: see the note above about the header.
    let py = near(y, rect.y, rect.h, 0.0)?;
    // The middle of both axes is the inside of the panel, which is nobody's edge.
    (px != 0 || py != 0).then_some(Pull { x: px, y: py })
}

/// Move the edges a `Pull` names by `(dx, dy)`, keeping the opposite ones where they are.
///
/// `least` is the size below which the rectangle stops shrinking, on both axes -- the same
/// something-left-to-grab rule a window is already held to when it is moved. The side that stops
/// is the one being dragged, so the fixed edge never creeps: a window dragged past its floor and
/// back comes out where it went in.
#[must_use]
pub fn pull_edges(rect: Rect, pull: Pull, dx: f32, dy: f32, least: f32) -> Rect {
    let along = |low: f32, size: f32, side: i8, d: f32| -> (f32, f32) {
        match side {
            -1 => {
                let want = (size - d).max(least);
                (low + size - want, want)
            }
            1 => (low, (size + d).max(least)),
            _ => (low, size),
        }
    };
    let (x, w) = along(rect.x, rect.w, pull.x, dx);
    let (y, h) = along(rect.y, rect.h, pull.y, dy);
    Rect::new(x, y, w, h)
}

/// The bars to draw for a [`Pull`]: one along each edge it names, `t` thick and centred on the
/// boundary, exactly as a divider's hint is drawn.
///
/// A corner gives two, which is what "both axes at once" looks like and needs no case of its own.
#[must_use]
pub fn edge_bands(rect: Rect, pull: Pull, t: f32) -> Vec<Rect> {
    let half = t / 2.0;
    let mut out = Vec::new();
    match pull.x {
        -1 => out.push(Rect::new(rect.x - half, rect.y, t, rect.h)),
        1 => out.push(Rect::new(rect.x + rect.w - half, rect.y, t, rect.h)),
        _ => {}
    }
    match pull.y {
        -1 => out.push(Rect::new(rect.x, rect.y - half, rect.w, t)),
        1 => out.push(Rect::new(rect.x, rect.y + rect.h - half, rect.w, t)),
        _ => {}
    }
    out
}

/// How much of a header is reserved for the panel strip.
///
/// **Reserved before the tabs are laid out, never after.** This is the one place a press always
/// means "this panel as a whole" -- its settings, and moving the window it is in -- so it has to
/// exist at every width and at every number of tabs. Tabs wrap onto another row rather than eat
/// it; nothing else in the header gets that promise.
///
/// It is a header's own height, which `theme::every_grab_surface_is_big_enough_for_a_pen` already
/// holds to the 4 mm floor: the strip inherits that floor rather than keeping a second number of
/// its own that could quietly fall below it.
///
/// It carries no mark of its own. It used to draw three short rules, and they read as a button --
/// so pressing beside them, which worked identically, looked like a mistake that happened to work.
/// The tabs are the buttons; the strip is what is left, and that is what it should look like.
pub fn strip_width(metrics: &Metrics) -> f32 {
    metrics.header
}

/// Which side of a panel its handle sits on.
///
/// **It follows the direction the panel's controls run, not the shape of the panel.** A panel set
/// to run its controls down has its handle across the top; one set to run them across has it down
/// the left-hand side --- the same corner either way, which is what makes the two look like one
/// rule rather than two. Deciding by shape instead was nearly right and read as arbitrary: the same
/// panel would move its handle when a neighbour was resized, which is not something the artist
/// asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Along {
    /// Controls run down the panel; the handle is the bar across the top.
    Down,
    /// Controls run across it; the handle is the bar down the left.
    Across,
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
    along: Along,
    mut measure: impl FnMut(usize) -> f32,
) -> PanelChrome {
    let half = metrics.gutter / 2.0;
    let outer = Rect::new(
        placed.rect.x + half,
        placed.rect.y + half,
        (placed.rect.w - metrics.gutter).max(0.0),
        (placed.rect.h - metrics.gutter).max(0.0),
    );

    // **The handle goes down the left when the controls run across.**
    //
    // A tool rail laid out across the bottom is a strip a couple of rows tall; a handle band on
    // top of it costs more height than the tools do. On its side it costs a sliver of width
    // instead. On the left, which is the same corner a panel running down puts it in: the handle
    // is always at the beginning, so the two arrangements read as one rule rather than two.
    //
    // Only compact headers: a named header carries tabs, and tabs need width to be readable.
    let side = style == HeaderStyle::Compact && along == Along::Across;

    let bar = match style {
        HeaderStyle::Named => metrics.header,
        HeaderStyle::Compact => metrics.header_compact,
    }
    // A panel dragged down to a sliver keeps a header and loses its content, rather than the other
    // way round: the header is the only part that can be grabbed to undo the drag by hand.
    .min(if side { outer.w } else { outer.h });

    let mut tabs = Vec::new();
    let mut rows = 1.0_f32;
    if style == HeaderStyle::Named {
        // **Tabs wrap onto more rows rather than running off the edge.**
        //
        // A panel is draggable anywhere, so any panel can end up narrow — there is no arrangement
        // to design against. A row that overflowed would put its later tabs permanently out of
        // reach, and for the menu that would mean losing the way to reopen a closed panel: the one
        // thing that must always be reachable.
        //
        // Shrinking tabs instead was the alternative and is worse: a tab narrower than its label
        // is one you can neither read nor aim at. Height is the cheaper thing to spend.
        // **Room the tabs may not have.** The strip beside them is the panel's own handle, and it
        // has to exist however many tabs there are -- otherwise a panel with enough tabs to fill
        // its bar could not be picked up at all.
        // **Nothing, if that is what is left.** The floor here used to be one unit, so a panel narrower
        // than the strip itself still gave the first tab a sliver -- and the one promise the strip
        // makes is that it is *always* there, at every width and every number of tabs. A tab too narrow
        // to press is worth nothing; a strip is worth the settings of everything in the panel.
        let room = (outer.w - strip_width(metrics)).max(0.0);
        let widths: Vec<f32> = (0..placed.tabs.len())
            .map(|i| (measure(i) + metrics.tab_padding * 2.0).min(room))
            .collect();
        let (mut x, mut y) = (outer.x, outer.y);
        for (index, w) in widths.iter().copied().enumerate() {
            if x > outer.x && x + w > outer.x + room {
                x = outer.x;
                y += bar;
                rows += 1.0;
            }
            tabs.push(Tab {
                index,
                rect: Rect::new(x, y, w, bar),
                active: index == placed.active,
            });
            // A gap, because each tab is its own button. Run together they read as one long
            // control and pressing any of them looks like pressing all of them -- which is
            // exactly how it was reported.
            x += w + metrics.gap;
        }
    }

    // The header grows to hold however many rows the tabs needed, and the content gives up the
    // room. Never past the panel itself: a header taller than its panel would leave no content at
    // all, and a panel squeezed to a sliver keeps its header because that is the only part that
    // can be grabbed to undo the squeeze.
    let bar_total = (bar * rows).min(if side { outer.w } else { outer.h });
    let (header, content) = if side {
        (
            Rect::new(outer.x, outer.y, bar_total, outer.h),
            Rect::new(
                outer.x + bar_total,
                outer.y,
                (outer.w - bar_total).max(0.0),
                outer.h,
            ),
        )
    } else {
        (
            Rect::new(outer.x, outer.y, outer.w, bar_total),
            Rect::new(
                outer.x,
                outer.y + bar_total,
                outer.w,
                (outer.h - bar_total).max(0.0),
            ),
        )
    };

    PanelChrome {
        outer,
        header,
        controls: Controls(Rect::new(
            content.x + metrics.padding,
            content.y + metrics.padding,
            (content.w - metrics.padding * 2.0).max(0.0),
            (content.h - metrics.padding * 2.0).max(0.0),
        )),
        content,
        tabs,
        style,
    }
}

/// What a press at a point means.
///
/// # Headers, then dividers, then content
///
/// Both grab surfaces have to be generous — a divider is a hairline and a header is how a panel is
/// picked up — and being generous makes them overlap, because a divider's grab width is centred on
/// the boundary and reaches into the panels either side.
///
/// The order settles it without shrinking either. **A header is the panel's own grab surface**, so
/// it wins where they meet: along the top strip you are picking a panel up, and everywhere below
/// you are resizing. Two clean rules that meet at a line the artist can see.
///
/// The first version had it the other way, so that a divider could be caught anywhere. It could —
/// including over the first few characters of the neighbouring tab's name, which is precisely
/// where anyone aiming for that tab would press.
/// `chrome_of` says what kind of chrome a leaf gets: whether its header carries names, and which
/// side it sits on. One question rather than two closures, because they are answered from the same
/// place and were only ever passed together.
#[must_use]
pub fn target_at(
    placed: &[Placed],
    splitters: &[crate::layout::Splitter],
    metrics: &Metrics,
    chrome_of: impl Fn(&Placed) -> (HeaderStyle, Along),
    mut measure: impl FnMut(&Placed, usize) -> f32,
    x: f32,
    y: f32,
) -> Target {
    // Headers first, and every panel's header — not just the one whose slot contains the point,
    // since a header can reach a little past its own slot once the gutter is taken out.
    for p in placed {
        let (style, along) = chrome_of(p);
        let chrome = panel(p, metrics, style, along, |i| measure(p, i));
        if chrome.header.contains(x, y) {
            return header_target(p, &chrome, x, y);
        }
    }
    for s in splitters {
        if s.rect.contains(x, y) {
            return Target::Splitter {
                path: s.path.clone(),
                index: s.index,
            };
        }
    }
    Target::Elsewhere
}

/// Which tab of a header was pressed, or the one on show.
fn header_target(p: &Placed, chrome: &PanelChrome, x: f32, y: f32) -> Target {
    if let Some(tab) = chrome.tabs.iter().find(|t| t.rect.contains(x, y)) {
        return Target::Tab {
            path: p.path.clone(),
            tab: tab.index,
        };
    }
    // **The strip beside the tabs is not a tab.** It is the panel's own handle, and giving it to
    // whichever tab happened to be on show is what made a header look like one control and what
    // made pressing the empty part of it move a panel nobody had pointed at.
    //
    // A header with *no* tabs is entirely strip, and that strip is the handle for the one panel it
    // holds. That is what makes the tool rail and the menu movable, and there is nothing else it
    // could mean.
    if !chrome.tabs.is_empty() {
        // Carrying the leaf it belongs to: a floating window can hold several after a five-zone
        // drop, each with its own strip and its own shown panel, and "the shown panel's settings"
        // has to mean the one whose strip was held.
        return Target::Strip {
            path: p.path.clone(),
        };
    }
    Target::Tab {
        path: p.path.clone(),
        tab: p.active,
    }
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

    /// A panel wider than it is tall: a strip along the bottom.
    fn wide() -> Placed {
        Placed {
            path: vec![],
            rect: Rect::new(0.0, 0.0, 900.0, 60.0),
            tabs: vec![CANVAS],
            active: 0,
        }
    }

    /// A panel taller than it is wide: a rail down the side.
    fn tall() -> Placed {
        Placed {
            path: vec![],
            rect: Rect::new(0.0, 0.0, 220.0, 700.0),
            tabs: vec![CANVAS],
            active: 0,
        }
    }

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
            let c = panel(&placed, &metrics(), HeaderStyle::Named, Along::Down, |_| {
                40.0
            });
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
        let a = panel(
            &placed[0],
            &metrics(),
            HeaderStyle::Named,
            Along::Down,
            |_| 40.0,
        );
        let b = panel(
            &placed[1],
            &metrics(),
            HeaderStyle::Named,
            Along::Down,
            |_| 40.0,
        );
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
        let c = panel(&placed, &metrics(), HeaderStyle::Named, Along::Down, |_| {
            40.0
        });
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
        let c = panel(leaf, &metrics(), HeaderStyle::Named, Along::Down, |i| {
            widths[i]
        });
        assert_eq!(c.tabs.len(), 2);
        assert!((c.tabs[0].rect.w - (50.0 + metrics().tab_padding * 2.0)).abs() < 0.001);
        assert!((c.tabs[1].rect.w - (90.0 + metrics().tab_padding * 2.0)).abs() < 0.001);
        // A gap between them, because each tab is its own button. Run together they read as one
        // long control, and pressing one looked like pressing the whole strip.
        assert!(
            (c.tabs[1].rect.x - (c.tabs[0].rect.x + c.tabs[0].rect.w) - metrics().gap).abs()
                < 0.001,
            "tabs must be a gap apart, not touching and not overlapping"
        );
        assert!(c.tabs[1].active, "the shown panel is the marked tab");
    }

    /// **Tabs wrap rather than running off the edge**, and the header grows to hold them.
    ///
    /// Any panel can be dragged somewhere narrow, so there is no arrangement to design against. A
    /// row that overflowed would put its later tabs permanently out of reach — and in the menu
    /// that means losing the way to reopen a closed panel.
    #[test]
    fn tabs_wrap_onto_more_rows_when_the_panel_is_narrow() {
        let placed = Placed {
            path: vec![],
            rect: Rect::new(0.0, 0.0, 160.0, 400.0),
            tabs: vec![CANVAS, LAYERS, HISTORY, PanelId(9)],
            active: 0,
        };
        // Four tabs of ~64 units each against a 160-unit panel: two per row.
        let c = panel(&placed, &metrics(), HeaderStyle::Named, Along::Down, |_| {
            40.0
        });
        assert_eq!(c.tabs.len(), 4);

        let rows: std::collections::BTreeSet<i32> =
            c.tabs.iter().map(|t| t.rect.y as i32).collect();
        assert!(
            rows.len() > 1,
            "they should have wrapped, all sat at {rows:?}"
        );
        for t in &c.tabs {
            assert!(
                t.rect.x + t.rect.w <= c.outer.x + c.outer.w + 0.01,
                "tab {} runs off the panel and can never be reached",
                t.index
            );
        }
        assert!(
            c.header.h > metrics().header,
            "the header should have grown to hold the extra row"
        );
        assert!(
            (c.header.h + c.content.h - c.outer.h).abs() < 0.001,
            "and the content gave up exactly that much"
        );
    }

    /// A header can never grow past its own panel, however many tabs it has.
    ///
    /// Otherwise a narrow panel with several tabs would be all header and no content — or worse,
    /// a negative content height.
    #[test]
    fn a_wrapped_header_never_outgrows_its_panel() {
        let placed = Placed {
            path: vec![],
            rect: Rect::new(0.0, 0.0, 90.0, 60.0),
            tabs: vec![CANVAS, LAYERS, HISTORY, PanelId(9), PanelId(10)],
            active: 0,
        };
        let c = panel(&placed, &metrics(), HeaderStyle::Named, Along::Down, |_| {
            40.0
        });
        assert!(
            c.header.h <= c.outer.h + 0.001,
            "header {} of {}",
            c.header.h,
            c.outer.h
        );
        assert!(c.content.h >= 0.0);
        assert!((c.header.h + c.content.h - c.outer.h).abs() < 0.001);
    }

    /// Controls are inset from the content by the padding, once, and never off the panel.
    ///
    /// This is where the padding is decided, so this is where it is checked. Applying it a second
    /// time at the call site is what produced a menu strip mostly made of nothing.
    #[test]
    fn controls_sit_one_padding_inside_the_content() {
        let m = metrics();
        for slot in [wide(), tall()] {
            for style in [HeaderStyle::Named, HeaderStyle::Compact] {
                let c = panel(&slot, &m, style, Along::Down, |_| 40.0);
                let controls = c.controls.rect();
                assert!((controls.x - c.content.x - m.padding).abs() < 0.001);
                assert!((controls.y - c.content.y - m.padding).abs() < 0.001);
                assert!((controls.w - (c.content.w - m.padding * 2.0)).abs() < 0.001);
                assert!((controls.h - (c.content.h - m.padding * 2.0)).abs() < 0.001);
            }
        }

        // A panel too small to hold its own padding gets an empty rectangle, not a negative one.
        let sliver = Placed {
            path: vec![],
            rect: Rect::new(0.0, 0.0, 10.0, 10.0),
            tabs: vec![CANVAS],
            active: 0,
        };
        let c = panel(&sliver, &m, HeaderStyle::Named, Along::Down, |_| 40.0);
        assert!(c.controls.rect().w >= 0.0 && c.controls.rect().h >= 0.0);
    }

    /// A compact header has no tabs and takes less room, but is still a header.
    #[test]
    fn a_compact_header_is_shorter_and_nameless() {
        // A tall panel, so the header is the band across the top.
        let placed = tall();
        let named = panel(&placed, &metrics(), HeaderStyle::Named, Along::Down, |_| {
            40.0
        });
        let compact = panel(
            &placed,
            &metrics(),
            HeaderStyle::Compact,
            Along::Down,
            |_| 40.0,
        );

        assert!(compact.tabs.is_empty());
        assert!(compact.header.h < named.header.h);
        assert!(compact.header.h > 0.0, "it is still a grab surface");
        assert!(
            compact.content.h > named.content.h,
            "and gives back the room"
        );
    }

    /// **The handle follows the direction the controls run, not the shape of the panel.**
    ///
    /// A rail whose controls run across is a strip a couple of rows tall, and a handle band on top
    /// of it costs more height than the controls do; on its side it costs a sliver of width. It
    /// used to be decided by shape, which read as arbitrary: the same panel moved its handle when
    /// a neighbour was resized, which is not something anybody asked for.
    #[test]
    fn a_compact_header_runs_along_the_short_side() {
        let m = metrics();

        let strip = panel(&wide(), &m, HeaderStyle::Compact, Along::Across, |_| 40.0);
        assert!(
            strip.header.w < strip.header.h,
            "on a wide panel the header should be a vertical bar, got {:?}",
            strip.header
        );
        assert!(
            (strip.header.x - strip.outer.x).abs() < 0.001,
            "and it belongs at the beginning, the same corner a panel running down puts it in"
        );
        assert!(
            (strip.content.x - (strip.outer.x + strip.header.w)).abs() < 0.001,
            "so the content starts just after it"
        );
        assert!(
            (strip.header.h - strip.outer.h).abs() < 0.001,
            "full height"
        );
        assert!(
            (strip.header.w + strip.content.w - strip.outer.w).abs() < 0.001,
            "and the content gives up exactly that width"
        );
        assert!(
            (strip.content.h - strip.outer.h).abs() < 0.001,
            "a strip must lose no height at all: that was the complaint"
        );

        // Named headers carry tabs, and tabs need width, so they never turn.
        // A tall panel keeps its handle on top even if something asks for across, because a
        // panel whose controls run across is by definition not a tall one -- but the rule is the
        // direction, so this is what it says.
        let rail = panel(&tall(), &m, HeaderStyle::Compact, Along::Down, |_| 40.0);
        assert!(
            rail.header.h < rail.header.w,
            "on a panel whose controls run down it stays across the top"
        );

        // Named headers carry tabs, and tabs need width, so they never turn.
        let named = panel(&wide(), &m, HeaderStyle::Named, Along::Across, |_| 40.0);
        assert!(named.header.w > named.header.h, "tabs stay across the top");
    }

    /// **Dividers win over panels**, because a divider's grab width is wider than the gutter and
    /// so overlaps its neighbours. Test the panels first and a divider can only be caught on the
    /// hairline it draws, which is the thing that makes splitters elsewhere feel unhittable.
    /// **Nine regions, and only the middle one is not an edge.**
    ///
    /// Written as a sweep over the whole rectangle rather than eight named probes: a table of
    /// eight has eight chances to name the wrong side, and it cannot notice a gap between two of
    /// them or an overlap where they meet.
    #[test]
    fn every_edge_and_corner_answers_for_itself() {
        let m = metrics();
        let r = Rect::new(100.0, 200.0, 300.0, 260.0);
        let (grab, least) = (m.splitter_grab, m.header.max(m.row));
        let half = grab / 2.0;

        // One unit inside each boundary, and the middle: nine probes that between them name every
        // combination.
        let xs = [
            (r.x + 1.0, -1_i8),
            (r.x + r.w / 2.0, 0),
            (r.x + r.w - 1.0, 1),
        ];
        // A unit *outside* the top, because a unit inside it is the header now and belongs to
        // whoever moves the window -- see `edge_at`.
        let ys = [
            (r.y - 1.0, -1_i8),
            (r.y + r.h / 2.0, 0),
            (r.y + r.h - 1.0, 1),
        ];
        for (x, px) in xs {
            for (y, py) in ys {
                let got = edge_at(r, grab, least, x, y);
                let want = (px != 0 || py != 0).then_some(Pull { x: px, y: py });
                assert_eq!(got, want, "at ({x}, {y})");
            }
        }

        // Outside the reach on either axis is nobody's edge at all, not a `Pull` of zeroes.
        for at in [
            (r.x - half - 1.0, r.y + r.h / 2.0),
            (r.x + r.w / 2.0, r.y - half - 1.0),
            (r.x + r.w + half + 1.0, r.y + r.h / 2.0),
            (r.x + r.w / 2.0, r.y + r.h + half + 1.0),
        ] {
            assert_eq!(edge_at(r, grab, least, at.0, at.1), None, "at {at:?}");
        }
    }

    /// **The border always leaves `least` units of interior**, because that interior is the handle.
    ///
    /// Two borders 13 units deep leave two units between them on a window 28 units tall, and those
    /// two units are the header -- the bar the window is dragged by. So the inner half gives way,
    /// reaching nothing at all at the floor.
    ///
    /// Stated as the *interior that survives*, not as "the middle is free": on a window a little
    /// larger than the floor the middle is free either way, and a sabotage removing the clamp
    /// outright went unnoticed by a test that only probed the centre.
    #[test]
    fn the_border_always_leaves_a_handle_behind() {
        let m = metrics();
        let (grab, least) = (m.splitter_grab, m.header.max(m.row));
        for size in [
            least,
            least + 1.0,
            least + 8.0,
            least + 26.0,
            least + 40.0,
            300.0,
        ] {
            let r = Rect::new(50.0, 50.0, size, size);
            // The centred square of side `least`: every point of it must be interior, on both
            // axes, or the handle has been eaten.
            let lo = (size - least) / 2.0;
            for (dx, dy) in [
                (lo + 0.1, lo + 0.1),
                (size - lo - 0.1, lo + 0.1),
                (lo + 0.1, size - lo - 0.1),
                (size - lo - 0.1, size - lo - 0.1),
                (size / 2.0, size / 2.0),
            ] {
                assert_eq!(
                    edge_at(r, grab, least, r.x + dx, r.y + dy),
                    None,
                    "a {size}-unit window lost its handle at ({dx}, {dy})"
                );
            }
            // And the outside is always reachable, whatever the size.
            assert!(
                edge_at(r, grab, least, r.x - 2.0, r.y + size / 2.0).is_some(),
                "the outer half should not give way"
            );
        }
    }

    /// **The whole of the header is the handle**, and the top border does not reach into it.
    ///
    /// The border straddles every other edge, half in and half out. On the top edge the inner half
    /// would be thirteen units of a twenty-eight unit header -- so a tab grabbed anywhere in its
    /// upper half resized the window instead of moving it. `windows-apart.txt` was written before
    /// this rule and holds a tab one physical pixel below the window's top edge; that step used to
    /// resize.
    #[test]
    fn the_top_border_stays_out_of_the_header() {
        let m = metrics();
        let (grab, least) = (m.splitter_grab, m.header.max(m.row));
        let r = Rect::new(60.0, 60.0, 320.0, 338.0);
        // Every depth into the header, from its very first unit to its last.
        for dy in [0.5, 1.0, 4.0, 13.0, m.header - 0.5] {
            assert_eq!(
                edge_at(r, grab, least, r.x + r.w / 2.0, r.y + dy),
                None,
                "{dy} units into the header answered as a border"
            );
        }
        // From outside, the top edge is still a border -- that is where a frame is.
        for dy in [0.5, 6.0, grab / 2.0 - 0.5] {
            assert_eq!(
                edge_at(r, grab, least, r.x + r.w / 2.0, r.y - dy),
                Some(Pull { x: 0, y: -1 }),
                "{dy} units above the window was not the top border"
            );
        }
        // And the other three edges keep their inner half: they have a panel body behind them,
        // not a handle.
        assert_eq!(
            edge_at(r, grab, least, r.x + 1.0, r.y + r.h / 2.0),
            Some(Pull { x: -1, y: 0 }),
            "the left border lost its inner half"
        );
        assert_eq!(
            edge_at(r, grab, least, r.x + r.w / 2.0, r.y + r.h - 1.0),
            Some(Pull { x: 0, y: 1 }),
            "the bottom border lost its inner half"
        );
    }

    /// The edges a pull names move; the others stay exactly where they were.
    #[test]
    fn pulling_an_edge_leaves_the_opposite_one_alone() {
        let r = Rect::new(100.0, 200.0, 300.0, 260.0);
        let least = 28.0;
        for (pull, dx, dy) in [
            (Pull { x: -1, y: 0 }, 40.0, 0.0),
            (Pull { x: 1, y: 0 }, 40.0, 0.0),
            (Pull { x: 0, y: -1 }, 0.0, 40.0),
            (Pull { x: 0, y: 1 }, 0.0, 40.0),
            (Pull { x: -1, y: -1 }, -30.0, -20.0),
            (Pull { x: 1, y: 1 }, -30.0, -20.0),
        ] {
            let got = pull_edges(r, pull, dx, dy, least);
            // The far edge is fixed when the near one is pulled, and the near one when the far.
            let far_x = |q: Rect| q.x + q.w;
            let far_y = |q: Rect| q.y + q.h;
            match pull.x {
                -1 => assert!(
                    (far_x(got) - far_x(r)).abs() < 0.01,
                    "{pull:?} moved the right"
                ),
                1 => assert!((got.x - r.x).abs() < 0.01, "{pull:?} moved the left"),
                _ => assert_eq!((got.x, got.w), (r.x, r.w), "{pull:?} touched x at all"),
            }
            match pull.y {
                -1 => assert!(
                    (far_y(got) - far_y(r)).abs() < 0.01,
                    "{pull:?} moved the bottom"
                ),
                1 => assert!((got.y - r.y).abs() < 0.01, "{pull:?} moved the top"),
                _ => assert_eq!((got.y, got.h), (r.y, r.h), "{pull:?} touched y at all"),
            }
            assert!(
                got.w >= least - 0.01 && got.h >= least - 0.01,
                "{pull:?} went under the floor"
            );
        }
    }

    /// **A pull stopped at the floor still holds its fixed edge.**
    ///
    /// The whole point of a floor is that it stops the edge being dragged, not that it drags the
    /// other one along: a window shrunk to nothing from the left must still have its right edge
    /// where it always was, or the window walks across the screen as it shrinks.
    #[test]
    fn a_pull_stopped_at_the_floor_does_not_walk() {
        let r = Rect::new(100.0, 200.0, 300.0, 260.0);
        let least = 28.0;
        let from_left = pull_edges(r, Pull { x: -1, y: 0 }, 9000.0, 0.0, least);
        assert!((from_left.w - least).abs() < 0.01);
        assert!(
            (from_left.x + from_left.w - (r.x + r.w)).abs() < 0.01,
            "the right edge walked: {from_left:?}"
        );
        let from_top = pull_edges(r, Pull { x: 0, y: -1 }, 0.0, 9000.0, least);
        assert!((from_top.h - least).abs() < 0.01);
        assert!(
            (from_top.y + from_top.h - (r.y + r.h)).abs() < 0.01,
            "the bottom edge walked: {from_top:?}"
        );
    }

    /// A corner draws two bands; an edge draws one; the middle draws none.
    #[test]
    fn the_bands_drawn_are_the_edges_being_pulled() {
        let r = Rect::new(100.0, 200.0, 300.0, 260.0);
        assert_eq!(edge_bands(r, Pull { x: 0, y: 0 }, 5.0).len(), 0);
        assert_eq!(edge_bands(r, Pull { x: 1, y: 0 }, 5.0).len(), 1);
        assert_eq!(edge_bands(r, Pull { x: -1, y: 1 }, 5.0).len(), 2);
        // Centred on the boundary, like a divider's hint.
        let right = edge_bands(r, Pull { x: 1, y: 0 }, 5.0)[0];
        assert!((right.x + right.w / 2.0 - (r.x + r.w)).abs() < 0.01);
    }

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
                |_| (HeaderStyle::Named, Along::Down),
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
        let c = panel(&leaf, &metrics(), HeaderStyle::Named, Along::Down, |_| 40.0);

        let hit = |x: f32, y: f32| {
            target_at(
                &placed,
                &splitters,
                &metrics(),
                |_| (HeaderStyle::Named, Along::Down),
                |_, _| 40.0,
                x,
                y,
            )
        };
        // The extreme left edge is fine now: headers beat dividers, so nothing else reaches it.
        let aim = 5.0;
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

    /// **A header beats a divider where they overlap**, so a generous divider does not eat the
    /// leading edge of the tab beside it.
    ///
    /// Both targets have to be big — a divider is a hairline, a header is how a panel is picked up
    /// — and being big makes them overlap. Ordering settles it. Without this the first characters
    /// of a tab's name are a resize handle, which is exactly where someone aiming for that tab
    /// presses.
    #[test]
    fn a_header_beats_a_divider_where_they_meet() {
        let l = workspace();
        let placed = l.resolve(area());
        let splitters = l.splitters(area(), metrics().splitter_grab);
        let leaf = placed[1].clone();
        let c = panel(&leaf, &metrics(), HeaderStyle::Named, Along::Down, |_| 40.0);
        let first = c.tabs[0].rect;

        // Right at the tab's leading edge, well inside the divider's grab width.
        let x = first.x + 1.0;
        let s = &splitters[0];
        assert!(
            s.rect.contains(x, first.y + 4.0),
            "the sample must be inside the divider's grab, or this proves nothing"
        );
        assert_eq!(
            target_at(
                &placed,
                &splitters,
                &metrics(),
                |_| (HeaderStyle::Named, Along::Down),
                |_, _| 40.0,
                x,
                first.y + 4.0
            ),
            Target::Tab {
                path: leaf.path,
                tab: 0
            },
            "a press on a header is the panel's, whatever else reaches that far"
        );
    }

    /// A compact header is still grabbable, which is what makes a tool rail movable — the whole
    /// point of §1c's "no exceptions".
    #[test]
    fn a_compact_header_can_still_be_grabbed() {
        let l = Layout::single(CANVAS);
        let placed = l.resolve(area());
        // Pressed where the header is actually drawn, rather than at a point that happened to be
        // right while headers were always on top. A test that hardcodes the geometry it is meant
        // to be checking stops checking anything the day the geometry moves.
        let c = panel(
            &placed[0],
            &metrics(),
            HeaderStyle::Compact,
            Along::Down,
            |_| 40.0,
        );
        let target = target_at(
            &placed,
            &[],
            &metrics(),
            |_| (HeaderStyle::Compact, Along::Down),
            |_, _| 40.0,
            c.header.x + c.header.w / 2.0,
            c.header.y + c.header.h / 2.0,
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

    /// **The bar beside the tabs belongs to the panel, not to any tab.**
    ///
    /// Each tab is its own button. A press on the bar picking whichever happened to be on show is
    /// what made a strip of tabs look like one control -- press one and the whole bar answered --
    /// and it moved a panel nobody had pointed at.
    #[test]
    fn the_bar_beside_several_tabs_is_the_panel_strip() {
        let mut l = Layout::single(CANVAS);
        l.insert(&[], Zone::Center, LAYERS);
        let placed = l.resolve(area());
        let leaf = placed[0].clone();
        let c = panel(&leaf, &metrics(), HeaderStyle::Named, Along::Down, |_| 40.0);
        let past = c.tabs.last().expect("two tabs").rect;

        assert_eq!(
            target_at(
                &placed,
                &[],
                &metrics(),
                |_| (HeaderStyle::Named, Along::Down),
                |_, _| 40.0,
                past.x + past.w + 15.0,
                past.y + 5.0
            ),
            Target::Strip { path: vec![] },
            "the bar beside the tabs belongs to the panel, not to a tab"
        );
        // And each tab still answers for itself.
        for tab in &c.tabs {
            assert_eq!(
                target_at(
                    &placed,
                    &[],
                    &metrics(),
                    |_| (HeaderStyle::Named, Along::Down),
                    |_, _| 40.0,
                    tab.rect.x + tab.rect.w / 2.0,
                    tab.rect.y + tab.rect.h / 2.0
                ),
                Target::Tab {
                    path: leaf.path.clone(),
                    tab: tab.index
                },
            );
        }
    }

    /// **A header with no tabs is entirely strip, and that strip is its panel's handle.**
    ///
    /// There is nothing else it could mean, and it is what makes the tool rail and the menu
    /// movable. A header that *has* tabs keeps its strip for the panel as a whole instead --
    /// pressing it used to move whichever panel happened to be on show, which is a panel nobody
    /// pointed at.
    #[test]
    fn a_header_with_no_tabs_is_the_panels_handle() {
        let l = Layout::single(CANVAS);
        let placed = l.resolve(area());
        let c = panel(
            &placed[0],
            &metrics(),
            HeaderStyle::Compact,
            Along::Down,
            |_| 40.0,
        );
        assert!(c.tabs.is_empty(), "a compact header carries no tabs");
        assert_eq!(
            target_at(
                &placed,
                &[],
                &metrics(),
                |_| (HeaderStyle::Compact, Along::Down),
                |_, _| 40.0,
                c.header.x + c.header.w / 2.0,
                c.header.y + c.header.h / 2.0
            ),
            Target::Tab {
                path: vec![],
                tab: 0
            },
        );

        // One tab is still a tab: the bar beside it belongs to the panel, not to it.
        let c = panel(
            &placed[0],
            &metrics(),
            HeaderStyle::Named,
            Along::Down,
            |_| 40.0,
        );
        let past = c.tabs[0].rect;
        assert_eq!(
            target_at(
                &placed,
                &[],
                &metrics(),
                |_| (HeaderStyle::Named, Along::Down),
                |_, _| 40.0,
                past.x + past.w + 40.0,
                past.y + 5.0
            ),
            Target::Strip { path: vec![] },
        );
    }

    /// **The strip is always there**, however many tabs a panel has.
    ///
    /// A panel with enough tabs to fill its bar would otherwise have nowhere left to be picked up.
    #[test]
    fn the_strip_survives_any_number_of_tabs() {
        let m = metrics();
        // **Widths chosen so a tab would land in the reserve if nothing stopped it.** The first
        // version of this used numbers where the tabs happened to wrap before the strip anyway, so
        // removing the reservation changed nothing and the test agreed with the bug.
        //
        // **And widths no window should ever be**, down to the smallest one is allowed to be: the
        // requirement is that the strip is there when there are too many tabs *and* the window is
        // too small, and a sweep that starts at 150 units has not been asked the second half of
        // that question.
        let least = m.header.max(m.row);
        for width in [
            least,
            least + 6.0,
            40.0,
            80.0,
            150.0,
            180.0,
            200.0,
            260.0,
            300.0,
            420.0,
        ] {
            for label in [30.0_f32, 45.0, 66.0, 90.0] {
                for count in [1_u32, 2, 3, 8] {
                    let leaf = Placed {
                        path: vec![],
                        rect: Rect::new(0.0, 0.0, width, least.max(400.0)),
                        tabs: (0..count).map(PanelId).collect(),
                        active: 0,
                    };
                    let c = panel(&leaf, &m, HeaderStyle::Named, Along::Down, |_| label);
                    let reserve = (c.outer.w - strip_width(&m)).max(0.0);
                    for tab in &c.tabs {
                        assert!(
                            tab.rect.x + tab.rect.w <= c.outer.x + reserve + 0.001,
                            "at {width} wide with {count} tabs of {label}, tab {} ate the strip",
                            tab.index
                        );
                    }
                    // Said the other way round, which is the way the promise is made: whatever the
                    // tabs did, the run of bar past them is a whole strip -- or the whole panel,
                    // when the panel is narrower than a strip and the tabs have given up entirely.
                    let free = c.outer.x + c.outer.w
                        - c.tabs.last().map_or(c.header.x, |t| t.rect.x + t.rect.w);
                    assert!(
                        free >= strip_width(&m).min(c.outer.w) - 0.001,
                        "at {width} wide with {count} tabs of {label}, only {free} was left to press"
                    );
                    // And the strip answers as the strip, wherever the tabs ended up.
                    let past = c.tabs.last().map_or(c.header.x, |t| t.rect.x + t.rect.w);
                    let at = (
                        past + strip_width(&m) / 2.0,
                        c.header.y + c.header.h - m.header / 2.0,
                    );
                    let placed = [leaf.clone()];
                    assert_eq!(
                        target_at(
                            &placed,
                            &[],
                            &m,
                            |_| (HeaderStyle::Named, Along::Down),
                            |_, _| label,
                            at.0,
                            at.1
                        ),
                        Target::Strip { path: vec![] },
                        "at {width} wide with {count} tabs of {label} there was no strip to press"
                    );
                }
            }
        }
        // And the strip is a target you could actually hit with a pen.
        const PER_MM: f32 = 96.0 / 25.4;
        assert!(strip_width(&m) / PER_MM >= 4.0);
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
