use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::time::Duration;

pub struct Api {
    base: String,
    agent: ureq::Agent,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorInfo,
}
#[derive(Deserialize)]
struct ErrorInfo {
    message: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Created {
    pub code: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Challenge {
    pub seed: String,
    pub selection: Vec<String>,
    pub cpu: Option<CpuPlan>,
    #[serde(default)]
    pub gpu: Option<GpuPlan>,
    #[serde(default)]
    pub disk: Option<DiskPlan>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DiskPlan {
    pub window_ms: u64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GpuPlan {
    pub window_ms: u64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CpuPlan {
    pub kernels: Vec<String>,
    pub threads: Vec<usize>,
    pub window_ms: u64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Submitted {
    pub url: String,
}

impl Api {
    pub fn new(base: &str) -> Api {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .user_agent(concat!("hwbench-agent/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Api { base: base.trim_end_matches('/').to_string(), agent }
    }

    fn post<B: Serialize, R: DeserializeOwned>(&self, path: &str, body: &B) -> Result<R, String> {
        let url = format!("{}{}", self.base, path);
        let mut resp = self.agent.post(&url).send_json(body).map_err(|e| format!("network error: {e}"))?;
        let status = resp.status();
        let text = resp.body_mut().with_config().limit(1 << 20).read_to_string().map_err(|e| e.to_string())?;
        if status.is_success() {
            serde_json::from_str(&text).map_err(|e| format!("bad response from server: {e}"))
        } else if (status == 403 || status == 503) && (text.contains("challenge-platform") || text.contains("Just a moment") || text.contains("cloudflare")) {
            // A firewall challenge page, not an API error: common from cloud servers and VPNs.
            Err(format!(
                "hwbench.io's firewall blocked this request ({status}). This happens most often on cloud servers and VPNs. \
                 Try again later or from another network."
            ))
        } else {
            let msg = serde_json::from_str::<ErrorBody>(&text).map(|e| e.error.message).unwrap_or(text);
            Err(format!("server returned {status}: {msg}"))
        }
    }

    pub fn create_session(&self, selection: &[&str]) -> Result<Created, String> {
        self.post("/api/v1/sessions", &serde_json::json!({ "selection": selection }))
    }

    pub fn claim(&self, code: &str) -> Result<Challenge, String> {
        self.post(
            &format!("/api/v1/sessions/{code}/claim"),
            &serde_json::json!({
                "protocol": 1,
                "agentVersion": env!("CARGO_PKG_VERSION"),
                "kernelsVersion": hwbench_kernels::KERNELS_VERSION,
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
            }),
        )
    }

    /// Commit to the CPU results as soon as the CPU tests finish (see `cpu::commitment`).
    pub fn commit_cpu(&self, code: &str, commitment: &str) -> Result<(), String> {
        self.post::<_, serde_json::Value>(&format!("/api/v1/sessions/{code}/cpu"), &serde_json::json!({ "commitment": commitment }))
            .map(|_| ())
    }

    pub fn submit(&self, code: &str, body: &serde_json::Value) -> Result<Submitted, String> {
        self.post(&format!("/api/v1/sessions/{code}/submit"), body)
    }
}
