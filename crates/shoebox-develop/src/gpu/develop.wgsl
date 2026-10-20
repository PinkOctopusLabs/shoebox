// The live pipe for one output pixel (see `cpu::apply` and
// `cpu::display_pixel`, which this follows operation for operation):
// geometry map → bilinear sample → colour matrix → display transform →
// output space and sRGB transfer curve.
//
// OUT_FORMAT is replaced with the output texture's format when the
// pipeline is built (rgba32float for read-back, rgba16float for display).

struct Params {
    map0: vec4<f32>,      // sx = map0.x·x + map0.y·y + map0.z
    map1: vec4<f32>,      // sy = map1.x·x + map1.y·y + map1.z
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
    curve: vec4<f32>,     // up scale, up hardness, toe a, toe floor
    misc: vec4<f32>,      // display grey, mid grey, preserve hue (0/1), display (0/1)
    size: vec4<f32>,      // out width, out height, src width, src height
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<OUT_FORMAT, write>;
@group(0) @binding(2) var<uniform> p: Params;

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

fn display(px: vec3<f32>) -> vec3<f32> {
    let v = mul3(p.inset0, p.inset1, p.inset2, px);
    var shaped: vec3<f32>;
    if (p.misc.z > 0.5) {
        let norm = max(max(max(v.x, v.y), v.z), 1e-10);
        let n = lin(curve(norm));
        let scaled = (max(v, vec3<f32>(0.0)) / norm) * n;
        let per = vec3<f32>(lin(curve(v.x)), lin(curve(v.y)), lin(curve(v.z)));
        let t = clamp(n * n, 0.0, 1.0);
        shaped = scaled + (per - scaled) * t;
    } else {
        shaped = vec3<f32>(lin(curve(v.x)), lin(curve(v.y)), lin(curve(v.z)));
    }
    let o = mul3(p.target0, p.target1, p.target2, mul3(p.outset0, p.outset1, p.outset2, shaped));
    let c = clamp(o, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec3<f32>(oetf(c.x), oetf(c.y), oetf(c.z));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (f32(id.x) >= p.size.x || f32(id.y) >= p.size.y) {
        return;
    }
    let x = f32(id.x);
    let y = f32(id.y);
    let sx = p.map0.x * x + p.map0.y * y + p.map0.z;
    let sy = p.map1.x * x + p.map1.y * y + p.map1.z;
    var px = mul3(p.colour0, p.colour1, p.colour2, bilinear(sx, sy));
    if (p.misc.w > 0.5) {
        px = display(px);
    }
    textureStore(dst, vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(px, 1.0));
}
