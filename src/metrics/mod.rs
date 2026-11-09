//! Metric collection and payload serialisation.
//!
//! CPU usage and network throughput are computed from two samples taken one
//! second apart, so every call to [`sample`] blocks for about one second.

use crate::json::ObjBuilder;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod imp_impl;
#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod imp_impl;

/// Everything the Pulse API accepts, normalised to bytes / seconds / percent.
#[derive(Debug, Default, Clone)]
pub struct Sample {
    pub timezone: Option<String>,
    pub hostname: Option<String>,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub arch: Option<String>,
    pub uptime: Option<u64>,

    pub cpu_model: Option<String>,
    pub cpu_cores: Option<u64>,
    pub cpu_usage: Option<f64>,

    pub mem_total: Option<u64>,
    pub mem_used: Option<u64>,
    pub swap_total: Option<u64>,
    pub swap_used: Option<u64>,

    pub disk_total: Option<u64>,
    pub disk_used: Option<u64>,

    pub net_up: Option<u64>,
    pub net_down: Option<u64>,
}

impl Sample {
    /// Serialise to the nested JSON shape documented in `PULSE-API.md`.
    ///
    /// `server_time` is emitted as a Unix timestamp (seconds); the API accepts
    /// that form directly and it sidesteps all timezone arithmetic.
    pub fn to_json(&self, server_time: i64) -> String {
        let mut o = ObjBuilder::new();
        o.int("server_time", server_time);
        o.opt_string("timezone", &self.timezone);
        o.opt_string("hostname", &self.hostname);
        o.opt_string("os", &self.os);
        o.opt_string("kernel", &self.kernel);
        o.opt_string("arch", &self.arch);
        o.opt_int("uptime", self.uptime.map(|v| v as i64));

        let mut cpu = ObjBuilder::new();
        cpu.opt_string("model", &self.cpu_model);
        cpu.opt_int("cores", self.cpu_cores.map(|v| v as i64));
        cpu.opt_float(
            "usage",
            self.cpu_usage.map(|v| (v * 100.0).round() / 100.0),
        );
        nest(&mut o, "cpu", cpu);

        let mut mem = ObjBuilder::new();
        mem.opt_int("total", self.mem_total.map(|v| v as i64));
        mem.opt_int("used", self.mem_used.map(|v| v as i64));
        nest(&mut o, "memory", mem);

        let mut swap = ObjBuilder::new();
        swap.opt_int("total", self.swap_total.map(|v| v as i64));
        swap.opt_int("used", self.swap_used.map(|v| v as i64));
        nest(&mut o, "swap", swap);

        let mut disk = ObjBuilder::new();
        disk.opt_int("total", self.disk_total.map(|v| v as i64));
        disk.opt_int("used", self.disk_used.map(|v| v as i64));
        nest(&mut o, "disk", disk);

        let mut net = ObjBuilder::new();
        net.opt_int("up", self.net_up.map(|v| v as i64));
        net.opt_int("down", self.net_down.map(|v| v as i64));
        nest(&mut o, "network", net);

        o.into_object()
    }
}

fn nest(parent: &mut ObjBuilder, key: &str, child: ObjBuilder) {
    if !child.is_empty() {
        let body = child.into_body();
        parent.raw(key, &format!("{{{}}}", body));
    }
}

/// Collect a sample. Blocks for roughly one second to measure rates.
pub fn sample() -> Sample {
    imp_impl::sample()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_expected_shape() {
        let sample = Sample {
            hostname: Some("web-01".into()),
            cpu_usage: Some(12.3456),
            mem_total: Some(100),
            mem_used: Some(40),
            ..Default::default()
        };
        let json = sample.to_json(1_700_000_000);
        assert!(json.contains("\"server_time\":1700000000"));
        assert!(json.contains("\"hostname\":\"web-01\""));
        assert!(json.contains("\"usage\":12.35"));
        assert!(json.contains("\"memory\":{\"total\":100,\"used\":40}"));
    }

    #[test]
    fn omits_empty_sections() {
        assert_eq!(Sample::default().to_json(0), "{\"server_time\":0}");
    }
}
