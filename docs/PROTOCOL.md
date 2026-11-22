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
{ "protocol": 1, "agentVersion": "0.1.0", "kernelsVersion": 1, "os": "linux", "arch": "x86_64" }
→ 200 {
  "seed": "<64 hex chars>",
  "selection": ["cpu", "ram"],
  "cpu": { "kernels": ["hash","compress","sort","float","physics"], "threads": [1, 4, 0], "windowMs": 1500 },
  "submitBy": "…"
}
```

A session can be claimed once. `threads: 0` means "all logical processors".
The seed only exists from this point on, so proofs cannot be precomputed.

## 3. Submit

```
POST /api/v1/sessions/{code}/submit
{
  "protocol": 1,
  "agent": { "version": "0.1.0", "kernelsVersion": 1, "target": "x86_64-unknown-linux-gnu" },
  "deviceId": "<hex>",          // salted hash of a stable machine id; never the raw id
  "system": { … see agent/src/system.rs … },
  "cpu": {
    "tests": [
      { "kernel": "hash", "threads": 1, "chunks": 291, "elapsedNs": 1503000000,
        "digests": "<chunks × 16 hex chars>" }
    ]
  },
  "ram": { "readGBs": 61.2, "writeGBs": 40.1, "copyGBs": 35.0, "latencyNs": 82.3 }
}
→ 201 { "publicId": "r_8Gm2…", "url": "https://hwbench.io/run/r_8Gm2…" }
```

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
