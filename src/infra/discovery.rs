use std::sync::Arc;
use std::time::Duration;

use std::collections::HashMap;

use crate::api::provider::LlmProvider;
use crate::api::types::{ChatMessage, MessageRole};
use crate::error::{AgentError, Result};
use crate::infra::graph::{
    Dependency, DependencyType, DiscoveryMethod, InfraGraph, Service, ServiceType,
};
use crate::tools::shell::ShellTool;
use crate::tools::target::TargetHost;
use crate::tools::traits::{Tool, ToolParams};

/// Discovery commands — all read-only, all allowlisted.
const DISCOVERY_COMMANDS: &[(&str, &str)] = &[
    ("systemd_services", "systemctl list-units --type=service --state=running --no-pager --plain"),
    ("listening_ports", "ss -tlnp"),
    ("processes", "ps aux --no-headers"),
    ("config_files", "find /etc -name '*.conf' -maxdepth 3 2>/dev/null"),
    ("docker_containers", "docker ps --format json 2>/dev/null"),
    ("hosts_file", "cat /etc/hosts"),
    ("env_hints", "printenv"),
];

pub struct DiscoveryAgent {
    client: Arc<dyn LlmProvider>,
}

impl DiscoveryAgent {
    pub fn new(client: Arc<dyn LlmProvider>) -> Self {
        Self { client }
    }

    /// Phase 1: Run read-only shell commands to collect raw system data.
    pub async fn collect_raw_data(&self, target: &TargetHost) -> Result<Vec<(String, String)>> {
        let shell = ShellTool::new(target.clone());
        let mut results = Vec::new();

        for (name, cmd) in DISCOVERY_COMMANDS {
            let params = ToolParams {
                tool_name: "shell".into(),
                action: "run".into(),
                args: [("command".into(), cmd.to_string())].into(),
            };

            match shell.execute(&params, Duration::from_secs(30)).await {
                Ok(result) if result.success => {
                    results.push((name.to_string(), result.output));
                }
                Ok(_result) => {
                    tracing::debug!(command = name, "Discovery command returned error (skipping)");
                }
                Err(e) => {
                    tracing::debug!(command = name, error = %e, "Discovery command failed (skipping)");
                }
            }
        }

        Ok(results)
    }

    /// Phase 2: Send raw data to Claude to extract structured graph.
    pub async fn extract_graph(&self, raw_data: &[(String, String)]) -> Result<InfraGraph> {
        let mut prompt = String::from("Extract the infrastructure graph from this raw system output.\n\n");
        for (name, output) in raw_data {
            prompt.push_str(&format!("### {name}\n```\n{}\n```\n\n", &output[..output.len().min(5000)]));
        }
        prompt.push_str(
            "Return ONLY valid JSON with this structure:\n\
{\n  \"services\": [{\"name\": \"svc\", \"host\": \"localhost\", \"port\": 80, \
\"service_type\": \"Web\", \"discovered_via\": \"Systemd\", \
\"log_paths\": [], \"config_paths\": []}],\n  \
\"dependencies\": [{\"from\": \"a\", \"to\": \"b\", \"dep_type\": \"Required\", \
\"discovered_via\": \"config\"}],\n  \"hosts\": [\"localhost\"]\n}"
        );

        let messages = vec![ChatMessage {
            role: MessageRole::User,
            content: prompt,
        }];

        let (response, _) = self
            .client
            .send_message(EXTRACTION_SYSTEM_PROMPT, &messages)
            .await?;

        // Flexible parsing: LLMs return services as either array or object
        let json_str = crate::api::schema::extract_json(&response);
        let raw: serde_json::Value = serde_json::from_str(&json_str)
            .map_err(|e| AgentError::InfraError(format!("Invalid JSON from LLM: {e}")))?;

        let graph = parse_graph_flexible(&raw)?;
        Ok(graph)
    }

    /// Full discovery: collect + extract + post-process + return graph.
    pub async fn discover(&self, target: &TargetHost) -> Result<InfraGraph> {
        tracing::info!("Phase 1: Collecting raw system data...");
        let raw_data = self.collect_raw_data(target).await?;
        tracing::info!(commands = raw_data.len(), "Raw data collected");

        tracing::info!("Phase 2: Extracting infrastructure graph...");
        let mut graph = self.extract_graph(&raw_data).await?;

        // Phase 3: Post-process — resolve any "unknown*" service names from raw ss/docker data
        let unknown_count = graph.services.keys().filter(|k| k.starts_with("unknown")).count();
        if unknown_count > 0 {
            tracing::info!(unknown = unknown_count, "Phase 3: Resolving unknown service names from raw data...");
            resolve_unknown_services(&mut graph, &raw_data);
            let remaining = graph.services.keys().filter(|k| k.starts_with("unknown")).count();
            tracing::info!(resolved = unknown_count - remaining, remaining, "Service names resolved");
        }

        tracing::info!(
            services = graph.services.len(),
            dependencies = graph.dependencies.len(),
            "Graph extracted"
        );

        Ok(graph)
    }
}

/// Flexibly parse LLM output into InfraGraph.
/// Handles both array format (DeepSeek) and object format (Claude).
fn parse_graph_flexible(raw: &serde_json::Value) -> Result<InfraGraph> {
    let mut graph = InfraGraph::new();

    // Parse services — accept array or object
    if let Some(services) = raw.get("services") {
        match services {
            serde_json::Value::Array(arr) => {
                for item in arr {
                    if let Some(svc) = parse_service_from_value(item) {
                        graph.services.insert(svc.name.clone(), svc);
                    }
                }
            }
            serde_json::Value::Object(map) => {
                for (name, item) in map {
                    if let Some(mut svc) = parse_service_from_value(item) {
                        if svc.name.is_empty() {
                            svc.name = name.clone();
                        }
                        graph.services.insert(svc.name.clone(), svc);
                    }
                }
            }
            _ => {}
        }
    }

    // Parse dependencies
    if let Some(serde_json::Value::Array(deps)) = raw.get("dependencies") {
        for item in deps {
            let from = item.get("from").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let to = item.get("to").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let dep_type_str = item.get("dep_type").and_then(|v| v.as_str()).unwrap_or("Required");
            let discovered_via = item.get("discovered_via").and_then(|v| v.as_str()).unwrap_or("").to_string();

            if !from.is_empty() && !to.is_empty() {
                graph.dependencies.push(Dependency {
                    from,
                    to,
                    dep_type: match dep_type_str {
                        "Optional" => DependencyType::Optional,
                        "LoadBalanced" => DependencyType::LoadBalanced,
                        _ => DependencyType::Required,
                    },
                    discovered_via,
                });
            }
        }
    }

    // Parse hosts
    if let Some(serde_json::Value::Array(hosts)) = raw.get("hosts") {
        graph.hosts = hosts.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    }
    if graph.hosts.is_empty() {
        // Derive from services
        let hosts: std::collections::HashSet<String> = graph.services.values().map(|s| s.host.clone()).collect();
        graph.hosts = hosts.into_iter().collect();
    }

    Ok(graph)
}

fn parse_service_from_value(v: &serde_json::Value) -> Option<Service> {
    let name = v.get("name").or(v.get("service_name")).and_then(|n| n.as_str())?.to_string();
    let host = v.get("host").and_then(|h| h.as_str()).unwrap_or("localhost").to_string();
    let port = v.get("port").and_then(|p| p.as_u64()).map(|p| p as u16);
    let process_name = v.get("process_name").and_then(|p| p.as_str()).map(String::from);

    let log_paths: Vec<String> = match v.get("log_paths").or(v.get("log_path")) {
        Some(serde_json::Value::Array(arr)) => arr.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        Some(serde_json::Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => vec![],
    };

    let config_paths: Vec<String> = match v.get("config_paths").or(v.get("config_path")) {
        Some(serde_json::Value::Array(arr)) => arr.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        Some(serde_json::Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => vec![],
    };

    let health_check = v.get("health_check").and_then(|h| h.as_str()).map(String::from);

    let service_type_str = v.get("service_type").and_then(|s| s.as_str()).unwrap_or("Custom");
    let service_type = match service_type_str {
        "Web" => ServiceType::Web,
        "Database" => ServiceType::Database,
        "Cache" => ServiceType::Cache,
        "Queue" => ServiceType::Queue,
        "LoadBalancer" => ServiceType::LoadBalancer,
        _ => ServiceType::Custom,
    };

    let discovered_via_str = v.get("discovered_via").and_then(|d| d.as_str()).unwrap_or("Process");
    let discovered_via = match discovered_via_str {
        "Systemd" => DiscoveryMethod::Systemd,
        "Port" => DiscoveryMethod::Port,
        "Docker" => DiscoveryMethod::Docker,
        "Manual" => DiscoveryMethod::Manual,
        _ => DiscoveryMethod::Process,
    };

    Some(Service {
        name,
        host,
        port,
        process_name,
        log_paths,
        config_paths,
        health_check,
        service_type,
        discovered_via,
    })
}

/// Post-process: resolve any "unknown*" service names from raw ss output.
/// Parses `users:(("program_name",pid=N,...))` from ss -tlnp to get real names.
fn resolve_unknown_services(graph: &mut InfraGraph, raw_data: &[(String, String)]) {
    // Build port → program name map from ss output
    let mut port_to_program: HashMap<u16, String> = HashMap::new();

    for (name, output) in raw_data {
        if name == "listening_ports" {
            for line in output.lines() {
                // Parse ss -tlnp output: look for port and users:(("name",...))
                if let (Some(port), Some(program)) = (extract_port_from_ss(line), extract_program_from_ss(line)) {
                    port_to_program.insert(port, program);
                }
            }
        }
    }

    // Also check docker_containers for container names
    let mut port_to_container: HashMap<u16, String> = HashMap::new();
    for (name, output) in raw_data {
        if name == "docker_containers" {
            for line in output.lines() {
                // Docker ps --format output: parse container names and port mappings
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                    let container_name = v.get("Names").and_then(|n| n.as_str()).unwrap_or_default();
                    let ports = v.get("Ports").and_then(|p| p.as_str()).unwrap_or_default();
                    // Parse port mappings like "0.0.0.0:3000->3000/tcp"
                    for mapping in ports.split(", ") {
                        if let Some(host_port) = mapping.split("->").next()
                            .and_then(|s| s.rsplit(':').next())
                            .and_then(|p| p.parse::<u16>().ok())
                            .filter(|_| !container_name.is_empty())
                        {
                            port_to_container.insert(host_port, container_name.to_string());
                        }
                    }
                }
            }
        }
    }

    // Rename unknown services
    let unknown_keys: Vec<String> = graph.services.keys()
        .filter(|k| k.starts_with("unknown"))
        .cloned()
        .collect();

    for old_name in unknown_keys {
        if let Some(svc) = graph.services.remove(&old_name) {
            let new_name = if let Some(port) = svc.port {
                // Prefer container name, then program name from ss
                if let Some(container) = port_to_container.get(&port) {
                    container.clone()
                } else if let Some(program) = port_to_program.get(&port) {
                    if program == "docker-proxy" {
                        // docker-proxy means it's a container, but we couldn't match it
                        format!("container-{port}")
                    } else {
                        format!("{program}-{port}")
                    }
                } else {
                    old_name.clone()
                }
            } else {
                old_name.clone()
            };
            let mut renamed = svc;
            renamed.name = new_name.clone();
            graph.services.insert(new_name, renamed);
        }
    }
}

fn extract_port_from_ss(line: &str) -> Option<u16> {
    // ss output format: "LISTEN 0 128 0.0.0.0:5432 ..." or "*:5432"
    // Find the local address:port before the peer address
    let parts: Vec<&str> = line.split_whitespace().collect();
    // Column 4 is usually the local address
    for part in &parts {
        if let Some(port) = part.rsplit(':').next()
            .and_then(|s| s.parse::<u16>().ok())
            .filter(|&p| p > 0)
        {
            return Some(port);
        }
    }
    None
}

fn extract_program_from_ss(line: &str) -> Option<String> {
    // Extract from users:(("program_name",pid=N,fd=N))
    let start = line.find("((\"")? + 3;
    let end = start + line[start..].find('"')?;
    let name = &line[start..end];
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

const EXTRACTION_SYSTEM_PROMPT: &str = r#"You receive raw system output from a Linux server. Extract all running services.

SERVICE IDENTIFICATION — follow this process for each listening port in ss output:
1. Read the "users:" field to get the program name (e.g., users:(("nginx",pid=1234,...)) → name is "nginx")
2. Cross-reference the PID with ps output to get the full command and working directory
3. If the program is "docker-proxy", find the matching container in docker ps output and use the CONTAINER NAME as the service name
4. For systemd-managed services, prefer the systemd unit name
5. NEVER use "unknown" as a name — always use the actual program name or container name

NAMING RULES:
- Use the real program/container name: "nginx", "postgres", "redis", "my-app-backend"
- If multiple instances of the same program: append the port, e.g., "postgres-5432", "postgres-5433"
- Docker containers: use the container name from docker ps, NOT "docker-proxy"

For service_type: Web (HTTP servers, APIs), Database (postgres, mysql, mongo), Cache (redis, memcached), Queue (rabbitmq, kafka), LoadBalancer (nginx/haproxy doing proxy_pass), Custom (everything else)
For discovered_via: Systemd, Process, Port, Docker, Manual

A dependency exists when:
- A config file references another service (proxy_pass, upstream, backend URL)
- An environment variable points to another service (DATABASE_URL, REDIS_URL)
- ss/netstat shows a connection between two known services

Return ONLY valid JSON. Be conservative with dependencies — only include those clearly evidenced in the data."#;
