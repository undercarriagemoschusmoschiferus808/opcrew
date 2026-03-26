use std::sync::Arc;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::agents::ceo::CeoAgent;
use crate::domain::agent::AgentOutput;
use crate::domain::squad::Squad;
use crate::domain::task::{Task, TaskId};
use crate::error::{AgentError, Result};

pub struct SquadRunner {
    ceo: Arc<CeoAgent>,
    cancellation: CancellationToken,
}

impl SquadRunner {
    pub fn new(ceo: Arc<CeoAgent>, cancellation: CancellationToken) -> Self {
        Self { ceo, cancellation }
    }

    /// Execute the squad: topological sort → concurrent execution per level → synthesis.
    pub async fn execute(
        &self,
        squad: &Squad,
        dry_run: bool,
    ) -> Result<(Vec<AgentOutput>, Vec<String>)> {
        if dry_run {
            return self.dry_run_execution(squad).await;
        }

        let levels = topological_sort(&squad.tasks)?;
        let mut all_outputs: Vec<AgentOutput> = Vec::new();
        let mut level_summaries: Vec<String> = Vec::new();

        for (level_idx, task_ids) in levels.iter().enumerate() {
            if self.cancellation.is_cancelled() {
                tracing::warn!("Cancellation requested, stopping execution");
                break;
            }

            tracing::info!(level = level_idx, tasks = task_ids.len(), "Starting level");

            let mut join_set: JoinSet<Result<AgentOutput>> = JoinSet::new();
            let cancel = self.cancellation.child_token();

            for task_id in task_ids {
                let task = squad
                    .tasks
                    .iter()
                    .find(|t| &t.id == task_id)
                    .ok_or_else(|| AgentError::ExecutionError {
                        agent_role: "runner".into(),
                        message: format!("Task {task_id} not found"),
                    })?;

                let agent = squad.agent_for_role(&task.assigned_role).ok_or_else(|| {
                    AgentError::ExecutionError {
                        agent_role: task.assigned_role.clone(),
                        message: format!("No agent for role: {}", task.assigned_role),
                    }
                })?;

                let input = build_task_input(task, &all_outputs);
                let agent = Arc::clone(agent);
                let cancel = cancel.clone();
                let role = task.assigned_role.clone();

                join_set.spawn(async move {
                    tokio::select! {
                        result = agent.execute(&input) => {
                            match result {
                                Ok(output) => {
                                    tracing::info!(agent = %role, "Task completed");
                                    Ok(output)
                                }
                                Err(e) => {
                                    tracing::error!(agent = %role, error = %e, "Task failed");
                                    // Return error output instead of propagating
                                    Ok(AgentOutput::new(
                                        agent.id().clone(),
                                        role,
                                        format!("FAILED: {e}"),
                                    ).with_confidence(0.0))
                                }
                            }
                        }
                        _ = cancel.cancelled() => {
                            tracing::warn!(agent = %role, "Cancelled");
                            Ok(AgentOutput::new(
                                agent.id().clone(),
                                role,
                                "CANCELLED".into(),
                            ).with_confidence(0.0))
                        }
                    }
                });
            }

            // Collect results from this level
            let mut level_outputs = Vec::new();
            while let Some(result) = join_set.join_next().await {
                match result {
                    Ok(Ok(output)) => level_outputs.push(output),
                    Ok(Err(e)) => {
                        tracing::error!(error = %e, "Agent task error");
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Join error (panic)");
                    }
                }
            }

            // Incremental summary from CEO
            if !level_outputs.is_empty() {
                match self.ceo.summarize_level(level_idx, &level_outputs).await {
                    Ok(summary) => {
                        tracing::info!(level = level_idx, "Level summarized");
                        level_summaries.push(summary);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Summary failed, using raw output");
                        let raw = level_outputs
                            .iter()
                            .map(|o| {
                                format!("[{}]: {}", o.role, &o.content[..o.content.len().min(500)])
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        level_summaries.push(raw);
                    }
                }
            }

            all_outputs.extend(level_outputs);
        }

        Ok((all_outputs, level_summaries))
    }

    async fn dry_run_execution(&self, squad: &Squad) -> Result<(Vec<AgentOutput>, Vec<String>)> {
        let levels = topological_sort(&squad.tasks)?;
        let mut summaries = Vec::new();

        for (level_idx, task_ids) in levels.iter().enumerate() {
            let mut level_info = format!("[DRY-RUN] Level {}:\n", level_idx);
            for task_id in task_ids {
                if let Some(task) = squad.tasks.iter().find(|t| &t.id == task_id) {
                    level_info.push_str(&format!(
                        "  - [{}] {} → {}\n",
                        task.assigned_role, task.title, task.description
                    ));
                }
            }
            summaries.push(level_info);
        }

        Ok((Vec::new(), summaries))
    }
}

/// Group tasks into dependency levels using Kahn's algorithm.
/// Returns Vec of levels, each level contains TaskIds that can run concurrently.
pub fn topological_sort(tasks: &[Task]) -> Result<Vec<Vec<TaskId>>> {
    let mut in_degree: std::collections::HashMap<TaskId, usize> = std::collections::HashMap::new();
    let mut dependents: std::collections::HashMap<TaskId, Vec<TaskId>> =
        std::collections::HashMap::new();

    // Initialize
    for task in tasks {
        in_degree.entry(task.id.clone()).or_insert(0);
        for dep in &task.depends_on {
            *in_degree.entry(task.id.clone()).or_insert(0) += 1;
            dependents
                .entry(dep.clone())
                .or_default()
                .push(task.id.clone());
        }
    }

    let mut levels: Vec<Vec<TaskId>> = Vec::new();
    let mut queue: Vec<TaskId> = in_degree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(id, _)| id.clone())
        .collect();

    let mut processed = 0;

    while !queue.is_empty() {
        levels.push(queue.clone());
        let mut next_queue = Vec::new();

        for id in &queue {
            processed += 1;
            if let Some(deps) = dependents.get(id) {
                for dep_id in deps {
                    if let Some(deg) = in_degree.get_mut(dep_id) {
                        *deg -= 1;
                        if *deg == 0 {
                            next_queue.push(dep_id.clone());
                        }
                    }
                }
            }
        }

        queue = next_queue;
    }

    if processed != tasks.len() {
        return Err(AgentError::PlanningError(
            "Circular dependency detected in task graph".into(),
        ));
    }

    Ok(levels)
}

fn build_task_input(task: &Task, previous_results: &[AgentOutput]) -> String {
    let mut input = format!("## Task: {}\n\n{}\n", task.title, task.description);

    if !previous_results.is_empty() {
        input.push_str("\n## Context from previous work:\n\n");
        for result in previous_results {
            let truncated = if result.content.len() > 1000 {
                format!("{}...", &result.content[..1000])
            } else {
                result.content.clone()
            };
            input.push_str(&format!(
                "### {} (confidence: {:.0}%)\n{}\n\n",
                result.role,
                result.confidence * 100.0,
                truncated,
            ));
        }
    }

    input
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::task::Task;

    #[test]
    fn topological_sort_no_deps() {
        let tasks = vec![
            Task::new("A".into(), "".into(), "dev".into()),
            Task::new("B".into(), "".into(), "dev".into()),
        ];
        let levels = topological_sort(&tasks).unwrap();
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].len(), 2);
    }

    #[test]
    fn topological_sort_linear_chain() {
        let a = Task::new("A".into(), "".into(), "dev".into());
        let b = Task::new("B".into(), "".into(), "dev".into());
        let c = Task::new("C".into(), "".into(), "dev".into());

        let b = b.with_depends_on(vec![a.id.clone()]);
        let c = c.with_depends_on(vec![b.id.clone()]);

        let tasks = vec![a, b, c];
        let levels = topological_sort(&tasks).unwrap();
        assert_eq!(levels.len(), 3);
    }

    #[test]
    fn topological_sort_diamond() {
        let a = Task::new("A".into(), "".into(), "dev".into());
        let b = Task::new("B".into(), "".into(), "dev".into()).with_depends_on(vec![a.id.clone()]);
        let c = Task::new("C".into(), "".into(), "dev".into()).with_depends_on(vec![a.id.clone()]);
        let d = Task::new("D".into(), "".into(), "dev".into())
            .with_depends_on(vec![b.id.clone(), c.id.clone()]);

        let tasks = vec![a, b, c, d];
        let levels = topological_sort(&tasks).unwrap();
        assert_eq!(levels.len(), 3); // [A], [B,C], [D]
        assert_eq!(levels[0].len(), 1);
        assert_eq!(levels[1].len(), 2);
        assert_eq!(levels[2].len(), 1);
    }

    #[test]
    fn topological_sort_detects_cycle() {
        let mut a = Task::new("A".into(), "".into(), "dev".into());
        let mut b = Task::new("B".into(), "".into(), "dev".into());
        // A depends on B, B depends on A = cycle
        let a_id = a.id.clone();
        let b_id = b.id.clone();
        a = a.with_depends_on(vec![b_id]);
        b = b.with_depends_on(vec![a_id]);

        let tasks = vec![a, b];
        let result = topological_sort(&tasks);
        assert!(result.is_err());
    }

    #[test]
    fn build_task_input_with_context() {
        let task = Task::new(
            "Check logs".into(),
            "Read nginx error log".into(),
            "analyst".into(),
        );
        let previous = vec![
            AgentOutput::new(
                AgentId::new(),
                "sysadmin".into(),
                "Server is running".into(),
            )
            .with_confidence(0.8),
        ];

        let input = build_task_input(&task, &previous);
        assert!(input.contains("Check logs"));
        assert!(input.contains("nginx error log"));
        assert!(input.contains("sysadmin"));
        assert!(input.contains("80%"));
    }

    use crate::domain::agent::AgentId;
}
