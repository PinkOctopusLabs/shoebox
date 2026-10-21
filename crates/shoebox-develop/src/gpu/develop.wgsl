// The live pipe for one output pixel (see `cpu::apply` and
// `cpu::display_pixel`, which this follows operation for operation):
// geometry map → lens grid (each channel from where the lens put it, times
// the vignetting gain: `cpu::lens_sample`) → bilinear sample → colour matrix → local (dehaze,
// highlights and shadows, clarity: `cpu::maps`) → look (colour, black and
// white: `cpu::look`) → display transform → tone curve → soft proof (into
// an export space, clipped, back: `cpu::Proof`) → output space and sRGB
// transfer curve.
//
// OUT_FORMAT is replaced with the output texture's format when the
// pipeline is built (rgba32float for read-back, rgba16float for display).

struct Params {
    map0: vec4<f32>,      // sx = map0.x·x + map0.y·y + map0.z; w: clipping shown (0/1)
    map1: vec4<f32>,      // sy = map1.x·x + map1.y·y + map1.z; w: display-referred (0/1)
    colour0: vec4<f32>,   // colour matrix rows
    colour1: vec4<f32>,
    colour2: vec4<f32>,
    inset0: vec4<f32>,
    inset1: vec4<f32>,
    inset2: vec4<f32>,
    outset0: vec4<f32>,
    outset1: vec4<f32>,
    outset2: vec4<f32>,
    target0: vec4<f32>,
    target1: vec4<f32>,
    target2: vec4<f32>,
    curve: vec4<f32>,     // up scale, up hardness, toe a, toe floor; display-referred: black, white, contrast
    misc: vec4<f32>,      // display grey, mid grey, preserve hue (0/1), display (0/1)
    size: vec4<f32>,      // out width, out height, src width, src height
    lms0: vec4<f32>,      // linear Rec.2020 → Oklab's LMS
    lms1: vec4<f32>,
    lms2: vec4<f32>,
    inv0: vec4<f32>,      // and back
    inv1: vec4<f32>,
    inv2: vec4<f32>,
    look: vec4<f32>,      // active (0/1), shadows tint, vibrance, saturation
    luma: vec4<f32>,      // Rec.2020 luminance weights; w: black and white (0/1)
    hue: array<vec4<f32>, 2>,  // per band, degrees
    sat: array<vec4<f32>, 2>,
    lum: array<vec4<f32>, 2>,
    mix: array<vec4<f32>, 2>,
    tone: vec4<f32>,      // x: tone curve on (0/1)
    local: vec4<f32>,     // shadows, highlights, clarity, dehaze (−1..1)
    maps: vec4<f32>,      // maps on (0/1), map width, map height
    scale: vec4<f32>,     // source → map pixels (x, y)
    air: vec4<f32>,       // dehaze airlight (rgb), its luminance
    lens: vec4<f32>,      // grid on (0/1), columns, rows, chromatic (0/1)
    map2: vec4<f32>,      // the map's perspective row: (sx, sy) are divided by map2·(x, y, 1)
    fringe: vec4<f32>,    // defringe on (0/1), purple, green
    look3: vec4<f32>,     // camera match table on (0/1), its size
    proof0: vec4<f32>,    // soft proof: Rec.2020 → the proof space; proof0.w: on (0/1)
    proof1: vec4<f32>,
    proof2: vec4<f32>,
    unproof0: vec4<f32>,  // and back; unproof0.w: gamut warning shown (0/1)
    unproof1: vec4<f32>,
    unproof2: vec4<f32>,
};

const GAMUT_SLACK: f32 = 2e-3;

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<OUT_FORMAT, write>;
@group(0) @binding(2) var<uniform> p: Params;
// The tone curve: red, green and blue tables of CURVE_SIZE entries.
@group(0) @binding(3) var<storage, read> tone: array<f32>;

// The local maps (see `cpu::maps`): STRIDE floats per map pixel.
@group(0) @binding(4) var<storage, read> maps: array<f32>;

const STRIDE: u32 = 6u;

// The lens grid (see `lens::Grid`): GRID floats per node.
@group(0) @binding(5) var<storage, read> grid: array<f32>;
const GRID: u32 = 7u;

// The camera match look (see `profile::Lut3`): size³ RGB entries, red fastest.
@group(0) @binding(6) var<storage, read> lut3: array<f32>;

fn lut_at(r: u32, g: u32, b: u32) -> vec3<f32> {
    let n = u32(p.look3.y);
    let i = ((b * n + g) * n + r) * 3u;
    return vec3<f32>(lut3[i], lut3[i + 1u], lut3[i + 2u]);
}

fn camera_look(px: vec3<f32>) -> vec3<f32> {
    let e = min(pow(max(px, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2)), vec3<f32>(1.0));
    let n = p.look3.y - 1.0;
    let pos = clamp(e, vec3<f32>(0.0), vec3<f32>(1.0)) * n;
    let i = min(vec3<u32>(floor(pos)), vec3<u32>(u32(n) - 1u));
    let f = pos - vec3<f32>(i);
    var out = vec3<f32>(0.0);
    for (var db = 0u; db < 2u; db++) {
        for (var dg = 0u; dg < 2u; dg++) {
            for (var dr = 0u; dr < 2u; dr++) {
                let wr = select(1.0 - f.x, f.x, dr == 1u);
                let wg = select(1.0 - f.y, f.y, dg == 1u);
                let wb = select(1.0 - f.z, f.z, db == 1u);
                out += wr * wg * wb * lut_at(i.x + dr, i.y + dg, i.z + db);
            }
        }
    }
    return pow(max(out, vec3<f32>(0.0)), vec3<f32>(2.2));
}
const CURVE_SIZE: u32 = 1024u;
const CENTRES = array<f32, 8>(29.0, 58.0, 110.0, 142.0, 195.0, 264.0, 295.0, 328.0);

fn mul3(r0: vec4<f32>, r1: vec4<f32>, r2: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(r0.xyz, v), dot(r1.xyz, v), dot(r2.xyz, v));
}

fn texel(x: f32, y: f32) -> vec3<f32> {
    if (x < 0.0 || y < 0.0 || x >= p.size.z || y >= p.size.w) {
        return vec3<f32>(0.0);
    }
    return textureLoad(src, vec2<i32>(i32(x), i32(y)), 0).xyz;
}

fn bilinear(x: f32, y: f32) -> vec3<f32> {
    let x0 = floor(x);
    let y0 = floor(y);
    let fx = x - x0;
    let fy = y - y0;
    let a = texel(x0, y0);
    let b = texel(x0 + 1.0, y0);
    let c = texel(x0, y0 + 1.0);
    let d = texel(x0 + 1.0, y0 + 1.0);
    let top = a + (b - a) * fx;
    let bot = c + (d - c) * fx;
    return top + (bot - top) * fy;
}

fn shoulder(t: f32, k: f32) -> f32 {
    return t / pow(1.0 + pow(t, k), 1.0 / k);
}

fn curve(v: f32) -> f32 {
    let grey = p.misc.x;
    let ev = log2(max(v, 1e-12) / p.misc.y);
    var y: f32;
    if (ev >= 0.0) {
        y = grey + (1.0 - grey) * shoulder(ev / p.curve.x, p.curve.y);
    } else {
        let t = grey * exp2(p.curve.z * ev);
        y = grey * (t - p.curve.w) / (grey - p.curve.w);
    }
    return clamp(y, 0.0, 1.0);
}

fn lin(v: f32) -> f32 {
    return pow(v, 2.2);
}

fn oetf(v: f32) -> f32 {
    if (v <= 0.0031308) {
        return 12.92 * v;
    }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

// A finished image's tone (see `cpu::Levels`).
fn levels(v: f32) -> f32 {
    let x = pow(max(v, 0.0), 1.0 / 2.2);
    let x1 = clamp((x - p.curve.x) / max(p.curve.y - p.curve.x, 1e-3), 0.0, 1.0);
    let s = x1 * x1 * (3.0 - 2.0 * x1);
    let y = clamp(x1 + p.curve.z * (s - x1), 0.0, 1.0);
    return pow(y, 2.2);
}

fn cbrt(v: f32) -> f32 {
    return sign(v) * pow(abs(v), 1.0 / 3.0);
}

fn smooth01(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn at8(v: array<vec4<f32>, 2>, i: u32) -> f32 {
    return v[i / 4u][i % 4u];
}

// The bands either side of hue h and how far between (see `cpu::look::bands`).
struct Bands { i: u32, j: u32, t: f32 };

fn bands(h: f32) -> Bands {
    var i = 7u;
    for (var k = 0u; k < 8u; k++) {
        if (h >= CENTRES[k]) {
            i = k;
        }
    }
    let j = (i + 1u) % 8u;
    let a = CENTRES[i];
    var b = CENTRES[j];
    var x = h;
    if (b <= a) {
        b += 360.0;
    }
    if (x < a) {
        x += 360.0;
    }
    return Bands(i, j, smooth01(0.0, 1.0, (x - a) / (b - a)));
}

fn band(v: array<vec4<f32>, 2>, at: Bands) -> f32 {
    let a = at8(v, at.i);
    return a + (at8(v, at.j) - a) * at.t;
}

fn rem360(h: f32) -> f32 {
    return h - 360.0 * floor(h / 360.0);
}

// The colour stage (see `cpu::look::look_pixel`).
fn look(px: vec3<f32>) -> vec3<f32> {
    let lms = mul3(p.lms0, p.lms1, p.lms2, px);
    let l3 = vec3<f32>(cbrt(lms.x), cbrt(lms.y), cbrt(lms.z));
    let lum0 = 0.21045426 * l3.x + 0.7936178 * l3.y - 0.004072047 * l3.z;
    var a = 1.9779985 * l3.x - 2.4285922 * l3.y + 0.4505937 * l3.z;
    let b = 0.025904037 * l3.x + 0.78277177 * l3.y - 0.80867577 * l3.z;
    let shade = clamp(1.0 - lum0 / 0.6, 0.0, 1.0);
    a += p.look.y * 0.04 * shade * shade * max(lum0, 0.0);
    var c = length(vec2<f32>(a, b));
    var h = rem360(degrees(atan2(b, a)));
    let at = bands(h);
    let rel = c / max(lum0, 1e-4);
    let coloured = smooth01(0.0, 0.08, rel);
    h += band(p.hue, at) * coloured;
    c *= max(1.0 + band(p.sat, at) * coloured, 0.0);
    let k = cbrt(exp2(band(p.lum, at) * coloured));
    let lum = lum0 * k;
    c *= k;
    var skin = 0.0;
    if (at.i == 1u) {
        skin = 1.0 - at.t;
    } else if (at.j == 1u) {
        skin = at.t;
    }
    let muted = 1.0 - smooth01(0.0, 0.3, rel);
    c *= max(1.0 + p.look.z * muted * (1.0 - 0.6 * skin), 0.0);
    c *= max(1.0 + p.look.w, 0.0);
    let hr = radians(h);
    let la = c * cos(hr);
    let lb = c * sin(hr);
    let m3 = vec3<f32>(
        lum + 0.39633778 * la + 0.21580376 * lb,
        lum - 0.105561346 * la - 0.06385417 * lb,
        lum - 0.08948418 * la - 1.2914855 * lb,
    );
    var out = mul3(p.inv0, p.inv1, p.inv2, m3 * m3 * m3);
    if (p.luma.w > 0.5) {
        let y = dot(p.luma.xyz, out);
        let r = c / max(lum, 1e-4);
        let mixv = band(p.mix, bands(rem360(h)));
        out = vec3<f32>(y * exp2(mixv * 1.5 * smooth01(0.0, 0.15, r)));
    }
    return out;
}

fn map_at(x: u32, y: u32, k: u32) -> f32 {
    return maps[(y * u32(p.maps.y) + x) * STRIDE + k];
}

struct Coeffs { hs: vec2<f32>, cl: vec2<f32>, dz: vec2<f32> };

// The maps at a source position (bilinear, clamped: `cpu::maps::sample`).
fn coeffs(sx: f32, sy: f32) -> Coeffs {
    let w = p.maps.y;
    let h = p.maps.z;
    let x = clamp((sx + 0.5) * p.scale.x - 0.5, 0.0, w - 1.0);
    let y = clamp((sy + 0.5) * p.scale.y - 0.5, 0.0, h - 1.0);
    let x0 = u32(floor(x));
    let y0 = u32(floor(y));
    let x1 = min(x0 + 1u, u32(w) - 1u);
    let y1 = min(y0 + 1u, u32(h) - 1u);
    let fx = x - f32(x0);
    let fy = y - f32(y0);
    var v: array<f32, 6>;
    for (var k = 0u; k < STRIDE; k++) {
        let a = map_at(x0, y0, k);
        let b = map_at(x1, y0, k);
        let c = map_at(x0, y1, k);
        let d = map_at(x1, y1, k);
        let top = a + (b - a) * fx;
        let bot = c + (d - c) * fx;
        v[k] = top + (bot - top) * fy;
    }
    return Coeffs(vec2<f32>(v[0], v[1]), vec2<f32>(v[2], v[3]), vec2<f32>(v[4], v[5]));
}

fn stops(y: f32) -> f32 {
    return log2(max(y, 1e-6) / p.misc.y);
}

fn dehaze(px: vec3<f32>, t0: f32) -> vec3<f32> {
    let amount = p.local.w;
    if (amount > 0.0) {
        let t = clamp(t0, 0.1, 1.0);
        let j = (px - p.air.xyz) / t + p.air.xyz;
        return max(px + amount * (j - px), vec3<f32>(0.0));
    }
    return px + (-amount) * 0.5 * (p.air.xyz - px);
}

// Dehaze, highlights and shadows, clarity (see `cpu::maps::local_pixel`).
fn local_px(px0: vec3<f32>, sx: f32, sy: f32) -> vec3<f32> {
    let m = coeffs(sx, sy);
    var px = px0;
    if (p.local.w != 0.0) {
        let g = clamp(dot(p.luma.xyz, px) / max(p.air.w, 1e-6), 0.0, 2.0);
        px = dehaze(px, m.dz.x * g + m.dz.y);
    }
    let s = stops(dot(p.luma.xyz, px));
    var gain = 0.0;
    if (p.local.x != 0.0 || p.local.y != 0.0) {
        let base = m.hs.x * s + m.hs.y;
        gain += p.local.x * 1.5 * (1.0 - smooth01(-3.0, 0.0, base));
        gain += p.local.y * 1.5 * smooth01(-0.5, 2.5, base);
    }
    if (p.local.z != 0.0) {
        let base = m.cl.x * s + m.cl.y;
        let detail = clamp(s - base, -3.0, 3.0);
        let mid = 1.0 - smooth01(2.0, 4.0, abs(base + 0.5));
        gain += p.local.z * 0.7 * detail * mid;
    }
    if (gain == 0.0) {
        return px;
    }
    return px * exp2(gain);
}

fn grid_at(x: u32, y: u32, k: u32) -> f32 {
    return grid[(y * u32(p.lens.y) + x) * GRID + k];
}

struct Lensed { px: vec3<f32>, at: vec2<f32> };

// The source through the lens grid (see `cpu::lens_sample`).
fn lens_sample(sx: f32, sy: f32) -> Lensed {
    let w = p.size.z;
    let h = p.size.w;
    let cols = p.lens.y;
    let rows = p.lens.z;
    let gx = clamp(sx / max(w - 1.0, 1.0) * (cols - 1.0), 0.0, cols - 1.0);
    let gy = clamp(sy / max(h - 1.0, 1.0) * (rows - 1.0), 0.0, rows - 1.0);
    let x0 = u32(floor(gx));
    let y0 = u32(floor(gy));
    let x1 = min(x0 + 1u, u32(cols) - 1u);
    let y1 = min(y0 + 1u, u32(rows) - 1u);
    let fx = gx - f32(x0);
    let fy = gy - f32(y0);
    var n: array<f32, 7>;
    for (var k = 0u; k < GRID; k++) {
        let a = grid_at(x0, y0, k);
        let b = grid_at(x1, y0, k);
        let c = grid_at(x0, y1, k);
        let d = grid_at(x1, y1, k);
        let top = a + (b - a) * fx;
        let bot = c + (d - c) * fx;
        n[k] = top + (bot - top) * fy;
    }
    let g = vec2<f32>(sx + n[2] * w, sy + n[3] * h);
    var px: vec3<f32>;
    if (p.lens.w > 0.5) {
        let r = bilinear(sx + n[0] * w, sy + n[1] * h).x;
        let mid = bilinear(g.x, g.y).y;
        let b = bilinear(sx + n[4] * w, sy + n[5] * h).z;
        px = vec3<f32>(r, mid, b);
    } else {
        px = bilinear(g.x, g.y);
    }
    return Lensed(px * n[6], g);
}

fn window(h: f32, lo: f32, hi: f32) -> f32 {
    return smooth01(lo - 15.0, lo, h) * (1.0 - smooth01(hi, hi + 15.0, h));
}

fn luma_at(x: f32, y: f32) -> f32 {
    return dot(p.luma.xyz, mul3(p.colour0, p.colour1, p.colour2, bilinear(x, y)));
}

// Defringe (see `cpu::look::defringe_pixel`, `cpu::look::edge`).
fn defringe(px: vec3<f32>, sx: f32, sy: f32) -> vec3<f32> {
    let l = luma_at(sx - 1.0, sy);
    let r = luma_at(sx + 1.0, sy);
    let u = luma_at(sx, sy - 1.0);
    let d = luma_at(sx, sy + 1.0);
    let g = (abs(r - l) + abs(d - u)) / max(0.25 * (l + r + u + d), 1e-3);
    let e = smooth01(0.1, 0.6, g);
    if (e <= 0.0) {
        return px;
    }
    let lms = mul3(p.lms0, p.lms1, p.lms2, px);
    let l3 = vec3<f32>(cbrt(lms.x), cbrt(lms.y), cbrt(lms.z));
    let lum = 0.21045426 * l3.x + 0.7936178 * l3.y - 0.004072047 * l3.z;
    let a = 1.9779985 * l3.x - 2.4285922 * l3.y + 0.4505937 * l3.z;
    let b = 0.025904037 * l3.x + 0.78277177 * l3.y - 0.80867577 * l3.z;
    let h = rem360(degrees(atan2(b, a)));
    let w = p.fringe.y * window(h, 285.0, 345.0) + p.fringe.z * window(h, 105.0, 165.0);
    let k = max(1.0 - min(w, 1.0) * e, 0.0);
    if (k >= 1.0) {
        return px;
    }
    let la = a * k;
    let lb = b * k;
    let m3 = vec3<f32>(
        lum + 0.39633778 * la + 0.21580376 * lb,
        lum - 0.105561346 * la - 0.06385417 * lb,
        lum - 0.08948418 * la - 1.2914855 * lb,
    );
    return mul3(p.inv0, p.inv1, p.inv2, m3 * m3 * m3);
}

fn tone_at(ch: u32, v: f32) -> f32 {
    let x = clamp(v, 0.0, 1.0) * f32(CURVE_SIZE - 1u);
    let i = min(u32(floor(x)), CURVE_SIZE - 2u);
    let t = x - f32(i);
    let base = ch * CURVE_SIZE;
    return tone[base + i] + (tone[base + i + 1u] - tone[base + i]) * t;
}

fn tone_curve(v: vec3<f32>) -> vec3<f32> {
    let e = pow(max(v, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2));
    return pow(vec3<f32>(tone_at(0u, e.x), tone_at(1u, e.y), tone_at(2u, e.z)), vec3<f32>(2.2));
}

// Display values, and (w) 1 where soft proofing changed the colour.
fn display(px: vec3<f32>) -> vec4<f32> {
    let v = mul3(p.inset0, p.inset1, p.inset2, px);
    var shaped: vec3<f32>;
    if (p.map1.w > 0.5) {
        if (p.misc.z > 0.5) {
            let norm = max(max(max(v.x, v.y), v.z), 1e-10);
            let n = levels(norm);
            let scaled = (max(v, vec3<f32>(0.0)) / norm) * n;
            let per = vec3<f32>(levels(v.x), levels(v.y), levels(v.z));
            let t = clamp(n * n, 0.0, 1.0);
            shaped = scaled + (per - scaled) * t;
        } else {
            shaped = vec3<f32>(levels(v.x), levels(v.y), levels(v.z));
        }
    } else if (p.misc.z > 0.5) {
        let norm = max(max(max(v.x, v.y), v.z), 1e-10);
        let n = lin(curve(norm));
        let scaled = (max(v, vec3<f32>(0.0)) / norm) * n;
        let per = vec3<f32>(lin(curve(v.x)), lin(curve(v.y)), lin(curve(v.z)));
        let t = clamp(n * n, 0.0, 1.0);
        shaped = scaled + (per - scaled) * t;
    } else {
        shaped = vec3<f32>(lin(curve(v.x)), lin(curve(v.y)), lin(curve(v.z)));
    }
    if (p.look3.x > 0.5) {
        shaped = camera_look(shaped);
    }
    if (p.tone.x > 0.5) {
        shaped = tone_curve(shaped);
    }
    var rec = mul3(p.outset0, p.outset1, p.outset2, shaped);
    var out = 0.0;
    if (p.proof0.w > 0.5) {
        let q = mul3(p.proof0, p.proof1, p.proof2, rec);
        let t = mul3(p.target0, p.target1, p.target2, rec);
        let low = min(min(q.x, q.y), q.z) < -GAMUT_SLACK;
        let high = max(max(q.x, q.y), q.z) > 1.0 + GAMUT_SLACK && max(max(t.x, t.y), t.z) <= 1.0;
        if (low || high) {
            out = 1.0;
        }
        rec = mul3(p.unproof0, p.unproof1, p.unproof2, clamp(q, vec3<f32>(0.0), vec3<f32>(1.0)));
    }
    let o = mul3(p.target0, p.target1, p.target2, rec);
    let c = clamp(o, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(oetf(c.x), oetf(c.y), oetf(c.z), out);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (f32(id.x) >= p.size.x || f32(id.y) >= p.size.y) {
        return;
    }
    let x = f32(id.x);
    let y = f32(id.y);
    let pw = p.map2.x * x + p.map2.y * y + p.map2.z;
    var sx = (p.map0.x * x + p.map0.y * y + p.map0.z) / pw;
    var sy = (p.map1.x * x + p.map1.y * y + p.map1.z) / pw;
    var raw: vec3<f32>;
    if (p.lens.x > 0.5) {
        let l = lens_sample(sx, sy);
        raw = l.px;
        sx = l.at.x;
        sy = l.at.y;
    } else {
        raw = bilinear(sx, sy);
    }
    var px = mul3(p.colour0, p.colour1, p.colour2, raw);
    if (p.fringe.x > 0.5) {
        px = defringe(px, sx, sy);
    }
    if (p.maps.x > 0.5) {
        px = local_px(px, sx, sy);
    }
    if (p.look.x > 0.5) {
        px = look(px);
    }
    if (p.misc.w > 0.5) {
        let d = display(px);
        px = d.xyz;
        if (p.unproof0.w > 0.5 && d.w > 0.5) {
            px = vec3<f32>(0.95, 0.2, 0.95);
        }
        if (p.map0.w > 0.5) {
            let hi = max(max(px.x, px.y), px.z);
            if (hi >= 0.999) {
                px = vec3<f32>(1.0, 0.1, 0.1);
            } else if (hi <= 0.001) {
                px = vec3<f32>(0.1, 0.3, 1.0);
            }
        }
    }
    textureStore(dst, vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(px, 1.0));
}
