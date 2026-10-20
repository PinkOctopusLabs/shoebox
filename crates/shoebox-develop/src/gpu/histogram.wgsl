// Histograms of a rendered image (display values): R, G, B and luma, 256
// bins each, counted as 8-bit values (luma = (77 R + 150 G + 29 B) >> 8, as
// the library's thumbnails count it). See `cpu::histogram`.

@group(0) @binding(0) var img: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>, 1024>;

fn q(v: f32) -> u32 {
    return u32(clamp(v, 0.0, 1.0) * 255.0 + 0.5);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(img);
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }
    let p = textureLoad(img, vec2<i32>(i32(id.x), i32(id.y)), 0);
    let r = q(p.x);
    let g = q(p.y);
    let b = q(p.z);
    atomicAdd(&bins[r], 1u);
    atomicAdd(&bins[256u + g], 1u);
    atomicAdd(&bins[512u + b], 1u);
    atomicAdd(&bins[768u + ((77u * r + 150u * g + 29u * b) >> 8u)], 1u);
}
