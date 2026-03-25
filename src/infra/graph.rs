use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AgentError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfraGraph {
    pub services: HashMap<String, Service>,
    pub dependencies: Vec<Dependency>,
    pub discovered_at: DateTime<Utc>,
    pub hosts: Vec<String>,
    #[serde(default)]
    pub gaps: Vec<String>,
}

/// Free-form execution context — works with any runtime.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecutionContext {
    /// Runtime type: "docker", "kubernetes", "podman", "systemd", "lxc", "nomad", "local", etc.
    pub runtime: String,
    /// Runtime-specific identifier: container name, pod name, unit name, etc.
    pub identifier: String,
    /// Additional context: namespace, host, user, image, etc.
    #[serde(default)]
    pub extra: std::collections::HashMap<String, String>,
}

impl ExecutionContext {
    pub fn local() -> Self {
        Self { runtime: "local".into(), identifier: String::new(), extra: Default::default() }
    }

    pub fn docker(container_name: &str) -> Self {
        Self { runtime: "docker".into(), identifier: container_name.into(), extra: Default::default() }
    }

    pub fn kubernetes(namespace: &str, name: &str) -> Self {
        let mut extra = std::collections::HashMap::new();
        extra.insert("namespace".into(), namespace.into());
        Self { runtime: "kubernetes".into(), identifier: name.into(), extra }
    }

    /// Format for LLM translation prompt.
    pub fn to_prompt_string(&self) -> String {
        let mut s = format!("runtime: {}", self.runtime);
        if !self.identifier.is_empty() {
            s.push_str(&format!(", identifier: {}", self.identifier));
        }
        for (k, v) in &self.extra {
            s.push_str(&format!(", {k}: {v}"));
        }
        s
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    pub name: String,
    pub host: String,
    pub port: Option<u16>,
    pub process_name: Option<String>,
    pub log_paths: Vec<String>,
    pub config_paths: Vec<String>,
    pub health_check: Option<String>,
    pub service_type: ServiceType,
    pub discovered_via: DiscoveryMethod,
    #[serde(default)]
    pub execution_context: ExecutionContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServiceType {
    Web,
    Database,
    Cache,
    Queue,
    LoadBalancer,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiscoveryMethod {
    Systemd,
    Process,
    Port,
    Docker,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    pub from: String,
    pub to: String,
    pub dep_type: DependencyType,
    pub discovered_via: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DependencyType {
    Required,
    Optional,
    LoadBalanced,
}

impl InfraGraph {
    pub fn new() -> Self {
        Self {
            services: HashMap::new(),
            dependencies: Vec::new(),
            discovered_at: Utc::now(),
            hosts: Vec::new(),
            gaps: Vec::new(),
        }
    }

    pub fn load_from_db(conn: &Connection) -> Result<Option<Self>> {
        let mut stmt = conn
            .prepare("SELECT name, host, port, process_name, log_paths, config_paths, health_check, service_type, discovered_via, discovered_at, execution_context FROM infra_services")
            .map_err(|e| AgentError::InfraError(format!("Prepare: {e}")))?;

        let services: Vec<Service> = stmt
            .query_map([], |row| {
                let log_paths_json: String = row.get(4)?;
                let config_paths_json: String = row.get(5)?;
                Ok(Service {
                    name: row.get(0)?,
                    host: row.get(1)?,
                    port: row.get(2)?,
                    process_name: row.get(3)?,
                    log_paths: serde_json::from_str(&log_paths_json).unwrap_or_default(),
                    config_paths: serde_json::from_str(&config_paths_json).unwrap_or_default(),
                    health_check: row.get(6)?,
                    service_type: parse_service_type(&row.get::<_, String>(7)?),
                    discovered_via: parse_discovery_method(&row.get::<_, String>(8)?),
                    execution_context: {
                        let ctx_json: String = row.get::<_, String>(10).unwrap_or_default();
                        serde_json::from_str(&ctx_json).unwrap_or_default()
                    },
                })
            })
            .map_err(|e| AgentError::InfraError(format!("Query: {e}")))?
            .filter_map(|r| r.ok())
            .collect();

        if services.is_empty() {
            return Ok(None);
        }

        let mut graph = InfraGraph::new();
        let mut hosts = std::collections::HashSet::new();
        for svc in services {
            hosts.insert(svc.host.clone());
            graph.services.insert(svc.name.clone(), svc);
        }
        graph.hosts = hosts.into_iter().collect();

        // Load dependencies
        let mut dep_stmt = conn
            .prepare("SELECT from_service, to_service, dep_type, discovered_via FROM infra_dependencies")
            .map_err(|e| AgentError::InfraError(format!("Prepare deps: {e}")))?;

        graph.dependencies = dep_stmt
            .query_map([], |row| {
                Ok(Dependency {
                    from: row.get(0)?,
                    to: row.get(1)?,
                    dep_type: parse_dep_type(&row.get::<_, String>(2)?),
                    discovered_via: row.get(3)?,
                })
            })
            .map_err(|e| AgentError::InfraError(format!("Query deps: {e}")))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(Some(graph))
    }

    pub fn save_to_db(&self, conn: &Connection) -> Result<()> {
        // Clear existing data (dependencies first, then services)
        conn.execute("DELETE FROM infra_dependencies", [])
            .map_err(|e| AgentError::InfraError(format!("Clear deps: {e}")))?;
        conn.execute("DELETE FROM infra_services", [])
            .map_err(|e| AgentError::InfraError(format!("Clear services: {e}")))?;

        let now = Utc::now().to_rfc3339();
        for svc in self.services.values() {
            let log_paths_json = serde_json::to_string(&svc.log_paths).unwrap_or_default();
            let config_paths_json = serde_json::to_string(&svc.config_paths).unwrap_or_default();
            let ctx_json = serde_json::to_string(&svc.execution_context).unwrap_or_default();
            conn.execute(
                "INSERT INTO infra_services (id, name, host, port, process_name, log_paths, config_paths, health_check, service_type, discovered_via, discovered_at, updated_at, execution_context) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11, ?12)",
                rusqlite::params![
                    Uuid::new_v4().to_string(), svc.name, svc.host, svc.port, svc.process_name,
                    log_paths_json, config_paths_json, svc.health_check,
                    format!("{:?}", svc.service_type), format!("{:?}", svc.discovered_via),
                    now, ctx_json
                ],
            ).map_err(|e| AgentError::InfraError(format!("Save service: {e}")))?;
        }

        for dep in &self.dependencies {
            conn.execute(
                "INSERT INTO infra_dependencies (id, from_service, to_service, dep_type, discovered_via) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    Uuid::new_v4().to_string(), dep.from, dep.to,
                    format!("{:?}", dep.dep_type), dep.discovered_via
                ],
            ).map_err(|e| AgentError::InfraError(format!("Save dep: {e}")))?;
        }

        Ok(())
    }

    pub fn dependencies_of(&self, service: &str) -> Vec<&Service> {
        self.dependencies
            .iter()
            .filter(|d| d.from == service)
            .filter_map(|d| self.services.get(&d.to))
            .collect()
    }

    pub fn dependents_of(&self, service: &str) -> Vec<&Service> {
        self.dependencies
            .iter()
            .filter(|d| d.to == service)
            .filter_map(|d| self.services.get(&d.from))
            .collect()
    }

    pub fn to_context_string(&self) -> String {
        if self.services.is_empty() {
            return String::new();
        }

        let mut out = String::from("INFRASTRUCTURE CONTEXT (auto-discovered):\n\nServices:\n");
        for svc in self.services.values() {
            let port = svc.port.map(|p| format!(":{p}")).unwrap_or_default();
            let logs = if svc.log_paths.is_empty() {
                "(no logs)".into()
            } else {
                svc.log_paths.join(", ")
            };
            out.push_str(&format!(
                "  {} ({:?}{}) logs={}\n",
                svc.name, svc.service_type, port, logs
            ));
        }

        if !self.dependencies.is_empty() {
            out.push_str("\nDependencies:\n");
            for dep in &self.dependencies {
                out.push_str(&format!(
                    "  {} → {} [{:?}] {}\n",
                    dep.from, dep.to, dep.dep_type, dep.discovered_via
                ));
            }
        }

        out.push_str(&format!("\nHosts: {}\n", self.hosts.join(", ")));
        out
    }

    pub fn is_stale(&self, max_age_hours: i64) -> bool {
        let age = Utc::now() - self.discovered_at;
        age.num_hours() > max_age_hours
    }
}

fn parse_service_type(s: &str) -> ServiceType {
    match s {
        "Web" => ServiceType::Web,
        "Database" => ServiceType::Database,
        "Cache" => ServiceType::Cache,
        "Queue" => ServiceType::Queue,
        "LoadBalancer" => ServiceType::LoadBalancer,
        _ => ServiceType::Custom,
    }
}

fn parse_discovery_method(s: &str) -> DiscoveryMethod {
    match s {
        "Systemd" => DiscoveryMethod::Systemd,
        "Process" => DiscoveryMethod::Process,
        "Port" => DiscoveryMethod::Port,
        "Docker" => DiscoveryMethod::Docker,
        "Manual" => DiscoveryMethod::Manual,
        _ => DiscoveryMethod::Process,
    }
}

fn parse_dep_type(s: &str) -> DependencyType {
    match s {
        "Required" => DependencyType::Required,
        "Optional" => DependencyType::Optional,
        "LoadBalanced" => DependencyType::LoadBalanced,
        _ => DependencyType::Required,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::store::MemoryStore;

    #[test]
    fn save_and_load_roundtrip() {
        let store = MemoryStore::open_in_memory().unwrap();
        let conn = store.connection().lock().unwrap();

        let mut graph = InfraGraph::new();
        graph.services.insert(
            "nginx".into(),
            Service {
                name: "nginx".into(),
                host: "localhost".into(),
                port: Some(80),
                process_name: Some("nginx".into()),
                log_paths: vec!["/var/log/nginx/".into()],
                config_paths: vec!["/etc/nginx/nginx.conf".into()],
                health_check: Some("curl -s localhost:80".into()),
                service_type: ServiceType::LoadBalancer,
                discovered_via: DiscoveryMethod::Systemd,
                execution_context: ExecutionContext::local(),
            },
        );
        graph.services.insert(
            "app".into(),
            Service {
                name: "app".into(),
                host: "localhost".into(),
                port: Some(3000),
                process_name: Some("node".into()),
                log_paths: vec!["/var/log/app/".into()],
                config_paths: vec![],
                health_check: None,
                service_type: ServiceType::Web,
                discovered_via: DiscoveryMethod::Process,
                execution_context: ExecutionContext::local(),
            },
        );
        graph.dependencies.push(Dependency {
            from: "nginx".into(),
            to: "app".into(),
            dep_type: DependencyType::Required,
            discovered_via: "proxy_pass in nginx.conf".into(),
        });
        graph.hosts = vec!["localhost".into()];

        graph.save_to_db(&conn).unwrap();

        let loaded = InfraGraph::load_from_db(&conn).unwrap().unwrap();
        assert_eq!(loaded.services.len(), 2);
        assert_eq!(loaded.dependencies.len(), 1);
        assert!(loaded.services.contains_key("nginx"));
    }

    #[test]
    fn dependency_traversal() {
        let mut graph = InfraGraph::new();
        graph.services.insert("a".into(), Service {
            name: "a".into(), host: "localhost".into(), port: None, process_name: None,
            log_paths: vec![], config_paths: vec![], health_check: None,
            service_type: ServiceType::Web, discovered_via: DiscoveryMethod::Process,
            execution_context: ExecutionContext::local(),
        });
        graph.services.insert("b".into(), Service {
            name: "b".into(), host: "localhost".into(), port: None, process_name: None,
            log_paths: vec![], config_paths: vec![], health_check: None,
            service_type: ServiceType::Database, discovered_via: DiscoveryMethod::Process,
            execution_context: ExecutionContext::local(),
        });
        graph.dependencies.push(Dependency {
            from: "a".into(), to: "b".into(),
            dep_type: DependencyType::Required, discovered_via: "env".into(),
        });

        let deps = graph.dependencies_of("a");
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "b");

        let dependents = graph.dependents_of("b");
        assert_eq!(dependents.len(), 1);
        assert_eq!(dependents[0].name, "a");
    }

    #[test]
    fn context_string_format() {
        let mut graph = InfraGraph::new();
        graph.services.insert("nginx".into(), Service {
            name: "nginx".into(), host: "localhost".into(), port: Some(80),
            process_name: None, log_paths: vec!["/var/log/nginx/".into()],
            config_paths: vec![], health_check: None,
            service_type: ServiceType::LoadBalancer, discovered_via: DiscoveryMethod::Systemd,
            execution_context: ExecutionContext::local(),
        });
        graph.hosts = vec!["localhost".into()];

        let ctx = graph.to_context_string();
        assert!(ctx.contains("nginx"));
        assert!(ctx.contains("LoadBalancer"));
        assert!(ctx.contains(":80"));
        assert!(ctx.contains("localhost"));
    }

    #[test]
    fn empty_db_returns_none() {
        let store = MemoryStore::open_in_memory().unwrap();
        let conn = store.connection().lock().unwrap();
        let result = InfraGraph::load_from_db(&conn).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn staleness_detection() {
        let mut graph = InfraGraph::new();
        graph.discovered_at = Utc::now() - chrono::Duration::hours(25);
        assert!(graph.is_stale(24));

        graph.discovered_at = Utc::now() - chrono::Duration::hours(1);
        assert!(!graph.is_stale(24));
    }
}
