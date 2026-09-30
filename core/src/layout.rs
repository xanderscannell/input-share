// The real monitor layout, replacing v1's single virtual-desktop rectangle.
//
// Crossing and landing use the monitor that is outermost at the cursor's
// height, so a small monitor on the right no longer blocks crossing from a
// taller one beside it, and a return lands on a real monitor instead of in
// empty space. Heights still travel as a fraction of the whole desktop.

use crate::edge::{Edge, Rect};

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub monitors: Vec<Rect>,
    pub primary: usize,
}

fn covers(m: &Rect, y: i32) -> bool {
    m.top <= y && y <= m.bottom()
}

fn opposite(e: Edge) -> Edge {
    match e {
        Edge::Left => Edge::Right,
        Edge::Right => Edge::Left,
    }
}

/// How far out a monitor reaches toward `side` (bigger is further out).
fn reach(m: &Rect, side: Edge) -> i32 {
    match side {
        Edge::Right => m.right(),
        Edge::Left => -m.left,
    }
}

impl Layout {
    pub fn single(r: Rect) -> Layout {
        Layout { monitors: vec![r], primary: 0 }
    }

    /// The bounding rectangle of all monitors (Windows' virtual screen).
    pub fn bounds(&self) -> Rect {
        let left = self.monitors.iter().map(|m| m.left).min().unwrap_or(0);
        let top = self.monitors.iter().map(|m| m.top).min().unwrap_or(0);
        let right = self.monitors.iter().map(|m| m.right()).max().unwrap_or(0);
        let bottom = self.monitors.iter().map(|m| m.bottom()).max().unwrap_or(0);
        Rect { left, top, w: right - left + 1, h: bottom - top + 1 }
    }

    pub fn y_frac(&self, y: i32) -> f32 {
        self.bounds().y_frac(y)
    }

    /// The monitor to use at height `y` on `side`: the outermost one covering
    /// `y`, or if none does (a gap, or past the edges) the vertically nearest,
    /// with `y` clamped into it.
    fn landing(&self, side: Edge, y: i32) -> (Rect, i32) {
        let dist = |m: &Rect| if covers(m, y) { 0 } else { (m.top - y).abs().min((m.bottom() - y).abs()) };
        let best = self.monitors.iter().map(dist).min().unwrap_or(0);
        let m = *self
            .monitors
            .iter()
            .filter(|m| dist(m) == best)
            .max_by_key(|m| reach(m, side))
            .expect("a layout has at least one monitor");
        (m, y.clamp(m.top, m.bottom()))
    }

    /// Server: is this cursor at (or past) the crossing edge for its height?
    pub fn crossing_hit(&self, edge: Edge, x: i32, y: i32) -> bool {
        let (m, _) = self.landing(edge, y);
        match edge {
            Edge::Right => x >= m.right(),
            Edge::Left => x <= m.left,
        }
    }

    /// Server: where the cursor reappears on `Leave`, one pixel in from the
    /// edge so it does not immediately re-cross.
    pub fn return_point(&self, edge: Edge, y_frac: f32) -> (i32, i32) {
        let (m, y) = self.landing(edge, self.bounds().y_from_frac(y_frac));
        let x = match edge {
            Edge::Right => m.right() - 1,
            Edge::Left => m.left + 1,
        };
        (x, y)
    }

    /// Server: where the cursor parks while control is remote. The primary
    /// monitor's center, since the bounding box's center can fall in a gap.
    pub fn park(&self) -> (i32, i32) {
        let m = self.monitors.get(self.primary).or(self.monitors.first()).copied().unwrap_or(self.bounds());
        (m.left + m.w / 2, m.top + m.h / 2)
    }

    /// The nearest point that is on a real monitor.
    pub fn clamp(&self, x: i32, y: i32) -> (i32, i32) {
        self.monitors
            .iter()
            .map(|m| (x.clamp(m.left, m.right()), y.clamp(m.top, m.bottom())))
            .min_by_key(|&(cx, cy)| {
                let (dx, dy) = ((cx - x) as i64, (cy - y) as i64);
                dx * dx + dy * dy
            })
            .unwrap_or((x, y))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Move(i32, i32),
    Leave(f32),
}

/// Client side: tracks the cursor while control is here.
pub struct ClientCursor {
    /// The server's crossing edge; the client's facing edge is the opposite.
    edge: Edge,
    layout: Layout,
    x: i32,
    y: i32,
}

impl ClientCursor {
    pub fn enter(edge: Edge, layout: Layout, y_frac: f32) -> Self {
        let facing = opposite(edge);
        let (m, y) = layout.landing(facing, layout.bounds().y_from_frac(y_frac));
        let x = match facing {
            Edge::Left => m.left,
            Edge::Right => m.right(),
        };
        ClientCursor { edge, layout, x, y }
    }

    pub fn pos(&self) -> (i32, i32) {
        (self.x, self.y)
    }

    /// Apply a delta. Pushing past the edge facing the server means Leave.
    pub fn apply(&mut self, dx: i32, dy: i32) -> Step {
        let (nx, ny) = (self.x.saturating_add(dx), self.y.saturating_add(dy));
        // Where the cursor really ends up. The facing edge is judged at that
        // height: judging it at the raw `ny` would call a move off the bottom
        // of a short monitor a Leave whenever a taller one sits beside it.
        let (cx, cy) = self.layout.clamp(nx, ny);
        let facing = opposite(self.edge);
        let (m, _) = self.layout.landing(facing, cy);
        let past = match facing {
            Edge::Left => nx < m.left,
            Edge::Right => nx > m.right(),
        };
        self.y = cy;
        if past {
            return Step::Leave(self.layout.y_frac(cy));
        }
        self.x = cx;
        Step::Move(cx, cy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESK: Rect = Rect { left: 0, top: 0, w: 1920, h: 1080 };
    const LAPTOP: Rect = Rect { left: 0, top: 0, w: 2560, h: 1600 };

    fn one(r: Rect) -> Layout {
        Layout::single(r)
    }

    // ---- Single monitor: v1 behavior, unchanged. ----

    #[test]
    fn single_edge_exact_and_past() {
        assert!(!one(DESK).crossing_hit(Edge::Right, 1918, 500));
        assert!(one(DESK).crossing_hit(Edge::Right, 1919, 500));
        assert!(one(DESK).crossing_hit(Edge::Right, 1920, 500));
        let neg = one(Rect { left: -1920, top: 0, w: 3840, h: 1080 });
        assert!(!neg.crossing_hit(Edge::Left, -1919, 500));
        assert!(neg.crossing_hit(Edge::Left, -1920, 500));
        assert!(neg.crossing_hit(Edge::Left, -1921, 500));
    }

    #[test]
    fn single_y_maps_1920x1080_to_2560x1600_and_back() {
        for (sy, cy) in [(0, 0), (1079, 1599), (540, 800)] {
            let c = ClientCursor::enter(Edge::Right, one(LAPTOP), one(DESK).y_frac(sy));
            assert_eq!(c.pos(), (0, cy), "server y {sy}");
        }
        assert_eq!(one(DESK).return_point(Edge::Right, LAPTOP.y_frac(1599)), (1918, 1079));
        assert_eq!(one(DESK).return_point(Edge::Right, LAPTOP.y_frac(800)), (1918, 540));
        assert_eq!(one(DESK).park(), (960, 540));
    }

    #[test]
    fn single_client_clamps_to_screen() {
        let mut c = ClientCursor::enter(Edge::Right, one(LAPTOP), 0.0);
        assert_eq!(c.apply(10_000, -50), Step::Move(2559, 0));
        assert_eq!(c.apply(0, 10_000), Step::Move(2559, 1599));
        assert_eq!(c.apply(i32::MAX, i32::MAX), Step::Move(2559, 1599));
    }

    #[test]
    fn single_client_leaves_only_past_facing_edge() {
        let mut c = ClientCursor::enter(Edge::Right, one(LAPTOP), 0.5);
        assert_eq!(c.apply(0, 0), Step::Move(0, 800)); // exactly on the edge: stays
        assert_eq!(c.apply(-1, 0), Step::Leave(LAPTOP.y_frac(800))); // one past: leaves
        let mut c = ClientCursor::enter(Edge::Right, one(LAPTOP), 0.5);
        c.apply(5, 0);
        assert_eq!(c.apply(-5, 0), Step::Move(0, 800));
        assert_eq!(c.apply(-6, 100), Step::Leave(LAPTOP.y_frac(900)));

        let mut c = ClientCursor::enter(Edge::Left, one(LAPTOP), 0.0);
        assert_eq!(c.pos(), (2559, 0));
        assert_eq!(c.apply(0, 0), Step::Move(2559, 0));
        assert_eq!(c.apply(1, 0), Step::Leave(0.0));
    }

    // ---- The reported layout: a large primary with a small monitor to its
    // right that covers only its lower part (bottom edges aligned). ----

    const BIG: Rect = Rect { left: 0, top: 0, w: 2560, h: 1440 };
    const SMALL: Rect = Rect { left: 2560, top: 360, w: 1920, h: 1080 };

    fn reported() -> Layout {
        Layout { monitors: vec![BIG, SMALL], primary: 0 }
    }

    #[test]
    fn reported_crossing_works_from_the_large_monitor_above_the_small_one() {
        let l = reported();
        assert!(l.crossing_hit(Edge::Right, 2559, 100)); // v1 needed x 4479 here: unreachable
        assert!(!l.crossing_hit(Edge::Right, 2558, 100));
        // Where the small monitor exists, the crossing edge is its right edge.
        assert!(!l.crossing_hit(Edge::Right, 2559, 1000));
        assert!(l.crossing_hit(Edge::Right, 4479, 1000));
    }

    #[test]
    fn reported_return_lands_on_a_real_monitor_at_the_same_height() {
        let l = reported();
        // High up: the large monitor's right edge, not the main display's middle.
        assert_eq!(l.return_point(Edge::Right, l.y_frac(100)), (2558, 100));
        // Low down: the small monitor.
        assert_eq!(l.return_point(Edge::Right, l.y_frac(1000)), (4478, 1000));
        // Every return lands on a monitor.
        for i in 0..=20 {
            let (x, y) = l.return_point(Edge::Right, i as f32 / 20.0);
            assert_eq!(l.clamp(x, y), (x, y), "return {i}/20 landed off-monitor at {x},{y}");
        }
        assert_eq!(l.park(), (1280, 720));
    }

    // ---- The user's desktop exactly as `win::layout()` read it: three
    // 2560x1440 monitors and an 800x480 one at the far right, near the bottom. ----

    fn measured() -> Layout {
        Layout {
            monitors: vec![
                Rect { left: 0, top: 0, w: 2560, h: 1440 },
                Rect { left: -2560, top: 0, w: 2560, h: 1440 },
                Rect { left: 2560, top: 0, w: 2560, h: 1440 },
                Rect { left: 5120, top: 959, w: 800, h: 480 },
            ],
            primary: 0,
        }
    }

    #[test]
    fn measured_desktop_crossing_and_return() {
        let l = measured();
        // v1's single rectangle put the edge at x 5919 at every height, which
        // only the small display reaches, and returned high up to x 5918, where
        // there is no monitor (Windows then moved the cursor to the main display).
        assert!(l.crossing_hit(Edge::Right, 5119, 100));
        assert_eq!(l.return_point(Edge::Right, l.y_frac(100)), (5118, 100));
        assert!(!l.crossing_hit(Edge::Right, 5119, 1200));
        assert!(l.crossing_hit(Edge::Right, 5919, 1200));
        assert_eq!(l.return_point(Edge::Right, l.y_frac(1200)), (5918, 1200));
        // The bottom row sits one pixel below the small display.
        assert_eq!(l.return_point(Edge::Right, 1.0), (5118, 1439));
        assert_eq!(l.park(), (1280, 720));
        for i in 0..=40 {
            let (x, y) = l.return_point(Edge::Right, i as f32 / 40.0);
            assert_eq!(l.clamp(x, y), (x, y), "return {i}/40 landed off-monitor at {x},{y}");
        }
    }

    // ---- A diagonal layout with a height gap, and a monitor at negative x. ----

    #[test]
    fn gap_between_monitors_uses_the_nearest_one() {
        let a = Rect { left: 0, top: 0, w: 1920, h: 1080 };
        let b = Rect { left: 1920, top: 1200, w: 1920, h: 1080 };
        let l = Layout { monitors: vec![a, b], primary: 0 };
        // y 1100 is in neither: A (21 px away) is nearer than B (100 px).
        assert_eq!(l.return_point(Edge::Right, l.y_frac(1100)), (1918, 1079));
        assert_eq!(l.return_point(Edge::Right, l.y_frac(1180)), (3838, 1200));
        assert_eq!(l.clamp(1900, 1100), (1900, 1079));
        // The bounding box's center (1920, 1140) is in the gap; park avoids it.
        assert_eq!(l.park(), (960, 540));
    }

    #[test]
    fn negative_origin_monitor_on_the_left() {
        let left = Rect { left: -1920, top: 0, w: 1920, h: 1080 };
        let main = Rect { left: 0, top: 0, w: 2560, h: 1440 };
        let l = Layout { monitors: vec![left, main], primary: 1 };
        assert!(l.crossing_hit(Edge::Left, -1920, 500));
        assert!(!l.crossing_hit(Edge::Left, 0, 500)); // the left monitor is further out here
        assert!(l.crossing_hit(Edge::Left, 0, 1200)); // below it, main's left edge is outermost
        assert_eq!(l.return_point(Edge::Left, l.y_frac(1200)), (1, 1200));
        assert_eq!(l.park(), (1280, 720));
    }

    #[test]
    fn client_with_two_monitors_enters_leaves_and_never_drifts_into_a_gap() {
        // Laptop panel plus an external monitor to its right, top-aligned.
        let panel = Rect { left: 0, top: 0, w: 1920, h: 1200 };
        let ext = Rect { left: 1920, top: 0, w: 2560, h: 1440 };
        let l = Layout { monitors: vec![panel, ext], primary: 0 };
        // Near the bottom only the external monitor exists: enter on its left edge.
        let mut c = ClientCursor::enter(Edge::Right, l.clone(), l.y_frac(1300));
        assert_eq!(c.pos(), (1920, 1300));
        assert_eq!(c.apply(-1, 0), Step::Leave(l.y_frac(1300)));
        // Up high, enter on the panel; moving down-left out of the panel clamps, not drifts.
        let mut c = ClientCursor::enter(Edge::Right, l.clone(), l.y_frac(100));
        assert_eq!(c.pos(), (0, 100));
        assert_eq!(c.apply(500, 2000), Step::Move(500, 1199)); // straight down off the panel: no Leave
        assert_eq!(c.apply(0, 0), Step::Move(500, 1199));
        assert_eq!(c.apply(-501, 0), Step::Leave(l.y_frac(1199)));
    }
}
