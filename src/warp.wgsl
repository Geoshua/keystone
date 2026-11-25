// Keystone warp: for every output pixel, map back through the inverse homography
// into the source texture. Outside the warped quad is black (projector "off").

struct Uniforms {
    // Columns of the 3x3 output->source matrix (w unused).
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
    // x: 1 = target is non-sRGB, encode manually. y,z: one output pixel in uv units.
    // w: 1 = draw the calibration grid lines over the source.
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Single oversized triangle covering the callback viewport.
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    var o: VsOut;
    o.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    o.uv = vec2<f32>(x, y);
    return o;
}

fn sample_warped(uv: vec2<f32>) -> vec3<f32> {
    let m = mat3x3<f32>(u.m0.xyz, u.m1.xyz, u.m2.xyz);
    let s = m * vec3<f32>(uv, 1.0);
    if (s.z <= 0.0) {
        return vec3<f32>(0.0);
    }
    let st = s.xy / s.z;
    if (any(st < vec2<f32>(0.0)) || any(st > vec2<f32>(1.0))) {
        return vec3<f32>(0.0);
    }
    var c = textureSampleLevel(src_tex, src_sampler, st, 0.0).rgb;
    if (u.params.w > 0.5) {
        // 8x8 alignment grid in source space, so straight lines prove the warp is right.
        // Distance to the nearest grid line, in source uv; lines ~1.5 output px wide.
        let dist = abs(fract(st * 8.0 + 0.5) - 0.5) / 8.0;
        let half_width = u.params.yz * 0.75;
        if (dist.x < half_width.x || dist.y < half_width.y) {
            c = vec3<f32>(0.0, 1.0, 0.4);
        }
    }
    return c;
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let cutoff = c < vec3<f32>(0.0031308);
    let lower = c * 12.92;
    let higher = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(higher, lower, cutoff);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // 4-tap rotated-grid supersampling: smooths the minified (narrowed) side and
    // anti-aliases the quad edges.
    let d = u.params.yz;
    var c = sample_warped(in.uv + d * vec2<f32>(-0.125, -0.375));
    c += sample_warped(in.uv + d * vec2<f32>(0.375, -0.125));
    c += sample_warped(in.uv + d * vec2<f32>(0.125, 0.375));
    c += sample_warped(in.uv + d * vec2<f32>(-0.375, 0.125));
    c *= 0.25;
    if (u.params.x > 0.5) {
        c = linear_to_srgb(c);
    }
    return vec4<f32>(c, 1.0);
}
