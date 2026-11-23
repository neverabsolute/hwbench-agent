# Scoring methodology

These are the formulas behind every hwbench score.
[`scoring/v2.json`](../scoring/v2.json) holds the exact values, and the site
reads that same file.

## CPU

Ten deterministic workloads (see `crates/kernels`). The first five are throughput
tests; the last five behave like real programs:

| Kernel | What it stresses |
|---|---|
| `hash` | Integer/crypto throughput (SHA-256, uses SHA extensions where present) |
| `compress` | Branchy integer code (DEFLATE level 6) |
| `sort` | Memory access patterns and branch prediction (unstable sort of 512 Ki u64) |
| `float` | Double-precision throughput (128×128 matrix multiply) |
| `physics` | Mixed FP with division and sqrt (N-body step) |
| `search` | Game AI: alpha-beta search of Othello positions with bitboards (unpredictable branches) |
| `script` | A bytecode interpreter running a small program (dispatch-heavy, like JavaScript or Python engines) |
| `render` | A path tracer rendering a small scene (floating point with irregular branches) |
| `crypto` | AES-256-GCM, as in HTTPS and disk encryption (uses AES and carry-less multiply instructions where present) |
| `parse` | Generating and parsing JSON-like text (browsers, APIs, config files) |

Each kernel runs for a fixed 0.8 s window at 1 thread, 4 threads, and all
available threads. A sub-score is the geometric mean, across kernels, of
`chunks/sec ÷ reference single-thread chunks/sec × 100`. So the reference CPU
scores 100 single-core, about 400 quad-core, and more multi-core.

The headline CPU score is a weighted geometric mean of the three sub-scores,
each divided by what the reference CPU scores (single 100, quad 400, multi-core
1000; weights and normalizers in `scoring/v2.json`). The multi-core normalizer comes
from modeling an i7-12700K's P-cores and E-cores with the per-workload thread
scaling measured on a Ryzen 9 9950X3D (about 985); public all-core ratios between
the two chips give about 980. A measured full-thread i7-12700K result will replace
it. The site shows the sub-scores next to the headline score, so readers can weigh
them differently.

## Memory

Multi-threaded read, write and copy bandwidth over a buffer larger than any
CPU cache, plus dependent-load latency over a random cycle of cache lines.
Score: geometric mean of the four ratios against the reference.

The agent also measures that latency at working-set sizes from 16 KiB to 256 MiB
(a "ladder"). Each flat stretch is a cache level and each rise is that level running
out. The ladder is shown with results but isn't scored.

## GPU

Eight tests, run through wgpu on the platform's native API (Metal on macOS, DX12
on Windows, Vulkan on Linux), in two groups.

Compute (shaders in `crates/agent/src/gpu/kernels.wgsl`):

| Test | What it measures |
|---|---|
| `fp32` | 8 independent vec4 fused multiply-add chains: 64 FLOP per iteration |
| `int32` | 4 independent multiply/add/xor/shift chains: 16 integer ops per iteration |
| `copy` | Buffer-to-buffer copy over up to 512 MiB: read + write bandwidth |
| `blur` | A 5×5 filter applied repeatedly to a 1024×1024 image, staged through workgroup shared memory; each pass filters the previous pass's output |

3D, rendered off-screen at 1920×1080 (`crates/agent/src/gpu/render.wgsl`; all
geometry is generated in the shaders, so every backend does identical work):

| Test | What it measures |
|---|---|
| `geometry` | 64×64 quad grids × 1,024 instances (≈8.4 M triangles per frame), depth-tested front to back: triangle setup, rasterization and depth rejection |
| `fill` | 32 alpha-blended full-screen layers: blending and render output throughput |
| `texture` | 8 layers sampling a 2048×2048 texture 8 times per pixel with bilinear filtering: texture units |
| `scene` | A 2048² shadow-map pass plus a lit, textured pass over 2,000 cubes and a ground plane, with 4-tap filtered shadows: frames per second |

Each test first sizes its batch so one batch takes about 30 ms (long enough to
time, far below OS GPU watchdogs), then repeats it for 2 s. The **fastest** batch
counts, which filters out interference from the desktop compositor and background
apps. Compute and 3D sub-scores are weighted geometric means against a reference
GPU; the GPU score combines them, 40% compute and 60% 3D (weights in
`scoring/v2.json`). **100 = NVIDIA GeForce RTX 4060.** The compute references are
projected from its rated specifications at the efficiency these kernels reach on a
real RTX 5090 (about 69% of rated FP32 throughput and memory bandwidth). The image
filter and 3D references are the 5090's measured results scaled by the ratio of the
hardware each test is bound by (SMs, raster units, ROPs or texture units, times boost
clock); the scene uses the geometric mean of those ratios. All are provisional until
measured RTX 4060 results replace them.

GPU results carry **no proof of work**. A GPU does far more work per second than
the server could re-check, so GPU results are kept honest by per-model
distributions, hard ceilings above any shipping GPU, and the same server-side
wall-clock bound as CPU runs. Software renderers (e.g. llvmpipe, WARP) are
recorded but never counted.

## Storage

Runs against a temporary file (1 GiB, or 256 MiB when free space is short) on the
drive holding the system temp directory, or the folder given with `--disk-path`.
The file is filled with random data, so drives that compress data can't inflate
results, and it's deleted afterwards. The OS page cache is bypassed (`O_DIRECT`
on Linux, `F_NOCACHE` on macOS, `FILE_FLAG_NO_BUFFERING` on Windows); the drive's
own cache is not, which matches how applications experience the drive.

| Test | Settings |
|---|---|
| Sequential read / write | 1 MiB blocks, queue depth 1, repeated passes for at least 2 s |
| 4K random read / write QD1 | one request at a time: latency-bound, what makes a system feel fast |
| 4K random read / write QD32 | 32 threads issuing requests in parallel: peak IOPS |

Score: weighted geometric mean against a reference drive (weights in
`scoring/v2.json`; 4K QD1 reads weigh most). Results where the OS cache couldn't
be bypassed, or that come from virtual disks, are recorded but not counted.
Storage results are sensitive to other disk activity on the machine.

## Profiles

There's no single right weighting: a gamer and someone who renders video value
different things. So CPU and storage scores also come in three **profiles**
that reweight the *same* measured sub-scores (weights and rationale in
`scoring/v2.json`):

| CPU profile | Single | Quad | Multi |
|---|---|---|---|
| Gaming | 40% | 40% | 20% |
| Everyday (headline score) | 40% | 30% | 30% |
| Workstation | 15% | 15% | 70% |

| Storage profile | Seq read | Seq write | 4K QD1 read | 4K QD1 write | 4K QD32 read | 4K QD32 write |
|---|---|---|---|---|---|---|
| Gaming | 35% | | 35% | | 30% | |
| Everyday (headline score) | 15% | 15% | 25% | 15% | 15% | 15% |
| Workstation | 25% | 25% | | | 25% | 25% |

Rankings can be sorted by any profile, and component pages show all three.

## Builds

A saved parts list ("build") gets an expected score per profile: a weighted
geometric mean of its parts' scores for that profile.

| Build profile | CPU | GPU | Storage |
|---|---|---|---|
| Gaming | 35% | 50% | 15% |
| Everyday | 50% | 20% | 30% |
| Workstation | 60% | 25% | 15% |

Parts without results are left out and the remaining weights rescaled. Runs
started from a build are listed on its page next to the expected score.

## Component scores and percentiles

A component's page shows the **median** of all accepted results for that model
and the full distribution. It never shows a mean, because a few broken or faked
results can't move a median much. "Your result is in the Nth percentile" compares
a run with every accepted result for the same model.

## Checking results

* CPU results include per-chunk digests for a seed the server issues. The server
  re-runs a random sample of chunks and rejects runs whose digests don't match.
* Consistency checks catch results such as multi-core scaling beyond the reported
  core count. Results more than 30% above the model's median (and more than 6
  median absolute deviations) are held for review and stay out of aggregates
  until a moderator reviews them.
* Overclocked and tuned parts count like any other result, without a label. Component
  pages show the fastest and slowest results. The agent reports live CPU clocks on Linux
  (cpufreq), shown with a run's hardware details; Windows and macOS don't expose reliable
  live clocks to programs without administrator rights.
* Runs where the OS limits the threads available (VMs, containers) are kept but
  left out of multi-core aggregates.
* CPU results whose four-thread test runs at less than 2.6 times the single-thread
  speed (on a CPU with at least four cores) are kept but not counted: the work ran on
  efficiency cores or was throttled. On Windows the agent opts out of power throttling
  (EcoQoS), which otherwise moves background programs onto efficiency cores.

Timing can't be proven cryptographically on someone else's machine. These checks
make a fake result take effort, and because component scores are medians, a few
fakes move them very little.

## Changes

Version 2 added five CPU workloads, the GPU 3D tests and the image filter, and
the memory latency ladder. Results from version 1 (agents before 0.6.0) aren't
comparable and aren't counted; the site turns older agents away. Provisional
reference values inside a version (currently the CPU multi-core normalizer and the
GPU and storage references) may be recalibrated in place; stored results are
rescored from their raw metrics when that happens.

Agent 0.6.2 changed the image filter. Earlier agents filtered the same input on every
pass, and AMD's shader compiler correctly worked out that it only needed to do it once,
so Radeon cards scored many times their real image-filter speed. GPU results from
earlier agents aren't counted. 0.6.2 also checks, before it starts, whether other
programs are keeping the computer busy (CPU, GPU or graphics memory), says which, and
waits up to 30 seconds for them to settle. Runs that start busy still count, and their
results page says so.
