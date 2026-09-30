// Screen rectangles and the crossing edge. Crossing logic lives in layout.rs.

/// A monitor or virtual desktop: origin may be negative on multi-monitor setups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn right(&self) -> i32 {
        self.left + self.w - 1
    }
    pub fn bottom(&self) -> i32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn y_frac_handles_offset_origin_and_clamps() {
        let r = Rect { left: 0, top: -200, w: 100, h: 201 };
        assert_eq!(r.y_frac(-200), 0.0);
        assert_eq!(r.y_frac(0), 1.0);
        assert_eq!(r.y_frac(500), 1.0);
        assert_eq!(r.y_from_frac(0.5), -100);
    }
}
