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
{ "protocol": 1, "agentVersion": "0.6.0", "kernelsVersion": 2, "os": "linux", "arch": "x86_64" }
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
The server answers `426` to agents older than 0.6.0 or with another `kernelsVersion`.
The seed only exists from this point on, so proofs cannot be precomputed.

## 3. Submit

```
POST /api/v1/sessions/{code}/submit
{
  "protocol": 1,
  "agent": { "version": "0.6.0", "kernelsVersion": 2, "target": "x86_64-unknown-linux-gnu" },
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
      { "kernel": "hash", "threads": 1, "chunks": 291, "elapsedNs": 1503000000,
        "digests": "<chunks × 16 hex chars>" }
    ],
    "clocks": { "singleMaxMhz": 5602, "allCoreMhz": 5150 }   // optional, Linux only (Windows 0.5.0 sent unreliable readings; servers ignore them)
  },
  "ram": { "readGBs": 61.2, "writeGBs": 40.1, "copyGBs": 35.0, "latencyNs": 82.3, "bufferBytes": 536870912, "threads": 16,
           "ladder": [ { "bytes": 16384, "ns": 1.2 }, …, { "bytes": 268435456, "ns": 78.4 } ] },
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
    }
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

For each test, chunk indices `0..chunks` are handed out to worker threads in
order. Each chunk `i` produces `hwbench_kernels::run_chunk(seed, kernel, i)`,
and the first 8 bytes of every digest are concatenated in index order.

The server re-runs a random sample of chunks with the same kernel code compiled
to WebAssembly. If any digest differs, the run is rejected. This proves the
work was done for this session's seed. It cannot prove how fast it was done,
so timing goes through the statistical checks described in
[METHODOLOGY.md](METHODOLOGY.md).

## Versioning

* `protocol`: wire format. The server rejects versions it doesn't support.
* `kernelsVersion`: kernel outputs. The server needs the matching verifier.
* `scoringVersion` (server side): how metrics turn into scores. Old runs are
  rescored from their stored raw metrics when scoring changes.
