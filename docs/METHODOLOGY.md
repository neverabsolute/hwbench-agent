# How hwbench scores hardware

hwbench measures hardware and publishes the numbers. No editorial adjustments,
no hand-tuned penalties for any brand. The formulas below are the whole method,
and [`scoring/v1.json`](../scoring/v1.json) is the exact configuration the site uses.

## CPU

Five deterministic workloads (see `crates/kernels`):

| Kernel | What it stresses |
|---|---|
| `hash` | Integer/crypto throughput (SHA-256, uses SHA extensions where present) |
| `compress` | Branchy integer code (DEFLATE level 6) |
| `sort` | Memory access patterns and branch prediction (unstable sort of 512 Ki u64) |
| `float` | Double-precision throughput (128×128 matrix multiply) |
| `physics` | Mixed FP with division and sqrt (N-body step) |

Each kernel runs for a fixed 1.5 s window at 1 thread, 4 threads, and all
available threads. A sub-score is the geometric mean, across kernels, of
`chunks/sec ÷ reference single-thread chunks/sec × 100`. So the reference CPU
scores 100 single-core, about 400 quad-core, and more multi-core.

The headline CPU score is a weighted geometric mean of the three sub-scores
(weights in `scoring/v1.json`). The sub-scores are always shown next to it,
so readers can apply their own weighting.

## Memory

Multi-threaded read, write and copy bandwidth over a buffer larger than any
CPU cache, plus dependent-load latency over a random cycle of cache lines.
Score: geometric mean of the four ratios against the reference.

## GPU

Three compute shaders, run through wgpu on the platform's native API (Metal on
macOS, DX12 on Windows, Vulkan on Linux):

| Test | What it measures |
|---|---|
| `fp32` | 8 independent vec4 fused multiply-add chains: 64 FLOP per iteration |
| `int32` | 4 independent multiply/add/xor/shift chains: 16 integer ops per iteration |
| `copy` | Buffer-to-buffer copy over up to 512 MiB: read + write bandwidth |

Each test first sizes its dispatch so one dispatch takes about 30 ms (long enough
to time, far below OS GPU watchdogs), then repeats it for 2 s. The **fastest**
dispatch counts, which filters out interference from the desktop compositor and
background apps. Score: weighted geometric mean against a reference GPU (weights in
`scoring/v1.json`). **100 = NVIDIA GeForce RTX 4060**, currently projected from
its rated specifications at the efficiency these kernels reach on a real
RTX 5090 (about 69% of rated FP32 throughput and memory bandwidth). It will be
replaced by measured RTX 4060 results.

These are compute tests. Gaps between GPUs are usually larger here than in
games, which are often limited by the CPU, the resolution, or fixed-function
hardware.

GPU results carry **no proof of work**. A GPU does far more work per second than
the server could re-check, so GPU results are kept honest by per-model
distributions, hard ceilings above any shipping GPU, and the same server-side
wall-clock bound as CPU runs. Software renderers (e.g. llvmpipe, WARP) are
recorded but never counted.

## Component scores and percentiles

A component's page shows the **median** of all accepted results for that model
and the full distribution. It never shows a mean, because a few broken or faked
results can't move a median much. "Your result is in the Nth percentile" compares
a run with every accepted result for the same model.

## Keeping results honest

* **Proof of work:** CPU results include per-chunk digests for a server-issued
  seed. The server re-runs a random sample of chunks and rejects mismatches.
* **Consistency checks:** e.g. multi-core scaling impossible for the reported
  core count, or results far outside the model's distribution (median absolute
  deviation test). Flagged results are excluded from aggregates until a
  moderator reviews them.
* **Constrained environments:** runs where the OS limits available threads
  (VMs, containers) are kept but excluded from multi-core aggregates.

Timing can't be proven cryptographically on someone else's machine. The checks
above are designed so that faking a result takes effort and still can't shift
the published medians.

## Changes

Scoring changes get a new version file, and all stored runs are rescored from
their raw metrics. Old versions stay in this repo. Provisional reference values
inside a version (currently the GPU reference) may be recalibrated in place;
stored results are rescored from their raw metrics when that happens.
