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

/// Which panel a leaf holds. Opaque on purpose — see the module note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PanelId(pub u32);

/// A rectangle in logical UI units, y down.
///
/// Serialisable because a floating panel's position is part of a saved workspace, and a rectangle
/// is the whole of what "where it is" means.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
    /// The size below which this child stops shrinking, in logical units along the split's axis.
    ///
    /// **A menu bar is not a fraction of a window.** Weights alone made every panel scale with the
    /// window, so the same arrangement that looked right at 1400x900 clipped its own menu bar in
    /// half at 900x600 -- found by looking at a screenshot, after a test that was meant to prevent
    /// exactly that checked one size and happened to pick a big one.
    ///
    /// A number rather than something derived from the contents, because the layout must never
    /// branch on which panel it holds (§1c). Whoever builds the arrangement knows what a strip
    /// needs and says so.
    min: f32,
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
                            // A panel arriving by drag has said nothing about a minimum, and
                            // inventing one would freeze it at a size nobody chose.
                            min: 0.0,
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
            min: 0.0,
            node: Node::leaf(vec![panel]),
        };
        let displaced = Child {
            weight: 0.5,
            // The displaced node keeps nothing: it is being wrapped, not resized, and whatever
            // minimum it had belonged to its old parent's axis, which may not be this one.
            min: 0.0,
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

    /// Every panel in this arrangement, in the order the tree holds them.
    ///
    /// For a floating window, which must be taken down when the last panel leaves it: a window
    /// with nothing in it is a rectangle of chrome with no way to tell it is not broken.
    #[must_use]
    pub fn panels(&self) -> Vec<PanelId> {
        let mut out = Vec::new();
        Self::gather(&self.root, &mut out);
        out
    }

    fn gather(node: &Node, out: &mut Vec<PanelId>) {
        match node {
            Node::Leaf { tabs, .. } => out.extend(tabs.iter().copied()),
            Node::Split { children, .. } => {
                for c in children {
                    Self::gather(&c.node, out);
                }
            }
        }
    }

    /// Where a panel currently is, as `(leaf path, tab index)`.
    #[must_use]
    pub fn find(&self, panel: PanelId) -> Option<(Path, usize)> {
        self.resolve(Rect::new(0.0, 0.0, 1.0, 1.0))
            .into_iter()
            .find_map(|p| p.tabs.iter().position(|q| *q == panel).map(|i| (p.path, i)))
    }

    /// Set a child's share of its split directly.
    ///
    /// For building a default arrangement and for restoring a saved one — the two cases where the
    /// proportions are known rather than dragged to. Weights are relative, so callers normally set
    /// every child of a split together and need not make them sum to anything in particular.
    ///
    /// Floored above zero for the same reason a splitter drag is: a child at exactly nothing has
    /// Say how small a child may get, in logical units along its parent's axis.
    ///
    /// **This is what makes a strip a strip.** A menu bar expressed only as a weight grows with
    /// the window and, in a small one, shrinks below its own contents: at 900x600 the default
    /// arrangement clipped its menu bar in half. A minimum is taken out first and the weights
    /// share what is left, so a bar stays the height it needs and the canvas takes the rest.
    pub fn set_min(&mut self, path: &[usize], min: f32) -> bool {
        let Some(index) = path.last().copied() else {
            return false;
        };
        let Some(Node::Split { children, .. }) = self.node_mut(&path[..path.len() - 1]) else {
            return false;
        };
        let Some(child) = children.get_mut(index) else {
            return false;
        };
        child.min = min.max(0.0);
        true
    }

    /// no header left to grab, and a layout you cannot grab is a layout you cannot fix.
    pub fn set_weight(&mut self, path: &[usize], weight: f32) -> bool {
        let Some((index, parent)) = path.split_last() else {
            // The root has no parent to take a share of.
            return false;
        };
        let Some(Node::Split { children, .. }) = self.node_mut(parent) else {
            return false;
        };
        let Some(child) = children.get_mut(*index) else {
            return false;
        };
        // Stored as given. The recoverability floor is applied when the split is *resolved*, not
        // written into the data: doing it here made the answer depend on whether the weight or the
        // minimum was set first, which is the kind of trap that is discovered months later.
        child.weight = weight.max(0.0);
        true
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
    pub fn drag_splitter(&mut self, path: &[usize], index: usize, delta: f32, extent: f32) -> bool {
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
        // **Worked in weights, but only where weights still mean size.** Once a child has a
        // minimum, its share is taken out before the weights are applied, so moving a weight by a
        // fraction no longer moves the divider by that fraction. Rather than pretend otherwise,
        // a pair where either side has a minimum is resized in *units*: the minimum stays put and
        // the delta lands on the weighted side.
        let (a, b) = (children[index].min, children[index + 1].min);
        if a > 0.0 || b > 0.0 {
            // A strip is dragged by changing what it will not shrink below -- which is exactly
            // what the artist means by dragging its edge. Never below zero, and never so far that
            // the other side is pushed out of existence.
            if a > 0.0 {
                children[index].min = (a + delta).max(0.0);
            } else {
                children[index + 1].min = (b - delta).max(0.0);
            }
            return true;
        }
        let floor = total * MIN_WEIGHT_FRACTION;
        let pair = children[index].weight + children[index + 1].weight;
        // Clamped against the *pair*, so neither side can be pushed past the other or below the
        // floor that keeps its header grabbable.
        // Back into weights, where they still mean size: a divider moved this far across a split
        // this wide is that fraction of it.
        let fraction = if extent > 0.0 { delta / extent } else { 0.0 };
        let want = (children[index].weight + fraction * total).clamp(floor, pair - floor);
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
    let mut out = Vec::with_capacity(children.len());
    if children.is_empty() {
        return out;
    }
    let extent = match axis {
        Axis::Horizontal => area.w,
        Axis::Vertical => area.h,
    };
    let sizes = split_extent(extent, children);
    let mut offset = 0.0_f32;
    for (i, size) in sizes.into_iter().enumerate() {
        // The last child takes whatever is left, so the pieces sum to the whole exactly.
        let size = if i + 1 == children.len() {
            extent - offset
        } else {
            size
        };
        out.push(match axis {
            Axis::Horizontal => Rect::new(area.x + offset, area.y, size, area.h),
            Axis::Vertical => Rect::new(area.x, area.y + offset, area.w, size),
        });
        offset += size;
    }
    out
}

/// How much of `extent` each child gets, honouring what each has said it cannot shrink below.
///
/// **Minimums first, then weights on what is left.** A menu bar wants a fixed height and a canvas
/// wants whatever remains; expressing that as weights alone means the bar grows with the window
/// and, worse, shrinks below its own contents in a small one.
///
/// When the minimums cannot all be met the space is shared out in proportion to them, so a window
/// too small for everything degrades evenly instead of starving whichever child happens to be
/// last.
#[must_use]
fn split_extent(extent: f32, children: &[Child]) -> Vec<f32> {
    // Taken as stored, not clamped again here. Every way a minimum can be set clamps it -- and a
    // second guard over the same rule would mean a sabotage that removed either one changed
    // nothing, which is how a guard stops being tested (`panel_ui::change_at` says the same).
    let mins: Vec<f32> = children.iter().map(|c| c.min).collect();
    let needed: f32 = mins.iter().sum();
    if needed > extent && needed > 0.0 {
        return mins.iter().map(|m| extent * (m / needed)).collect();
    }
    let spare = extent - needed;

    // The recoverability floor, applied here rather than stored: a child dragged to nothing has no
    // header left to grab, so the layout could not be undone by hand. A child with a *minimum*
    // cannot reach nothing in the first place, and giving it a floor as well would make it grow
    // with the window -- which is precisely what a strip must not do.
    let stated: f32 = children.iter().map(|c| c.weight.max(0.0)).sum();
    let floor = stated.max(1.0) * MIN_WEIGHT_FRACTION;
    let weights: Vec<f32> = children
        .iter()
        .map(|c| {
            if c.min > 0.0 {
                c.weight.max(0.0)
            } else {
                c.weight.max(floor)
            }
        })
        .collect();
    let total: f32 = weights.iter().sum();

    mins.iter()
        .zip(&weights)
        .map(|(min, weight)| {
            let share = if total > 0.0 {
                spare * (weight / total)
            } else {
                spare / children.len() as f32
            };
            min + share
        })
        .collect()
}

/// A layout as it goes to disk: the same tree, with panels named rather than numbered.
///
/// **Names, not ids.** A [`PanelId`] is whatever position a panel happens to occupy in the app's
/// table, so a saved file full of them would rearrange itself the day a panel is added or removed
/// — silently, and into a workspace nobody asked for. A name survives that.
///
/// The tree itself is unchanged, which is the point: a saved workspace is this structure and
/// nothing more (§1c).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SavedLayout {
    root: SavedNode,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SavedNode {
    Split {
        axis: SavedAxis,
        children: Vec<SavedChild>,
    },
    Leaf {
        panels: Vec<String>,
        active: usize,
    },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct SavedChild {
    weight: f32,
    /// Absent in files written before minimums existed, which is what the default is for.
    #[serde(default)]
    min: f32,
    node: SavedNode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum SavedAxis {
    Horizontal,
    Vertical,
}

impl Layout {
    /// Convert to the on-disk form, naming each panel.
    ///
    /// A panel the caller cannot name is dropped rather than guessed at — the same tolerance a
    /// missing font gets (§6a). What comes back is still a valid layout, just without it.
    #[must_use]
    pub fn to_saved(&self, name_of: impl Fn(PanelId) -> Option<String> + Copy) -> SavedLayout {
        SavedLayout {
            root: save_node(&self.root, name_of),
        }
    }

    /// Rebuild from the on-disk form.
    ///
    /// Names the caller does not recognise are dropped — a workspace saved by a newer build, or
    /// one naming a panel since removed, still opens. It comes back missing that panel rather than
    /// refusing to open at all, which is the only answer that lets someone recover.
    ///
    /// Empty leaves and one-child splits are collapsed afterwards, so dropping panels cannot leave
    /// the tree carrying nodes that hold nothing.
    #[must_use]
    pub fn from_saved(saved: &SavedLayout, id_of: impl Fn(&str) -> Option<PanelId> + Copy) -> Self {
        let mut layout = Self {
            root: load_node(&saved.root, id_of),
        };
        Self::collapse(&mut layout.root);
        layout
    }
}

fn save_node(node: &Node, name_of: impl Fn(PanelId) -> Option<String> + Copy) -> SavedNode {
    match node {
        Node::Leaf { tabs, active } => {
            let panels: Vec<String> = tabs.iter().filter_map(|p| name_of(*p)).collect();
            SavedNode::Leaf {
                active: (*active).min(panels.len().saturating_sub(1)),
                panels,
            }
        }
        Node::Split { axis, children } => SavedNode::Split {
            axis: match axis {
                Axis::Horizontal => SavedAxis::Horizontal,
                Axis::Vertical => SavedAxis::Vertical,
            },
            children: children
                .iter()
                .map(|c| SavedChild {
                    weight: c.weight,
                    min: c.min,
                    node: save_node(&c.node, name_of),
                })
                .collect(),
        },
    }
}

fn load_node(node: &SavedNode, id_of: impl Fn(&str) -> Option<PanelId> + Copy) -> Node {
    match node {
        SavedNode::Leaf { panels, active } => {
            let tabs: Vec<PanelId> = panels.iter().filter_map(|n| id_of(n)).collect();
            Node::Leaf {
                active: (*active).min(tabs.len().saturating_sub(1)),
                tabs,
            }
        }
        SavedNode::Split { axis, children } => Node::Split {
            axis: match axis {
                SavedAxis::Horizontal => Axis::Horizontal,
                SavedAxis::Vertical => Axis::Vertical,
            },
            children: children
                .iter()
                .map(|c| Child {
                    weight: c.weight,
                    // Clamped on the way in: a hand-edited file is the one route a negative
                    // minimum could arrive by, and `shares` trusts what it is given.
                    min: c.min.max(0.0),
                    node: load_node(&c.node, id_of),
                })
                .collect(),
        },
    }
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
/// Generic over what it remembers, because remembering only the *docked* arrangement was a bug:
/// undoing a float put the panel back into the layout and left it floating as well, so it was in
/// two places at once. Whatever counts as "the arrangement" has to be one value, or history keeps
/// half of it.
#[derive(Debug)]
pub struct History<T> {
    undo: Vec<T>,
    redo: Vec<T>,
}

impl<T> Default for History<T> {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }
}

impl<T: Clone> History<T> {
    /// How many layout changes are kept.
    ///
    /// Deep enough to walk back out of a bad rearranging session, shallow enough that the whole
    /// history is still trivial beside one canvas tile.
    const DEPTH: usize = 64;

    /// Record the layout as it was *before* a change is applied.
    ///
    /// Before rather than after, so undo restores the state the artist was looking at when they
    /// started the drag. Recording afterwards is the off-by-one that makes undo feel like it skips.
    pub fn record(&mut self, before: T) {
        self.undo.push(before);
        if self.undo.len() > Self::DEPTH {
            self.undo.remove(0);
        }
        // A new change makes any redo unreachable, exactly as it does for the canvas.
        self.redo.clear();
    }

    /// Step back, given the layout as it stands. Returns what to replace it with.
    pub fn undo(&mut self, current: &T) -> Option<T> {
        let previous = self.undo.pop()?;
        self.redo.push(current.clone());
        Some(previous)
    }

    /// Step forward again.
    pub fn redo(&mut self, current: &T) -> Option<T> {
        let next = self.redo.pop()?;
        self.undo.push(current.clone());
        Some(next)
    }

    /// How much there is to undo and to redo. Read by the tests that pin "a tap records nothing".
    #[cfg(test)]
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
            l.drag_splitter(&[], 0, 0.05, 1.0);
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
        assert!(l.drag_splitter(&[], 0, 0.1, 1.0));
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
            l.drag_splitter(&[], 0, -1.0, 1.0);
        }
        let placed = l.resolve(area());
        assert!(
            placed[0].rect.w > 0.5,
            "still grabbable, got {}",
            placed[0].rect.w
        );
        assert!(placed[0].rect.w < area().w * 0.05, "but really a sliver");
    }

    /// **Every panel, including the ones inside splits.**
    ///
    /// A floating window is taken down when this comes back empty, so a version that only looked
    /// at the top of the tree would take down a window still holding a split full of panels.
    #[test]
    fn every_panel_is_counted_however_deep_it_is() {
        let mut l = three_across();
        // Two deep: a split inside a split inside the root.
        l.insert(&[2], Zone::Bottom, HISTORY);
        l.insert(&[2, 1], Zone::Right, COLOUR);
        l.insert(&[0], Zone::Center, PanelId(9));

        let mut found = l.panels();
        found.sort_by_key(|p| p.0);
        let mut expected = vec![PanelId(1), CANVAS, LAYERS, HISTORY, COLOUR, PanelId(9)];
        expected.sort_by_key(|p| p.0);
        assert_eq!(found, expected);

        // And each appears once, however the tree is shaped.
        for panel in &expected {
            assert_eq!(
                found.iter().filter(|p| *p == panel).count(),
                1,
                "{panel:?} was counted more than once"
            );
        }
    }

    /// **A minimum survives being saved.** Otherwise a workspace reopens with its menu bar as a
    /// fraction again, and the bug this whole mechanism exists to fix comes back on restart.
    #[test]
    fn a_minimum_survives_the_saved_form() {
        let mut original = three_across();
        original.set_min(&[0], 38.0);
        original.set_min(&[1], 0.0);
        original.set_weight(&[2], 0.6);

        let saved = original.to_saved(|p| Some(format!("p{}", p.0)));
        let text = serde_json::to_string(&saved).expect("serialise");
        let back: SavedLayout = serde_json::from_str(&text).expect("parse");
        let restored = Layout::from_saved(&back, |n| {
            n.strip_prefix('p')
                .and_then(|d| d.parse().ok())
                .map(PanelId)
        });
        assert_eq!(restored, original, "the minimum did not come back");

        // And the arrangement it produces is the same one, which is what actually matters.
        let area = Rect::new(0.0, 0.0, 1000.0, 400.0);
        assert_eq!(restored.resolve(area), original.resolve(area));
    }

    /// **A window too small for every minimum shares what there is, evenly.**
    ///
    /// The alternative is handing each child its full minimum in turn until the space runs out,
    /// which starves whichever happens to be last -- and "last" is an accident of how the tree was
    /// built, not something anyone chose.
    #[test]
    fn a_window_too_small_for_its_minimums_shares_what_there_is() {
        let mut l = three_across();
        l.set_min(&[0], 300.0);
        l.set_min(&[1], 300.0);
        l.set_min(&[2], 300.0);

        // Half the room the minimums ask for.
        let area = Rect::new(0.0, 0.0, 450.0, 200.0);
        let placed = l.resolve(area);
        assert_eq!(placed.len(), 3);
        for p in &placed {
            assert!(
                (p.rect.w - 150.0).abs() < 0.01,
                "one child got {} of the 450 available",
                p.rect.w
            );
        }
        // Nothing was lost or invented on the way.
        let total: f32 = placed.iter().map(|p| p.rect.w).sum();
        assert!((total - area.w).abs() < 0.01);
    }

    /// **A panel cannot be dragged away to nothing**, because a panel with no header left has no
    /// way back. The floor is about recoverability, not about size (§1c).
    #[test]
    fn a_panel_dragged_hard_keeps_something_to_grab() {
        let area = Rect::new(0.0, 0.0, 1000.0, 400.0);
        let mut l = three_across();
        // Shove the first divider as far left as it will go, several times over.
        for _ in 0..20 {
            l.drag_splitter(&[], 0, -1000.0, area.w);
        }
        let first = l.resolve(area)[0].rect.w;
        assert!(
            first > 1.0,
            "the first panel was squeezed to {first}, with no header left to grab"
        );

        // And the same in the other direction, for its neighbour.
        for _ in 0..20 {
            l.drag_splitter(&[], 0, 1000.0, area.w);
        }
        let second = l.resolve(area)[1].rect.w;
        assert!(second > 1.0, "its neighbour was squeezed to {second}");
    }

    /// A weight of nothing still leaves something to grab.
    ///
    /// Dragging clamps on its own way in, so that path never reaches zero; setting a weight
    /// directly does, and a child at exactly zero has no header left to take hold of. This is the
    /// floor's real job, and the only route that tests it.
    #[test]
    fn a_weight_of_nothing_still_leaves_a_header_to_grab() {
        let area = Rect::new(0.0, 0.0, 1000.0, 400.0);
        let mut l = three_across();
        l.set_weight(&[0], 0.0);
        let first = l.resolve(area)[0].rect.w;
        assert!(
            first > 1.0,
            "a panel weighted to nothing came out {first} wide, with no way to get it back"
        );

        // A child with a *minimum* is exempt, because it can never reach nothing anyway -- and a
        // floor would make it grow with the window, which is the one thing a strip must not do.
        let mut strip = three_across();
        strip.set_min(&[0], 40.0);
        strip.set_weight(&[0], 0.0);
        let narrow = strip.resolve(Rect::new(0.0, 0.0, 1000.0, 400.0))[0].rect.w;
        let wide = strip.resolve(Rect::new(0.0, 0.0, 4000.0, 400.0))[0].rect.w;
        assert!(
            (narrow - wide).abs() < 0.01,
            "the strip grew from {narrow} to {wide} with the window"
        );
    }

    /// A strip's edge can be dragged, and it cannot be dragged inside out.
    ///
    /// A strip is resized by moving what it will not shrink below -- which is what the artist
    /// means by dragging its edge -- so that is the number that has to stay sane.
    #[test]
    fn a_strip_can_be_resized_but_not_turned_inside_out() {
        let area = Rect::new(0.0, 0.0, 1000.0, 600.0);
        let mut l = Layout::single(PanelId(1));
        l.insert(&[], Zone::Top, PanelId(0));
        l.set_min(&[0], 40.0);
        l.set_weight(&[0], 0.0);
        l.set_weight(&[1], 1.0);
        assert!((l.resolve(area)[0].rect.h - 40.0).abs() < 0.01);

        l.drag_splitter(&[], 0, 25.0, area.h);
        assert!(
            (l.resolve(area)[0].rect.h - 65.0).abs() < 0.01,
            "dragging the strip's edge should have made it taller"
        );

        // Dragged far past its own top, it stops at nothing rather than going negative.
        for _ in 0..10 {
            l.drag_splitter(&[], 0, -500.0, area.h);
        }
        let h = l.resolve(area)[0].rect.h;
        assert!(h >= 0.0, "the strip was dragged inside out to {h}");
    }

    /// A layout survives a round trip through its saved form, weights and tabs included.
    #[test]
    fn a_layout_round_trips_through_its_saved_form() {
        let name = |p: PanelId| Some(format!("panel-{}", p.0));
        let id = |n: &str| {
            n.strip_prefix("panel-")
                .and_then(|d| d.parse().ok())
                .map(PanelId)
        };

        // Both axes, because a layout of only horizontal splits cannot tell whether the axis
        // survived the trip — a sabotage that wrote every split as horizontal passed against
        // exactly that. Tabs and an uneven weight too, so nothing is left to chance.
        let mut original = three_across();
        original.insert(&[2], Zone::Center, HISTORY);
        original.insert(&[1], Zone::Bottom, COLOUR);
        original.drag_splitter(&[], 0, 0.07, 1.0);
        assert!(
            original.split_axis(&[1]) == Some(Axis::Vertical)
                && original.split_axis(&[]) == Some(Axis::Horizontal),
            "the fixture must contain both axes, or this proves less than it looks"
        );

        let saved = original.to_saved(name);
        let back = Layout::from_saved(&saved, id);
        assert_eq!(back, original);
    }

    /// The saved form names panels rather than numbering them, so adding a panel to the app's
    /// table cannot silently rearrange somebody's workspace.
    #[test]
    fn the_saved_form_is_written_in_names() {
        let name = |p: PanelId| Some(format!("panel-{}", p.0));
        let text = serde_json::to_string(&three_across().to_saved(name)).expect("serialises");
        assert!(
            text.contains("panel-1") && !text.contains("\"tabs\""),
            "the file should be readable andname-based, got:\n{text}"
        );
    }

    /// A workspace naming a panel this build does not have still opens, without it.
    ///
    /// The alternative — refusing to open — leaves someone with a file they cannot recover from,
    /// which is a much worse answer than a workspace missing one panel.
    #[test]
    fn an_unknown_panel_is_dropped_rather_than_refused() {
        let name = |p: PanelId| Some(format!("panel-{}", p.0));
        let saved = three_across().to_saved(name);

        // This build knows nothing about panel 1 -- say it was removed since.
        let picky = |n: &str| id_from(n).filter(|p| *p != TOOLS);
        let back = Layout::from_saved(&saved, picky);
        assert!(back.find(TOOLS).is_none(), "the unknown panel is gone");
        assert!(back.find(CANVAS).is_some(), "and the rest survived");
        assert_eq!(
            back.resolve(area()).len(),
            2,
            "the leaf it left behind collapsed rather than lingering"
        );
    }

    fn id_from(n: &str) -> Option<PanelId> {
        n.strip_prefix("panel-")
            .and_then(|d| d.parse().ok())
            .map(PanelId)
    }

    /// A panel the caller cannot name is dropped on the way *out* too, rather than written as
    /// something that will not come back.
    #[test]
    fn an_unnameable_panel_is_dropped_on_the_way_out() {
        let saved = three_across().to_saved(|p| (p != TOOLS).then(|| format!("panel-{}", p.0)));
        let back = Layout::from_saved(&saved, id_from);
        assert!(back.find(TOOLS).is_none());
        assert_eq!(back.resolve(area()).len(), 2);
    }

    /// Undo restores the layout exactly, and redo puts it back.
    #[test]
    fn layout_undo_restores_what_was_there() {
        let mut history: History<Layout> = History::default();
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
        let mut history: History<Layout> = History::default();
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
