# hwbench agent

The benchmark program and scoring configuration behind
[hwbench.io](https://hwbench.io). It tests a computer's CPU, GPU, storage and memory,
uploads the results, and opens a page comparing them with other machines that have
the same parts.

```sh
# macOS / Linux
curl -fsSL https://hwbench.io/run.sh | sh
# Windows (PowerShell)
irm https://hwbench.io/run.ps1 | iex
```

These commands run every test. The site's [run page](https://hwbench.io/run) gives a
one-time command for a chosen set of tests instead. Either way, the script downloads
the binary for your platform from this repository's releases, checks its SHA-256
against checksums pinned on the server, runs it once and deletes it. It doesn't need
administrator rights.

## What it measures

| Component | Tests | Time |
|---|---|---|
| CPU | Ten workloads at 1, 4 and all threads: hashing, compression, sorting, matrix math, physics, game AI search, a bytecode interpreter, ray tracing, AES encryption and JSON parsing | ~30 s |
| GPU | Compute (FP32, INT32, memory bandwidth, an image filter) and 3D at 1080p (geometry, fill rate, texturing, a shadowed scene) through wgpu (Metal, DirectX 12 or Vulkan) | ~20 s |
| Storage | Sequential and 4K random reads and writes at queue depths 1 and 32, bypassing the OS cache | ~15 s |
| Memory | Read, write and copy bandwidth, latency, and latency across cache levels | ~6 s |

[docs/METHODOLOGY.md](docs/METHODOLOGY.md) explains how results become scores, and
[`scoring/v2.json`](scoring/v2.json) holds every weight and reference value the site
uses.

## Options

```
hwbench [--session CODE] [--cpu] [--gpu] [--disk] [--ram] [--all]

--session CODE   use a session created on hwbench.io/run
--cpu, --gpu, --disk, --ram   choose tests (default: all)
--disk-path DIR  test the drive holding DIR (default: the temp folder, or /var/tmp
                 when the temp folder is in RAM)
--list-gpus      show detected GPUs and exit
--gpu-index N    benchmark GPU N from --list-gpus (default: a discrete GPU if there is one)
--no-open        don't open the results page
--no-wait        start even if other programs are keeping the computer busy
--dry-run        run locally and submit nothing
--json           with --dry-run, print the payload that would be submitted
--api URL        server to submit to (default https://hwbench.io)
```

If the binary is renamed to `hwbench_<code>` (or `hwbench_<code>.exe`), it reads the
session code from its own file name and keeps its window open when it finishes, so it
can be started by double-clicking. The site doesn't offer such downloads yet.

## Privacy

The agent sends the benchmark results; the model names of the CPU, GPU and test drive;
core counts, memory size, OS and kernel version; on Linux, the CPU clock
speeds seen during the test; the motherboard model and BIOS version (or the computer's
model, for prebuilt PCs and laptops); each memory module's size, type, speed,
manufacturer and part number; CPU cache sizes; the GPU's memory size, driver version,
maximum clocks, PCIe link and power limit; the device names or volume labels,
filesystem types and sizes of mounted drives (a volume label is whatever you named the
drive); and a salted hash of the OS machine id, which lets the server group
repeat runs without learning the id. It doesn't send serial numbers, asset tags,
your username, your hostname or MAC addresses. The server stores only a keyed hash of your IP
address with benchmark runs. The site's [privacy policy](https://hwbench.io/privacy)
covers everything else, including accounts.

## Results are checked

CPU results include proof-of-work digests for a one-time challenge; the server re-runs a
sample of the work with the WebAssembly build of the same code
([`crates/kernels-wasm`](crates/kernels-wasm)) and rejects mismatches. GPU and storage
results are checked against each part's rated specifications and physical limits. The
details are in [docs/METHODOLOGY.md](docs/METHODOLOGY.md#checking-results), and the wire
format in [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Repository layout

| Path | Contents |
|---|---|
| [`crates/agent`](crates/agent) | The `hwbench` binary |
| [`crates/kernels`](crates/kernels) | Deterministic CPU workloads, shared with the server's verifier |
| [`crates/kernels-wasm`](crates/kernels-wasm) | WebAssembly build of the workloads |
| [`scoring/`](scoring) | Scoring configuration |
| [`docs/`](docs) | Methodology and protocol |

## Building

```sh
cargo build --release -p hwbench
cargo test --release --workspace   # includes golden digests that must match on every platform
rustup target add wasm32-unknown-unknown
cargo build -p hwbench-kernels-wasm --target wasm32-unknown-unknown --profile wasm
```

CI runs the tests and a dry run on Linux, macOS and Windows. Pushing a `v*` tag builds
release binaries for Linux, macOS and Windows on x86-64 and ARM64. Linux builds target
glibc 2.28 so the GPU test can load the system's Vulkan driver.

Changing the CPU workloads changes their digests: bump `KERNELS_VERSION` in
`crates/kernels/src/lib.rs` and update the expected digests in the `golden` test at the
bottom of that file. The website verifies CPU results with the WebAssembly from a
release, so it has to pin the new release before agents built from it can submit.

MIT licensed.
