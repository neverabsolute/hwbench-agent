# hwbench agent

The open-source benchmark agent and scoring methodology behind
[hwbench.io](https://hwbench.io).

```sh
# macOS / Linux
curl -fsSL https://hwbench.io/run.sh | sh
# Windows (PowerShell)
irm https://hwbench.io/run.ps1 | iex
```

* [`crates/agent`](crates/agent): the `hwbench` binary
* [`crates/kernels`](crates/kernels): deterministic CPU workloads, shared with the server verifier
* [`crates/kernels-wasm`](crates/kernels-wasm): WebAssembly build of the kernels for verification
* [`docs/PROTOCOL.md`](docs/PROTOCOL.md): agent ↔ server protocol
* [`docs/METHODOLOGY.md`](docs/METHODOLOGY.md) and [`scoring/`](scoring): how scores are computed

## Privacy

The agent sends hardware models, OS version, benchmark results and a salted
hash of the OS machine id. It never sends serial numbers, usernames, hostnames,
MAC or IP addresses (the server sees your IP and stores only a keyed hash of it).

## Building

```sh
cargo build --release -p hwbench
cargo build -p hwbench-kernels-wasm --target wasm32-unknown-unknown --profile wasm
```

MIT licensed.
