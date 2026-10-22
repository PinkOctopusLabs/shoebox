// The live pipe for one output pixel (see `cpu::apply` and
// `cpu::display_pixel`, which this follows operation for operation):
// geometry map → lens grid (each channel from where the lens put it, times
// the vignetting gain: `cpu::lens_sample`) → bilinear sample → colour matrix
// → DNG profile tables (`dcp::Stage`) → local (dehaze,
// highlights and shadows, clarity: `cpu::maps`) → local adjustments
// (`cpu::layers`, masks from `mask`) → look (colour, black and
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
    layers: vec4<f32>,    // local adjustments: count, mask width, mask height, mask shown (index + 1; 0: none)
    mscale: vec4<f32>,    // source → mask pixels (x, y); z: the mask alone (0/1); w: unused
    points: vec4<f32>,    // Point Color: count, point shown (index + 1; 0: none)
    smart: vec4<f32>,     // Smart Lighting: strength (0–1), bias (−1..1)
    grade: vec4<f32>,     // colour grading: on (0/1), pivot, power
    grade_wheels: array<vec4<f32>, 4>,  // shadows, midtones, highlights, global: a, b, gain
    fx0: vec4<f32>,       // vignette: amount, midpoint, round, feather
    fx1: vec4<f32>,       // vignette highlights; grain: amount, cell, roughness
    fx2: vec4<f32>,       // the whole frame's size, this render's offset in it
    srcmix: vec4<f32>,    // images beside the source the layers use (array layer + 1; 0: none): AI strength 1, 0, sharpening, softening
    srcg: vec4<f32>,      // the AI strength and the sharpening amount the source has
    dcp: vec4<f32>,       // DNG profile: tables on (0/1), the cooler HueSatMap's weight, back to before exposure, tone curve on (0/1)
    hsm: vec4<f32>,       // HueSatMap tables (offset + 1; 0: none): cooler, warmer; hues, saturations
    hsm2: vec4<f32>,      // HueSatMap values, sRGB-encoded (0/1); LookTable, tone curve (offset + 1; 0: none)
    lkt: vec4<f32>,       // LookTable: hues, saturations, values, sRGB-encoded (0/1)
    pp0: vec4<f32>,       // linear Rec.2020 → linear ProPhoto
    pp1: vec4<f32>,
    pp2: vec4<f32>,
    unpp0: vec4<f32>,     // and back
    unpp1: vec4<f32>,
    unpp2: vec4<f32>,
};

const GAMUT_SLACK: f32 = 2e-3;

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<OUT_FORMAT, write>;
@group(0) @binding(2) var<uniform> p: Params;
// The tone curve: red, green and blue tables of CURVE_SIZE entries.
@group(0) @binding(3) var<storage, read> tone: array<f32>;

// The local maps (see `cpu::maps`): STRIDE floats per map pixel.
@group(0) @binding(4) var<storage, read> maps: array<f32>;

const STRIDE: u32 = 8u;

// The lens grid (see `lens::Grid`): GRID floats per node.
@group(0) @binding(5) var<storage, read> grid: array<f32>;
const GRID: u32 = 7u;

// The colour tables (see `gpu::Tables`), each at an offset given in the
// params (+1): the camera match look (see `profile::Lut3`: size³ RGB
// entries, red fastest), a DNG profile's HueSatMap tables, LookTable and
// tone curve (see `dcp`).
@group(0) @binding(6) var<storage, read> tables: array<f32>;

fn tab3(i: u32) -> vec3<f32> {
    return vec3<f32>(tables[i], tables[i + 1u], tables[i + 2u]);
}

fn lut_at(r: u32, g: u32, b: u32) -> vec3<f32> {
    let n = u32(p.look3.y);
    return tab3(u32(p.look3.z) - 1u + ((b * n + g) * n + r) * 3u);
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

// Images beside the source (see `cpu::Aux`), one array layer each.
@group(0) @binding(10) var aux: texture_2d_array<f32>;

// How this pixel mixes them in (see `cpu::layers::Mix`).
var<private> mix_more: f32 = 0.0;
var<private> mix_less: f32 = 0.0;
var<private> mix_sharpen: f32 = 0.0;
var<private> mix_soften: f32 = 0.0;

// The source as this pixel reads it (see `cpu::Source`).
fn texel(x: f32, y: f32) -> vec3<f32> {
    if (x < 0.0 || y < 0.0 || x >= p.size.z || y >= p.size.w) {
        return vec3<f32>(0.0);
    }
    let at = vec2<i32>(i32(x), i32(y));
    var v = textureLoad(src, at, 0).xyz;
    if (mix_more > 0.0) {
        v = v + mix_more * (textureLoad(aux, at, i32(p.srcmix.x) - 1, 0).xyz - v);
    }
    if (mix_less > 0.0) {
        v = v + mix_less * (textureLoad(aux, at, i32(p.srcmix.y) - 1, 0).xyz - v);
    }
    if (mix_sharpen > 0.0) {
        // Sharpening's fit (see `cpu::detail::Sharpen::at`): alpha, beta.
        let s = textureLoad(aux, at, i32(p.srcmix.z) - 1, 0);
        v = v + s.xyz * (mix_sharpen / (1.0 + s.w * mix_sharpen));
    }
    if (mix_soften > 0.0) {
        v = v + mix_soften * textureLoad(aux, at, i32(p.srcmix.w) - 1, 0).xyz;
    }
    return v;
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

fn eotf(v: f32) -> f32 {
    if (v <= 0.04045) {
        return v / 12.92;
    }
    return pow((v + 0.055) / 1.055, 2.4);
}

// The DNG SDK's HSV (see `dcp::hsv`): hue 0–6, saturation, value.
fn dcp_hsv(c: vec3<f32>) -> vec3<f32> {
    let v = max(max(c.x, c.y), c.z);
    let gap = v - min(min(c.x, c.y), c.z);
    if (gap <= 0.0 || v <= 0.0) {
        return vec3<f32>(0.0, 0.0, v);
    }
    var h: f32;
    if (c.x == v) {
        h = (c.y - c.z) / gap;
    } else if (c.y == v) {
        h = 2.0 + (c.z - c.x) / gap;
    } else {
        h = 4.0 + (c.x - c.y) / gap;
    }
    if (h < 0.0) {
        h += 6.0;
    }
    return vec3<f32>(h, gap / v, v);
}

// And back (see `dcp::rgb`).
fn dcp_rgb(hsv: vec3<f32>) -> vec3<f32> {
    var h = hsv.x;
    let s = hsv.y;
    let v = hsv.z;
    if (s <= 0.0) {
        return vec3<f32>(v);
    }
    h -= 6.0 * floor(h / 6.0);
    let i = min(u32(floor(h)), 5u);
    let f = h - f32(i);
    let pp = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    switch i {
        case 0u: { return vec3<f32>(v, t, pp); }
        case 1u: { return vec3<f32>(q, v, pp); }
        case 2u: { return vec3<f32>(pp, v, t); }
        case 3u: { return vec3<f32>(pp, q, v); }
        case 4u: { return vec3<f32>(t, pp, v); }
        default: { return vec3<f32>(v, pp, q); }
    }
}

// A DNG profile's table: offsets (+1; b 0: one table), b's share 1 − w.
struct Tab { a: u32, b: u32, w: f32, hues: u32, sats: u32, vals: u32, srgb: bool };

fn tab_entry(t: Tab, i: u32) -> vec3<f32> {
    let x = tab3(t.a - 1u + i * 3u);
    if (t.b > 0u) {
        let y = tab3(t.b - 1u + i * 3u);
        return t.w * x + (1.0 - t.w) * y;
    }
    return x;
}

fn lerp3(x: vec3<f32>, y: vec3<f32>, t: f32) -> vec3<f32> {
    return x + (y - x) * t;
}

fn tab_plane(t: Tab, vi: u32, h0: u32, h1: u32, hf: f32, s0: u32, sf: f32) -> vec3<f32> {
    let row = vi * t.hues;
    let a = lerp3(tab_entry(t, (row + h0) * t.sats + s0), tab_entry(t, (row + h1) * t.sats + s0), hf);
    let b = lerp3(
        tab_entry(t, (row + h0) * t.sats + s0 + 1u),
        tab_entry(t, (row + h1) * t.sats + s0 + 1u),
        hf,
    );
    return lerp3(a, b, sf);
}

// The table's change at (h, s, v) (see `dcp::lookup`).
fn tab_lookup(t: Tab, h: f32, s: f32, v: f32) -> vec3<f32> {
    var hs = 0.0;
    if (t.hues >= 2u) {
        hs = h * f32(t.hues) / 6.0;
    }
    var h0 = u32(floor(max(hs, 0.0)));
    var h1 = h0 + 1u;
    if (h0 >= t.hues - 1u) {
        h0 = t.hues - 1u;
        h1 = 0u;
    }
    let hf = hs - f32(h0);
    let ss = clamp(s, 0.0, 1.0) * f32(t.sats - 1u);
    let s0 = min(u32(floor(ss)), t.sats - 2u);
    let sf = ss - f32(s0);
    if (t.vals < 2u) {
        return tab_plane(t, 0u, h0, h1, hf, s0, sf);
    }
    let vs = clamp(v, 0.0, 1.0) * f32(t.vals - 1u);
    let v0 = min(u32(floor(vs)), t.vals - 2u);
    let vf = vs - f32(v0);
    return lerp3(tab_plane(t, v0, h0, h1, hf, s0, sf), tab_plane(t, v0 + 1u, h0, h1, hf, s0, sf), vf);
}

// One table on a linear ProPhoto pixel (see `dcp::hsv_table`).
fn hsv_table(t: Tab, c: vec3<f32>, pre: f32) -> vec3<f32> {
    let hsv = dcp_hsv(c);
    if (hsv.z <= 0.0) {
        return c;
    }
    let at = hsv.z * pre;
    var enc = at;
    if (t.srgb) {
        enc = oetf(at);
    }
    let d = tab_lookup(t, hsv.x, hsv.y, enc);
    let s2 = max(min(hsv.y * d.y, max(hsv.y, 1.0)), 0.0);
    var v2 = hsv.z * d.z;
    if (t.srgb) {
        v2 = eotf(enc * d.z) / pre;
    }
    return dcp_rgb(vec3<f32>(hsv.x + d.x * (6.0 / 360.0), s2, v2));
}

// A DNG profile's tables on linear Rec.2020 (see `dcp::Stage::apply`).
fn dcp_stage(px: vec3<f32>) -> vec3<f32> {
    var q = mul3(p.pp0, p.pp1, p.pp2, px);
    if (p.hsm.x > 0.5) {
        let t = Tab(u32(p.hsm.x), u32(p.hsm.y), p.dcp.y, u32(p.hsm.z), u32(p.hsm.w), u32(p.hsm2.x), p.hsm2.y > 0.5);
        q = hsv_table(t, q, p.dcp.z);
    }
    if (p.hsm2.z > 0.5) {
        let t = Tab(u32(p.hsm2.z), 0u, 1.0, u32(p.lkt.x), u32(p.lkt.y), u32(p.lkt.z), p.lkt.w > 0.5);
        q = hsv_table(t, q, 1.0);
    }
    return mul3(p.unpp0, p.unpp1, p.unpp2, q);
}

const DCP_TONE: u32 = 1024u;

fn dcp_tone_at(v: f32) -> f32 {
    let x = pow(clamp(v, 0.0, 1.0), 1.0 / 2.2) * f32(DCP_TONE - 1u);
    let i = min(u32(floor(x)), DCP_TONE - 2u);
    let f = x - f32(i);
    let o = u32(p.hsm2.w) - 1u + i;
    return tables[o] + (tables[o + 1u] - tables[o]) * f;
}

// A DNG profile's tone curve, hue kept (see `dcp::Tone::apply`).
fn dcp_tone(px: vec3<f32>) -> vec3<f32> {
    let q = clamp(mul3(p.pp0, p.pp1, p.pp2, px), vec3<f32>(0.0), vec3<f32>(1.0));
    let hi = max(max(q.x, q.y), q.z);
    let lo = min(min(q.x, q.y), q.z);
    let th = dcp_tone_at(hi);
    let tl = dcp_tone_at(lo);
    var out = vec3<f32>(th);
    if (hi - lo > 0.0) {
        out = vec3<f32>(tl) + (th - tl) * (q - vec3<f32>(lo)) / (hi - lo);
    }
    return mul3(p.unpp0, p.unpp1, p.unpp2, out);
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

// The colour stage (see `cpu::look::look_point`); w: how much the Point
// Color point shown applied.
fn look(px: vec3<f32>) -> vec4<f32> {
    let lms = mul3(p.lms0, p.lms1, p.lms2, px);
    let l3 = vec3<f32>(cbrt(lms.x), cbrt(lms.y), cbrt(lms.z));
    let lum0 = 0.21045426 * l3.x + 0.7936178 * l3.y - 0.004072047 * l3.z;
    var a = 1.9779985 * l3.x - 2.4285922 * l3.y + 0.4505937 * l3.z;
    let b = 0.025904037 * l3.x + 0.78277177 * l3.y - 0.80867577 * l3.z;
    let shade = clamp(1.0 - lum0 / 0.6, 0.0, 1.0);
    a += p.look.y * 0.04 * shade * shade * max(lum0, 0.0);
    var c = length(vec2<f32>(a, b));
    var h = rem360(degrees(atan2(b, a)));
    let c0 = c;
    let h0 = h;
    let at = bands(h);
    let rel = c / max(lum0, 1e-4);
    let coloured = smooth01(0.0, 0.08, rel);
    h += band(p.hue, at) * coloured;
    c *= max(1.0 + band(p.sat, at) * coloured, 0.0);
    let k = cbrt(exp2(band(p.lum, at) * coloured));
    var lum = lum0 * k;
    c *= k;
    var seen = 0.0;
    let np = u32(p.points.x);
    if (np > 0u) {
        var dh = 0.0;
        var ks = 1.0;
        var stops = 0.0;
        for (var i = 0u; i < np; i++) {
            let w = point_weight(i, lum0, c0, h0);
            if (u32(p.points.y) == i + 1u) {
                seen = w;
            }
            dh += w * points[i * POINT + 3u];
            ks *= max(1.0 + w * points[i * POINT + 4u], 0.0);
            stops += w * points[i * POINT + 5u];
        }
        h += dh;
        c *= ks;
        let kk = cbrt(exp2(stops));
        lum *= kk;
        c *= kk;
    }
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
    if (p.grade.x > 0.5) {
        out = grade(out);
    }
    return vec4<f32>(out, seen);
}

// Colour grading (see `cpu::look::grade`), through Oklab.
fn grade(px: vec3<f32>) -> vec3<f32> {
    let lms = mul3(p.lms0, p.lms1, p.lms2, px);
    let l3 = vec3<f32>(cbrt(lms.x), cbrt(lms.y), cbrt(lms.z));
    let l = 0.21045426 * l3.x + 0.7936178 * l3.y - 0.004072047 * l3.z;
    var a = 1.9779985 * l3.x - 2.4285922 * l3.y + 0.4505937 * l3.z;
    var b = 0.025904037 * l3.x + 0.78277177 * l3.y - 0.80867577 * l3.z;
    let x = pow(clamp(l, 0.0, 1.0), p.grade.y);
    let ws = pow(1.0 - x, p.grade.z);
    let wh = pow(x, p.grade.z);
    let wm = max(1.0 - ws - wh, 0.0);
    let w = array<f32, 4>(ws, wm, wh, 1.0);
    var gain = 0.0;
    for (var i = 0u; i < 4u; i++) {
        a += w[i] * p.grade_wheels[i].x * max(l, 0.0);
        b += w[i] * p.grade_wheels[i].y * max(l, 0.0);
        gain += w[i] * p.grade_wheels[i].z;
    }
    let lum = l * max(1.0 + gain, 0.0);
    let m3 = vec3<f32>(
        lum + 0.39633778 * a + 0.21580376 * b,
        lum - 0.105561346 * a - 0.06385417 * b,
        lum - 0.08948418 * a - 1.2914855 * b,
    );
    return mul3(p.inv0, p.inv1, p.inv2, m3 * m3 * m3);
}

// Local adjustment masks (see `mask::Masks`): one byte per pixel, four to
// a word, layer after layer.
@group(0) @binding(7) var<storage, read> masks: array<u32>;
// Their layers (see `cpu::layers::Layer`): LAYER floats each.
@group(0) @binding(8) var<storage, read> layer: array<f32>;
const LAYER: u32 = 20u;

// Point Color's points (see `cpu::look::Point`): POINT floats each.
@group(0) @binding(9) var<storage, read> points: array<f32>;
const POINT: u32 = 12u;

// How much point i applies to a colour (see `cpu::look::Point::weight`).
fn point_weight(i: u32, lum: f32, c: f32, h: f32) -> f32 {
    let b = i * POINT;
    let d = rem360(h - points[b + 2u]);
    let dh = min(d, 360.0 - d);
    var wh = 1.0 - smooth01(points[b + 6u], 2.0 * points[b + 6u], dh);
    let hued = smooth01(0.0, 0.04, min(c, points[b + 1u]));
    wh = 1.0 + (wh - 1.0) * hued;
    let wc = 1.0 - smooth01(points[b + 7u], 2.0 * points[b + 7u], abs(c - points[b + 1u]));
    let wl = 1.0 - smooth01(points[b + 8u], 2.0 * points[b + 8u], abs(lum - points[b]));
    return wh * wc * wl;
}

fn map_at(x: u32, y: u32, k: u32) -> f32 {
    return maps[(y * u32(p.maps.y) + x) * STRIDE + k];
}

struct Coeffs { hs: vec2<f32>, cl: vec2<f32>, dz: vec2<f32>, sl: vec2<f32> };

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
    var v: array<f32, 8>;
    for (var k = 0u; k < STRIDE; k++) {
        let a = map_at(x0, y0, k);
        let b = map_at(x1, y0, k);
        let c = map_at(x0, y1, k);
        let d = map_at(x1, y1, k);
        let top = a + (b - a) * fx;
        let bot = c + (d - c) * fx;
        v[k] = top + (bot - top) * fy;
    }
    return Coeffs(
        vec2<f32>(v[0], v[1]),
        vec2<f32>(v[2], v[3]),
        vec2<f32>(v[4], v[5]),
        vec2<f32>(v[6], v[7]),
    );
}

fn stops(y: f32) -> f32 {
    return log2(max(y, 1e-6) / p.misc.y);
}

fn dehaze(px: vec3<f32>, t0: f32, amount: f32) -> vec3<f32> {
    if (amount > 0.0) {
        let t = clamp(t0, 0.1, 1.0);
        let j = (px - p.air.xyz) / t + p.air.xyz;
        return max(px + amount * (j - px), vec3<f32>(0.0));
    }
    return px + (-amount) * 0.5 * (p.air.xyz - px);
}

// Dehaze with the maps there (see `cpu::maps::dehaze_at`).
fn dehaze_at(m: Coeffs, px: vec3<f32>, amount: f32) -> vec3<f32> {
    let g = clamp(dot(p.luma.xyz, px) / max(p.air.w, 1e-6), 0.0, 2.0);
    return dehaze(px, m.dz.x * g + m.dz.y, amount);
}

// Highlights, shadows and clarity as a gain in stops (see
// `cpu::maps::tone_gain`).
fn tone_gain(m: Coeffs, s: f32, shadows: f32, highlights: f32, clarity: f32) -> f32 {
    var gain = 0.0;
    if (shadows != 0.0 || highlights != 0.0) {
        let base = m.hs.x * s + m.hs.y;
        gain += shadows * 1.5 * (1.0 - smooth01(-3.0, 0.0, base));
        gain += highlights * 1.5 * smooth01(-0.5, 2.5, base);
    }
    if (clarity != 0.0) {
        let base = m.cl.x * s + m.cl.y;
        let detail = clamp(s - base, -3.0, 3.0);
        let mid = 1.0 - smooth01(2.0, 4.0, abs(base + 0.5));
        gain += clarity * 0.7 * detail * mid;
    }
    return gain;
}

// Dehaze, highlights and shadows, clarity (see `cpu::maps::local_pixel`).
fn local_px(px0: vec3<f32>, sx: f32, sy: f32) -> vec3<f32> {
    let m = coeffs(sx, sy);
    var px = px0;
    if (p.local.w != 0.0) {
        px = dehaze_at(m, px, p.local.w);
    }
    let s = stops(dot(p.luma.xyz, px));
    var gain = tone_gain(m, s, p.local.x, p.local.y, p.local.z);
    if (p.smart.x != 0.0) {
        // Smart Lighting (see `cpu::maps::smart_gain`).
        let g = m.sl.x * s + m.sl.y;
        let k = select(1.0 - p.smart.y, 1.0 + p.smart.y, g > 0.0);
        gain += p.smart.x * g * k;
    }
    if (gain == 0.0) {
        return px;
    }
    return px * exp2(gain);
}

fn mask_byte(i: u32) -> f32 {
    return f32((masks[i / 4u] >> (8u * (i % 4u))) & 0xffu) / 255.0;
}

// Layer k's mask at grid position (x, y) (see `mask::sample`).
fn mask_at(k: u32, x0f: f32, y0f: f32) -> f32 {
    let w = p.layers.y;
    let h = p.layers.z;
    let x = clamp(x0f, 0.0, w - 1.0);
    let y = clamp(y0f, 0.0, h - 1.0);
    let x0 = u32(floor(x));
    let y0 = u32(floor(y));
    let x1 = min(x0 + 1u, u32(w) - 1u);
    let y1 = min(y0 + 1u, u32(h) - 1u);
    let fx = x - f32(x0);
    let fy = y - f32(y0);
    let wi = u32(w);
    let base = k * wi * u32(h);
    let a = mask_byte(base + y0 * wi + x0);
    let b = mask_byte(base + y0 * wi + x1);
    let c = mask_byte(base + y1 * wi + x0);
    let d = mask_byte(base + y1 * wi + x1);
    let top = a + (b - a) * fx;
    let bot = c + (d - c) * fx;
    return top + (bot - top) * fy;
}

fn layer_row(k: u32, r: u32) -> vec3<f32> {
    let i = k * LAYER + r * 4u;
    return vec3<f32>(layer[i], layer[i + 1u], layer[i + 2u]);
}

// The local adjustments (see `cpu::layers::layers_pixel`): masks read at
// (mx, my), before lens corrections; the maps at (sx, sy).
fn layers_px(px0: vec3<f32>, mx: f32, my: f32, sx: f32, sy: f32) -> vec3<f32> {
    let gx = (mx + 0.5) * p.mscale.x - 0.5;
    let gy = (my + 0.5) * p.mscale.y - 0.5;
    let has_maps = p.maps.x > 0.5;
    var m = Coeffs(vec2<f32>(0.0), vec2<f32>(0.0), vec2<f32>(0.0), vec2<f32>(0.0));
    if (has_maps) {
        m = coeffs(sx, sy);
    }
    var px = px0;
    for (var k = 0u; k < u32(p.layers.x); k++) {
        let i = k * LAYER;
        if (layer[i + 17u] < 0.5) {
            continue;
        }
        let w = mask_at(k, gx, gy);
        if (w <= 0.0) {
            continue;
        }
        let contrast = layer[i + 12u];
        let dz = layer[i + 16u];
        // (Sharpness was applied to the source as it was read.)
        var q = px;
        if (dz != 0.0 && has_maps) {
            q = dehaze_at(m, q, dz);
        }
        q = vec3<f32>(dot(layer_row(k, 0u), q), dot(layer_row(k, 1u), q), dot(layer_row(k, 2u), q));
        let s = stops(dot(p.luma.xyz, q));
        var gain = clamp(s, -4.0, 4.0) * 0.35 * contrast;
        if (has_maps) {
            gain += tone_gain(m, s, layer[i + 13u], layer[i + 14u], layer[i + 15u]);
        }
        if (gain != 0.0) {
            q = q * exp2(gain);
        }
        px = px + (q - px) * w;
    }
    return px;
}

// How the layers mix the source at (mx, my) (see `cpu::layers::mix_at`).
fn source_mix(mx: f32, my: f32) {
    let gx = (mx + 0.5) * p.mscale.x - 0.5;
    let gy = (my + 0.5) * p.mscale.y - 0.5;
    let g = p.srcg.x;
    var k = g;
    var a = p.srcg.y;
    var soft = 0.0;
    for (var i = 0u; i < u32(p.layers.x); i++) {
        let b = i * LAYER;
        let ai = layer[b + 19u];
        let sharp = layer[b + 18u];
        if (layer[b + 17u] < 0.5 || (ai == 0.0 && sharp == 0.0)) {
            continue;
        }
        let m = mask_at(i, gx, gy);
        if (m <= 0.0) {
            continue;
        }
        if (ai != 0.0) {
            let t = clamp(k + ai, 0.0, 1.0);
            k += (t - k) * m;
        }
        if (sharp > 0.0) {
            let t = max(min(a + sharp, MAX_SHARPEN), a);
            a += (t - a) * m;
        } else if (sharp < 0.0) {
            let t = min(soft - sharp, 1.0);
            soft += (t - soft) * m;
        }
    }
    if (k > g && p.srcmix.x > 0.5) {
        mix_more = (k - g) / (1.0 - g);
    }
    if (k < g && p.srcmix.y > 0.5) {
        mix_less = (g - k) / g;
    }
    if (p.srcmix.z > 0.5) {
        mix_sharpen = a - p.srcg.y;
    }
    if (p.srcmix.w > 0.5) {
        mix_soften = soft;
    }
}

// The most sharpening a layer reaches (see `cpu::layers::MAX_SHARPEN`).
const MAX_SHARPEN: f32 = 1.5;

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
    } else if (p.dcp.w > 0.5) {
        shaped = dcp_tone(v);
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

// The effects' hash (see `cpu::effects::hash`).
fn fx_hash(x: u32, y: u32) -> f32 {
    var h = (x * 0x8da6b343u) ^ (y * 0xd8163841u);
    h = h ^ (h >> 15u);
    h = h * 0x2c1b3c6du;
    h = h ^ (h >> 12u);
    h = h * 0x297a2d39u;
    h = h ^ (h >> 15u);
    return f32(h & 0xffffu) / 32767.5 - 1.0;
}

fn fx_noise(x: f32, y: f32) -> f32 {
    let x0 = floor(x);
    let y0 = floor(y);
    let tx = x - x0;
    let ty = y - y0;
    let sx = tx * tx * (3.0 - 2.0 * tx);
    let sy = ty * ty * (3.0 - 2.0 * ty);
    let i = u32(x0);
    let j = u32(y0);
    let a = fx_hash(i, j);
    let b = fx_hash(i + 1u, j);
    let c = fx_hash(i, j + 1u);
    let d = fx_hash(i + 1u, j + 1u);
    let top = a + (b - a) * sx;
    let bot = c + (d - c) * sx;
    return top + (bot - top) * sy;
}

// Vignette and grain (see `cpu::effects::apply`).
fn effects(d0: vec3<f32>, x: f32, y: f32) -> vec3<f32> {
    let fx = x + p.fx2.z + 0.5;
    let fy = y + p.fx2.w + 0.5;
    var d = d0;
    if (p.fx0.x != 0.0) {
        let w = max(p.fx2.x, 1.0);
        let h = max(p.fx2.y, 1.0);
        let k = pow(w / h, p.fx0.z);
        let dx = (fx / w - 0.5) * 2.0 * k;
        let dy = (fy / h - 0.5) * 2.0 / k;
        let r = sqrt(dx * dx + dy * dy) / 1.4142135;
        let t = smooth01(p.fx0.y - 0.5 * p.fx0.w, p.fx0.y + 0.5 * p.fx0.w, r);
        let luma = 0.2126 * d.x + 0.7152 * d.y + 0.0722 * d.z;
        if (p.fx0.x < 0.0) {
            let keep = 1.0 - p.fx1.x * smooth01(0.6, 1.0, luma);
            d = d * (1.0 + p.fx0.x * t * keep);
        } else {
            d = d + (vec3<f32>(1.0) - d) * (p.fx0.x * t);
        }
    }
    if (p.fx1.y != 0.0) {
        let gx = fx / p.fx1.z;
        let gy = fy / p.fx1.z;
        var n = fx_noise(gx, gy);
        n += p.fx1.w * 0.5 * fx_noise(gx * 2.0 + 17.0, gy * 2.0 + 31.0);
        let even = max(1.0 / p.fx1.z, 1.0);
        let luma = clamp(0.2126 * d.x + 0.7152 * d.y + 0.0722 * d.z, 0.0, 1.0);
        let g = p.fx1.y * 0.12 * n * (4.0 * luma * (1.0 - luma)) / even;
        d = clamp(d + vec3<f32>(g), vec3<f32>(0.0), vec3<f32>(1.0));
    }
    return d;
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
    let mx = sx;
    let my = sy;
    if (p.layers.x > 0.5 && dot(p.srcmix, vec4<f32>(1.0)) > 0.5) {
        source_mix(mx, my);
    }
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
    if (p.dcp.x > 0.5) {
        px = dcp_stage(px);
    }
    if (p.fringe.x > 0.5) {
        px = defringe(px, sx, sy);
    }
    if (p.maps.x > 0.5) {
        px = local_px(px, sx, sy);
    }
    if (p.layers.x > 0.5) {
        px = layers_px(px, mx, my, sx, sy);
    }
    var seen = 0.0;
    if (p.look.x > 0.5) {
        let lk = look(px);
        px = lk.xyz;
        seen = lk.w;
    }
    if (p.misc.w > 0.5) {
        let d = display(px);
        px = d.xyz;
        if (p.fx0.x != 0.0 || p.fx1.y != 0.0) {
            px = effects(px, x, y);
        }
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
        // A Point Color point's reach (see `cpu::greyed`).
        if (p.points.y > 0.5) {
            let g = 0.2126 * px.x + 0.7152 * px.y + 0.0722 * px.z;
            px = vec3<f32>(g) + (px - vec3<f32>(g)) * seen;
        }
        // A layer's mask shown (see `cpu::layers::mask_shown`).
        if (p.layers.w > 0.5 && u32(p.layers.w) <= u32(p.layers.x)) {
            let w = mask_at(
                u32(p.layers.w) - 1u,
                (mx + 0.5) * p.mscale.x - 0.5,
                (my + 0.5) * p.mscale.y - 0.5,
            );
            if (p.mscale.z > 0.5) {
                px = vec3<f32>(w);
            } else {
                px = px + (vec3<f32>(1.0, 0.0, 0.0) - px) * (0.5 * w);
            }
        }
    }
    textureStore(dst, vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(px, 1.0));
}
