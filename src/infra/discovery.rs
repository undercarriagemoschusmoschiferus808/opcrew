use std::sync::Arc;
use std::time::Duration;

use crate::api::provider::LlmProvider;
use crate::api::schema::validate_and_retry;
use crate::api::types::{ChatMessage, MessageRole};
use crate::error::{AgentError, Result};
use crate::infra::graph::InfraGraph;
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
        prompt.push_str("Return ONLY valid JSON matching the InfraGraph schema.");

        let messages = vec![ChatMessage {
            role: MessageRole::User,
            content: prompt,
        }];

        let (response, _) = self
            .client
            .send_message(EXTRACTION_SYSTEM_PROMPT, &messages)
            .await?;

        let schema = infra_graph_schema();
        let (graph, _): (InfraGraph, _) = validate_and_retry(
            self.client.as_ref(),
            EXTRACTION_SYSTEM_PROMPT,
            &messages,
            &response,
            &schema,
            2,
        )
        .await?;

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

fn infra_graph_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["services", "dependencies"],
        "properties": {
            "services": {
                "type": "object",
                "additionalProperties": {
                    "type": "object",
                    "required": ["name", "host", "service_type", "discovered_via"],
                    "properties": {
                        "name": { "type": "string" },
                        "host": { "type": "string" },
                        "port": { "type": ["integer", "null"] },
                        "process_name": { "type": ["string", "null"] },
                        "log_paths": { "type": "array", "items": { "type": "string" } },
                        "config_paths": { "type": "array", "items": { "type": "string" } },
                        "health_check": { "type": ["string", "null"] },
                        "service_type": { "type": "string" },
                        "discovered_via": { "type": "string" }
                    }
                }
            },
            "dependencies": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["from", "to", "dep_type", "discovered_via"],
                    "properties": {
                        "from": { "type": "string" },
                        "to": { "type": "string" },
                        "dep_type": { "type": "string" },
                        "discovered_via": { "type": "string" }
                    }
                }
            },
            "discovered_at": { "type": "string" },
            "hosts": { "type": "array", "items": { "type": "string" } }
        }
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
