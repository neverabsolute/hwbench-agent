// hwbench 3D tests. Everything is procedural (no vertex buffers, constants in the shader)
// so the work is identical on every backend. Work-per-frame constants used for scoring
// live in gpu/render.rs; keep them in sync with the draw calls there.

const GRID: u32 = 64u;        // geometry: GRID x GRID quads (2 triangles each) per instance
const CUBES_X: u32 = 50u;     // scene: CUBES_X x CUBES_Z cubes
const CUBES_Z: u32 = 40u;

struct VOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) @interpolate(flat) layer: u32,
};

// ---------- geometry: many tiny triangles, depth-tested front to back --------------------

@vertex
fn vs_geom(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> @builtin(position) vec4<f32> {
  var corner = array<vec2<f32>, 6>(
    vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
    vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0));
  let quad = vi / 6u;
  let cell = vec2<f32>(f32(quad % GRID), f32(quad / GRID));
  let jitter = vec2<f32>(f32(ii % 7u), f32(ii % 5u)) * 0.0007;
  let p = (cell + corner[vi % 6u]) / f32(GRID) * 2.0 - 1.0 + jitter;
  // Later instances sit further back, so most of their fragments fail the depth test:
  // the cost is vertex processing and triangle setup, not shading.
  let z = 0.05 + 0.9 * f32(ii) / 1024.0;
  return vec4<f32>(p, z, 1.0);
}

@fragment
fn fs_flat() -> @location(0) vec4<f32> {
  return vec4<f32>(0.2, 0.45, 0.85, 1.0);
}

// ---------- full-screen layers (fill rate, texturing) ---------------------------------------

@vertex
fn vs_full(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {
  // One triangle covering the screen.
  let xy = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
  var o: VOut;
  o.pos = vec4<f32>(xy * 2.0 - 1.0, 0.5, 1.0);
  o.uv = xy;
  o.layer = ii;
  return o;
}

@fragment
fn fs_fill(i: VOut) -> @location(0) vec4<f32> {
  let t = f32(i.layer % 16u) * 0.0625;
  return vec4<f32>(t, 0.5 * i.uv.x, 0.5 * i.uv.y, 0.12);
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@fragment
fn fs_tex(i: VOut) -> @location(0) vec4<f32> {
  var acc = vec4<f32>(0.0);
  let base = i.uv * 3.0 + vec2<f32>(f32(i.layer) * 0.173, f32(i.layer) * 0.071);
  for (var k = 0u; k < 8u; k = k + 1u) {
    let o = vec2<f32>(f32(k) * 0.3711, f32(k) * 0.1913);
    acc = acc + textureSample(tex, samp, base + o);
  }
  return acc * 0.125;
}

// ---------- scene: shadow-mapped, lit field of cubes ---------------------------------------

fn look_at(eye: vec3<f32>, focus: vec3<f32>) -> mat4x4<f32> {
  let f = normalize(focus - eye);
  let s = normalize(cross(f, vec3<f32>(0.0, 1.0, 0.0)));
  let u = cross(s, f);
  return mat4x4<f32>(
    vec4<f32>(s.x, u.x, -f.x, 0.0),
    vec4<f32>(s.y, u.y, -f.y, 0.0),
    vec4<f32>(s.z, u.z, -f.z, 0.0),
    vec4<f32>(-dot(s, eye), -dot(u, eye), dot(f, eye), 1.0));
}

fn perspective(fovy: f32, aspect: f32, near: f32, far: f32) -> mat4x4<f32> {
  let t = 1.0 / tan(fovy * 0.5);
  return mat4x4<f32>(
    vec4<f32>(t / aspect, 0.0, 0.0, 0.0),
    vec4<f32>(0.0, t, 0.0, 0.0),
    vec4<f32>(0.0, 0.0, far / (near - far), -1.0),
    vec4<f32>(0.0, 0.0, near * far / (near - far), 0.0));
}

fn ortho(half: f32, near: f32, far: f32) -> mat4x4<f32> {
  return mat4x4<f32>(
    vec4<f32>(1.0 / half, 0.0, 0.0, 0.0),
    vec4<f32>(0.0, 1.0 / half, 0.0, 0.0),
    vec4<f32>(0.0, 0.0, 1.0 / (near - far), 0.0),
    vec4<f32>(0.0, 0.0, near / (near - far), 1.0));
}

fn camera() -> mat4x4<f32> {
  return perspective(0.9, 16.0 / 9.0, 0.5, 300.0) * look_at(vec3<f32>(0.0, 38.0, 70.0), vec3<f32>(0.0, 0.0, -10.0));
}

fn light() -> mat4x4<f32> {
  return ortho(80.0, 1.0, 250.0) * look_at(vec3<f32>(60.0, 120.0, 40.0), vec3<f32>(0.0, 0.0, -10.0));
}

struct Vert { pos: vec3<f32>, normal: vec3<f32> };

// Instance 0 is the ground; the rest are cubes on a grid with varied heights.
fn cube_vertex(vi: u32, ii: u32) -> Vert {
  var normals = array<vec3<f32>, 6>(
    vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(-1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0),
    vec3<f32>(0.0, -1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 0.0, -1.0));
  var quad = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0));
  let face = vi / 6u;
  let n = normals[face];
  let q = quad[vi % 6u];
  // Build the face from its normal and two tangent axes.
  var t1 = vec3<f32>(0.0, 1.0, 0.0);
  if (abs(n.y) > 0.5) { t1 = vec3<f32>(1.0, 0.0, 0.0); }
  let t2 = cross(n, t1);
  var local = n + t1 * q.x + t2 * q.y;
  var center = vec3<f32>(0.0, -1.0, -10.0);
  var scale = vec3<f32>(90.0, 1.0, 70.0);
  if (ii > 0u) {
    let c = ii - 1u;
    let gx = f32(c % CUBES_X) - f32(CUBES_X) * 0.5;
    let gz = f32(c / CUBES_X) - f32(CUBES_Z) * 0.5;
    let h = 1.0 + f32((c * 2654435761u) >> 28u) * 0.35;
    center = vec3<f32>(gx * 2.6, h, gz * 2.6 - 10.0);
    scale = vec3<f32>(0.9, h, 0.9);
  }
  return Vert(center + local * scale, n);
}

@vertex
fn vs_shadow(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> @builtin(position) vec4<f32> {
  let v = cube_vertex(vi, ii);
  return light() * vec4<f32>(v.pos, 1.0);
}

struct SceneOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) world: vec3<f32>,
  @location(1) normal: vec3<f32>,
  @location(2) @interpolate(flat) id: u32,
};

@vertex
fn vs_scene(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> SceneOut {
  let v = cube_vertex(vi, ii);
  var o: SceneOut;
  o.pos = camera() * vec4<f32>(v.pos, 1.0);
  o.world = v.pos;
  o.normal = v.normal;
  o.id = ii;
  return o;
}

@group(0) @binding(0) var shadow_map: texture_depth_2d;
@group(0) @binding(1) var shadow_samp: sampler_comparison;
@group(0) @binding(2) var albedo: texture_2d<f32>;
@group(0) @binding(3) var albedo_samp: sampler;

@fragment
fn fs_scene(i: SceneOut) -> @location(0) vec4<f32> {
  let l = light() * vec4<f32>(i.world, 1.0);
  let uv = vec2<f32>(l.x * 0.5 + 0.5, 0.5 - l.y * 0.5);
  // 4-tap percentage-closer filtering.
  let texel = 1.0 / 2048.0;
  var lit = 0.0;
  for (var k = 0u; k < 4u; k = k + 1u) {
    let o = vec2<f32>(f32(k & 1u) - 0.5, f32(k >> 1u) - 0.5) * texel;
    lit = lit + textureSampleCompare(shadow_map, shadow_samp, uv + o, l.z - 0.002);
  }
  lit = lit * 0.25;
  let n = normalize(i.normal);
  let ldir = normalize(vec3<f32>(60.0, 120.0, 40.0));
  let view = normalize(vec3<f32>(0.0, 38.0, 70.0) - i.world);
  let diffuse = max(dot(n, ldir), 0.0);
  let spec = pow(max(dot(n, normalize(ldir + view)), 0.0), 32.0);
  let base = textureSample(albedo, albedo_samp, i.world.xz * 0.11 + i.world.y * 0.07).rgb
    * (0.6 + 0.4 * vec3<f32>(f32(i.id % 3u), f32(i.id % 5u), f32(i.id % 7u)) / vec3<f32>(2.0, 4.0, 6.0));
  let color = base * (0.15 + 0.85 * diffuse * lit) + vec3<f32>(spec * lit * 0.4);
  return vec4<f32>(color, 1.0);
}
