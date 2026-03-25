use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::Result;
use crate::output::formatter::OutputFormatter;
use crate::watch::monitor::{CheckResult, CheckStatus, MonitorCheck};

#[derive(Debug, Clone, Deserialize)]
pub struct WatchConfig {
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    #[serde(default)]
    pub auto_fix: bool,
    #[serde(default = "default_max_rounds")]
    pub max_auto_rounds: u8,
    #[serde(default)]
    pub checks: Vec<CheckConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum CheckConfig {
    DiskUsage { path: String, threshold_pct: u8 },
    MemoryUsage { threshold_pct: u8 },
    ServiceDown { service_name: String, check_cmd: String },
    LogErrorRate { log_path: String, pattern: String, max_per_minute: u32 },
    PortUnreachable { host: String, port: u16 },
    CustomCommand { cmd: String, expected_exit: i32 },
}

impl CheckConfig {
    pub fn to_monitor_check(&self) -> MonitorCheck {
        match self {
            Self::DiskUsage { path, threshold_pct } => MonitorCheck::DiskUsage {
                path: path.clone(), threshold_pct: *threshold_pct,
            },
            Self::MemoryUsage { threshold_pct } => MonitorCheck::MemoryUsage {
                threshold_pct: *threshold_pct,
            },
            Self::ServiceDown { service_name, check_cmd } => MonitorCheck::ServiceDown {
                service_name: service_name.clone(), check_cmd: check_cmd.clone(),
            },
            Self::LogErrorRate { log_path, pattern, max_per_minute } => MonitorCheck::LogErrorRate {
                log_path: log_path.clone(), pattern: pattern.clone(), max_per_minute: *max_per_minute,
            },
            Self::PortUnreachable { host, port } => MonitorCheck::PortUnreachable {
                host: host.clone(), port: *port,
            },
            Self::CustomCommand { cmd, expected_exit } => MonitorCheck::CustomCommand {
                cmd: cmd.clone(), expected_exit: *expected_exit,
            },
        }
    }
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self { interval_secs: 60, auto_fix: false, max_auto_rounds: 2, checks: Vec::new() }
    }
}

impl WatchConfig {
    pub fn from_toml(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| crate::error::AgentError::WatchError(format!("Read config: {e}")))?;
        toml::from_str(&content)
            .map_err(|e| crate::error::AgentError::WatchError(format!("Parse config: {e}")))
    }
}

fn default_interval() -> u64 { 60 }
fn default_max_rounds() -> u8 { 2 }

/// Channel for watch mode to send detected problems to the main pipeline.
pub type ProblemSender = mpsc::Sender<String>;
pub type ProblemReceiver = mpsc::Receiver<String>;

pub struct WatchLoop {
    checks: Vec<MonitorCheck>,
    interval: Duration,
    auto_fix: bool,
    formatter: OutputFormatter,
    cancellation: CancellationToken,
    problem_tx: Option<ProblemSender>,
}

impl WatchLoop {
    pub fn new(
        config: WatchConfig,
        cancellation: CancellationToken,
        json_mode: bool,
    ) -> Self {
        let checks: Vec<MonitorCheck> = config.checks.iter().map(|c| c.to_monitor_check()).collect();
        Self {
            checks,
            interval: Duration::from_secs(config.interval_secs),
            auto_fix: config.auto_fix,
            formatter: OutputFormatter::new(json_mode),
            cancellation,
            problem_tx: None,
        }
    }

    /// Set the channel for sending detected problems to the pipeline.
    pub fn with_problem_sender(mut self, tx: ProblemSender) -> Self {
        self.problem_tx = Some(tx);
        self
    }

    pub fn add_check(&mut self, check: MonitorCheck) {
        self.checks.push(check);
    }

    pub async fn run(&self) -> Result<()> {
        tracing::info!(
            checks = self.checks.len(),
            interval_secs = self.interval.as_secs(),
            auto_fix = self.auto_fix,
            "Watch mode started"
        );

        if self.checks.is_empty() {
            eprintln!("⚠ No checks configured. Use --watch-config or run `opcrew infra discover` first.");
            return Ok(());
        }

        loop {
            let results = self.run_checks().await;
            let healthy = results.iter().filter(|r| r.status == CheckStatus::Healthy).count();
            let total = results.len();

            if healthy == total {
                println!("{}", self.formatter.format_watch_status(healthy, total));
            } else {
                println!("{}", self.formatter.format_watch_status(healthy, total));

                let mut critical_problems = Vec::new();
                for result in &results {
                    if result.status != CheckStatus::Healthy {
                        let severity = match result.status {
                            CheckStatus::Critical => "critical",
                            CheckStatus::Warning => "warning",
                            _ => "info",
                        };
                        println!("{}", self.formatter.format_alert(&result.check_name, &result.message, severity));

                        if result.status == CheckStatus::Critical {
                            critical_problems.push(result.message.clone());
                        }
                    }
                }

                // Auto-fix: send problems to pipeline
                if self.auto_fix && !critical_problems.is_empty() {
                    let problem = format!(
                        "Watch mode detected {} critical issues:\n{}",
                        critical_problems.len(),
                        critical_problems.iter().enumerate()
                            .map(|(i, p)| format!("{}. {p}", i + 1))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );

                    if let Some(tx) = &self.problem_tx {
                        tracing::info!(issues = critical_problems.len(), "Triggering auto-fix pipeline");
                        println!("  >>> Triggering auto-fix for {} critical issues...", critical_problems.len());
                        let _ = tx.send(problem).await;
                    } else {
                        tracing::info!(issues = critical_problems.len(), "Auto-fix: no pipeline connected (dry-run or missing config)");
                        for p in &critical_problems {
                            println!("  [auto-fix] Would fix: {p}");
                        }
                    }
                }
            }

            tokio::select! {
                _ = tokio::time::sleep(self.interval) => continue,
                _ = self.cancellation.cancelled() => {
                    tracing::info!("Watch mode stopped");
                    return Ok(());
                }
            }
        }
    }

    async fn run_checks(&self) -> Vec<CheckResult> {
        let mut results = Vec::new();
        for check in &self.checks {
            let result = check.run().await;
            results.push(result);
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_config_from_toml() {
        let toml_str = r#"
interval_secs = 30
auto_fix = true

[[checks]]
type = "DiskUsage"
path = "/var"
threshold_pct = 85

[[checks]]
type = "ServiceDown"
service_name = "nginx"
check_cmd = "systemctl is-active nginx"

[[checks]]
type = "MemoryUsage"
threshold_pct = 90
"#;
        let config: WatchConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.interval_secs, 30);
        assert!(config.auto_fix);
        assert_eq!(config.checks.len(), 3);
    }

    #[test]
    fn check_config_to_monitor_check() {
        let config = CheckConfig::DiskUsage { path: "/var".into(), threshold_pct: 85 };
        let check = config.to_monitor_check();
        assert_eq!(check.name(), "disk:/var");
    }

    #[test]
    fn default_config() {
        let config = WatchConfig::default();
        assert_eq!(config.interval_secs, 60);
        assert!(!config.auto_fix);
    }
}
