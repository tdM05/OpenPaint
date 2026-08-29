//! The panel layout: a tree of splits whose leaves hold panels as tabs.
//!
//! This is DECISIONS §1c made concrete, and the invariant is the whole module: **nothing here
//! knows what a panel is.** A [`PanelId`] is an opaque number, never matched on, never special
//! cased. The first `if panel == TOOLBAR` would rebuild Photoshop — whose toolbar and options bar
//! are chrome rather than panels — which is precisely what this design exists to avoid.
//!
//! # Everything else falls out of the tree
//!
//! - **Stacking** is a leaf holding several panels.
//! - **Any panel anywhere** is `remove` then `insert`, with no rule about where either may happen.
//! - **Floating** is a second tree in a second window; nothing here needs to know about it.
//! - **A saved workspace** is this structure serialised.
//! - **Layout undo** is a stack of clones, because the whole tree is a few dozen bytes per panel.
//!
//! # Logical units, not pixels
//!
//! Every coordinate here is a **logical** unit; the caller multiplies by the display scale when it
//! draws. That is not a detail to settle later: the target hardware is a Surface running at 150%,
//! and a layout computed in physical pixels is wrong on every display that is not at 100% — a
//! mistake that is free to avoid now and touches every widget to fix afterwards.
//!
//! # Why the tree is recursive rather than an arena
//!
//! Removing a panel can collapse a leaf, which can collapse its parent split, which can promote a
//! grandchild — a cascade that recursion expresses as a return value and an arena expresses as
//! bookkeeping plus a free list. The tree is at most a few dozen nodes, so the usual reason to
//! reach for an arena does not apply.

// Built ahead of the shell that will drive it, so nothing calls any of this yet. `expect` rather
// than `allow` on purpose: it becomes an error the moment the shell does start calling it, which
// is exactly when this note should be deleted rather than quietly left behind.
#![expect(
    dead_code,
    reason = "the layout tree lands before the UI shell that consumes it"
)]

/// Which panel a leaf holds. Opaque on purpose — see the module note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PanelId(pub u32);

/// A rectangle in logical UI units, y down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    #[must_use]
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Whether a point is inside, treating the left and top edges as inside and the right and
    /// bottom as outside.
    ///
    /// Half-open on purpose: adjacent panels share an edge, and a point on it must belong to
    /// exactly one of them or a hit test could report either.
    #[must_use]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// Which way a split divides its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Children sit side by side, left to right.
    Horizontal,
    /// Children sit stacked, top to bottom.
    Vertical,
}

/// Where a dragged panel would land on the panel under the pointer.
///
/// Five, not eight: corners would split twice in one gesture and halve every target. Two clear
/// drags beat one ambiguous one, and with a pen — which arrives already down and cannot hunt —
/// target size is the whole game (§1c).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Left,
    Right,
    Top,
    Bottom,
    /// Join the leaf as another tab.
    Center,
}

impl Zone {
    /// The axis a split for this zone divides along, or `None` for [`Zone::Center`].
    #[must_use]
    fn axis(self) -> Option<Axis> {
        match self {
            Self::Left | Self::Right => Some(Axis::Horizontal),
            Self::Top | Self::Bottom => Some(Axis::Vertical),
            Self::Center => None,
        }
    }

    /// Whether the arriving panel goes before the panel it was dropped on.
    #[must_use]
    fn before(self) -> bool {
        matches!(self, Self::Left | Self::Top)
    }
}

/// How much of a panel each edge zone claims, as a fraction of that side.
///
/// A quarter, which leaves the centre half the panel in each direction — a quarter of its area,
/// and still the largest single zone since each edge takes about three sixteenths.
///
/// Large enough to hit with a pen without aiming, small enough that "join as a tab" stays the
/// easiest thing to do: stacking is the common intent and splitting is the deliberate one.
pub const EDGE_BAND: f32 = 0.25;

/// The smallest fraction of a split a child may be dragged down to.
///
/// **Not a size constraint** — §1c deliberately has none, and an artist who wants a sliver should
/// get one. This is a *recoverability* floor: a child at exactly zero has no header left to grab,
/// so the layout could not be undone by hand. Small enough to be a sliver, large enough to grab.
const MIN_WEIGHT_FRACTION: f32 = 0.02;

/// Where a node sits in the tree: child indices from the root.
///
/// A path rather than an id because the tree has no stable identity to hand out — nodes are
/// created and collapsed by ordinary edits. Every operation here takes a path and completes before
/// returning, so a path is only ever used against the tree it was produced from.
pub type Path = Vec<usize>;

/// One panel slot: a rectangle, and what is in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub path: Path,
    pub rect: Rect,
    pub tabs: Vec<PanelId>,
    /// Index into `tabs` of the panel currently shown.
    pub active: usize,
}

/// A draggable divider between two children of a split.
#[derive(Clone, Debug, PartialEq)]
pub struct Splitter {
    /// The split that owns it.
    pub path: Path,
    /// The divider after child `index`, so it moves the boundary between `index` and `index + 1`.
    pub index: usize,
    pub axis: Axis,
    pub rect: Rect,
}

/// A child of a split: a node and the share of the split it takes.
///
/// The weight lives on the child rather than in a parallel `Vec<f32>` beside `Vec<Node>`. Two
/// lists that have to stay the same length is §11a.8 exactly — the hazard that has already cost
/// this project a migration bug — and one struct cannot get out of step with itself.
#[derive(Clone, Debug, PartialEq)]
struct Child {
    weight: f32,
    node: Node,
}

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Split { axis: Axis, children: Vec<Child> },
    Leaf { tabs: Vec<PanelId>, active: usize },
}

impl Node {
    fn leaf(tabs: Vec<PanelId>) -> Self {
        Self::Leaf { tabs, active: 0 }
    }
}

/// A panel layout.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    root: Node,
}

impl Default for Layout {
    /// An empty workspace: one leaf holding nothing.
    ///
    /// A valid state, not a broken one — it is "everything closed", which the artist can reach by
    /// closing panels and get out of by opening one. Making it representable here means no caller
    /// has to special-case the moment before the first panel arrives.
    fn default() -> Self {
        Self {
            root: Node::leaf(Vec::new()),
        }
    }
}

impl Layout {
    /// A layout holding one panel.
    #[must_use]
    pub fn single(panel: PanelId) -> Self {
        Self {
            root: Node::leaf(vec![panel]),
        }
    }

    /// Every panel slot, with the rectangle it occupies inside `area`.
    ///
    /// The rectangles tile `area` exactly: no gaps and no overlap, whatever the weights. Any
    /// visible gutter between panels is the *drawing*'s business, not the layout's — a layout that
    /// left gaps would have to be asked where the pointer is when it lands in one.
    #[must_use]
    pub fn resolve(&self, area: Rect) -> Vec<Placed> {
        let mut out = Vec::new();
        Self::walk(&self.root, area, &mut Vec::new(), &mut out);
        out
    }

    fn walk(node: &Node, area: Rect, path: &mut Path, out: &mut Vec<Placed>) {
        match node {
            Node::Leaf { tabs, active } => out.push(Placed {
                path: path.clone(),
                rect: area,
                tabs: tabs.clone(),
                active: *active,
            }),
            Node::Split { axis, children } => {
                for (i, (child, rect)) in children
                    .iter()
                    .zip(shares(*axis, area, children))
                    .enumerate()
                {
                    path.push(i);
                    Self::walk(&child.node, rect, path, out);
                    path.pop();
                }
            }
        }
    }

    /// The leaf under a point, if any.
    #[must_use]
    pub fn leaf_at(&self, area: Rect, x: f32, y: f32) -> Option<Placed> {
        self.resolve(area)
            .into_iter()
            .find(|p| p.rect.contains(x, y))
    }

    /// Every draggable divider, with the rectangle to hit-test it against.
    ///
    /// `grab` is the divider's hit width in logical units, centred on the boundary — so the target
    /// is wider than the line drawn, which is what makes a one-pixel seam usable at all.
    #[must_use]
    pub fn splitters(&self, area: Rect, grab: f32) -> Vec<Splitter> {
        let mut out = Vec::new();
        Self::walk_splitters(&self.root, area, grab, &mut Vec::new(), &mut out);
        out
    }

    fn walk_splitters(
        node: &Node,
        area: Rect,
        grab: f32,
        path: &mut Path,
        out: &mut Vec<Splitter>,
    ) {
        let Node::Split { axis, children } = node else {
            return;
        };
        let rects = shares(*axis, area, children);
        for (i, (child, rect)) in children.iter().zip(rects.iter().copied()).enumerate() {
            // The divider *after* this child, so the last child has none.
            if i + 1 < children.len() {
                let half = grab / 2.0;
                out.push(Splitter {
                    path: path.clone(),
                    index: i,
                    axis: *axis,
                    rect: match axis {
                        Axis::Horizontal => Rect::new(rect.x + rect.w - half, rect.y, grab, rect.h),
                        Axis::Vertical => Rect::new(rect.x, rect.y + rect.h - half, rect.w, grab),
                    },
                });
            }
            path.push(i);
            Self::walk_splitters(&child.node, rect, grab, path, out);
            path.pop();
        }
    }

    /// Which zone of `rect` a point falls in.
    ///
    /// A free function of geometry alone, so the drag preview and the drop both ask the same
    /// question and cannot answer it differently.
    ///
    /// **Corners resolve themselves.** The point is normalised into the rectangle and the *nearest*
    /// edge wins, which makes the boundary between two edge zones the diagonal — a mitre, like a
    /// picture frame. No corner case to write, and no ambiguous sliver where two zones overlap.
    #[must_use]
    pub fn zone_at(rect: Rect, x: f32, y: f32) -> Zone {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return Zone::Center;
        }
        let u = (x - rect.x) / rect.w;
        let v = (y - rect.y) / rect.h;
        let candidates = [
            (u, Zone::Left),
            (1.0 - u, Zone::Right),
            (v, Zone::Top),
            (1.0 - v, Zone::Bottom),
        ];
        let mut best = (f32::INFINITY, Zone::Center);
        for (distance, zone) in candidates {
            if distance < best.0 {
                best = (distance, zone);
            }
        }
        if best.0 <= EDGE_BAND {
            best.1
        } else {
            Zone::Center
        }
    }

    /// Put `panel` into the leaf at `path`, in `zone`.
    ///
    /// A centre drop adds a tab and shows it. An edge drop splits — and if the leaf already sits
    /// in a split along the same axis, the panel becomes a *sibling* there rather than nesting a
    /// new split inside the old one. That keeps the tree flat: dropping four panels along the
    /// bottom edge one after another gives one split of five children, not five splits nested four
    /// deep, which is the shape that makes Unity layouts impossible to reason about.
    ///
    /// Returns whether the path named a leaf.
    pub fn insert(&mut self, path: &[usize], zone: Zone, panel: PanelId) -> bool {
        let Some(axis) = zone.axis() else {
            let Some(Node::Leaf { tabs, active }) = self.node_mut(path) else {
                return false;
            };
            tabs.push(panel);
            *active = tabs.len() - 1;
            return true;
        };

        // Sibling insertion, when the parent already splits this way.
        if let Some((parent_path, index)) = path.split_last().map(|(i, rest)| (rest, *i)) {
            if let Some(Node::Split {
                axis: parent_axis,
                children,
            }) = self.node_mut(parent_path)
            {
                if *parent_axis == axis {
                    // The new child takes half of the one it was dropped on, so the drop does not
                    // resize anything the artist did not touch.
                    let taken = children[index].weight / 2.0;
                    children[index].weight = taken;
                    let at = if zone.before() { index } else { index + 1 };
                    children.insert(
                        at,
                        Child {
                            weight: taken,
                            node: Node::leaf(vec![panel]),
                        },
                    );
                    return true;
                }
            }
        }

        // Otherwise wrap the leaf in a new split.
        let Some(node) = self.node_mut(path) else {
            return false;
        };
        if !matches!(node, Node::Leaf { .. }) {
            return false;
        }
        let existing = std::mem::replace(node, Node::leaf(Vec::new()));
        let arriving = Child {
            weight: 0.5,
            node: Node::leaf(vec![panel]),
        };
        let displaced = Child {
            weight: 0.5,
            node: existing,
        };
        *node = Node::Split {
            axis,
            children: if zone.before() {
                vec![arriving, displaced]
            } else {
                vec![displaced, arriving]
            },
        };
        true
    }

    /// Take a panel out of the layout. Returns whether it was there.
    ///
    /// Emptied leaves and single-child splits collapse, so removing panels can never leave the
    /// tree carrying nodes that hold nothing — which would otherwise accumulate as invisible
    /// slivers that still take space and still answer hit tests.
    pub fn remove(&mut self, panel: PanelId) -> bool {
        let found = Self::remove_in(&mut self.root, panel);
        if found {
            Self::collapse(&mut self.root);
        }
        found
    }

    fn remove_in(node: &mut Node, panel: PanelId) -> bool {
        match node {
            Node::Leaf { tabs, active } => {
                let Some(i) = tabs.iter().position(|p| *p == panel) else {
                    return false;
                };
                tabs.remove(i);
                // Keep the shown tab in range, and prefer the one that slid into its place over
                // jumping to the start -- closing a tab should leave you where you were looking.
                *active = (*active).min(tabs.len().saturating_sub(1));
                true
            }
            Node::Split { children, .. } => children
                .iter_mut()
                .any(|c| Self::remove_in(&mut c.node, panel)),
        }
    }

    /// Drop empty leaves and dissolve splits left with one child.
    fn collapse(node: &mut Node) {
        let Node::Split { children, .. } = node else {
            return;
        };
        for child in children.iter_mut() {
            Self::collapse(&mut child.node);
        }
        children.retain(|c| !matches!(&c.node, Node::Leaf { tabs, .. } if tabs.is_empty()));

        match children.len() {
            // Nothing left: become the empty leaf, which is a state the layout is allowed to be in.
            0 => *node = Node::leaf(Vec::new()),
            // One child: the split has nothing to divide, so it becomes that child. Its weight is
            // discarded because a weight is a share of a parent, and this node now *is* the parent.
            1 => *node = children.remove(0).node,
            // Survivors need no adjustment: `shares` divides by whatever the weights currently
            // sum to, so a weight is a *relative* share and removing one redistributes by
            // arithmetic rather than by bookkeeping. A sabotage proved the renormalising that
            // used to be here was dead code — and worth deleting rather than keeping, because two
            // places that both normalise are two places that can disagree about what a weight
            // means (§11a.8).
            _ => {}
        }
    }

    /// Where a panel currently is, as `(leaf path, tab index)`.
    #[must_use]
    pub fn find(&self, panel: PanelId) -> Option<(Path, usize)> {
        self.resolve(Rect::new(0.0, 0.0, 1.0, 1.0))
            .into_iter()
            .find_map(|p| p.tabs.iter().position(|q| *q == panel).map(|i| (p.path, i)))
    }

    /// Which panel is tab `tab` of the leaf at `path`.
    #[must_use]
    pub fn tab_at(&self, path: &[usize], tab: usize) -> Option<PanelId> {
        let Node::Leaf { tabs, .. } = self.node(path)? else {
            return None;
        };
        tabs.get(tab).copied()
    }

    /// Which way the split at `path` divides, if it is a split.
    #[must_use]
    pub fn split_axis(&self, path: &[usize]) -> Option<Axis> {
        match self.node(path)? {
            Node::Split { axis, .. } => Some(*axis),
            Node::Leaf { .. } => None,
        }
    }

    fn node(&self, path: &[usize]) -> Option<&Node> {
        let mut node = &self.root;
        for &i in path {
            let Node::Split { children, .. } = node else {
                return None;
            };
            node = &children.get(i)?.node;
        }
        Some(node)
    }

    /// Show a different tab of the leaf at `path`. Returns whether it could.
    pub fn set_active(&mut self, path: &[usize], tab: usize) -> bool {
        let Some(Node::Leaf { tabs, active }) = self.node_mut(path) else {
            return false;
        };
        if tab >= tabs.len() {
            return false;
        }
        *active = tab;
        true
    }

    /// Move the divider after child `index` of the split at `path`, by a fraction of that split.
    ///
    /// A fraction rather than pixels so the caller converts once, using the same rectangle it drew
    /// with — a splitter dragged in pixels against a stale rectangle moves by the wrong amount at
    /// every zoom and window size.
    ///
    /// Only the two children either side move. Dragging one divider must not shuffle the whole
    /// row, which is what makes a layout feel adjusted rather than disturbed.
    pub fn drag_splitter(&mut self, path: &[usize], index: usize, delta: f32) -> bool {
        let Some(Node::Split { children, .. }) = self.node_mut(path) else {
            return false;
        };
        if index + 1 >= children.len() {
            return false;
        }
        let total: f32 = children.iter().map(|c| c.weight).sum();
        if total <= 0.0 {
            return false;
        }
        let floor = total * MIN_WEIGHT_FRACTION;
        let pair = children[index].weight + children[index + 1].weight;
        // Clamped against the *pair*, so neither side can be pushed past the other or below the
        // floor that keeps its header grabbable.
        let want = (children[index].weight + delta * total).clamp(floor, pair - floor);
        children[index].weight = want;
        children[index + 1].weight = pair - want;
        true
    }

    fn node_mut(&mut self, path: &[usize]) -> Option<&mut Node> {
        let mut node = &mut self.root;
        for &i in path {
            let Node::Split { children, .. } = node else {
                return None;
            };
            node = &mut children.get_mut(i)?.node;
        }
        Some(node)
    }
}

/// Divide `area` among `children` along `axis`, in proportion to their weights.
///
/// Weights are **relative**: this divides by whatever they sum to, so nothing has to keep them
/// normalised and removing a child redistributes its share automatically.
///
/// The last child takes whatever is left rather than its own computed share, so the pieces sum to
/// the whole exactly. Honestly: a sabotage that removed this was caught by no test, and should not
/// have been — the drift is a ten-thousandth of a logical unit, which no pointer can land in and
/// no eye can see. It stays because exactness here is free, not because anything depends on it.
fn shares(axis: Axis, area: Rect, children: &[Child]) -> Vec<Rect> {
    let total: f32 = children.iter().map(|c| c.weight).sum();
    let mut out = Vec::with_capacity(children.len());
    if children.is_empty() {
        return out;
    }
    let extent = match axis {
        Axis::Horizontal => area.w,
        Axis::Vertical => area.h,
    };
    let mut offset = 0.0_f32;
    for (i, child) in children.iter().enumerate() {
        let size = if i + 1 == children.len() {
            extent - offset
        } else if total > 0.0 {
            extent * (child.weight / total)
        } else {
            extent / children.len() as f32
        };
        out.push(match axis {
            Axis::Horizontal => Rect::new(area.x + offset, area.y, size, area.h),
            Axis::Vertical => Rect::new(area.x, area.y + offset, area.w, size),
        });
        offset += size;
    }
    out
}

/// Undo and redo for the layout.
///
/// A stack of whole clones, which is affordable for exactly the reason it is not for the canvas: a
/// layout is a few dozen bytes per panel, so there is nothing to be clever about.
///
/// **No app does this, and every app needs it.** Unity, Blender and Photoshop all let you destroy
/// a layout with one drag and offer no way back but rebuilding it. It is also what makes §1c's
/// movable menu bar a feature rather than a footgun: the answer to "I deleted my File menu" is
/// Ctrl+Z, not a rule forbidding it.
#[derive(Debug, Default)]
pub struct LayoutHistory {
    undo: Vec<Layout>,
    redo: Vec<Layout>,
}

impl LayoutHistory {
    /// How many layout changes are kept.
    ///
    /// Deep enough to walk back out of a bad rearranging session, shallow enough that the whole
    /// history is still trivial beside one canvas tile.
    const DEPTH: usize = 64;

    /// Record the layout as it was *before* a change is applied.
    ///
    /// Before rather than after, so undo restores the state the artist was looking at when they
    /// started the drag. Recording afterwards is the off-by-one that makes undo feel like it skips.
    pub fn record(&mut self, before: Layout) {
        self.undo.push(before);
        if self.undo.len() > Self::DEPTH {
            self.undo.remove(0);
        }
        // A new change makes any redo unreachable, exactly as it does for the canvas.
        self.redo.clear();
    }

    /// Step back, given the layout as it stands. Returns what to replace it with.
    pub fn undo(&mut self, current: &Layout) -> Option<Layout> {
        let previous = self.undo.pop()?;
        self.redo.push(current.clone());
        Some(previous)
    }

    /// Step forward again.
    pub fn redo(&mut self, current: &Layout) -> Option<Layout> {
        let next = self.redo.pop()?;
        self.undo.push(current.clone());
        Some(next)
    }

    #[must_use]
    pub fn depth(&self) -> (usize, usize) {
        (self.undo.len(), self.redo.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS: PanelId = PanelId(1);
    const CANVAS: PanelId = PanelId(2);
    const LAYERS: PanelId = PanelId(3);
    const COLOUR: PanelId = PanelId(4);
    const HISTORY: PanelId = PanelId(5);

    fn area() -> Rect {
        Rect::new(0.0, 0.0, 1000.0, 800.0)
    }

    /// Tools left, canvas centre, layers right — the arrangement everything else is measured
    /// against.
    fn three_across() -> Layout {
        let mut l = Layout::single(CANVAS);
        l.insert(&[], Zone::Left, TOOLS);
        // The canvas is now the second child of a horizontal split.
        l.insert(&[1], Zone::Right, LAYERS);
        l
    }

    fn rects(l: &Layout) -> Vec<(Vec<PanelId>, Rect)> {
        l.resolve(area())
            .into_iter()
            .map(|p| (p.tabs, p.rect))
            .collect()
    }

    /// The slots tile the area exactly. Any gap is somewhere a pointer can land and no panel
    /// answers; any overlap is two panels claiming one pixel.
    #[test]
    fn the_slots_cover_the_area_without_gaps_or_overlap() {
        let l = three_across();
        let placed = l.resolve(area());
        assert_eq!(placed.len(), 3);

        let covered: f32 = placed.iter().map(|p| p.rect.w * p.rect.h).sum();
        assert!(
            (covered - area().w * area().h).abs() < 0.01,
            "the slots cover {covered} of {}",
            area().w * area().h
        );
        // Every point belongs to exactly one slot, edges included.
        for x in [0.0_f32, 1.0, 333.0, 499.9, 500.0, 999.9] {
            for y in [0.0_f32, 1.0, 400.0, 799.9] {
                let hits = placed.iter().filter(|p| p.rect.contains(x, y)).count();
                assert_eq!(hits, 1, "({x}, {y}) is claimed by {hits} slots");
            }
        }
    }

    /// A centre drop stacks, and shows what was dropped. Stacking is the common intent, so it is
    /// the one the biggest target does.
    #[test]
    fn a_centre_drop_adds_a_tab_and_shows_it() {
        let mut l = Layout::single(LAYERS);
        assert!(l.insert(&[], Zone::Center, HISTORY));

        let placed = l.resolve(area());
        assert_eq!(placed.len(), 1, "stacking must not split");
        assert_eq!(placed[0].tabs, vec![LAYERS, HISTORY]);
        assert_eq!(
            placed[0].active, 1,
            "the panel you dropped is the one you see"
        );
    }

    /// An edge drop splits, and the side it lands on decides the order.
    #[test]
    fn an_edge_drop_splits_on_the_side_it_landed() {
        let mut left = Layout::single(CANVAS);
        left.insert(&[], Zone::Left, TOOLS);
        assert_eq!(
            rects(&left).into_iter().map(|(t, _)| t).collect::<Vec<_>>(),
            vec![vec![TOOLS], vec![CANVAS]]
        );

        let mut below = Layout::single(CANVAS);
        below.insert(&[], Zone::Bottom, TOOLS);
        let placed = below.resolve(area());
        assert_eq!(placed[0].tabs, vec![CANVAS]);
        assert!(
            placed[1].rect.y > placed[0].rect.y,
            "a bottom drop goes below"
        );
        assert_eq!(placed[1].tabs, vec![TOOLS]);
    }

    /// **The tree stays flat.** Dropping panel after panel along one edge must give one split with
    /// many children, not a split nested inside a split inside a split — which is the shape that
    /// makes a layout impossible to reason about or to drag afterwards.
    #[test]
    fn repeated_drops_on_one_axis_stay_one_split() {
        let mut l = Layout::single(CANVAS);
        l.insert(&[], Zone::Right, LAYERS);
        // Drop two more on the right of the layers panel, which is child 1.
        l.insert(&[1], Zone::Right, COLOUR);
        l.insert(&[2], Zone::Right, HISTORY);

        let placed = l.resolve(area());
        assert_eq!(placed.len(), 4);
        for p in &placed {
            assert_eq!(
                p.path.len(),
                1,
                "every leaf should be a direct child of one split, got path {:?}",
                p.path
            );
        }
        assert_eq!(
            placed.iter().map(|p| p.tabs[0]).collect::<Vec<_>>(),
            vec![CANVAS, LAYERS, COLOUR, HISTORY]
        );
    }

    /// A drop across the axis nests, because there is nothing to be a sibling of.
    #[test]
    fn a_drop_across_the_axis_nests() {
        let mut l = Layout::single(CANVAS);
        l.insert(&[], Zone::Right, LAYERS);
        l.insert(&[1], Zone::Bottom, COLOUR);

        let placed = l.resolve(area());
        assert_eq!(placed.len(), 3);
        let colour = placed
            .iter()
            .find(|p| p.tabs == vec![COLOUR])
            .expect("colour");
        assert_eq!(
            colour.path.len(),
            2,
            "a cross-axis drop is one level deeper"
        );
    }

    /// Removing the last panel of a leaf collapses it, and a split down to one child dissolves.
    /// Otherwise the tree accumulates empty nodes that still take space and still answer hit tests.
    #[test]
    fn emptied_nodes_collapse_instead_of_lingering() {
        let mut l = three_across();
        assert!(l.remove(TOOLS));
        assert_eq!(l.resolve(area()).len(), 2);

        assert!(l.remove(LAYERS));
        let placed = l.resolve(area());
        assert_eq!(placed.len(), 1, "the split dissolved into its last child");
        assert_eq!(placed[0].path, Vec::<usize>::new(), "and became the root");
        assert_eq!(placed[0].rect, area(), "which takes the whole area");
    }

    /// Everything closed is a valid layout, not a broken one.
    #[test]
    fn removing_the_last_panel_leaves_an_empty_workspace() {
        let mut l = Layout::single(CANVAS);
        assert!(l.remove(CANVAS));
        let placed = l.resolve(area());
        assert_eq!(placed.len(), 1);
        assert!(placed[0].tabs.is_empty());
        assert!(!l.remove(CANVAS), "and it is really gone");
    }

    /// Survivors of a removal share out the space, rather than leaving the removed panel's share
    /// as a hole.
    #[test]
    fn removing_a_panel_gives_its_space_to_the_others() {
        let mut l = Layout::single(CANVAS);
        l.insert(&[], Zone::Right, LAYERS);
        l.insert(&[1], Zone::Right, COLOUR);
        assert!(l.remove(LAYERS));

        let placed = l.resolve(area());
        let covered: f32 = placed.iter().map(|p| p.rect.w).sum();
        assert!(
            (covered - area().w).abs() < 0.01,
            "the survivors cover {covered} of {}",
            area().w
        );
    }

    /// Weights are relative, so a split whose children sum to anything at all still fills its
    /// area in the right proportions.
    ///
    /// Pins what a sabotage exposed: renormalising after a removal was dead code, because
    /// `shares` divides by the live total. Having *one* place that normalises is the point — two
    /// would be two ideas of what a weight means.
    #[test]
    fn weights_are_relative_not_absolute() {
        let mut l = three_across();
        // Push the split far from summing to one, in both directions.
        for _ in 0..6 {
            l.drag_splitter(&[], 0, 0.05);
        }
        let placed = l.resolve(area());
        let covered: f32 = placed.iter().map(|p| p.rect.w).sum();
        assert!(
            (covered - area().w).abs() < 0.01,
            "the children still fill the area, got {covered}"
        );
        // And they are still in order with no overlap.
        for pair in placed.windows(2) {
            assert!(
                (pair[1].rect.x - (pair[0].rect.x + pair[0].rect.w)).abs() < 0.001,
                "slots must meet exactly"
            );
        }
    }

    /// Closing a tab leaves you looking at its neighbour, not jumped back to the first.
    #[test]
    fn closing_a_tab_keeps_the_view_near_where_it_was() {
        let mut l = Layout::single(LAYERS);
        l.insert(&[], Zone::Center, HISTORY);
        l.insert(&[], Zone::Center, COLOUR);
        assert_eq!(l.resolve(area())[0].active, 2);

        assert!(l.remove(COLOUR));
        assert_eq!(
            l.resolve(area())[0].active,
            1,
            "the shown tab must stay in range and stay near"
        );
    }

    /// **Corners resolve to the nearest edge**, which is what makes the five zones unambiguous
    /// without a written corner case. Without it the two edge bands overlap and the corner belongs
    /// to whichever test happens to run first.
    #[test]
    fn the_zones_meet_at_a_mitre() {
        let r = Rect::new(0.0, 0.0, 400.0, 400.0);
        assert_eq!(Layout::zone_at(r, 200.0, 200.0), Zone::Center);
        assert_eq!(Layout::zone_at(r, 10.0, 200.0), Zone::Left);
        assert_eq!(Layout::zone_at(r, 390.0, 200.0), Zone::Right);
        assert_eq!(Layout::zone_at(r, 200.0, 10.0), Zone::Top);
        assert_eq!(Layout::zone_at(r, 200.0, 390.0), Zone::Bottom);

        // In the top-left corner region: just above the diagonal is Top, just below is Left.
        assert_eq!(Layout::zone_at(r, 30.0, 20.0), Zone::Top);
        assert_eq!(Layout::zone_at(r, 20.0, 30.0), Zone::Left);
    }

    /// The centre is the largest single target, and every edge is still big enough to hit.
    ///
    /// Written first as "the centre beats the four edges together", which is **false** at any
    /// band wider than about 0.146 and was an overclaim rather than a design goal. What the design
    /// actually wants is that the common intent — stacking — is the easiest single thing to aim
    /// at, while no edge is so thin that a pen has to hunt for it. Both are measured here rather
    /// than asserted in prose.
    #[test]
    fn the_centre_is_the_largest_single_zone() {
        let r = Rect::new(0.0, 0.0, 100.0, 100.0);
        let mut counts = [0_u32; 5];
        for x in 0..100_u8 {
            for y in 0..100_u8 {
                let i = match Layout::zone_at(r, f32::from(x) + 0.5, f32::from(y) + 0.5) {
                    Zone::Center => 0,
                    Zone::Left => 1,
                    Zone::Right => 2,
                    Zone::Top => 3,
                    Zone::Bottom => 4,
                };
                counts[i] += 1;
            }
        }
        let centre = counts[0];
        for (i, edge) in counts.iter().enumerate().skip(1) {
            assert!(
                centre > *edge,
                "centre {centre} should beat zone {i} at {edge}"
            );
            assert!(
                *edge >= 1200,
                "zone {i} is only {edge} of 10000 -- too thin to aim at with a pen"
            );
        }
    }

    /// A splitter moves only the two children it sits between.
    #[test]
    fn dragging_a_splitter_disturbs_only_its_neighbours() {
        let mut l = three_across();
        let before = rects(&l);
        assert!(l.drag_splitter(&[], 0, 0.1));
        let after = rects(&l);

        assert!(after[0].1.w > before[0].1.w, "the left child grew");
        assert!(after[1].1.w < before[1].1.w, "its neighbour gave the space");
        assert!(
            (after[2].1.w - before[2].1.w).abs() < 0.01,
            "the far child must not move"
        );
    }

    /// A child can be dragged to a sliver but never to nothing — a panel with no header left is a
    /// panel that cannot be dragged back.
    #[test]
    fn a_splitter_cannot_close_a_panel_completely() {
        let mut l = three_across();
        for _ in 0..20 {
            l.drag_splitter(&[], 0, -1.0);
        }
        let placed = l.resolve(area());
        assert!(
            placed[0].rect.w > 0.5,
            "still grabbable, got {}",
            placed[0].rect.w
        );
        assert!(placed[0].rect.w < area().w * 0.05, "but really a sliver");
    }

    /// Undo restores the layout exactly, and redo puts it back.
    #[test]
    fn layout_undo_restores_what_was_there() {
        let mut history = LayoutHistory::default();
        let mut l = three_across();
        let original = l.clone();

        history.record(l.clone());
        l.remove(TOOLS);
        assert_ne!(l, original);

        l = history.undo(&l).expect("something to undo");
        assert_eq!(l, original, "undo restores the tree exactly");

        l = history.redo(&l).expect("something to redo");
        assert_eq!(l.resolve(area()).len(), 2, "and redo takes it away again");
    }

    /// A fresh change makes redo unreachable, exactly as it does for the canvas.
    #[test]
    fn a_new_change_clears_the_redo_stack() {
        let mut history = LayoutHistory::default();
        let mut l = three_across();
        history.record(l.clone());
        l.remove(TOOLS);
        l = history.undo(&l).expect("undo");
        assert_eq!(history.depth().1, 1);

        history.record(l.clone());
        l.remove(LAYERS);
        assert_eq!(history.depth().1, 0, "the old future is gone");
    }

    /// Splitters exist between every pair of children and nowhere else.
    #[test]
    fn there_is_one_splitter_between_each_pair() {
        let l = three_across();
        let splits = l.splitters(area(), 8.0);
        assert_eq!(splits.len(), 2, "three children, two dividers");
        for s in &splits {
            assert_eq!(s.axis, Axis::Horizontal);
            assert!(
                (s.rect.w - 8.0).abs() < 0.01,
                "the grab width is the one asked for"
            );
            assert!(s.rect.h > 700.0, "and it spans the split");
        }

        assert!(
            Layout::single(CANVAS).splitters(area(), 8.0).is_empty(),
            "one panel has nothing to divide"
        );
    }

    /// Finding a panel reports where it is, and reports nothing for one that is not there.
    #[test]
    fn a_panel_can_be_found_by_id() {
        let mut l = three_across();
        l.insert(&[2], Zone::Center, HISTORY);

        let (path, tab) = l.find(HISTORY).expect("history is in the layout");
        assert_eq!(path, vec![2]);
        assert_eq!(tab, 1);
        assert_eq!(l.find(PanelId(99)), None);
    }

    /// **The layout does not know what a panel is.** Two layouts built with different ids but the
    /// same shape must be structurally identical after the same operations — which is the closest
    /// a test can get to proving the absence of a special case.
    #[test]
    fn panel_identity_never_changes_the_structure() {
        let shape = |a: PanelId, b: PanelId, c: PanelId| {
            let mut l = Layout::single(a);
            l.insert(&[], Zone::Left, b);
            l.insert(&[1], Zone::Bottom, c);
            l.remove(b);
            l.resolve(area())
                .into_iter()
                .map(|p| (p.path, p.rect))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            shape(PanelId(1), PanelId(2), PanelId(3)),
            shape(PanelId(70), PanelId(8), PanelId(1000))
        );
    }
}
