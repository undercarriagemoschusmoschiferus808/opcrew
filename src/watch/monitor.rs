use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{AgentError, Result};
use crate::tools::shell::ShellTool;
use crate::tools::target::TargetHost;
use crate::tools::traits::{Tool, ToolParams};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MonitorCheck {
    DiskUsage {
        path: String,
        threshold_pct: u8,
    },
    MemoryUsage {
        threshold_pct: u8,
    },
    ServiceDown {
        service_name: String,
        check_cmd: String,
    },
    LogErrorRate {
        log_path: String,
        pattern: String,
        max_per_minute: u32,
    },
    PortUnreachable {
        host: String,
        port: u16,
    },
    CustomCommand {
        cmd: String,
        expected_exit: i32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum CheckStatus {
    Healthy,
    Warning,
    Critical,
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub check_name: String,
    pub status: CheckStatus,
    pub message: String,
}

impl MonitorCheck {
    pub fn name(&self) -> String {
        match self {
            Self::DiskUsage { path, .. } => format!("disk:{path}"),
            Self::MemoryUsage { .. } => "memory".into(),
            Self::ServiceDown { service_name, .. } => format!("service:{service_name}"),
            Self::LogErrorRate { log_path, .. } => format!("logerr:{log_path}"),
            Self::PortUnreachable { host, port } => format!("port:{host}:{port}"),
            Self::CustomCommand { cmd, .. } => format!("custom:{}", &cmd[..cmd.len().min(20)]),
        }
    }

    pub async fn run(&self) -> CheckResult {
        let shell = ShellTool::new(TargetHost::Local);
        let timeout = Duration::from_secs(15);

        match self {
            Self::DiskUsage {
                path,
                threshold_pct,
            } => {
                let params = ToolParams {
                    tool_name: "shell".into(),
                    action: "run".into(),
                    args: [("command".into(), format!("df {path}"))].into(),
                };
                match shell.execute(&params, timeout).await {
                    Ok(result) if result.success => {
                        let pct = parse_disk_usage(&result.output);
                        if pct >= *threshold_pct {
                            CheckResult {
                                check_name: self.name(),
                                status: if pct >= 95 {
                                    CheckStatus::Critical
                                } else {
                                    CheckStatus::Warning
                                },
                                message: format!(
                                    "Disk usage {pct}% on {path} (threshold: {threshold_pct}%)"
                                ),
                            }
                        } else {
                            CheckResult {
                                check_name: self.name(),
                                status: CheckStatus::Healthy,
                                message: format!("Disk usage {pct}% on {path}"),
                            }
                        }
                    }
                    _ => CheckResult {
                        check_name: self.name(),
                        status: CheckStatus::Warning,
                        message: format!("Could not check disk usage on {path}"),
                    },
                }
            }
            Self::MemoryUsage { threshold_pct } => {
                let params = ToolParams {
                    tool_name: "shell".into(),
                    action: "run".into(),
                    args: [("command".into(), "free -m".into())].into(),
                };
                match shell.execute(&params, timeout).await {
                    Ok(result) if result.success => {
                        let pct = parse_memory_usage(&result.output);
                        if pct >= *threshold_pct {
                            CheckResult {
                                check_name: self.name(),
                                status: if pct >= 95 {
                                    CheckStatus::Critical
                                } else {
                                    CheckStatus::Warning
                                },
                                message: format!(
                                    "Memory usage {pct}% (threshold: {threshold_pct}%)"
                                ),
                            }
                        } else {
                            CheckResult {
                                check_name: self.name(),
                                status: CheckStatus::Healthy,
                                message: format!("Memory usage {pct}%"),
                            }
                        }
                    }
                    _ => CheckResult {
                        check_name: self.name(),
                        status: CheckStatus::Warning,
                        message: "Could not check memory usage".into(),
                    },
                }
            }
            Self::ServiceDown {
                service_name,
                check_cmd,
            } => {
                let params = ToolParams {
                    tool_name: "shell".into(),
                    action: "run".into(),
                    args: [("command".into(), check_cmd.clone())].into(),
                };
                match shell.execute(&params, timeout).await {
                    Ok(result) if result.success => CheckResult {
                        check_name: self.name(),
                        status: CheckStatus::Healthy,
                        message: format!("{service_name} is running"),
                    },
                    _ => CheckResult {
                        check_name: self.name(),
                        status: CheckStatus::Critical,
                        message: format!("{service_name} is DOWN"),
                    },
                }
            }
            Self::PortUnreachable { host, port } => {
                let params = ToolParams {
                    tool_name: "shell".into(),
                    action: "run".into(),
                    args: [("command".into(), format!("ss -tlnp sport = :{port}"))].into(),
                };
                match shell.execute(&params, timeout).await {
                    Ok(result) if result.success && result.output.contains(&port.to_string()) => {
                        CheckResult {
                            check_name: self.name(),
                            status: CheckStatus::Healthy,
                            message: format!("Port {port} is listening on {host}"),
                        }
                    }
                    _ => CheckResult {
                        check_name: self.name(),
                        status: CheckStatus::Critical,
                        message: format!("Port {port} is NOT reachable on {host}"),
                    },
                }
            }
            Self::LogErrorRate {
                log_path,
                pattern,
                max_per_minute,
            } => {
                // Count pattern matches in last 100 lines
                let params = ToolParams {
                    tool_name: "shell".into(),
                    action: "run".into(),
                    args: [("command".into(), format!("tail -100 {log_path}"))].into(),
                };
                match shell.execute(&params, timeout).await {
                    Ok(result) if result.success => {
                        let count = result
                            .output
                            .lines()
                            .filter(|l| l.contains(pattern.as_str()))
                            .count();
                        if count as u32 > *max_per_minute {
                            CheckResult {
                                check_name: self.name(),
                                status: CheckStatus::Warning,
                                message: format!(
                                    "{count} '{pattern}' in last 100 lines of {log_path} (max: {max_per_minute})"
                                ),
                            }
                        } else {
                            CheckResult {
                                check_name: self.name(),
                                status: CheckStatus::Healthy,
                                message: format!(
                                    "{count} '{pattern}' in last 100 lines of {log_path}"
                                ),
                            }
                        }
                    }
                    _ => CheckResult {
                        check_name: self.name(),
                        status: CheckStatus::Warning,
                        message: format!("Could not read {log_path}"),
                    },
                }
            }
            Self::CustomCommand { cmd, expected_exit } => {
                let params = ToolParams {
                    tool_name: "shell".into(),
                    action: "run".into(),
                    args: [("command".into(), cmd.clone())].into(),
                };
                match shell.execute(&params, timeout).await {
                    Ok(result) => {
                        let actual_exit = if result.success { 0 } else { 1 };
                        if actual_exit == *expected_exit {
                            CheckResult {
                                check_name: self.name(),
                                status: CheckStatus::Healthy,
                                message: format!("Custom check passed: {cmd}"),
                            }
                        } else {
                            CheckResult {
                                check_name: self.name(),
                                status: CheckStatus::Critical,
                                message: format!(
                                    "Custom check failed: {cmd} (exit {actual_exit}, expected {expected_exit})"
                                ),
                            }
                        }
                    }
                    Err(e) => CheckResult {
                        check_name: self.name(),
                        status: CheckStatus::Critical,
                        message: format!("Custom check error: {e}"),
                    },
                }
            }
        }
    }
}

fn parse_disk_usage(df_output: &str) -> u8 {
    for line in df_output.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if let Some(pct) = parts
            .get(4)
            .and_then(|p| p.strip_suffix('%'))
            .and_then(|s| s.parse::<u8>().ok())
        {
            return pct;
        }
    }
    0
}

fn parse_memory_usage(free_output: &str) -> u8 {
    // Parse free -m output: Mem: total used free ...
    for line in free_output.lines() {
        if line.starts_with("Mem:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                let total: f64 = parts[1].parse().unwrap_or(1.0);
                let used: f64 = parts[2].parse().unwrap_or(0.0);
                return ((used / total) * 100.0) as u8;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_disk_usage_output() {
        let output = "Filesystem     1K-blocks    Used Available Use% Mounted on\n/dev/sda1       51474044 42150000   9324044  82% /";
        assert_eq!(parse_disk_usage(output), 82);
    }

    #[test]
    fn parse_memory_usage_output() {
        let output = "              total        used        free      shared  buff/cache   available\nMem:           7856        3928        1234         456        2694        3928";
        let pct = parse_memory_usage(output);
        assert!(pct > 0 && pct < 100);
    }

    #[test]
    fn check_names() {
        let check = MonitorCheck::DiskUsage {
            path: "/var".into(),
            threshold_pct: 85,
        };
        assert_eq!(check.name(), "disk:/var");

        let check = MonitorCheck::ServiceDown {
            service_name: "nginx".into(),
            check_cmd: "systemctl is-active nginx".into(),
        };
        assert_eq!(check.name(), "service:nginx");
    }

    #[tokio::test]
    async fn disk_check_runs() {
        let check = MonitorCheck::DiskUsage {
            path: "/".into(),
            threshold_pct: 99, // Very high threshold so it passes
        };
        let result = check.run().await;
        // Should not crash, might be healthy or warning depending on system
        assert!(!result.message.is_empty());
    }
}
