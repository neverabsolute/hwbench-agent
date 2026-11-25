//! Re-runs CPU proof chunks for the hwbench server. One JSON request on stdin, one JSON answer
//! on stdout (see `Request` and `Answer`); `--version` prints the kernels version it checks.
//!
//! Spot checks re-run single chunks from the predecessor the agent reported. Lane checks re-run
//! a whole lane from its start, which is the only way to tell a real chain from one stitched
//! together out of pieces computed in parallel.

use hwbench_kernels::{KERNELS_VERSION, Kernel, ShortDigest, run_chained, test_seed, verify_lane};
use serde::{Deserialize, Serialize};
use std::io::Read;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    kernels_version: u32,
    seed: String,
    #[serde(default)]
    spot: Vec<Spot>,
    #[serde(default)]
    lanes: Vec<Lane>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Spot {
    kernel: String,
    threads: u32,
    lane: u32,
    index: u32,
    /// Short digest of the chunk before `index` in this lane (ignored for index 0).
    prev: String,
    expect: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Lane {
    kernel: String,
    threads: u32,
    lane: u32,
    digests: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Answer {
    /// Per spot check: whether the chunk matched.
    spot: Vec<bool>,
    /// Per lane: the first chunk that differs, or null when the whole lane matched.
    lanes: Vec<Option<usize>>,
}

fn short(hex: &str) -> Result<ShortDigest, String> {
    if hex.len() != 16 {
        return Err(format!("bad digest length {}", hex.len()));
    }
    let mut out = [0u8; 8];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

fn seed(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 {
        return Err("seed must be 64 hex characters".into());
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

fn kernel(name: &str) -> Result<Kernel, String> {
    Kernel::from_name(name).ok_or_else(|| format!("unknown kernel {name}"))
}

fn check(req: Request) -> Result<Answer, String> {
    if req.kernels_version != KERNELS_VERSION {
        return Err(format!("this checker is for kernels v{KERNELS_VERSION}, not v{}", req.kernels_version));
    }
    let seed = seed(&req.seed)?;
    let spot = req.spot.iter().map(|s| {
        let prev = if s.index == 0 { [0u8; 8] } else { short(&s.prev)? };
        let got = run_chained(&test_seed(&seed, s.threads), kernel(&s.kernel)?, s.lane, s.index, &prev);
        Ok(got == short(&s.expect)?)
    }).collect::<Result<_, String>>()?;
    let lanes = req.lanes.iter().map(|l| {
        if l.digests.len() % 16 != 0 {
            return Err("lane digests must be 16 hex characters each".into());
        }
        let digests = (0..l.digests.len() / 16).map(|i| short(&l.digests[i * 16..i * 16 + 16])).collect::<Result<Vec<_>, _>>()?;
        Ok(verify_lane(&test_seed(&seed, l.threads), kernel(&l.kernel)?, l.lane, &digests))
    }).collect::<Result<_, String>>()?;
    Ok(Answer { spot, lanes })
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("{KERNELS_VERSION}");
        return;
    }
    let mut input = String::new();
    let result = std::io::stdin().read_to_string(&mut input).map_err(|e| e.to_string())
        .and_then(|_| serde_json::from_str::<Request>(&input).map_err(|e| e.to_string()))
        .and_then(check);
    match result {
        Ok(answer) => println!("{}", serde_json::to_string(&answer).unwrap()),
        Err(e) => {
            eprintln!("hwbench-verify: {e}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn checks_spots_and_lanes() {
        let s = [1u8; 32];
        let ts = test_seed(&s, 1);
        let d0 = run_chained(&ts, Kernel::Parse, 0, 0, &[0; 8]);
        let d1 = run_chained(&ts, Kernel::Parse, 0, 1, &d0);
        let req = |spot: Vec<Spot>, lanes: Vec<Lane>| Request { kernels_version: KERNELS_VERSION, seed: hex(&s), spot, lanes };
        let a = check(req(
            vec![
                Spot { kernel: "parse".into(), threads: 1, lane: 0, index: 1, prev: hex(&d0), expect: hex(&d1) },
                Spot { kernel: "parse".into(), threads: 1, lane: 0, index: 1, prev: hex(&[7; 8]), expect: hex(&d1) },
            ],
            vec![
                Lane { kernel: "parse".into(), threads: 1, lane: 0, digests: format!("{}{}", hex(&d0), hex(&d1)) },
                Lane { kernel: "parse".into(), threads: 1, lane: 0, digests: format!("{}{}", hex(&d0), hex(&d0)) },
            ],
        )).unwrap();
        assert_eq!(a.spot, vec![true, false]);
        assert_eq!(a.lanes, vec![None, Some(1)]);
        assert!(check(Request { kernels_version: KERNELS_VERSION - 1, seed: hex(&s), spot: vec![], lanes: vec![] }).is_err());
    }
}
