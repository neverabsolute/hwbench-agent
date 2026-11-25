# hwbench agent protocol (v1)

All requests are JSON over HTTPS to `https://hwbench.io` (configurable with
`--api`). Responses use `{"error": {"code", "message"}}` on failure.

## 1. Create a run session

Normally the website creates the session when you pick components on `/run`,
and the one-liner already contains its code. The agent creates one itself only
when started without `--session`.

```
POST /api/v1/sessions
{ "selection": ["cpu", "ram"] }
→ 201 { "code": "k3J9xq2a", "expiresAt": "…" }
```

Sessions expire after 30 minutes and are rate limited per IP.

## 2. Claim the challenge

```
POST /api/v1/sessions/{code}/claim
{ "protocol": 1, "agentVersion": "0.8.0", "kernelsVersion": 4, "os": "linux", "arch": "x86_64" }
→ 200 {
  "seed": "<64 hex chars>",
  "selection": ["cpu", "ram"],
  "cpu": { "kernels": ["hash","compress","sort","float","physics","search","script","render","crypto","parse"],
           "threads": [1, 4, 0], "windowMs": 800 },
  "gpu": { "windowMs": 2000 },
  "submitBy": "…"
}
```

A session can be claimed once. `threads: 0` means "all logical processors".
The server answers `426` to agents older than 0.8.0 or with another `kernelsVersion`.
The seed only exists from this point on, so proofs cannot be precomputed.

## 3. Commit to the CPU results (0.7+)

As soon as the CPU tests finish, before anything else runs, the agent sends a
commitment to their results:

```
POST /api/v1/sessions/{code}/cpu
{ "commitment": "<64 hex chars>" }
→ 200 { "ok": true }
```

The commitment is SHA-256 over `"{kernel}:{threads}:{chunks}:{elapsedNs}:{lane0},{lane1},…\n"`
for each test, in the order they are submitted. The server records when it
arrived. The time from the claim to the commitment is the CPU phase on the
server's clock: the tests' `elapsedNs` must fit in it, and it must not be much
longer than they add up to (spare time could be used to do more work than the
run reports). The submission's CPU section must match the commitment.

## 4. Submit

```
POST /api/v1/sessions/{code}/submit
{
  "protocol": 1,
  "agent": { "version": "0.8.0", "kernelsVersion": 4, "target": "x86_64-unknown-linux-gnu" },
  "deviceId": "<hex>",          // salted hash of a stable machine id; never the raw id
  "system": {
    …os, cpu, memory and drives; see crates/agent/src/system.rs…
    "hardware": {                     // optional (0.5+); every field is optional
      "board": { "vendor": "ASRock", "model": "Z690 Extreme", "biosVersion": "7.03", "biosDate": "06/10/2022" },
      "systemModel": "Dell Inc. XPS 15 9520",   // prebuilts and laptops
      "memory": [ { "sizeBytes": 17179869184, "type": "DDR5", "formFactor": "DIMM", "speedMts": 4800,
                    "configuredMts": 6000, "manufacturer": "Corsair", "partNumber": "CMK32GX5M2B6000C36",
                    "locator": "DIMM_A2", "rank": 2 } ],
      "cpuCache": { "l2Kb": 12288, "l3Kb": 25600 }
    }
  },
  "cpu": {
    "tests": [
      { "kernel": "hash", "threads": 4, "chunks": 1164, "elapsedNs": 801000000,
        "lanes": ["<16 hex chars per chunk>", …] }        // one per thread (0.8+)
    ],
    "clocks": { "singleMaxMhz": 5602, "allCoreMhz": 5150 }   // optional, Linux only (Windows 0.5.0 sent unreliable readings; servers ignore them)
  },
  "ram": { "readGBs": 61.2, "writeGBs": 40.1, "copyGBs": 35.0, "latencyNs": 82.3, "bufferBytes": 536870912, "threads": 16,
           "ladder": [ { "bytes": 16384, "ns": 1.2 }, …, { "bytes": 268435456, "ns": 78.4 } ] },
  "load": { "cpuPct": 6.2, "gpuPct": 3, "vramUsedBytes": 1073741824, "vramTotalBytes": 12884901888,   // 0.6.2+; GPU fields optional
            "waitedMs": 1100, "busy": false },   // sampled just before benchmarking, after waiting up to 30 s for a busy system to settle
  "power": { "onBattery": false, "saver": false },   // 0.6.4+; each field omitted when the OS doesn't say
  "gpus": [ { "name": "NVIDIA GeForce RTX 4070", "vendorId": 4318, "deviceId": 10118, "deviceType": "DiscreteGpu", "backend": "Vulkan", … } ],
  "gpu": {
    "adapter": { …one entry of "gpus"… },
    "fp32Gflops": 21400.5, "int32Giops": 11050.2, "bandwidthGBs": 452.7, "blurGpix": 41.3,
    "geometryGtris": 5.1, "fillGpix": 82.4, "textureGtex": 151.0, "sceneFps": 1630.2, "bufferBytes": 536870912,
    "tests": { "fp32": { "opsPerDispatch": …, "bestNs": …, "medianNs": …, "runs": …, "iters": … },
               …"int32", "copy", "blur", "geometry", "fill", "texture", "scene" alike… },
    "details": {                      // optional (0.5+); every field is optional
      "vramBytes": 12884901888, "driverVersion": "566.36", "coreClockMaxMhz": 2100, "memoryClockMaxMhz": 7501,
      "pcieGen": 4, "pcieGenMax": 4, "pcieWidth": 16, "pcieWidthMax": 16, "powerLimitW": 170,
      "cores": 19,                    // Apple GPUs
      "sources": ["nvidia-smi"]       // nvidia-smi, sysfs, registry or system_profiler
    },
    "alternates": [                   // optional (0.6.4+): the same GPU through its other graphics APIs
      { "backend": "Vulkan", "fp32Gflops": 21010.2, …the same eight summary fields… }
    ]                                 // the result above is the faster API; Windows tests DirectX 12 and Vulkan
  }
}
→ 201 { "publicId": "r_8Gm2…", "url": "https://hwbench.io/run/r_8Gm2…" }
```

A GPU that can't be benchmarked is reported as `"gpu": { "error": "…" }`.

For the 3D tests, `opsPerDispatch` is the work in one timed batch: triangles
(`geometry`), blended pixels (`fill`), texture samples (`texture`) or frames
(`scene`); `iters` is the number of frames in the batch. The server recomputes
every rate from `opsPerDispatch` and `bestNs` and rejects summaries that don't match.

`pcieGen` is the highest generation the card and slot support together (from nvidia-smi),
or absent; `pcieGenMax` is the card's own. `pcieWidth` is the current lane count. The
server uses `vramBytes` to tell apart models sold with several memory sizes under one
name, such as the RTX 3060 8 GB and 12 GB.

### CPU proofs

Each test runs under `test_seed = hwbench_kernels::test_seed(seed, threads)`
(kernels v3+), so the chunks computed for one test can't be reported as another
test's.

From kernels v4 (agent 0.8), each worker thread runs a chain of chunks, its
*lane*. Chunk `i` of lane `l` is
`hwbench_kernels::run_chained(test_seed, kernel, l, i, prev)`, where `prev` is
the first 8 bytes of chunk `i - 1`'s digest in the same lane (zeros for the
first). `lanes[l]` holds those 8-byte digests in order, as hex. Because every
chunk needs the one before it, a lane runs at the speed of one core: a test
can't have used more cores than its `threads`.

The server checks proofs after the CPU commitment, so the agent can't know in
advance which chunks it will pick:

* Spot checks: a random sample of chunks in every test, each re-run from the
  predecessor the agent reported.
* Full lanes: whole lanes re-run from the start, for two randomly chosen
  single-thread kernels and one random lane of one four-thread kernel. A lane
  stitched together from pieces computed in parallel has a break where the
  pieces join, and only re-running the whole lane finds it.

Checks run natively with `hwbench-verify` (`crates/verifier`), released with
the agent with build provenance. If any chunk differs, the run is rejected.
This proves the work was done, for this seed, by no more threads than the run
reports. It can't prove which CPU did it, so the model a run reports goes
through the statistical checks in [METHODOLOGY.md](METHODOLOGY.md).

## Versioning

* `protocol`: wire format. The server rejects versions it doesn't support.
* `kernelsVersion`: kernel outputs. The server needs the matching verifier.
* `scoringVersion` (server side): how metrics turn into scores. Old runs are
  rescored from their stored raw metrics when scoring changes.
