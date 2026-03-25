use std::sync::Arc;

use colored::*;

use crate::api::provider::LlmProvider;
use crate::cli::InfraAction;
use crate::error::Result;
use crate::infra::discovery::DiscoveryAgent;
use crate::infra::graph::InfraGraph;
use crate::memory::store::MemoryStore;
use crate::tools::target::TargetHost;

pub async fn handle_infra_command(
    action: &InfraAction,
    memory: &MemoryStore,
    client: &Arc<dyn LlmProvider>,
) -> Result<()> {
    match action {
        InfraAction::Discover { host } => {
            let target = host
                .as_ref()
                .and_then(|h| TargetHost::parse_target(h))
                .unwrap_or_default();

            let agent = DiscoveryAgent::new(Arc::clone(client));
            let graph = agent.discover(&target).await?;

            let conn = memory.connection().lock().unwrap();
            graph.save_to_db(&conn)?;

            println!("{} Discovery complete", "✓".green());
            println!(
                "  Found {} services, {} dependencies",
                graph.services.len(),
                graph.dependencies.len()
            );
            for svc in graph.services.values() {
                let port = svc.port.map(|p| format!(":{p}")).unwrap_or_default();
                println!(
                    "  {:<28} {:?}{port}",
                    svc.name.bold(),
                    svc.service_type
                );
            }
            if !graph.dependencies.is_empty() {
                println!("  Dependencies:");
                for dep in &graph.dependencies {
                    println!(
                        "    {} → {} [{:?}] {}",
                        dep.from, dep.to, dep.dep_type, dep.discovered_via
                    );
                }
            }
            println!("  Saved to ~/.opcrew/memory.db");
        }
        InfraAction::Show { json } => {
            let conn = memory.connection().lock().unwrap();
            match InfraGraph::load_from_db(&conn)? {
                Some(graph) => {
                    if *json {
                        println!("{}", serde_json::to_string_pretty(&graph).unwrap_or_default());
                    } else {
                        print_graph(&graph);
                    }
                }
                None => {
                    println!("No infrastructure graph found. Run `opcrew infra discover` first.");
                }
            }
        }
        InfraAction::Clear => {
            let conn = memory.connection().lock().unwrap();
            conn.execute("DELETE FROM infra_dependencies", []).ok();
            conn.execute("DELETE FROM infra_services", []).ok();
            println!("{} Infrastructure graph cleared", "✓".green());
        }
        InfraAction::Add { host, user } => {
            println!("Added host {}@{} (run `infra discover --host {}@{}` to scan)", user, host, user, host);
        }
        InfraAction::Update { service } => {
            println!("Re-scanning service: {service} (not yet implemented — use `infra discover` for full rescan)");
        }
    }
    Ok(())
}

fn print_graph(graph: &InfraGraph) {
    println!("\n{}", "Infrastructure Graph".bold());
    if graph.is_stale(24) {
        eprintln!(
            "{}",
            "⚠ Infra graph is stale. Run `opcrew infra discover` to refresh."
                .yellow()
        );
    }

    println!("\nServices:");
    for svc in graph.services.values() {
        let port = svc.port.map(|p| format!(":{p}")).unwrap_or_default();
        let logs = if svc.log_paths.is_empty() {
            "(no logs)".into()
        } else {
            svc.log_paths.join(", ")
        };
        println!(
            "  {} {:<28} {:<15} {}",
            "●".green(),
            svc.name.bold(),
            format!("{:?}{}", svc.service_type, port).dimmed(),
            logs.dimmed(),
        );
    }

    if !graph.dependencies.is_empty() {
        println!("\nDependencies:");
        for dep in &graph.dependencies {
            println!(
                "  {} → {}  [{}]  {}",
                dep.from.cyan(),
                dep.to.cyan(),
                format!("{:?}", dep.dep_type).dimmed(),
                dep.discovered_via.dimmed(),
            );
        }
    }

    println!("\nHosts: {}", graph.hosts.join(", "));
}
