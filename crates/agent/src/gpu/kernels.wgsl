// hwbench GPU kernels. Ops-per-iteration constants used for scoring live in
// gpu/mod.rs next to the dispatch code; keep them in sync with these loops.

struct Params { iters: u32, salt: u32, len: u32, _pad: u32 }
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> outf: array<f32>;
@group(0) @binding(2) var<storage, read_write> outu: array<u32>;

// 8 independent vec4 FMA chains per iteration = 8 * 4 * 2 = 64 FLOP.
// Multipliers < 1 keep every chain bounded (converges to c / (1 - b)).
@compute @workgroup_size(256)
fn fp32(@builtin(global_invocation_id) gid: vec3<u32>) {
  let s = f32((gid.x ^ p.salt) & 1023u) * 0.0009765625;
  let c = vec4<f32>(0.001, 0.002, 0.003, 0.004);
  var a0 = vec4<f32>(s, s + 0.1, s + 0.2, s + 0.3);
  var a1 = a0 + 0.01; var a2 = a0 + 0.02; var a3 = a0 + 0.03;
  var a4 = a0 + 0.04; var a5 = a0 + 0.05; var a6 = a0 + 0.06; var a7 = a0 + 0.07;
  let b0 = vec4<f32>(0.9990, 0.9991, 0.9992, 0.9993);
  let b1 = vec4<f32>(0.9994, 0.9995, 0.9996, 0.9997);
  for (var i = 0u; i < p.iters; i = i + 1u) {
    a0 = fma(a0, b0, c); a1 = fma(a1, b1, c); a2 = fma(a2, b0, c); a3 = fma(a3, b1, c);
    a4 = fma(a4, b0, c); a5 = fma(a5, b1, c); a6 = fma(a6, b0, c); a7 = fma(a7, b1, c);
  }
  let r = a0 + a1 + a2 + a3 + a4 + a5 + a6 + a7;
  outf[gid.x] = r.x + r.y + r.z + r.w;
}

// 4 independent chains per iteration, each: mul, add, xor, shift = 4 ops,
// so 16 integer ops per iteration.
@compute @workgroup_size(256)
fn int32(@builtin(global_invocation_id) gid: vec3<u32>) {
  var x0 = gid.x ^ p.salt;
  var x1 = x0 + 0x9e3779b9u; var x2 = x0 + 0x7f4a7c15u; var x3 = x0 + 0x6c8e9cf5u;
  for (var i = 0u; i < p.iters; i = i + 1u) {
    x0 = x0 * 1664525u + 1013904223u; x0 = x0 ^ (x0 >> 13u);
    x1 = x1 * 22695477u + 1u;         x1 = x1 ^ (x1 >> 11u);
    x2 = x2 * 1103515245u + 12345u;   x2 = x2 ^ (x2 >> 15u);
    x3 = x3 * 134775813u + 1u;        x3 = x3 ^ (x3 >> 7u);
  }
  outu[gid.x] = x0 ^ x1 ^ x2 ^ x3;
}

// Bandwidth: copy the first half of `outu` into the second half, 16 bytes per
// load/store via 4 consecutive u32s (grid-stride loop over `len` u32 words).
@compute @workgroup_size(256)
fn copy(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
  let half = p.len / 2u;
  let stride = nwg.x * 256u * 4u;
  for (var i = gid.x * 4u; i + 3u < half; i = i + stride) {
    outu[half + i] = outu[i];
    outu[half + i + 1u] = outu[i + 1u];
    outu[half + i + 2u] = outu[i + 2u];
    outu[half + i + 3u] = outu[i + 3u];
  }
}

// Image convolution: a 5×5 filter over a 1024×1024 image (from `outu`) into `outf`, staged
// through workgroup shared memory as image filters do. Each iteration filters the previous
// iteration's output (ping-ponging between two tiles), so p.iters scales the work:
// 1024×1024 pixels per iteration. Every pass depends on the one before, so a compiler can't
// hoist the filter out of the loop (agent 0.6.1 and earlier re-filtered the same input, and
// AMD's compiler legitimately computed it once).
const IMG: u32 = 1024u;
var<workgroup> tile: array<f32, 800>; // two (16 + 4) × (16 + 4) tiles

@compute @workgroup_size(16, 16)
fn blur(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>,
        @builtin(workgroup_id) wid: vec3<u32>) {
  let origin = vec2<i32>(wid.xy * 16u) - vec2<i32>(2, 2);
  for (var t = lid.y * 16u + lid.x; t < 400u; t = t + 256u) {
    let q = clamp(origin + vec2<i32>(i32(t % 20u), i32(t / 20u)), vec2<i32>(0, 0), vec2<i32>(i32(IMG) - 1, i32(IMG) - 1));
    let v = f32((outu[u32(q.y) * IMG + u32(q.x)] ^ p.salt) & 0xffffu) * (1.0 / 65535.0);
    tile[t] = v;
    tile[400u + t] = v; // the 2-pixel border stays fixed in both tiles
  }
  workgroupBarrier();
  let centre = (lid.y + 2u) * 20u + lid.x + 2u;
  var sum = 0.0;
  for (var it = 0u; it < p.iters; it = it + 1u) {
    let src = (it & 1u) * 400u;
    sum = 0.0;
    for (var dy = 0u; dy < 5u; dy = dy + 1u) {
      for (var dx = 0u; dx < 5u; dx = dx + 1u) {
        let w = f32((dx + 1u) * (dy + 1u)) * 0.0044444; // weights sum to 1
        sum = sum + tile[src + (lid.y + dy) * 20u + lid.x + dx] * w;
      }
    }
    tile[400u - src + centre] = sum;
    workgroupBarrier();
  }
  outf[gid.y * IMG + gid.x] = sum;
}
