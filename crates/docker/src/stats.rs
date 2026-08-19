use serde::Deserialize;

use crate::client::Docker;
use crate::error::Result;
use crate::request_ext::ReqwestExt;

/// One-shot stats snapshot for a container (`GET /containers/{id}/stats?stream=false&one-shot=true`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContainerStats {
    #[serde(default)]
    pub memory_stats: MemoryStats,
    #[serde(default)]
    pub cpu_stats: CpuStats,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MemoryStats {
    /// Current memory use in bytes. `None` if not available (e.g. cgroup v1
    /// without memory accounting enabled).
    pub usage: Option<u64>,
}

/// Cumulative CPU counters. A percentage needs two samples; `one-shot=true`
/// zeroes `precpu_stats`, so callers diff against a prior snapshot themselves.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CpuStats {
    #[serde(default)]
    pub cpu_usage: CpuUsage,
    /// Host-wide cumulative CPU time, when reported.
    pub system_cpu_usage: Option<u64>,
    /// Online CPU count, when reported.
    pub online_cpus: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CpuUsage {
    /// Cumulative container CPU time (ns).
    #[serde(default)]
    pub total_usage: u64,
}

impl Docker {
    /// `GET /containers/{id}/stats?stream=false&one-shot=true` — a single
    /// snapshot of cgroup-level stats.
    pub async fn stats(&self, id: &str) -> Result<ContainerStats> {
        let mut url = self.url(["containers", id, "stats"])?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("stream", "false");
            pairs.append_pair("one-shot", "true");
        }
        self.http().get(url).try_send().await
    }
}
