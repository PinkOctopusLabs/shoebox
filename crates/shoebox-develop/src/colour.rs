//! Colour maths shared by the CPU reference pipe and the shaders' setup:
//! 3×3 matrices, RGB spaces from their primaries, Bradford adaptation, and
//! colour temperature and tint (Robertson's method, as the DNG SDK uses it).

// Channel and matrix loops read clearer indexed (c, r) than zipped.
#![allow(clippy::needless_range_loop)]

pub type M3 = [[f64; 3]; 3];
pub type V3 = [f64; 3];

/// CIE xy chromaticity.
pub type Xy = (f64, f64);

pub const D65: Xy = (0.3127, 0.3290);
pub const D50: Xy = (0.3457, 0.3585);

/// Primaries (R, G, B) of the spaces Shoebox works and displays in.
pub const REC2020: [Xy; 3] = [(0.708, 0.292), (0.170, 0.797), (0.131, 0.046)];
pub const SRGB: [Xy; 3] = [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06)];
pub const DISPLAY_P3: [Xy; 3] = [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060)];
pub const ADOBE_RGB: [Xy; 3] = [(0.64, 0.33), (0.21, 0.71), (0.15, 0.06)];
pub const PRO_PHOTO: [Xy; 3] = [(0.7347, 0.2653), (0.1596, 0.8404), (0.0366, 0.0001)];

pub const IDENTITY: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

pub fn mul(a: &M3, b: &M3) -> M3 {
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

pub fn mulv(m: &M3, v: V3) -> V3 {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

pub fn diag(v: V3) -> M3 {
    [[v[0], 0.0, 0.0], [0.0, v[1], 0.0], [0.0, 0.0, v[2]]]
}

/// The inverse, or None for a singular matrix.
pub fn inv(m: &M3) -> Option<M3> {
    let c = |a: usize, b: usize, e: usize, f: usize| m[a][b] * m[e][f] - m[a][f] * m[e][b];
    let det = m[0][0] * c(1, 1, 2, 2) - m[0][1] * c(1, 0, 2, 2) + m[0][2] * c(1, 0, 2, 1);
    if det.abs() < 1e-12 {
        return None;
    }
    Some([
        [
            c(1, 1, 2, 2) / det,
            -c(0, 1, 2, 2) / det,
            c(0, 1, 1, 2) / det,
        ],
        [
            -c(1, 0, 2, 2) / det,
            c(0, 0, 2, 2) / det,
            -c(0, 0, 1, 2) / det,
        ],
        [
            c(1, 0, 2, 1) / det,
            -c(0, 0, 2, 1) / det,
            c(0, 0, 1, 1) / det,
        ],
    ])
}

/// XYZ of a chromaticity at Y = 1.
pub fn xy_to_xyz(xy: Xy) -> V3 {
    let (x, y) = xy;
    let y = y.max(1e-9);
    [x / y, 1.0, (1.0 - x - y) / y]
}

pub fn xyz_to_xy(v: V3) -> Xy {
    let s = v[0] + v[1] + v[2];
    if s.abs() < 1e-12 {
        return D50;
    }
    (v[0] / s, v[1] / s)
}

/// RGB → XYZ for a space with these primaries and white.
pub fn rgb_to_xyz(primaries: &[Xy; 3], white: Xy) -> M3 {
    let p: Vec<V3> = primaries.iter().map(|&xy| xy_to_xyz(xy)).collect();
    let m = [
        [p[0][0], p[1][0], p[2][0]],
        [p[0][1], p[1][1], p[2][1]],
        [p[0][2], p[1][2], p[2][2]],
    ];
    let s = inv(&m).map_or([1.0; 3], |mi| mulv(&mi, xy_to_xyz(white)));
    mul(&m, &diag(s))
}

/// XYZ → RGB for a space with these primaries and white.
pub fn xyz_to_rgb(primaries: &[Xy; 3], white: Xy) -> M3 {
    inv(&rgb_to_xyz(primaries, white)).unwrap_or(IDENTITY)
}

/// Bradford chromatic adaptation of XYZ from one white to another.
pub fn bradford(from: Xy, to: Xy) -> M3 {
    const B: M3 = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    let Some(bi) = inv(&B) else { return IDENTITY };
    let s = mulv(&B, xy_to_xyz(from));
    let d = mulv(&B, xy_to_xyz(to));
    let scale = [d[0] / s[0], d[1] / s[1], d[2] / s[2]];
    mul(&bi, &mul(&diag(scale), &B))
}

/// Robertson's isotemperature lines: (mired, u, v, slope).
const ROBERTSON: [[f64; 4]; 31] = [
    [0.0, 0.18006, 0.26352, -0.24341],
    [10.0, 0.18066, 0.26589, -0.25479],
    [20.0, 0.18133, 0.26846, -0.26876],
    [30.0, 0.18208, 0.27119, -0.28539],
    [40.0, 0.18293, 0.27407, -0.30470],
    [50.0, 0.18388, 0.27709, -0.32675],
    [60.0, 0.18494, 0.28021, -0.35156],
    [70.0, 0.18611, 0.28342, -0.37915],
    [80.0, 0.18740, 0.28668, -0.40955],
    [90.0, 0.18880, 0.28997, -0.44278],
    [100.0, 0.19032, 0.29326, -0.47888],
    [125.0, 0.19462, 0.30141, -0.58204],
    [150.0, 0.19962, 0.30921, -0.70471],
    [175.0, 0.20525, 0.31647, -0.84901],
    [200.0, 0.21142, 0.32312, -1.0182],
    [225.0, 0.21807, 0.32909, -1.2168],
    [250.0, 0.22511, 0.33439, -1.4512],
    [275.0, 0.23247, 0.33904, -1.7298],
    [300.0, 0.24010, 0.34308, -2.0637],
    [325.0, 0.24702, 0.34655, -2.4681],
    [350.0, 0.25591, 0.34951, -2.9641],
    [375.0, 0.26400, 0.35200, -3.5814],
    [400.0, 0.27218, 0.35407, -4.3633],
    [425.0, 0.28039, 0.35577, -5.3762],
    [450.0, 0.28863, 0.35714, -6.7262],
    [475.0, 0.29685, 0.35823, -8.5955],
    [500.0, 0.30505, 0.35907, -11.324],
    [525.0, 0.31320, 0.35968, -15.628],
    [550.0, 0.32129, 0.36011, -23.325],
    [575.0, 0.32931, 0.36038, -40.770],
    [600.0, 0.33724, 0.36051, -116.45],
];

/// Tint units per unit of distance from the Planckian locus (the DNG SDK's
/// scale, so values match Lightroom's).
const TINT_SCALE: f64 = -3000.0;

fn unit(du: f64, dv: f64) -> (f64, f64) {
    let len = (du * du + dv * dv).sqrt();
    (du / len, dv / len)
}

/// Colour temperature (K) and tint of a chromaticity.
pub fn xy_to_temp_tint(xy: Xy) -> (f64, f64) {
    let (x, y) = xy;
    let d = 1.5 - x + 6.0 * y;
    let (u, v) = (2.0 * x / d, 3.0 * y / d);
    let (mut last_dt, mut last_du, mut last_dv) = (0.0, 0.0, 0.0);
    for i in 1..ROBERTSON.len() {
        let (du, dv) = unit(1.0, ROBERTSON[i][3]);
        let (uu, vv) = (u - ROBERTSON[i][1], v - ROBERTSON[i][2]);
        let mut dt = -uu * dv + vv * du;
        if dt <= 0.0 || i == ROBERTSON.len() - 1 {
            if dt > 0.0 {
                dt = 0.0;
            }
            dt = -dt;
            let f = if i == 1 { 0.0 } else { dt / (last_dt + dt) };
            let mired = ROBERTSON[i - 1][0] * f + ROBERTSON[i][0] * (1.0 - f);
            let temp = 1.0e6 / mired.max(1e-6);
            let uu = u - (ROBERTSON[i - 1][1] * f + ROBERTSON[i][1] * (1.0 - f));
            let vv = v - (ROBERTSON[i - 1][2] * f + ROBERTSON[i][2] * (1.0 - f));
            let (du, dv) = unit(du * (1.0 - f) + last_du * f, dv * (1.0 - f) + last_dv * f);
            let tint = (uu * du + vv * dv) * TINT_SCALE;
            return (temp, tint);
        }
        last_dt = dt;
        last_du = du;
        last_dv = dv;
    }
    (5000.0, 0.0)
}

/// The chromaticity of a colour temperature (K) and tint.
pub fn temp_tint_to_xy(temp: f64, tint: f64) -> Xy {
    let r = 1.0e6 / temp.clamp(1000.0, 1.0e6);
    let offset = tint / TINT_SCALE;
    let n = ROBERTSON.len();
    for i in 0..n - 1 {
        if r < ROBERTSON[i + 1][0] || i == n - 2 {
            let f = (ROBERTSON[i + 1][0] - r) / (ROBERTSON[i + 1][0] - ROBERTSON[i][0]);
            let mut u = ROBERTSON[i][1] * f + ROBERTSON[i + 1][1] * (1.0 - f);
            let mut v = ROBERTSON[i][2] * f + ROBERTSON[i + 1][2] * (1.0 - f);
            let (u1, v1) = unit(1.0, ROBERTSON[i][3]);
            let (u2, v2) = unit(1.0, ROBERTSON[i + 1][3]);
            let (u3, v3) = unit(u1 * f + u2 * (1.0 - f), v1 * f + v2 * (1.0 - f));
            u += u3 * offset;
            v += v3 * offset;
            let d = u - 4.0 * v + 2.0;
            return (1.5 * u / d, v / d);
        }
    }
    D50
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_illuminants_have_their_temperatures() {
        let (t, tint) = xy_to_temp_tint((0.44757, 0.40745)); // A
        assert!((t - 2856.0).abs() < 15.0 && tint.abs() < 2.0, "{t} {tint}");
        let (t, _) = xy_to_temp_tint(D65);
        assert!((t - 6504.0).abs() < 40.0, "{t}");
        for (temp, tint) in [(2850.0, 0.0), (5500.0, 10.0), (7500.0, -20.0)] {
            let (t2, n2) = xy_to_temp_tint(temp_tint_to_xy(temp, tint));
            assert!(
                (t2 - temp).abs() < 1.0 && (n2 - tint).abs() < 0.1,
                "{temp} {tint}: {t2} {n2}"
            );
        }
    }

    #[test]
    fn spaces_map_white_to_white() {
        let m = rgb_to_xyz(&SRGB, D65);
        // The familiar sRGB matrix.
        assert!((m[0][0] - 0.4124).abs() < 1e-3 && (m[1][1] - 0.7152).abs() < 1e-3);
        let w = mulv(&m, [1.0; 3]);
        let d = xy_to_xyz(D65);
        assert!(w.iter().zip(d).all(|(a, b)| (a - b).abs() < 1e-9));
        let b = bradford(D50, D65);
        let to = mulv(&b, xy_to_xyz(D50));
        assert!(to.iter().zip(d).all(|(a, b)| (a - b).abs() < 1e-6));
        let i = mul(&m, &inv(&m).unwrap());
        assert!((i[0][0] - 1.0).abs() < 1e-12 && i[0][1].abs() < 1e-12);
    }
}
