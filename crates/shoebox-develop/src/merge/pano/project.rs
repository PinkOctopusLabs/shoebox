//! Projections: where each pixel of the result looks, and so where it is in
//! each frame. The result is laid out on a plane of coordinates `(a, b)`
//! (b down), scaled to pixels:
//!
//! - **Spherical** (equirectangular): `a` longitude, `b` minus latitude,
//!   both radians: any field, to 360° × 180°.
//! - **Cylindrical**: `a` longitude, `b` height on a cylinder of radius 1
//!   (minus the tangent of latitude): a row of any width, verticals
//!   straight; heights beyond ±75° are left out.
//! - **Perspective** (rectilinear): the plane one unit ahead: straight
//!   lines stay straight, refused beyond 120° across (the edges stretch
//!   without end).
//! - **Flat** frames (on one plane) are laid on their first frame's pixels.
//!
//! The scale keeps the frames' own resolution at the centre: one focal
//! length of pixels per radian (or per unit).

use super::bundle::{Planes, Rotations};
use super::geom::{self, V3};
use crate::colour::{self, M3};
use crate::merge::align;

/// How a turning camera's panorama is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Projection {
    /// Chosen from the frames' extent: Perspective up to 90° each way,
    /// Cylindrical for a row (up to 120° high), else Spherical.
    #[default]
    Auto,
    Spherical,
    Cylindrical,
    Perspective,
}

impl Projection {
    pub const ALL: [Projection; 4] = [
        Projection::Auto,
        Projection::Spherical,
        Projection::Cylindrical,
        Projection::Perspective,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Projection::Auto => "Auto",
            Projection::Spherical => "Spherical",
            Projection::Cylindrical => "Cylindrical",
            Projection::Perspective => "Perspective",
        }
    }
}

/// Perspective's widest, degrees.
pub const PERSPECTIVE_MAX: f64 = 120.0;
/// Cylindrical's highest latitude, degrees.
const CYLINDER_MAX: f64 = 75.0;

/// The direction plane point `(a, b)` looks along.
#[inline]
pub fn direction(p: Projection, a: f64, b: f64) -> V3 {
    match p {
        Projection::Perspective => [a, b, 1.0],
        Projection::Cylindrical => {
            let (s, c) = a.sin_cos();
            [s, b, c]
        }
        _ => {
            let (sa, ca) = a.sin_cos();
            let (sb, cb) = b.sin_cos();
            [cb * sa, sb, cb * ca]
        }
    }
}

/// The plane point direction `d` is drawn at (None: Perspective, behind).
#[inline]
pub fn plane(p: Projection, d: V3) -> Option<[f64; 2]> {
    match p {
        Projection::Perspective => (d[2] > 1e-9).then(|| [d[0] / d[2], d[1] / d[2]]),
        Projection::Cylindrical => {
            let r = d[0].hypot(d[2]);
            (r > 1e-12).then(|| [d[0].atan2(d[2]), d[1] / r])
        }
        _ => {
            let n = geom::norm(d);
            (n > 1e-12).then(|| [d[0].atan2(d[2]), (d[1] / n).clamp(-1.0, 1.0).asin()])
        }
    }
}

/// How the frames sit.
#[derive(Debug, Clone)]
pub enum Geometry {
    /// A camera turning about one point (straightened), drawn by a
    /// projection (never Auto).
    Turning {
        cams: Rotations,
        projection: Projection,
    },
    /// Frames on one plane: each one's homography onto it, and back.
    Flat { planes: Planes, inverse: Vec<M3> },
}

/// The result's pixels and where they look.
#[derive(Debug, Clone)]
pub struct Layout {
    pub geometry: Geometry,
    /// Pixels per unit of the plane.
    pub scale: f64,
    /// The plane point at pixel (0, 0).
    pub origin: [f64; 2],
    pub width: usize,
    pub height: usize,
}

impl Layout {
    #[inline]
    fn plane_at(&self, u: f64, v: f64) -> [f64; 2] {
        [
            self.origin[0] + u / self.scale,
            self.origin[1] + v / self.scale,
        ]
    }

    /// The direction pixel `(u, v)` looks along (a turning camera's
    /// straightened world: x right, y down, z the panorama's centre).
    pub fn direction(&self, u: f64, v: f64) -> Option<V3> {
        match &self.geometry {
            Geometry::Turning { projection, .. } => {
                let [a, b] = self.plane_at(u, v);
                Some(direction(*projection, a, b))
            }
            Geometry::Flat { .. } => None,
        }
    }

    /// Where pixel `(u, v)` is in frame `i` (its corrected pixels; None:
    /// behind it). Whether that's inside the frame is the caller's to ask.
    #[inline]
    pub fn to_frame(&self, i: usize, u: f64, v: f64) -> Option<[f64; 2]> {
        let [a, b] = self.plane_at(u, v);
        match &self.geometry {
            Geometry::Turning { cams, projection } => cams.pixel(i, direction(*projection, a, b)),
            Geometry::Flat { inverse, .. } => {
                let (x, y) = align::apply(&inverse[i], a, b);
                (x.is_finite() && y.is_finite()).then_some([x, y])
            }
        }
    }

    /// Where frame `i`'s pixel `(x, y)` is in the result (None: nowhere).
    pub fn from_frame(&self, i: usize, x: f64, y: f64) -> Option<[f64; 2]> {
        let p = match &self.geometry {
            Geometry::Turning { cams, projection } => plane(*projection, cams.ray(i, x, y))?,
            Geometry::Flat { planes, .. } => {
                let (a, b) = align::apply(&planes.g[i], x, y);
                [a, b]
            }
        };
        Some([
            (p[0] - self.origin[0]) * self.scale,
            (p[1] - self.origin[1]) * self.scale,
        ])
    }

    /// The same layout showing only `[x0, x1) × [y0, y1)` of it.
    pub fn cropped(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> Layout {
        Layout {
            origin: [
                self.origin[0] + x0 as f64 / self.scale,
                self.origin[1] + y0 as f64 / self.scale,
            ],
            width: x1 - x0,
            height: y1 - y0,
            ..self.clone()
        }
    }

    /// The same layout `k` times smaller.
    pub fn shrunk(&self, k: f64) -> Layout {
        Layout {
            scale: self.scale / k,
            width: ((self.width as f64 / k).ceil() as usize).max(1),
            height: ((self.height as f64 / k).ceil() as usize).max(1),
            ..self.clone()
        }
    }

    /// Pixels of the result frame `i` may reach (`[x0, y0, x1, y1)`,
    /// within it), from its edges and a grid inside.
    pub fn reach(&self, i: usize, size: (usize, usize)) -> Option<[usize; 4]> {
        let (w, h) = (size.0 as f64 - 1.0, size.1 as f64 - 1.0);
        let mut pts = Vec::new();
        let n = 48;
        for k in 0..=n {
            let t = k as f64 / n as f64;
            pts.extend([(t * w, 0.0), (t * w, h), (0.0, t * h), (w, t * h)]);
        }
        for j in 1..8 {
            for k in 1..8 {
                pts.push((w * k as f64 / 8.0, h * j as f64 / 8.0));
            }
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        let mut lons = Vec::new();
        for (x, y) in pts {
            if let Some([u, v]) = self.from_frame(i, x, y) {
                x0 = x0.min(u);
                x1 = x1.max(u);
                y0 = y0.min(v);
                y1 = y1.max(v);
                lons.push(u);
            }
        }
        if x0 > x1 {
            return None;
        }
        if let Geometry::Turning { cams, projection } = &self.geometry
            && *projection != Projection::Perspective
        {
            // Across the back (±180°): every longitude. Over a pole: every
            // longitude and up (or down) to it.
            let half = std::f64::consts::PI * self.scale;
            if x1 - x0 > half {
                x0 = f64::MIN;
                x1 = f64::MAX;
            }
            for (pole, sign) in [([0.0, -1.0, 0.0], -1.0), ([0.0, 1.0, 0.0], 1.0)] {
                if let Some(p) = cams.pixel(i, pole)
                    && (0.0..=w).contains(&p[0])
                    && (0.0..=h).contains(&p[1])
                {
                    x0 = f64::MIN;
                    x1 = f64::MAX;
                    if *projection == Projection::Spherical {
                        let v = (sign * std::f64::consts::FRAC_PI_2 - self.origin[1]) * self.scale;
                        y0 = y0.min(v);
                        y1 = y1.max(v);
                    } else {
                        y0 = f64::MIN;
                        y1 = f64::MAX;
                    }
                }
            }
        }
        let clamp = |v: f64, hi: usize| v.clamp(0.0, hi as f64) as usize;
        let r = [
            clamp(x0.floor() - 2.0, self.width),
            clamp(y0.floor() - 2.0, self.height),
            clamp(x1.ceil() + 3.0, self.width),
            clamp(y1.ceil() + 3.0, self.height),
        ];
        (r[2] > r[0] && r[3] > r[1]).then_some(r)
    }
}

/// What the frames cover, for choosing a projection: longitude arc and
/// latitude range (degrees), and the frames' sample directions.
pub struct Extent {
    pub width: f64,
    pub lat: (f64, f64),
    /// Whether it goes all the way round.
    pub all_round: bool,
}

/// Sample directions along each frame's edges and inside.
fn samples(cams: &Rotations, sizes: &[(usize, usize)]) -> Vec<V3> {
    let mut out = Vec::new();
    for (i, &(w, h)) in sizes.iter().enumerate() {
        let (w, h) = (w as f64 - 1.0, h as f64 - 1.0);
        let n = 16;
        for k in 0..=n {
            let t = k as f64 / n as f64;
            for (x, y) in [
                (t * w, 0.0),
                (t * w, h),
                (0.0, t * h),
                (w, t * h),
                (t * w, h / 2.0),
            ] {
                out.push(cams.ray(i, x, y));
            }
        }
    }
    out
}

/// Turn the world about its vertical so the frames' longitudes are centred
/// on 0 (the largest gap behind), and measure what they cover.
pub fn centre(cams: &mut Rotations, sizes: &[(usize, usize)]) -> Extent {
    let dirs = samples(cams, sizes);
    let mut lons: Vec<f64> = dirs
        .iter()
        .filter_map(|d| plane(Projection::Spherical, *d))
        .map(|p| p[0])
        .collect();
    lons.sort_by(f64::total_cmp);
    let tau = std::f64::consts::TAU;
    // The largest gap between neighbouring longitudes (round the back too).
    let mut gap = (0.0, 0.0);
    for (k, &l) in lons.iter().enumerate() {
        let next = if k + 1 < lons.len() {
            lons[k + 1]
        } else {
            lons[0] + tau
        };
        if next - l > gap.0 {
            gap = (next - l, l);
        }
    }
    // Coverage runs from the gap's end round to its start; whether frames
    // in fact meet across the gap is for the gap to say (small: all round).
    let all_round = gap.0 < 10f64.to_radians();
    let middle = if all_round {
        // Centred on the frames' mean view.
        let mut z = [0.0; 3];
        for m in &cams.r {
            for k in 0..3 {
                z[k] += m[2][k];
            }
        }
        if z[0].hypot(z[2]) > 1e-6 {
            z[0].atan2(z[2])
        } else {
            0.0
        }
    } else {
        gap.1 + gap.0 + (tau - gap.0) / 2.0
    };
    let (s, c) = middle.sin_cos();
    let y: M3 = [[c, 0.0, -s], [0.0, 1.0, 0.0], [s, 0.0, c]];
    for r in &mut cams.r {
        *r = geom::mul(r, &geom::t(&y));
    }
    let lats: Vec<f64> = dirs
        .iter()
        .filter_map(|d| plane(Projection::Spherical, geom::mv(&y, *d)))
        .map(|p| -p[1].to_degrees())
        .collect();
    let lo = lats.iter().copied().fold(f64::MAX, f64::min);
    let hi = lats.iter().copied().fold(f64::MIN, f64::max);
    // Over a pole: to it.
    let mut lat = (lo, hi);
    for (k, pole) in [[0.0, -1.0, 0.0], [0.0, 1.0, 0.0]].iter().enumerate() {
        for (i, &(w, h)) in sizes.iter().enumerate() {
            if let Some(p) = cams.pixel(i, *pole)
                && (0.0..w as f64).contains(&p[0])
                && (0.0..h as f64).contains(&p[1])
            {
                if k == 0 {
                    lat.1 = 90.0;
                } else {
                    lat.0 = -90.0;
                }
            }
        }
    }
    Extent {
        width: if all_round {
            360.0
        } else {
            (tau - gap.0).to_degrees()
        },
        lat,
        all_round,
    }
}

impl Extent {
    /// Auto's choice.
    pub fn auto(&self) -> Projection {
        let high = self.lat.1 - self.lat.0;
        if self.width <= 90.0 && high <= 90.0 && self.lat.0 > -60.0 && self.lat.1 < 60.0 {
            Projection::Perspective
        } else if high <= 120.0 && self.lat.0 > -CYLINDER_MAX && self.lat.1 < CYLINDER_MAX {
            Projection::Cylindrical
        } else {
            Projection::Spherical
        }
    }
}

/// The plane box (`[a0, b0, a1, b1]`) the frames cover in `projection`,
/// or why they can't be drawn so.
pub fn bounds(
    cams: &Rotations,
    sizes: &[(usize, usize)],
    projection: Projection,
    extent: &Extent,
) -> Result<[f64; 4], String> {
    if projection == Projection::Perspective {
        let wide = extent.width.max(extent.lat.1 - extent.lat.0);
        if wide > PERSPECTIVE_MAX || extent.all_round {
            return Err(format!(
                "the panorama is {wide:.0}° across: Perspective can't show more than \
                 {PERSPECTIVE_MAX:.0}° (choose Cylindrical or Spherical)"
            ));
        }
    }
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for d in samples(cams, sizes) {
        let Some(p) = plane(projection, d) else {
            return Err(
                "the panorama reaches behind the camera: choose Cylindrical or Spherical".into(),
            );
        };
        b[0] = b[0].min(p[0]);
        b[1] = b[1].min(p[1]);
        b[2] = b[2].max(p[0]);
        b[3] = b[3].max(p[1]);
    }
    let pi = std::f64::consts::PI;
    match projection {
        Projection::Spherical => {
            if extent.all_round {
                b[0] = -pi;
                b[2] = pi;
            }
            b[1] = b[1].min(-extent.lat.1.to_radians());
            b[3] = b[3].max(-extent.lat.0.to_radians());
        }
        Projection::Cylindrical => {
            if extent.all_round {
                b[0] = -pi;
                b[2] = pi;
            }
            let most = CYLINDER_MAX.to_radians().tan();
            b[1] = b[1].max(-most);
            b[3] = b[3].min(most);
        }
        _ => {}
    }
    Ok(b)
}

/// The plane box flat frames cover, or why they don't make one picture.
pub fn flat_bounds(
    planes: &Planes,
    sizes: &[(usize, usize)],
) -> Result<([f64; 4], Vec<M3>), String> {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let mut inverse = Vec::new();
    for (i, &(w, h)) in sizes.iter().enumerate() {
        let g = &planes.g[i];
        inverse.push(colour::inv(g).ok_or("a frame's plane is degenerate")?);
        let (w, h) = (w as f64 - 1.0, h as f64 - 1.0);
        let corners = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];
        let mut area = 0.0;
        let mut pts = Vec::new();
        for &(x, y) in &corners {
            // On the plane's side of the horizon.
            let d = g[2][0] * x + g[2][1] * y + g[2][2];
            if d <= 1e-9 {
                return Err("the photos don't lie on one plane".into());
            }
            let p = align::apply(g, x, y);
            pts.push(p);
        }
        for k in 0..4 {
            let (p, q) = (pts[k], pts[(k + 1) % 4]);
            area += p.0 * q.1 - q.0 * p.1;
        }
        let own = (w + 1.0) * (h + 1.0);
        if !(area / 2.0 > own / 50.0 && area / 2.0 < own * 50.0) {
            return Err("the photos don't lie on one plane".into());
        }
        for p in pts {
            b[0] = b[0].min(p.0);
            b[1] = b[1].min(p.1);
            b[2] = b[2].max(p.0);
            b[3] = b[3].max(p.1);
        }
    }
    Ok((b, inverse))
}
