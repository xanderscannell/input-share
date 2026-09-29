// Edge crossing and y mapping between screens. See DESIGN.md "How it works".

/// A virtual desktop: origin may be negative on multi-monitor setups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    fn right(&self) -> i32 {
        self.left + self.w - 1
    }
    fn bottom(&self) -> i32 {
        self.top + self.h - 1
    }

    /// 0.0 at the top pixel row, 1.0 at the bottom one.
    pub fn y_frac(&self, y: i32) -> f32 {
        if self.h <= 1 {
            return 0.0;
        }
        ((y - self.top) as f32 / (self.h - 1) as f32).clamp(0.0, 1.0)
    }

    pub fn y_from_frac(&self, frac: f32) -> i32 {
        self.top + (frac.clamp(0.0, 1.0) * (self.h - 1) as f32).round() as i32
    }
}

/// Which server edge leads to the client. The client leaves by the opposite edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
}

/// Server side: does this cursor x touch (or pass) the crossing edge?
pub fn server_hit(edge: Edge, desk: Rect, x: i32) -> bool {
    match edge {
        Edge::Right => x >= desk.right(),
        Edge::Left => x <= desk.left,
    }
}

/// Server side: where the cursor reappears on `Leave`.
pub fn server_return_point(edge: Edge, desk: Rect, y_frac: f32) -> (i32, i32) {
    // Step one pixel in from the edge so the return does not immediately re-cross.
    let x = match edge {
        Edge::Right => desk.right() - 1,
        Edge::Left => desk.left + 1,
    };
    (x, desk.y_from_frac(y_frac))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Move(i32, i32),
    Leave(f32),
}

/// Client side: tracks the cursor while the server is Remote.
pub struct ClientCursor {
    /// The server's crossing edge; the client's facing edge is the opposite.
    edge: Edge,
    screen: Rect,
    x: i32,
    y: i32,
}

impl ClientCursor {
    pub fn enter(edge: Edge, screen: Rect, y_frac: f32) -> Self {
        let x = match edge {
            Edge::Right => screen.left,
            Edge::Left => screen.right(),
        };
        ClientCursor { edge, screen, x, y: screen.y_from_frac(y_frac) }
    }

    pub fn pos(&self) -> (i32, i32) {
        (self.x, self.y)
    }

    /// Apply a delta. Pushing past the edge facing the server means Leave.
    pub fn apply(&mut self, dx: i32, dy: i32) -> Step {
        let nx = self.x.saturating_add(dx);
        let past = match self.edge {
            Edge::Right => nx < self.screen.left,
            Edge::Left => nx > self.screen.right(),
        };
        self.y = self.y.saturating_add(dy).clamp(self.screen.top, self.screen.bottom());
        if past {
            return Step::Leave(self.screen.y_frac(self.y));
        }
        self.x = nx.clamp(self.screen.left, self.screen.right());
        Step::Move(self.x, self.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESK: Rect = Rect { left: 0, top: 0, w: 1920, h: 1080 };
    const LAPTOP: Rect = Rect { left: 0, top: 0, w: 2560, h: 1600 };

    #[test]
    fn server_edge_exact_and_past() {
        assert!(!server_hit(Edge::Right, DESK, 1918));
        assert!(server_hit(Edge::Right, DESK, 1919));
        assert!(server_hit(Edge::Right, DESK, 1920));
        let neg = Rect { left: -1920, top: 0, w: 3840, h: 1080 };
        assert!(!server_hit(Edge::Left, neg, -1919));
        assert!(server_hit(Edge::Left, neg, -1920));
        assert!(server_hit(Edge::Left, neg, -1921));
    }

    #[test]
    fn y_maps_1920x1080_to_2560x1600_and_back() {
        for (sy, cy) in [(0, 0), (1079, 1599), (540, 800)] {
            let c = ClientCursor::enter(Edge::Right, LAPTOP, DESK.y_frac(sy));
            assert_eq!(c.pos(), (0, cy), "server y {sy}");
        }
        assert_eq!(server_return_point(Edge::Right, DESK, LAPTOP.y_frac(1599)), (1918, 1079));
        assert_eq!(server_return_point(Edge::Right, DESK, LAPTOP.y_frac(800)), (1918, 540));
    }

    #[test]
    fn y_frac_handles_offset_origin_and_clamps() {
        let r = Rect { left: 0, top: -200, w: 100, h: 201 };
        assert_eq!(r.y_frac(-200), 0.0);
        assert_eq!(r.y_frac(0), 1.0);
        assert_eq!(r.y_frac(500), 1.0);
        assert_eq!(r.y_from_frac(0.5), -100);
    }

    #[test]
    fn client_clamps_to_screen() {
        let mut c = ClientCursor::enter(Edge::Right, LAPTOP, 0.0);
        assert_eq!(c.apply(10_000, -50), Step::Move(2559, 0));
        assert_eq!(c.apply(0, 10_000), Step::Move(2559, 1599));
        assert_eq!(c.apply(i32::MAX, i32::MAX), Step::Move(2559, 1599));
    }

    #[test]
    fn client_leaves_only_past_facing_edge() {
        let mut c = ClientCursor::enter(Edge::Right, LAPTOP, 0.5);
        assert_eq!(c.apply(0, 0), Step::Move(0, 800)); // exactly on the edge: stays
        assert_eq!(c.apply(-1, 0), Step::Leave(LAPTOP.y_frac(800))); // one past: leaves
        let mut c = ClientCursor::enter(Edge::Right, LAPTOP, 0.5);
        c.apply(5, 0);
        assert_eq!(c.apply(-5, 0), Step::Move(0, 800));
        assert_eq!(c.apply(-6, 100), Step::Leave(LAPTOP.y_frac(900)));

        let mut c = ClientCursor::enter(Edge::Left, LAPTOP, 0.0);
        assert_eq!(c.pos(), (2559, 0));
        assert_eq!(c.apply(0, 0), Step::Move(2559, 0));
        assert_eq!(c.apply(1, 0), Step::Leave(0.0));
    }
}
