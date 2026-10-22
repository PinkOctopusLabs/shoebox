//! Small 3-D maths for cameras: rotations as matrices (world → camera),
//! the exponential map, the nearest rotation to a matrix, and a symmetric
//! eigen-decomposition.

use crate::colour::M3;

pub type V3 = [f64; 3];

#[inline]
pub fn mv(m: &M3, v: V3) -> V3 {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// `mᵀ·v`.
#[inline]
pub fn mtv(m: &M3, v: V3) -> V3 {
    [
        m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2],
        m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2],
        m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2],
    ]
}

pub fn t(m: &M3) -> M3 {
    std::array::from_fn(|i| std::array::from_fn(|j| m[j][i]))
}

pub fn mul(a: &M3, b: &M3) -> M3 {
    crate::colour::mul(a, b)
}

#[inline]
pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}

pub fn unit(a: V3) -> Option<V3> {
    let n = norm(a);
    (n > 1e-12).then(|| a.map(|v| v / n))
}

/// The rotation by `|w|` radians about `w` (Rodrigues).
pub fn exp(w: V3) -> M3 {
    let th = norm(w);
    let k = [[0.0, -w[2], w[1]], [w[2], 0.0, -w[0]], [-w[1], w[0], 0.0]];
    let (a, b) = if th < 1e-8 {
        (1.0, 0.5)
    } else {
        (th.sin() / th, (1.0 - th.cos()) / (th * th))
    };
    let k2 = mul(&k, &k);
    std::array::from_fn(|i| {
        std::array::from_fn(|j| f64::from(u8::from(i == j)) + a * k[i][j] + b * k2[i][j])
    })
}

/// The angle of rotation `r` (radians).
pub fn angle(r: &M3) -> f64 {
    ((r[0][0] + r[1][1] + r[2][2] - 1.0) / 2.0)
        .clamp(-1.0, 1.0)
        .acos()
}

pub fn det(m: &M3) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

/// The rotation nearest `m` (up to scale; polar decomposition by Newton's
/// iteration). None if `m` is singular.
pub fn nearest_rotation(m: &M3) -> Option<M3> {
    let det = det(m);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let s = det.signum() * det.abs().cbrt();
    let mut r = m.map(|row| row.map(|v| v / s));
    for _ in 0..30 {
        let it = t(&crate::colour::inv(&r)?);
        let next: M3 = std::array::from_fn(|i| std::array::from_fn(|j| 0.5 * (r[i][j] + it[i][j])));
        let d: f64 = (0..3)
            .flat_map(|i| (0..3).map(move |j| (i, j)))
            .map(|(i, j)| (next[i][j] - r[i][j]).abs())
            .sum();
        r = next;
        if d < 1e-14 {
            break;
        }
    }
    r.iter().flatten().all(|v| v.is_finite()).then_some(r)
}

/// Eigenvalues (ascending) and unit eigenvectors (`vectors[k]` goes with
/// `values[k]`) of a symmetric matrix, by Jacobi rotations.
pub fn eigen_sym(a: &M3) -> ([f64; 3], [V3; 3]) {
    let mut a = *a;
    let mut v = crate::colour::IDENTITY;
    for _ in 0..50 {
        let off = a[0][1].abs() + a[0][2].abs() + a[1][2].abs();
        if off < 1e-15 {
            break;
        }
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            if a[p][q].abs() < 1e-300 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let tt = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let tt = if theta == 0.0 { 1.0 } else { tt };
            let c = 1.0 / (tt * tt + 1.0).sqrt();
            let s = tt * c;
            let mut j = crate::colour::IDENTITY;
            j[p][p] = c;
            j[q][q] = c;
            j[p][q] = s;
            j[q][p] = -s;
            a = mul(&t(&j), &mul(&a, &j));
            v = mul(&v, &j);
        }
    }
    let mut order = [0usize, 1, 2];
    order.sort_by(|&x, &y| a[x][x].total_cmp(&a[y][y]));
    (
        order.map(|k| a[k][k]),
        order.map(|k| [v[0][k], v[1][k], v[2][k]]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotations_and_eigenvectors() {
        let r = exp([0.1, -0.2, 0.3]);
        assert!((angle(&r) - 0.14f64.sqrt()).abs() < 1e-12);
        let id = mul(&r, &t(&r));
        for i in 0..3 {
            for j in 0..3 {
                assert!((id[i][j] - f64::from(u8::from(i == j))).abs() < 1e-12);
            }
        }
        // A scaled, slightly sheared rotation comes back to the rotation.
        let mut m = r.map(|row| row.map(|v| v * 2.5));
        m[0][1] += 1e-4;
        let n = nearest_rotation(&m).unwrap();
        assert!(angle(&mul(&n, &t(&r))) < 1e-4);
        let s = [[2.0, 1.0, 0.0], [1.0, 2.0, 0.0], [0.0, 0.0, 5.0]];
        let (vals, vecs) = eigen_sym(&s);
        assert!((vals[0] - 1.0).abs() < 1e-12 && (vals[2] - 5.0).abs() < 1e-12);
        let av = mv(&s, vecs[0]);
        for k in 0..3 {
            assert!((av[k] - vals[0] * vecs[0][k]).abs() < 1e-12);
        }
    }
}
