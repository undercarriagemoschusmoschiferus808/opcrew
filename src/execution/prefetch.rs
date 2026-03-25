use std::time::{Duration, Instant};

use crate::infra::graph::InfraGraph;
use crate::tools::shell::ShellTool;
use crate::tools::target::TargetHost;
use crate::tools::traits::{Tool, ToolParams};

/// Pre-fetched system context — collected in parallel before any LLM call.
#[derive(Debug, Clone)]
pub struct SystemContext {
    pub data: Vec<(String, String)>,
    pub fetch_duration_ms: u64,
}

impl SystemContext {
    pub fn to_prompt_context(&self) -> String {
        if self.data.is_empty() {
            return String::new();
        }
        let mut out = format!("SYSTEM CONTEXT (pre-fetched in {}ms):\n\n", self.fetch_duration_ms);
        for (label, output) in &self.data {
            let truncated = &output[..output.len().min(1500)];
            out.push_str(&format!("### {label}\n```\n{truncated}\n```\n\n"));
        }
        out
    }
}

/// Compute which commands to pre-fetch based on the problem text + infra graph.
fn compute_prefetch_commands(problem: &str, infra: Option<&InfraGraph>) -> Vec<(&'static str, String)> {
    let lower = problem.to_lowercase();
    let mut commands: Vec<(&str, String)> = vec![
        ("memory", "free -m".into()),
        ("disk", "df -h".into()),
        ("uptime", "uptime".into()),
        ("failed_services", "systemctl list-units --failed --no-pager --no-legend".into()),
    ];

    // ─── Docker context ───
    if lower.contains("container") || lower.contains("docker") || lower.contains("restart") {
        commands.push(("docker_ps", "docker ps -a".into()));

        // Extract container name from problem
        if let Some(name) = extract_name_from_problem(&lower, &["container", "docker"]) {
            commands.push(("container_logs", format!("docker logs {name} --tail 50")));
            commands.push(("container_inspect", format!("docker inspect {name}")));
            commands.push(("container_events", format!("docker events --since 5m --until 0s --filter container={name} --format json")));
        }
    }

    // ─── Kubernetes context ───
    if lower.contains("pod") || lower.contains("k8s") || lower.contains("kube")
        || lower.contains("deployment") || lower.contains("namespace")
    {
        commands.push(("k8s_pods", "kubectl get pods -A --no-headers".into()));
        commands.push(("k8s_events", "kubectl get events -A --sort-by=.lastTimestamp --no-headers".into()));
        commands.push(("k8s_failed", "kubectl get pods -A --no-headers --field-selector=status.phase!=Running,status.phase!=Succeeded".into()));

        if let Some(ns) = extract_name_from_problem(&lower, &["namespace"]) {
            commands.push(("k8s_ns_pods", format!("kubectl get pods -n {ns} -o wide")));
            commands.push(("k8s_ns_events", format!("kubectl get events -n {ns} --sort-by=.lastTimestamp")));
        }
    }

    // ─── Disk context ───
    if lower.contains("disk") || lower.contains("full") || lower.contains("space") || lower.contains("storage") {
        commands.push(("disk_usage_root", "du -sh /* 2>/dev/null".into()));
    }

    // ─── Network/service context ───
    if lower.contains("502") || lower.contains("503") || lower.contains("timeout")
        || lower.contains("connection") || lower.contains("port") || lower.contains("unreachable")
    {
        commands.push(("listening_ports", "ss -tlnp".into()));
    }

    // ─── Specific service context ───
    if lower.contains("nginx") {
        commands.push(("nginx_status", "systemctl status nginx".into()));
        commands.push(("nginx_error_log", "tail -30 /var/log/nginx/error.log".into()));
    }
    if lower.contains("postgres") || lower.contains("pg") || lower.contains("database") {
        commands.push(("pg_status", "systemctl status postgresql".into()));
    }
    if lower.contains("redis") {
        commands.push(("redis_status", "systemctl status redis".into()));
    }

    // ─── CPU/memory context ───
    if lower.contains("cpu") || lower.contains("slow") || lower.contains("high load") || lower.contains("performance") {
        commands.push(("top_cpu", "ps aux --sort=-pcpu --no-headers".into()));
    }
    if lower.contains("oom") || lower.contains("memory") || lower.contains("killed") {
        commands.push(("dmesg_oom", "dmesg -T --since '30 min ago'".into()));
    }

    // ─── Infra graph dependencies ───
    if let Some(graph) = infra
        && let Some(service_name) = extract_any_service_name(&lower, graph) {
            for dep in graph.dependencies_of(&service_name) {
                if let Some(port) = dep.port {
                    commands.push(("dep_port_check", format!("ss -tlnp sport = :{port}")));
                }
            }
        }

    // ─── Kernel messages (always useful for crashes) ───
    if lower.contains("crash") || lower.contains("restart") || lower.contains("killed") || lower.contains("oom") {
        commands.push(("dmesg_recent", "dmesg -T --since '10 min ago'".into()));
    }

    commands
}

/// Execute all pre-fetch commands in parallel and return results.
pub async fn prefetch_system_context(
    problem: &str,
    target: &TargetHost,
    infra: Option<&InfraGraph>,
) -> SystemContext {
    let start = Instant::now();
    let commands = compute_prefetch_commands(problem, infra);
    let _shell = ShellTool::new(target.clone());

    // Launch all commands concurrently
    let mut handles = Vec::new();
    for (label, cmd) in commands {
        let shell_clone = ShellTool::new(target.clone());
        let label = label.to_string();
        let cmd = cmd.clone();

        handles.push(tokio::spawn(async move {
            // Skip commands with shell composition (pipes etc.)
            if ShellTool::has_composition(&cmd) {
                return (label, None);
            }
            let params = ToolParams {
                tool_name: "shell".into(),
                action: "run".into(),
                args: [("command".into(), cmd)].into(),
            };
            match shell_clone.execute(&params, Duration::from_secs(10)).await {
                Ok(result) if result.success && !result.output.is_empty() => {
                    (label, Some(result.output))
                }
                _ => (label, None),
            }
        }));
    }

    // Collect results
    let mut data = Vec::new();
    for handle in handles {
        if let Ok((label, Some(output))) = handle.await {
            data.push((label, output));
        }
    }

    let duration = start.elapsed().as_millis() as u64;
    tracing::info!(commands = data.len(), duration_ms = duration, "Pre-fetch complete");

    SystemContext {
        data,
        fetch_duration_ms: duration,
    }
}

/// Try to extract a service/container name from the problem text.
fn extract_name_from_problem(lower: &str, keywords: &[&str]) -> Option<String> {
    for kw in keywords {
        // Pattern: "container X" or "container named X"
        if let Some(pos) = lower.find(kw) {
            let after = &lower[pos + kw.len()..];
            let after = after.trim_start();
            let after = after.strip_prefix("named ").unwrap_or(after);
            let name: String = after.chars()
                .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
                .collect();
            if name.len() >= 2 {
                return Some(name);
            }
        }
    }
    None
}

/// Try to find a service name in the problem that matches the infra graph.
fn extract_any_service_name(lower: &str, graph: &InfraGraph) -> Option<String> {
    for name in graph.services.keys() {
        if lower.contains(&name.to_lowercase()) {
            return Some(name.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_container_name() {
        assert_eq!(
            extract_name_from_problem("container song-stream-server is restarting", &["container"]),
            Some("song-stream-server".into())
        );
        assert_eq!(
            extract_name_from_problem("container named my-app crashes", &["container"]),
            Some("my-app".into())
        );
    }

    #[test]
    fn extract_namespace() {
        assert_eq!(
            extract_name_from_problem("namespace test-opcrew has failing pods", &["namespace"]),
            Some("test-opcrew".into())
        );
    }

    #[test]
    fn commands_for_docker_problem() {
        let cmds = compute_prefetch_commands("container my-app is in restart loop", None);
        let labels: Vec<&str> = cmds.iter().map(|(l, _)| *l).collect();
        assert!(labels.contains(&"memory"));
        assert!(labels.contains(&"docker_ps"));
        assert!(labels.contains(&"container_logs"));
        assert!(labels.contains(&"dmesg_recent"));
    }

    #[test]
    fn commands_for_k8s_problem() {
        let cmds = compute_prefetch_commands("pod payment-service in namespace test is failing", None);
        let labels: Vec<&str> = cmds.iter().map(|(l, _)| *l).collect();
        assert!(labels.contains(&"k8s_pods"));
        assert!(labels.contains(&"k8s_ns_pods"));
    }

    #[test]
    fn commands_for_disk_problem() {
        let cmds = compute_prefetch_commands("disk full on /var", None);
        let labels: Vec<&str> = cmds.iter().map(|(l, _)| *l).collect();
        assert!(labels.contains(&"disk"));
        assert!(labels.contains(&"disk_usage_root"));
    }

    #[test]
    fn baseline_always_present() {
        let cmds = compute_prefetch_commands("something random", None);
        let labels: Vec<&str> = cmds.iter().map(|(l, _)| *l).collect();
        assert!(labels.contains(&"memory"));
        assert!(labels.contains(&"disk"));
        assert!(labels.contains(&"uptime"));
        assert!(labels.contains(&"failed_services"));
    }
}
