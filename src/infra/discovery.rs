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

    /// Full discovery: collect + extract + return graph.
    pub async fn discover(&self, target: &TargetHost) -> Result<InfraGraph> {
        tracing::info!("Phase 1: Collecting raw system data...");
        let raw_data = self.collect_raw_data(target).await?;
        tracing::info!(commands = raw_data.len(), "Raw data collected");

        tracing::info!("Phase 2: Extracting infrastructure graph...");
        let graph = self.extract_graph(&raw_data).await?;
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

const EXTRACTION_SYSTEM_PROMPT: &str = r#"You receive raw system output from a Linux server. Extract all running services, their ports, log paths, config paths, and dependencies between them.

A dependency exists when:
- A config file contains proxy_pass, upstream, backend pointing to another service
- An environment variable contains a URL/host pointing to another service (DATABASE_URL, REDIS_URL, etc.)
- A process connects to a port owned by another known process (from ss output)

For service_type use: Web, Database, Cache, Queue, LoadBalancer, Custom
For discovered_via use: Systemd, Process, Port, Docker, Manual

Return ONLY valid JSON. Be conservative: only include dependencies you can clearly evidence from the data. Do not invent dependencies."#;
