// Lens Blur's two passes on the working copy (see `cpu::lens_blur`, which
// these follow operation for operation): `prepare` averages the source
// into the working copy with each pixel's circle of confusion, `gather`
// gathers it as if scattered into the back and front layers. The develop
// pass mixes them into the source as it reads it (`blur_texel`).

struct Blur {
    size: vec4<f32>,   // source width, height; working width, height
    focus: vec4<f32>,  // focus depth, half the range, circle per depth unit, boost (0–1)
    depth: vec4<f32>,  // depth map width, height; source → depth map (x, y)
    work: vec4<f32>,   // source pixels per working pixel (x, y); taps to use; where the area table starts
    area: vec4<f32>,   // the area table's last step but one
};

const BOOST: f32 = 4.0;
const AREA_STEPS: f32 = 8.0;

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var depth: texture_2d<f32>;
@group(0) @binding(2) var<uniform> b: Blur;
@group(0) @binding(3) var work_out: texture_storage_2d<rgba32float, write>;

@group(0) @binding(4) var work: texture_2d<f32>;
// Taps (dx, dy, distance, 0), nearest first, then the area table.
@group(0) @binding(5) var<storage, read> taps: array<f32>;
@group(0) @binding(6) var<uniform> g: Blur;
@group(0) @binding(7) var layers_out: texture_storage_2d_array<rgba32float, write>;

fn depth_px(x: u32, y: u32) -> f32 {
    return textureLoad(depth, vec2<i32>(i32(x), i32(y)), 0).x;
}

// The depth map at grid position (x, y), bilinear, clamped (`Depth::at`).
fn depth_at_grid(x0f: f32, y0f: f32) -> f32 {
    let w = b.depth.x;
    let h = b.depth.y;
    let x = clamp(x0f, 0.0, w - 1.0);
    let y = clamp(y0f, 0.0, h - 1.0);
    let x0 = u32(floor(x));
    let y0 = u32(floor(y));
    let x1 = min(x0 + 1u, u32(w) - 1u);
    let y1 = min(y0 + 1u, u32(h) - 1u);
    let fx = x - f32(x0);
    let fy = y - f32(y0);
    let a = depth_px(x0, y0);
    let bb = depth_px(x1, y0);
    let c = depth_px(x0, y1);
    let d = depth_px(x1, y1);
    let top = a + (bb - a) * fx;
    let bot = c + (d - c) * fx;
    return top + (bot - top) * fy;
}

fn coc(d: f32) -> f32 {
    let e = d - b.focus.x;
    let m = max(abs(e) - b.focus.y, 0.0) * b.focus.z;
    return select(-m, m, e >= 0.0);
}

fn coc_at(x: f32, y: f32) -> f32 {
    return coc(depth_at_grid((x + 0.5) * b.depth.z - 0.5, (y + 0.5) * b.depth.w - 0.5));
}

fn boosted(p: vec3<f32>) -> vec3<f32> {
    if (b.focus.w <= 0.0) {
        return p;
    }
    let m = max(max(p.x, p.y), p.z);
    let t = clamp((m - 0.4) / 0.6, 0.0, 1.0);
    let k = 1.0 + BOOST * b.focus.w * t * t * (3.0 - 2.0 * t);
    return p * k;
}

fn alike(d: f32) -> f32 {
    return 1.0 / (1.0 + d * d);
}

// The source pixels working pixel o of m averages, of n (`lens_blur::span`).
fn span(o: u32, n: u32, m: u32) -> vec2<u32> {
    let a = o * n / m;
    return vec2<u32>(a, max(((o + 1u) * n + m - 1u) / m, a + 1u));
}

@compute @workgroup_size(16, 16)
fn prepare(@builtin(global_invocation_id) id: vec3<u32>) {
    let ww = u32(b.size.z);
    let wh = u32(b.size.w);
    if (id.x >= ww || id.y >= wh) {
        return;
    }
    let xs = span(id.x, u32(b.size.x), ww);
    let ys = span(id.y, u32(b.size.y), wh);
    var c = -3.4028235e38;
    for (var y = ys.x; y < ys.y; y++) {
        for (var x = xs.x; x < xs.y; x++) {
            c = max(c, coc_at(f32(x), f32(y)));
        }
    }
    var sum = vec3<f32>(0.0);
    var n = 0.0;
    for (var y = ys.x; y < ys.y; y++) {
        for (var x = xs.x; x < xs.y; x++) {
            let p = boosted(textureLoad(src, vec2<i32>(i32(x), i32(y)), 0).xyz);
            let w = alike(coc_at(f32(x), f32(y)) - c);
            sum += w * p;
            n += w;
        }
    }
    textureStore(work_out, vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(sum / n, c));
}

fn cover(r: f32, d: f32) -> f32 {
    return clamp(max(r, 0.5) + 0.5 - d, 0.0, 1.0);
}

fn area(r: f32) -> f32 {
    let x = max(r, 0.5) * AREA_STEPS;
    let i = min(floor(x), g.area.x);
    let t = x - i;
    let at = u32(g.work.w) + u32(i);
    return taps[at] + (taps[at + 1u] - taps[at]) * t;
}

@compute @workgroup_size(16, 16)
fn gather(@builtin(global_invocation_id) id: vec3<u32>) {
    let ww = g.size.z;
    let wh = g.size.w;
    if (f32(id.x) >= ww || f32(id.y) >= wh) {
        return;
    }
    let at = vec2<i32>(i32(id.x), i32(id.y));
    let p = textureLoad(work, at, 0);
    let cp = p.w;
    let rp = abs(cp);
    let margin = max(0.5 * rp, 1.0);
    var bs = vec3<f32>(0.0);
    var bw = 0.0;
    var fs = vec3<f32>(0.0);
    var fw = 0.0;
    let count = u32(g.work.z);
    for (var k = 0u; k < count; k++) {
        let qx = f32(id.x) + taps[k * 4u];
        let qy = f32(id.y) + taps[k * 4u + 1u];
        if (qx < 0.0 || qy < 0.0 || qx >= ww || qy >= wh) {
            continue;
        }
        let d = taps[k * 4u + 2u];
        let q = textureLoad(work, vec2<i32>(i32(qx), i32(qy)), 0);
        let cq = q.w;
        let rq = abs(cq);
        let a = area(rq);
        let s = clamp(cq - cp - margin, 0.0, 1.0);
        if (s > 0.0) {
            let w = s * cover(rq, d) / a;
            fs += w * q.xyz;
            fw += w;
        }
        if (s < 1.0) {
            var r = rq;
            if (cq < cp) {
                r = min(rq, rp);
            }
            let w = (1.0 - s) * cover(r, d) / a;
            bs += w * q.xyz;
            bw += w;
        }
    }
    var back = p.xyz;
    if (bw > 0.0) {
        back = bs / bw;
    }
    textureStore(layers_out, at, 0, vec4<f32>(back, cp));
    var front = vec4<f32>(0.0);
    if (fw > 0.0) {
        let alpha = min(fw, 1.0);
        front = vec4<f32>(fs * (alpha / fw), alpha);
    }
    textureStore(layers_out, at, 1, front);
}
