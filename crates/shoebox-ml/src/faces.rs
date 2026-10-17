//! Faces: OpenCV Zoo's YuNet detector (MIT) and SFace recognizer
//! (Apache 2.0), run through [`crate::onnx`]. Detection, alignment and the
//! post-processing follow OpenCV's `FaceDetectorYN` and `FaceRecognizerSF`.

use candle_core::{Device, Tensor};
use shoebox_decode::Rgb8;

use crate::onnx::{Graph, unit_rows};
use crate::{MlError, Result};

/// Identifies the face rows this module produces (detector + recognizer +
/// alignment): stored faces from anything else are recomputed.
pub const MODEL_ID: &str = "yunet-2023mar+sface-2021dec/1";

/// Long edge images are detected at. YuNet finds faces of about 10 to 300
/// pixels, so at 640 px a face is found from ~1.5% to ~45% of the frame.
pub const DETECT_SIZE: u32 = 640;
/// Side of the aligned face SFace takes.
pub const FACE_SIZE: u32 = 112;
/// SFace embedding length.
pub const FACE_DIM: usize = 128;
/// OpenCV's same-person threshold for SFace's cosine similarity.
pub const SAME_PERSON: f32 = 0.363;

/// A detected face, in the pixel coordinates of the image it was found in.
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    /// x, y, width, height.
    pub rect: [f32; 4],
    /// Right eye, left eye, nose, right and left mouth corners (as seen
    /// from the camera: the subject's right eye is on the image's left).
    pub landmarks: [[f32; 2]; 5],
    pub score: f32,
}

impl Face {
    /// The same face in an image `k` times the size.
    pub fn scaled(&self, k: f32) -> Face {
        Face {
            rect: self.rect.map(|v| v * k),
            landmarks: self.landmarks.map(|[x, y]| [x * k, y * k]),
            score: self.score,
        }
    }
}

pub struct FaceDetector {
    graph: Graph,
    device: Device,
}

impl FaceDetector {
    pub fn load(onnx: &[u8], device: &Device) -> Result<Self> {
        Ok(Self {
            graph: Graph::load(onnx, device)?,
            device: device.clone(),
        })
    }

    /// Faces in `img` (best first) scoring at least `min_score`, after
    /// non-maximum suppression. Images bigger than [`DETECT_SIZE`] should be
    /// scaled down first.
    pub fn detect(&self, img: &Rgb8, min_score: f32) -> Result<Vec<Face>> {
        let (w, h) = (img.width as usize, img.height as usize);
        // Pad right and bottom to a multiple of 32 (the coarsest stride).
        let (pw, ph) = (w.div_ceil(32) * 32, h.div_ceil(32) * 32);
        let mut x = vec![0f32; 3 * pw * ph];
        for (i, px) in img.px.as_chunks::<3>().0.iter().enumerate() {
            let (r, c) = (i / w, i % w);
            // YuNet takes BGR, 0-255.
            for (ch, v) in [px[2], px[1], px[0]].into_iter().enumerate() {
                x[ch * pw * ph + r * pw + c] = f32::from(v);
            }
        }
        let out = self
            .graph
            .run(Tensor::from_vec(x, (1, 3, ph, pw), &self.device)?)?;
        let flat = |name: &str| -> Result<Vec<f32>> {
            Ok(out
                .get(name)
                .ok_or_else(|| MlError::Format(format!("YuNet: no output {name}")))?
                .flatten_all()?
                .to_vec1::<f32>()?)
        };
        let mut faces = Vec::new();
        for stride in [8usize, 16, 32] {
            let cls = flat(&format!("cls_{stride}"))?;
            let obj = flat(&format!("obj_{stride}"))?;
            let bbox = flat(&format!("bbox_{stride}"))?;
            let kps = flat(&format!("kps_{stride}"))?;
            let cols = pw / stride;
            let s = stride as f32;
            for (idx, (&c, &o)) in cls.iter().zip(&obj).enumerate() {
                let score = (c.clamp(0.0, 1.0) * o.clamp(0.0, 1.0)).sqrt();
                if score < min_score {
                    continue;
                }
                let (r, col) = ((idx / cols) as f32, (idx % cols) as f32);
                let b = &bbox[idx * 4..idx * 4 + 4];
                let (cx, cy) = ((col + b[0]) * s, (r + b[1]) * s);
                let (bw, bh) = (b[2].exp() * s, b[3].exp() * s);
                let k = &kps[idx * 10..idx * 10 + 10];
                let landmarks =
                    std::array::from_fn(|n| [(k[2 * n] + col) * s, (k[2 * n + 1] + r) * s]);
                faces.push(Face {
                    rect: [cx - bw / 2.0, cy - bh / 2.0, bw, bh],
                    landmarks,
                    score,
                });
            }
        }
        Ok(nms(faces, 0.3))
    }
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let x1 = a[0].max(b[0]);
    let y1 = a[1].max(b[1]);
    let x2 = (a[0] + a[2]).min(b[0] + b[2]);
    let y2 = (a[1] + a[3]).min(b[1] + b[3]);
    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union > 0.0 { inter / union } else { 0.0 }
}

/// Keep the best of overlapping faces.
fn nms(mut faces: Vec<Face>, max_iou: f32) -> Vec<Face> {
    faces.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Face> = Vec::new();
    for f in faces {
        if kept.iter().all(|k| iou(&k.rect, &f.rect) <= max_iou) {
            kept.push(f);
        }
    }
    kept
}

/// Where the five landmarks go in the 112 × 112 crop (ArcFace's template,
/// as OpenCV's `FaceRecognizerSF::alignCrop` uses).
const TEMPLATE: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

/// The least-squares similarity transform (scale, rotation, shift) taking
/// `src` to `dst`: maps (x, y) to (a·x − b·y + tx, b·x + a·y + ty).
fn similarity(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> [f32; 4] {
    let mean = |p: &[[f32; 2]; 5]| {
        let s = p.iter().fold([0f32; 2], |m, q| [m[0] + q[0], m[1] + q[1]]);
        [s[0] / 5.0, s[1] / 5.0]
    };
    let (ms, md) = (mean(src), mean(dst));
    let (mut num_a, mut num_b, mut den) = (0f32, 0f32, 0f32);
    for (s, d) in src.iter().zip(dst) {
        let (sx, sy) = (s[0] - ms[0], s[1] - ms[1]);
        let (dx, dy) = (d[0] - md[0], d[1] - md[1]);
        num_a += sx * dx + sy * dy;
        num_b += sx * dy - sy * dx;
        den += sx * sx + sy * sy;
    }
    let (a, b) = if den > 0.0 {
        (num_a / den, num_b / den)
    } else {
        (1.0, 0.0)
    };
    let tx = md[0] - (a * ms[0] - b * ms[1]);
    let ty = md[1] - (b * ms[0] + a * ms[1]);
    [a, b, tx, ty]
}

/// The face cut out of `img`, upright and scaled so its landmarks sit on
/// the template: what SFace expects. Bilinear; outside the image is black.
pub fn align(img: &Rgb8, face: &Face) -> Rgb8 {
    let [a, b, tx, ty] = similarity(&face.landmarks, &TEMPLATE);
    let k = a * a + b * b;
    let n = FACE_SIZE as usize;
    let (w, h) = (img.width as usize, img.height as usize);
    let mut px = vec![0u8; n * n * 3];
    if k <= 0.0 {
        return Rgb8 {
            width: FACE_SIZE,
            height: FACE_SIZE,
            px,
        };
    }
    for v in 0..n {
        for u in 0..n {
            // Invert the transform: source = R⁻¹ (dst − t) / scale.
            let (dx, dy) = (u as f32 - tx, v as f32 - ty);
            let sx = (a * dx + b * dy) / k;
            let sy = (-b * dx + a * dy) / k;
            let (x0, y0) = (sx.floor(), sy.floor());
            let (fx, fy) = (sx - x0, sy - y0);
            let at = |x: f32, y: f32, c: usize| -> f32 {
                if x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 {
                    0.0
                } else {
                    f32::from(img.px[(y as usize * w + x as usize) * 3 + c])
                }
            };
            for c in 0..3 {
                let top = at(x0, y0, c) * (1.0 - fx) + at(x0 + 1.0, y0, c) * fx;
                let bot = at(x0, y0 + 1.0, c) * (1.0 - fx) + at(x0 + 1.0, y0 + 1.0, c) * fx;
                px[(v * n + u) * 3 + c] = (top * (1.0 - fy) + bot * fy).round() as u8;
            }
        }
    }
    Rgb8 {
        width: FACE_SIZE,
        height: FACE_SIZE,
        px,
    }
}

pub struct FaceEmbedder {
    graph: Graph,
    device: Device,
}

impl FaceEmbedder {
    pub fn load(onnx: &[u8], device: &Device) -> Result<Self> {
        Ok(Self {
            graph: Graph::load(onnx, device)?,
            device: device.clone(),
        })
    }

    /// Unit-length embeddings of aligned faces ([`align`]). Two faces whose
    /// embeddings have a cosine similarity of at least [`SAME_PERSON`] are
    /// probably the same person.
    pub fn embed(&self, faces: &[Rgb8]) -> Result<Vec<Vec<f32>>> {
        if faces.is_empty() {
            return Ok(Vec::new());
        }
        let n = FACE_SIZE as usize;
        let mut x = vec![0f32; faces.len() * 3 * n * n];
        for (k, f) in faces.iter().enumerate() {
            if (f.width, f.height) != (FACE_SIZE, FACE_SIZE) {
                return Err(MlError::Format("face isn't aligned".into()));
            }
            let base = k * 3 * n * n;
            for (i, px) in f.px.as_chunks::<3>().0.iter().enumerate() {
                // RGB, 0-255 (the model normalises).
                for c in 0..3 {
                    x[base + c * n * n + i] = f32::from(px[c]);
                }
            }
        }
        let t = Tensor::from_vec(x, (faces.len(), 3, n, n), &self.device)?;
        let fc = self
            .graph
            .run(t)?
            .into_values()
            .next()
            .ok_or_else(|| MlError::Format("SFace: no output".into()))?;
        unit_rows(&fc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_recovers_a_known_transform() {
        // Rotate 30°, scale 2, shift (5, -3).
        let (c, s) = (
            30f32.to_radians().cos() * 2.0,
            30f32.to_radians().sin() * 2.0,
        );
        let src = TEMPLATE;
        let dst = src.map(|[x, y]| [c * x - s * y + 5.0, s * x + c * y - 3.0]);
        let [a, b, tx, ty] = similarity(&src, &dst);
        assert!((a - c).abs() < 1e-3 && (b - s).abs() < 1e-3, "{a} {b}");
        assert!(
            (tx - 5.0).abs() < 1e-2 && (ty + 3.0).abs() < 1e-2,
            "{tx} {ty}"
        );
    }

    #[test]
    fn align_maps_landmarks_onto_the_template() {
        // A face twice the template size, offset: landmark pixels marked white.
        let (w, h) = (400u32, 400u32);
        let mut px = vec![0u8; (w * h * 3) as usize];
        let marks = TEMPLATE.map(|[x, y]| [x * 2.0 + 60.0, y * 2.0 + 40.0]);
        for [x, y] in marks {
            for dy in -1..=1i32 {
                for dx in -1..=1i32 {
                    let i = (((y as i32 + dy) as u32 * w + (x as i32 + dx) as u32) * 3) as usize;
                    px[i..i + 3].copy_from_slice(&[255, 255, 255]);
                }
            }
        }
        let img = Rgb8::new(w, h, px).unwrap();
        let face = Face {
            rect: [0.0; 4],
            landmarks: marks,
            score: 1.0,
        };
        let a = align(&img, &face);
        for [x, y] in TEMPLATE {
            let i = ((y.round() as u32 * FACE_SIZE + x.round() as u32) * 3) as usize;
            assert!(a.px[i] > 100, "landmark at ({x}, {y}) is bright");
        }
    }

    #[test]
    fn nms_keeps_the_best_of_overlaps() {
        let f = |x: f32, score: f32| Face {
            rect: [x, 0.0, 10.0, 10.0],
            landmarks: [[0.0; 2]; 5],
            score,
        };
        let kept = nms(vec![f(0.0, 0.8), f(1.0, 0.9), f(50.0, 0.7)], 0.3);
        let xs: Vec<f32> = kept.iter().map(|k| k.rect[0]).collect();
        assert_eq!(xs, [1.0, 50.0]);
    }
}
