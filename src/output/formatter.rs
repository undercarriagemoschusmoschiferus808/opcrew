use colored::*;

use crate::domain::plan::Plan;

pub struct OutputFormatter {
    json_mode: bool,
}

impl OutputFormatter {
    pub fn new(json_mode: bool) -> Self {
        Self { json_mode }
    }

    pub fn format_plan(&self, plan: &Plan) -> String {
        if self.json_mode {
            return serde_json::to_string_pretty(plan).unwrap_or_default();
        }

        let mut out = String::new();
        out.push_str(&format!("\n{}\n", "=".repeat(60).dimmed()));
        out.push_str(&format!("  {}\n", "SQUAD PLAN".bold().cyan()));
        out.push_str(&format!("{}\n\n", "=".repeat(60).dimmed()));
        out.push_str(&format!("{}  {}\n\n", "Analysis:".bold(), plan.analysis));

        out.push_str(&format!("{}\n", "Roles:".bold().yellow()));
        for (i, role) in plan.roles.iter().enumerate() {
            out.push_str(&format!(
                "  {}. {} — {}\n",
                i + 1,
                role.role_name.bold(),
                role.responsibility
            ));
            out.push_str(&format!(
                "     Skills: {}  |  Tools: {}\n",
                role.expertise.join(", ").dimmed(),
                role.allowed_tools.join(", ").dimmed(),
            ));
        }

        out.push_str(&format!("\n{}\n", "Tasks:".bold().yellow()));
        for (i, task) in plan.tasks.iter().enumerate() {
            let deps = if task.depends_on.is_empty() {
                String::new()
            } else {
                format!(" (depends: {})", task.depends_on.join(", "))
            };
            out.push_str(&format!(
                "  {}. [{}] {} {}{}\n",
                i + 1,
                task.assigned_role.cyan(),
                task.title,
                format!("(P{})", task.priority).dimmed(),
                deps.dimmed(),
            ));
        }

        out
    }

    pub fn format_progress(&self, agent_role: &str, status: &str) -> String {
        if self.json_mode {
            return serde_json::json!({"agent": agent_role, "status": status}).to_string();
        }
        format!("  {} {}", format!("[{agent_role}]").cyan().bold(), status)
    }

    pub fn format_final_result(&self, synthesis: &str, token_usage: u32) -> String {
        if self.json_mode {
            return serde_json::json!({
                "result": synthesis,
                "tokens_used": token_usage,
            })
            .to_string();
        }

        let mut out = String::new();
        out.push_str(&format!("\n{}\n", "=".repeat(60).dimmed()));
        out.push_str(&format!("  {}\n", "FINAL RESULT".bold().green()));
        out.push_str(&format!("{}\n\n", "=".repeat(60).dimmed()));
        out.push_str(synthesis);
        out.push_str(&format!(
            "\n\n{}",
            format!("Tokens used: {token_usage}").dimmed()
        ));
        out.push('\n');
        out
    }

    pub fn format_dry_run_header(&self) -> String {
        if self.json_mode {
            return String::new();
        }
        format!(
            "\n{}\n  {}\n{}\n",
            "=".repeat(60).dimmed(),
            "DRY RUN — No commands will be executed".bold().yellow(),
            "=".repeat(60).dimmed(),
        )
    }

    pub fn format_guardian_simulation(&self, tool: &str, command: &str, decision: &str) -> String {
        if self.json_mode {
            return serde_json::json!({
                "dry_run": true,
                "tool": tool,
                "command": command,
                "decision": decision,
            })
            .to_string();
        }

        let colored_decision = match decision {
            d if d.starts_with("APPROVE") => d.green().to_string(),
            d if d.starts_with("BLOCK") => d.red().to_string(),
            d => d.yellow().to_string(),
        };

        format!(
            "  [DRY-RUN] {}: {} → {}",
            tool.cyan(),
            command,
            colored_decision
        )
    }

    pub fn format_escalation(
        &self,
        problem: &str,
        rounds_info: &[(u8, String)],
        verifier_findings: &str,
        ceo_recommendation: &str,
    ) -> String {
        if self.json_mode {
            return serde_json::json!({
                "escalation": true,
                "problem": problem,
                "rounds": rounds_info.iter().map(|(r, s)| serde_json::json!({"round": r, "summary": s})).collect::<Vec<_>>(),
                "current_state": verifier_findings,
                "recommended_steps": ceo_recommendation,
            })
            .to_string();
        }

        let mut out = String::new();
        out.push_str(&format!("\n{}\n", "=".repeat(60).red()));
        out.push_str(&format!("  {}\n", "ESCALATION REQUIRED".bold().red()));
        out.push_str(&format!("{}\n", "=".repeat(60).red()));
        out.push_str(&format!("  I attempted to resolve: {}\n", problem));
        out.push_str(&format!(
            "  Rounds attempted: {}/{}\n\n",
            rounds_info.len(),
            rounds_info.len()
        ));

        for (round, summary) in rounds_info {
            out.push_str(&format!("  Round {}: {}\n", round, summary));
        }

        out.push_str(&format!(
            "\n  {}:\n  {}\n",
            "Current system state".bold(),
            verifier_findings
        ));
        out.push_str(&format!(
            "\n  {}:\n  {}\n",
            "Recommended manual steps".bold(),
            ceo_recommendation
        ));
        out.push_str(&format!("{}\n", "=".repeat(60).red()));
        out
    }

    pub fn format_alert(&self, check_name: &str, message: &str, severity: &str) -> String {
        if self.json_mode {
            return serde_json::json!({
                "alert": true,
                "check": check_name,
                "message": message,
                "severity": severity,
            })
            .to_string();
        }

        let icon = match severity {
            "critical" => "!!!".red().bold().to_string(),
            "warning" => "!".yellow().bold().to_string(),
            _ => "i".dimmed().to_string(),
        };
        format!("  {} [{}] {}", icon, check_name, message)
    }

    pub fn format_watch_status(&self, healthy: usize, total: usize) -> String {
        if self.json_mode {
            return serde_json::json!({"healthy": healthy, "total": total}).to_string();
        }
        if healthy == total {
            format!("  {} All {} checks healthy", "✓".green(), total)
        } else {
            format!("  {} {}/{} checks healthy", "!".yellow(), healthy, total)
        }
    }
}
