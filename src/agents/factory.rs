use std::sync::Arc;

use crate::api::provider::LlmProvider;
use crate::domain::agent::{AgentConfig, AgentId};
use crate::domain::plan::{Plan, PlannedRole};
use crate::domain::squad::Squad;
use crate::domain::task::{Task, TaskId};
use crate::error::Result;
use crate::execution::budget::TokenBudget;
use crate::safety::guardian::GuardianAgent;
use crate::observability::metrics::Metrics;
use crate::safety::secrets::SecretMasker;
use crate::tools::registry::ToolRegistry;

use super::specialist::SpecialistAgent;

pub struct AgentFactory {
    client: Arc<dyn LlmProvider>,
    tools: Arc<ToolRegistry>,
    guardian: Arc<GuardianAgent>,
    budget: Arc<TokenBudget>,
    masker: Arc<SecretMasker>,
    metrics: Arc<Metrics>,
}

impl AgentFactory {
    pub fn new(
        client: Arc<dyn LlmProvider>,
        tools: Arc<ToolRegistry>,
        guardian: Arc<GuardianAgent>,
        budget: Arc<TokenBudget>,
        masker: Arc<SecretMasker>,
        metrics: Arc<Metrics>,
    ) -> Self {
        Self {
            client,
            tools,
            guardian,
            budget,
            masker,
            metrics,
        }
    }

    pub fn create_squad_from_plan(&self, plan: &Plan, max_agents: u8) -> Result<Squad> {
        let mut squad = Squad::new();

        // Cap roles to max_agents
        let roles: Vec<&PlannedRole> = plan.roles.iter().take(max_agents as usize).collect();

        for role in &roles {
            let agent = self.create_specialist(role)?;
            squad.add_agent(Arc::new(agent));
        }

        // Create tasks and resolve dependencies
        let mut tasks_by_title: std::collections::HashMap<String, TaskId> =
            std::collections::HashMap::new();

        for planned_task in &plan.tasks {
            let mut task = Task::new(
                planned_task.title.clone(),
                planned_task.description.clone(),
                planned_task.assigned_role.clone(),
            )
            .with_priority(planned_task.priority);

            let task_id = task.id.clone();
            tasks_by_title.insert(planned_task.title.clone(), task_id);

            // Resolve dependencies by title
            let deps: Vec<TaskId> = planned_task
                .depends_on
                .iter()
                .filter_map(|dep_title| tasks_by_title.get(dep_title).cloned())
                .collect();
            task = task.with_depends_on(deps);

            // Assign to agent
            if let Some(agent) = squad.agent_for_role(&planned_task.assigned_role) {
                task.assigned_to = Some(agent.id().clone());
            }

            squad.add_task(task);
        }

        tracing::info!(
            agents = squad.agent_count(),
            tasks = squad.task_count(),
            "Squad assembled"
        );

        Ok(squad)
    }

    fn create_specialist(&self, role: &PlannedRole) -> Result<SpecialistAgent> {
        let system_prompt = generate_system_prompt(role);

        let config = AgentConfig {
            id: AgentId::new(),
            role: role.role_name.clone(),
            expertise: role.expertise.clone(),
            system_prompt,
            goal: role.responsibility.clone(),
            allowed_tools: role.allowed_tools.clone(),
            token_budget: role.token_budget,
            max_conversation_turns: 30,
        };

        Ok(SpecialistAgent::new(
            config,
            Arc::clone(&self.client),
            Arc::clone(&self.tools),
            Arc::clone(&self.guardian),
            Arc::clone(&self.budget),
            Arc::clone(&self.masker),
            Arc::clone(&self.metrics),
        ))
    }
}

fn generate_system_prompt(role: &PlannedRole) -> String {
    format!(
        r#"You are a specialist agent with the role: {role_name}.

Your areas of expertise: {expertise}

Your responsibility: {responsibility}

Available tools: {tools}

To use a tool, output a JSON object:
{{"tool": "tool_name", "action": "action_name", "args": {{"key": "value"}}}}

Tool reference:
- shell: {{"tool": "shell", "action": "run", "args": {{"command": "your command here"}}}}
- file_ops: {{"tool": "file_ops", "action": "read|write|list|exists|delete", "args": {{"path": "/path", "content": "..."}}}}
- log_reader: {{"tool": "log_reader", "action": "read|search", "args": {{"path": "/path", "lines": "100", "pattern": "error"}}}}
- code_writer: {{"tool": "code_writer", "action": "create|edit", "args": {{"path": "/path", "content": "...", "old_text": "...", "new_text": "..."}}}}

RULES:
- Output ONLY the JSON tool call, nothing else. No explanation before or after.
- Do NOT wrap JSON in markdown code blocks.
- Be EFFICIENT: use the fewest tool calls possible. 2-3 calls is ideal, 5 max.
- If one command can answer the question, use one command. Don't run 5 variations.
- APPLY fixes, don't just recommend them.
- If blocked, try ONE alternative, then move on.
- When done, give your final answer WITHOUT a tool call.

EFFICIENCY EXAMPLES:
- To check a container: docker logs X --tail 30 (one call, not docker ps + docker inspect + docker logs separately)
- To check a service: systemctl status X (one call gives status + recent logs)
- To check disk: df -h (one call, not df + du + find separately)

Final answer format:
RESULT: [what you found and did]
HYPOTHESIS: confirmed/denied
EVIDENCE: [key evidence]"#,
        role_name = role.role_name,
        expertise = role.expertise.join(", "),
        responsibility = role.responsibility,
        tools = role.allowed_tools.join(", "),
    )
}

/// Base system prompt for specialist agents (used by fast-path in main.rs)
pub const SPECIALIST_SYSTEM_PROMPT: &str = r#"You are a specialist agent that diagnoses and fixes infrastructure problems.

To use a tool, output ONLY a JSON object (no markdown, no explanation before it):
{"tool": "shell", "action": "run", "args": {"command": "your command here"}}

Available tools:
- shell: {"tool": "shell", "action": "run", "args": {"command": "..."}}
- file_ops: {"tool": "file_ops", "action": "read|write|list", "args": {"path": "...", "content": "..."}}
- log_reader: {"tool": "log_reader", "action": "read|search", "args": {"path": "...", "lines": "100", "pattern": "..."}}

RULES:
- ONE tool call per message — output ONLY the JSON, nothing else
- Do NOT wrap JSON in markdown code blocks
- You MUST execute commands, not just describe them
- After confirming a problem, APPLY the fix
- When done, provide your final answer WITHOUT a tool call"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::plan::{Plan, PlannedRole, PlannedTask};

    #[test]
    fn generate_prompt_includes_role_info() {
        let role = PlannedRole {
            role_name: "Log Analyst".into(),
            expertise: vec!["log analysis".into(), "regex".into()],
            responsibility: "Analyze nginx logs".into(),
            allowed_tools: vec!["shell".into(), "log_reader".into()],
            token_budget: 50000,
            target_host: None,
        };

        let prompt = generate_system_prompt(&role);
        assert!(prompt.contains("Log Analyst"));
        assert!(prompt.contains("log analysis"));
        assert!(prompt.contains("Analyze nginx logs"));
        assert!(prompt.contains("shell"));
    }
}
