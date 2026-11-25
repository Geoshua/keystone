//! Keystone geometry: projector-style H/V keystone plus free four-corner offsets,
//! and the homography math that turns the resulting quad into a shader matrix.
//!
//! Coordinate conventions:
//! * "unit" coords: (0,0) = top-left of the fitted image rect, (1,1) = bottom-right.
//! * "output" coords: (0,0) = top-left of the output window, (1,1) = bottom-right.

use serde::{Deserialize, Serialize};

/// Corner order used everywhere: top-left, top-right, bottom-right, bottom-left.
pub const CORNER_NAMES: [&str; 4] = ["Top left", "Top right", "Bottom right", "Bottom left"];

/// Projector keystone range, in the same "-40..+40" units most projector menus use.
pub const KEYSTONE_RANGE: f32 = 40.0;

/// The user's warp settings. This describes the projector's placement, not the
/// content, so one global setting is persisted and reused for every source.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Warp {
    /// Horizontal keystone, -40..+40. Positive narrows the right edge, negative the left.
    pub horizontal: f32,
    /// Vertical keystone, -40..+40. Positive narrows the top edge, negative the bottom.
    pub vertical: f32,
    /// Free per-corner offsets on top of the keystone, in unit coords (TL, TR, BR, BL).
    pub corner_offsets: [[f32; 2]; 4],
}

impl Default for Warp {
    fn default() -> Self {
        Self {
            horizontal: 0.0,
            vertical: 0.0,
            corner_offsets: [[0.0; 2]; 4],
        }
    }
}

impl Warp {
    /// Final quad corners in unit coords (TL, TR, BR, BL).
    pub fn unit_corners(&self) -> [[f32; 2]; 4] {
        // One keystone step shrinks the far edge by 1% of the image size, so the
        // maximum of 40 narrows an edge to 60% of its length.
        let h = (self.horizontal / 100.0).clamp(-0.4, 0.4);
        let v = (self.vertical / 100.0).clamp(-0.4, 0.4);

        let mut c = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

        // Horizontal keystone: pinch the right (h > 0) or left (h < 0) edge vertically.
        if h > 0.0 {
            c[1][1] += h / 2.0;
            c[2][1] -= h / 2.0;
        } else if h < 0.0 {
            c[0][1] += -h / 2.0;
            c[3][1] -= -h / 2.0;
        }
        // Vertical keystone: pinch the top (v > 0) or bottom (v < 0) edge horizontally.
        if v > 0.0 {
            c[0][0] += v / 2.0;
            c[1][0] -= v / 2.0;
        } else if v < 0.0 {
            c[3][0] += -v / 2.0;
            c[2][0] -= -v / 2.0;
        }

        for (corner, off) in c.iter_mut().zip(self.corner_offsets) {
            corner[0] += off[0];
            corner[1] += off[1];
        }
        c
    }
}

/// Rect (in output coords) that the source occupies before warping.
/// `stretch == false` letterboxes so the source keeps its aspect ratio.
pub fn fit_rect(output_px: [f32; 2], source_px: [f32; 2], stretch: bool) -> [[f32; 2]; 2] {
    if stretch || output_px[0] <= 0.0 || output_px[1] <= 0.0 || source_px[0] <= 0.0 || source_px[1] <= 0.0 {
        return [[0.0, 0.0], [1.0, 1.0]];
    }
    let out_aspect = output_px[0] / output_px[1];
    let src_aspect = source_px[0] / source_px[1];
    if src_aspect > out_aspect {
        // Source is wider: full width, bars top and bottom.
        let h = out_aspect / src_aspect;
        [[0.0, (1.0 - h) / 2.0], [1.0, (1.0 + h) / 2.0]]
    } else {
        let w = src_aspect / out_aspect;
        [[(1.0 - w) / 2.0, 0.0], [(1.0 + w) / 2.0, 1.0]]
    }
}

/// Map unit coords into output coords through the fit rect.
pub fn unit_to_output(p: [f32; 2], fit: [[f32; 2]; 2]) -> [f32; 2] {
    [
        fit[0][0] + p[0] * (fit[1][0] - fit[0][0]),
        fit[0][1] + p[1] * (fit[1][1] - fit[0][1]),
    ]
}

/// Row-major 3x3 matrix.
pub type Mat3 = [[f64; 3]; 3];

/// Homography mapping the unit square (0,0),(1,0),(1,1),(0,1) onto quad `q`
/// (Heckbert, "Fundamentals of Texture Mapping and Image Warping", 1989).
pub fn square_to_quad(q: [[f32; 2]; 4]) -> Mat3 {
    let [x0, y0] = [q[0][0] as f64, q[0][1] as f64];
    let [x1, y1] = [q[1][0] as f64, q[1][1] as f64];
    let [x2, y2] = [q[2][0] as f64, q[2][1] as f64];
    let [x3, y3] = [q[3][0] as f64, q[3][1] as f64];

    let dx3 = x0 - x1 + x2 - x3;
    let dy3 = y0 - y1 + y2 - y3;

    if dx3.abs() < 1e-12 && dy3.abs() < 1e-12 {
        // Parallelogram: plain affine map.
        return [[x1 - x0, x3 - x0, x0], [y1 - y0, y3 - y0, y0], [0.0, 0.0, 1.0]];
    }

    let dx1 = x1 - x2;
    let dx2 = x3 - x2;
    let dy1 = y1 - y2;
    let dy2 = y3 - y2;
    let det = dx1 * dy2 - dx2 * dy1;
    let g = (dx3 * dy2 - dx2 * dy3) / det;
    let h = (dx1 * dy3 - dx3 * dy1) / det;

    [
        [x1 - x0 + g * x1, x3 - x0 + h * x3, x0],
        [y1 - y0 + g * y1, y3 - y0 + h * y3, y0],
        [g, h, 1.0],
    ]
}

/// Inverse via adjugate. Returns None for a degenerate quad (e.g. corners collapsed).
pub fn invert(m: Mat3) -> Option<Mat3> {
    let [[a, b, c], [d, e, f], [g, h, i]] = m;
    let co00 = e * i - f * h;
    let co01 = -(d * i - f * g);
    let co02 = d * h - e * g;
    let det = a * co00 + b * co01 + c * co02;
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [co00 * inv, -(b * i - c * h) * inv, (b * f - c * e) * inv],
        [co01 * inv, (a * i - c * g) * inv, -(a * f - c * d) * inv],
        [co02 * inv, -(a * h - b * g) * inv, (a * e - b * d) * inv],
    ])
}

#[cfg(test)]
pub fn apply(m: Mat3, p: [f64; 2]) -> [f64; 2] {
    let x = m[0][0] * p[0] + m[0][1] * p[1] + m[0][2];
    let y = m[1][0] * p[0] + m[1][1] * p[1] + m[1][2];
    let w = m[2][0] * p[0] + m[2][1] * p[1] + m[2][2];
    [x / w, y / w]
}

/// True if the quad is convex and consistently wound (a valid projector shape).
pub fn is_convex(q: [[f32; 2]; 4]) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let a = q[i];
        let b = q[(i + 1) % 4];
        let c = q[(i + 2) % 4];
        let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
        if cross.abs() < 1e-9 {
            return false;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

/// Matrix that maps output coords -> source texture coords, as three column
/// vectors padded to vec4 for a WGSL uniform. None if the quad is unusable.
pub fn shader_matrix(output_quad: [[f32; 2]; 4]) -> Option<[[f32; 4]; 3]> {
    if !is_convex(output_quad) {
        return None;
    }
    let m = invert(square_to_quad(output_quad))?;
    let col = |j: usize| [m[0][j] as f32, m[1][j] as f32, m[2][j] as f32, 0.0];
    Some([col(0), col(1), col(2)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0] as f64).abs() < 1e-5 && (a[1] - b[1] as f64).abs() < 1e-5
    }

    #[test]
    fn homography_maps_square_corners_onto_quad() {
        let q = [[0.1, 0.05], [0.95, 0.2], [0.9, 0.85], [0.0, 1.0]];
        let m = square_to_quad(q);
        let sq = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        for (s, t) in sq.iter().zip(q) {
            assert!(close(apply(m, *s), t), "{s:?} -> {:?} != {t:?}", apply(m, *s));
        }
        let inv = invert(m).unwrap();
        for (s, t) in sq.iter().zip(q) {
            let back = apply(inv, [t[0] as f64, t[1] as f64]);
            assert!(close(back, [s[0] as f32, s[1] as f32]));
        }
    }

    #[test]
    fn identity_warp_is_identity() {
        let q = Warp::default().unit_corners();
        assert_eq!(q, [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
        let m = invert(square_to_quad(q)).unwrap();
        assert!(close(apply(m, [0.3, 0.7]), [0.3, 0.7]));
    }

    #[test]
    fn horizontal_keystone_narrows_the_right_edge() {
        let w = Warp { horizontal: 20.0, ..Default::default() };
        let c = w.unit_corners();
        assert!((c[1][1] - 0.1).abs() < 1e-6 && (c[2][1] - 0.9).abs() < 1e-6);
        assert_eq!(c[0], [0.0, 0.0]);
        let w = Warp { horizontal: -20.0, ..Default::default() };
        let c = w.unit_corners();
        assert!((c[0][1] - 0.1).abs() < 1e-6 && (c[3][1] - 0.9).abs() < 1e-6);
    }

    #[test]
    fn vertical_keystone_narrows_top_or_bottom() {
        let c = Warp { vertical: 10.0, ..Default::default() }.unit_corners();
        assert!((c[0][0] - 0.05).abs() < 1e-6 && (c[1][0] - 0.95).abs() < 1e-6);
        let c = Warp { vertical: -10.0, ..Default::default() }.unit_corners();
        assert!((c[3][0] - 0.05).abs() < 1e-6 && (c[2][0] - 0.95).abs() < 1e-6);
    }

    #[test]
    fn rejects_self_intersecting_quads() {
        assert!(shader_matrix([[0.0, 0.0], [1.0, 1.0], [1.0, 0.0], [0.0, 1.0]]).is_none());
        assert!(shader_matrix([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]).is_some());
    }

    #[test]
    fn fit_rect_letterboxes() {
        // 4:3 source in 16:9 output -> pillarbox.
        let r = fit_rect([1920.0, 1080.0], [1440.0, 1080.0], false);
        assert!((r[0][0] - 0.125).abs() < 1e-6 && (r[1][0] - 0.875).abs() < 1e-6);
        assert_eq!(fit_rect([1920.0, 1080.0], [1440.0, 1080.0], true), [[0.0, 0.0], [1.0, 1.0]]);
    }
}
