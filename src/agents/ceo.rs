use std::sync::Arc;

use async_trait::async_trait;

use crate::api::provider::LlmProvider;
use crate::api::schema::validate_and_retry;
use crate::api::types::{ChatMessage, MessageRole};
use crate::domain::agent::{AgentBehavior, AgentConfig, AgentId, AgentOutput};
use crate::domain::plan::{plan_json_schema, Plan};
use crate::error::{AgentError, Result};

pub struct CeoAgent {
    config: AgentConfig,
    client: Arc<dyn LlmProvider>,
}

impl CeoAgent {
    pub fn new(client: Arc<dyn LlmProvider>) -> Self {
        let config = AgentConfig {
            id: AgentId::new(),
            role: "CEO".to_string(),
            expertise: vec![
                "strategic planning".into(),
                "problem decomposition".into(),
                "team coordination".into(),
            ],
            system_prompt: PLANNING_SYSTEM_PROMPT.to_string(),
            goal: "Analyze problems and create optimal squad plans".to_string(),
            allowed_tools: vec![],
            token_budget: 200_000,
            max_conversation_turns: 10,
        };
        Self { config, client }
    }

    /// Analyze a problem and create a structured execution plan.
    pub async fn create_plan(&self, problem: &str) -> Result<Plan> {
        let messages = vec![ChatMessage {
            role: MessageRole::User,
            content: format!(
                "Analyze this problem and create an execution plan:\n\n{problem}"
            ),
        }];

        let (response, _usage) = self
            .client
            .send_message(PLANNING_SYSTEM_PROMPT, &messages)
            .await?;

        // Validate against schema with retry
        let schema = plan_json_schema();
        let (mut plan, _extra_usage): (Plan, _) = validate_and_retry(
            self.client.as_ref(),
            PLANNING_SYSTEM_PROMPT,
            &messages,
            &response,
            &schema,
            2,
        )
        .await?;

        plan.problem_statement = problem.to_string();
        Ok(plan)
    }

    /// Synthesize results from all agents into a final report.
    /// Uses incremental summaries to keep context bounded.
    pub async fn synthesize(
        &self,
        problem: &str,
        level_summaries: &[String],
    ) -> Result<String> {
        let mut prompt = format!(
            "## Original Problem\n\n{problem}\n\n## Work Completed\n\n"
        );

        for (i, summary) in level_summaries.iter().enumerate() {
            prompt.push_str(&format!("### Phase {}\n\n{summary}\n\n---\n\n", i + 1));
        }

        prompt.push_str(
            "Synthesize these results into a comprehensive final report:\n\
             - Executive Summary (2-3 sentences)\n\
             - What was done\n\
             - What was found\n\
             - Recommendations or fixes applied\n\
             - Remaining issues (if any)",
        );

        let messages = vec![ChatMessage {
            role: MessageRole::User,
            content: prompt,
        }];

        let (response, _usage) = self
            .client
            .send_message(SYNTHESIS_SYSTEM_PROMPT, &messages)
            .await?;

        Ok(response)
    }

    /// Summarize results from one execution level (for incremental synthesis).
    pub async fn summarize_level(
        &self,
        level: usize,
        outputs: &[AgentOutput],
    ) -> Result<String> {
        if outputs.is_empty() {
            return Ok(format!("Level {level}: No results"));
        }

        let mut prompt = format!("Summarize the work from these {n} agents into a concise paragraph:\n\n",
            n = outputs.len());

        for output in outputs {
            prompt.push_str(&format!(
                "### {} (confidence: {:.0}%)\n{}\n\n",
                output.role,
                output.confidence * 100.0,
                &output.content[..output.content.len().min(2000)],
            ));
        }

        prompt.push_str("Keep the summary under 500 words. Preserve key findings and actions taken.");

        let messages = vec![ChatMessage {
            role: MessageRole::User,
            content: prompt,
        }];

        let (response, _usage) = self
            .client
            .send_message(
                "You are a concise summarizer. Distill key findings into a brief summary.",
                &messages,
            )
            .await?;

        Ok(response)
    }
}

#[async_trait]
impl AgentBehavior for CeoAgent {
    fn config(&self) -> &AgentConfig {
        &self.config
    }

    async fn execute(&self, input: &str) -> Result<AgentOutput> {
        let plan = self.create_plan(input).await?;
        let content = serde_json::to_string_pretty(&plan)
            .map_err(|e| AgentError::PlanningError(e.to_string()))?;

        Ok(AgentOutput::new(
            self.config.id.clone(),
            "CEO".into(),
            content,
        )
        .with_confidence(0.9))
    }
}

const PLANNING_SYSTEM_PROMPT: &str = r#"You are the CEO of a specialist agent squad diagnosing and fixing infrastructure problems.
You are working with: developers, devops engineers, and sysadmins.

REASONING PROTOCOL — you must follow this structure for every plan:

## Step 1: Problem Analysis
- What exactly is failing? (symptom vs root cause)
- What components are likely involved?
- What information do I NOT have yet that I need?

## Step 2: Hypothesis Generation
- List 3-5 possible root causes, ranked by probability (most likely first)
- For each hypothesis: what evidence would confirm or deny it?

## Step 3: Efficient Investigation Strategy
- What is the FASTEST way to confirm or deny the most likely hypothesis?
- What read-only checks should run FIRST before any changes?
- What dependencies exist between checks?

## Step 4: Plan
- Only AFTER completing steps 1-3, create the agent plan
- Assign agents to test specific hypotheses, not to "investigate generally"
- Each agent must have a specific hypothesis to confirm or deny

RULES:
- Never start with destructive actions — read before you write
- If you lack information, create an agent to gather it before creating agents to fix
- Be specific: "check if port 8080 is listening on upstream" not "check the backend"
- Use past experience provided to you — don't repeat what already failed

OUTPUT FORMAT: valid JSON matching this schema exactly:
{
  "analysis": "Your step 1+2 analysis...",
  "roles": [
    {
      "role_name": "Role Name",
      "expertise": ["skill1", "skill2"],
      "responsibility": "What this role does",
      "allowed_tools": ["shell", "log_reader"],
      "token_budget": 50000
    }
  ],
  "tasks": [
    {
      "title": "Task title",
      "description": "Detailed description with clear deliverables",
      "assigned_role": "Role Name",
      "depends_on": [],
      "priority": 1,
      "hypothesis": "H1: upstream server is down"
    }
  ],
  "synthesis_strategy": "How to combine results"
}

Create 2-5 roles, 1-10 tasks. ONLY output valid JSON."#;

const SYNTHESIS_SYSTEM_PROMPT: &str = r#"You are a CEO agent synthesizing results from your specialist team.

Provide a clear, actionable final report. Be specific about:
- What the problem was
- What was investigated and found
- What actions were taken or recommended
- What remains to be done

Be concise but thorough. Format with clear sections."#;
