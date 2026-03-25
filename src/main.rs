#![allow(dead_code, unused_imports)]

mod api;
mod cli;
mod config;
mod domain;
mod error;
mod execution;
mod infra;
mod memory;
mod observability;
mod agents;
mod output;
mod safety;
mod tools;
mod watch;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tokio_util::sync::CancellationToken;

use crate::agents::ceo::CeoAgent;
use crate::agents::factory::AgentFactory;
use crate::api::client::ClaudeClient;
use crate::api::gemini::GeminiClient;
use crate::api::local::LocalClient;
use crate::api::openai::OpenAiClient;
use crate::api::provider::LlmProvider;
use crate::cli::{Cli, Command};
use crate::execution::budget::TokenBudget;
use crate::execution::runner::SquadRunner;
use crate::observability::logging::{init_logging, init_logging_pretty, SessionContext};
use crate::observability::metrics::Metrics;
use crate::output::formatter::OutputFormatter;
use crate::safety::audit::{AuditAction, AuditLog};
use crate::safety::guardian::GuardianAgent;
use crate::safety::secrets::SecretMasker;
use crate::tools::code_writer::CodeWriterTool;
use crate::tools::file_ops::FileOpsTool;
use crate::tools::log_reader::LogReaderTool;
use crate::tools::registry::ToolRegistry;
use crate::tools::shell::ShellTool;
use crate::tools::target::TargetHost;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Init logging
    if cli.json {
        init_logging(cli.verbose);
    } else {
        init_logging_pretty(cli.verbose);
    }

    // Handle subcommands that exit early
    match &cli.command {
        Some(Command::Examples) => {
            cli::print_examples();
            return Ok(());
        }
        Some(Command::Infra(action)) => {
            let config = Arc::new(config::Config::from_env()?);
            let client = create_provider(&cli.provider, &config)?;
            let memory = memory::store::MemoryStore::open()?;
            infra::commands::handle_infra_command(action, &memory, &client).await?;
            return Ok(());
        }
        None => {} // Continue to main pipeline
    }

    let session = SessionContext::new();
    tracing::info!(session_id = %session.session_id, "Session started");

    // Read problem
    let problem = cli.read_problem()?;
    tracing::info!(problem_len = problem.len(), "Problem loaded");

    // Load config
    let config = Arc::new(config::Config::from_env()?);
    tracing::info!(model = %config.model, "Configuration loaded");

    // Formatter
    let formatter = OutputFormatter::new(cli.json);

    // Metrics
    let metrics = Arc::new(Metrics::new());

    // Secret masker
    let masker = Arc::new(SecretMasker::new());

    // Audit log
    let audit_path = PathBuf::from("./audit.log");
    let audit_log = Arc::new(AuditLog::new(
        audit_path,
        session.session_id,
        SecretMasker::new(),
        50,
    ));

    // Log session start
    let mut session_entry = audit_log.create_entry(AuditAction::SessionStarted);
    session_entry.result_output = Some(problem.clone());
    audit_log.log(session_entry)?;

    // LLM provider
    let client = create_provider(&cli.provider, &config)?;
    tracing::info!(provider = client.provider_name(), model = client.model_name(), "LLM provider initialized");

    // Token budget
    let budget = Arc::new(TokenBudget::new(
        cli.session_budget / cli.max_agents as u32,
        cli.session_budget,
    ));

    // Tool registry
    let target = cli
        .target
        .as_ref()
        .and_then(|t| TargetHost::parse_target(t))
        .unwrap_or(TargetHost::Local);

    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Arc::new(ShellTool::new(target)));
    tool_registry.register(Arc::new(FileOpsTool::new(None)));
    tool_registry.register(Arc::new(LogReaderTool::new()));
    tool_registry.register(Arc::new(CodeWriterTool::new()));
    let tool_registry = Arc::new(tool_registry);

    // Guardian
    let guardian = Arc::new(GuardianAgent::new(
        Arc::clone(&client),
        Arc::clone(&audit_log),
        cli.max_prompts,
        cli.auto_approve,
    ));

    // Cancellation token + Ctrl+C handler
    let cancellation = CancellationToken::new();
    let cancel_clone = cancellation.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::warn!("Ctrl+C received, shutting down gracefully...");
        cancel_clone.cancel();
    });

    // =========================================================
    // Phase 1: CEO creates plan
    // =========================================================
    if !cli.json {
        println!("\nAgent Creator v{}", env!("CARGO_PKG_VERSION"));
        println!("Session: {}", session.session_id);
        println!(
            "Problem: {}...\n",
            &problem[..problem.len().min(100)]
        );
    }

    tracing::info!("CEO analyzing problem...");
    let ceo = Arc::new(CeoAgent::new(Arc::clone(&client)));
    let plan = ceo.create_plan(&problem).await?;

    // Log plan
    let mut plan_entry = audit_log.create_entry(AuditAction::PlanCreated);
    plan_entry.result_output = Some(serde_json::to_string(&plan)?);
    audit_log.log(plan_entry)?;

    println!("{}", formatter.format_plan(&plan));

    if cli.dry_run {
        println!("{}", formatter.format_dry_run_header());
    }

    // =========================================================
    // Phase 2: Factory creates squad from plan
    // =========================================================
    tracing::info!("Assembling squad...");
    let factory = AgentFactory::new(
        Arc::clone(&client),
        Arc::clone(&tool_registry),
        Arc::clone(&guardian),
        Arc::clone(&budget),
        Arc::clone(&masker),
    );

    let squad = factory.create_squad_from_plan(&plan, cli.max_agents)?;

    if !cli.json {
        println!(
            "{}",
            formatter.format_progress(
                "Squad",
                &format!("{} agents, {} tasks", squad.agent_count(), squad.task_count())
            )
        );
    }

    // =========================================================
    // Phase 3: Runner executes squad
    // =========================================================
    tracing::info!("Executing squad...");
    let runner = SquadRunner::new(Arc::clone(&ceo), cancellation.clone());
    let (outputs, level_summaries) = runner.execute(&squad, cli.dry_run).await?;

    let total_tokens: u32 = outputs.iter().map(|o| o.tokens_used).sum();
    metrics.record_tokens(total_tokens);

    if cli.dry_run {
        for summary in &level_summaries {
            println!("{summary}");
        }
        println!("\n{}", metrics.summary());

        let mut end_entry = audit_log.create_entry(AuditAction::SessionCompleted);
        end_entry.tokens_used = Some(total_tokens);
        audit_log.log(end_entry)?;

        return Ok(());
    }

    // =========================================================
    // Phase 4: CEO synthesizes results
    // =========================================================
    if !outputs.is_empty() {
        tracing::info!("CEO synthesizing results...");
        let synthesis = ceo.synthesize(&problem, &level_summaries).await?;
        println!("{}", formatter.format_final_result(&synthesis, total_tokens));
    } else {
        println!("\nNo results to synthesize.");
    }

    // =========================================================
    // Phase 5: Final summary
    // =========================================================
    if !cli.json {
        println!("\n{}", metrics.summary());
    }

    let mut end_entry = audit_log.create_entry(AuditAction::SessionCompleted);
    end_entry.tokens_used = Some(total_tokens);
    audit_log.log(end_entry)?;

    tracing::info!(tokens = total_tokens, "Session completed");

    Ok(())
}

fn create_provider(
    provider_name: &str,
    config: &Arc<config::Config>,
) -> anyhow::Result<Arc<dyn LlmProvider>> {
    match provider_name {
        "claude" => Ok(Arc::new(ClaudeClient::new(Arc::clone(config)))),
        "openai" => Ok(Arc::new(OpenAiClient::new_openai(config))),
        "deepseek" => Ok(Arc::new(OpenAiClient::new_deepseek(config))),
        "gemini" => Ok(Arc::new(GeminiClient::new(config))),
        "local" => Ok(Arc::new(LocalClient::new(config))),
        other => Err(anyhow::anyhow!(
            "Unknown provider: '{other}'. Use: claude, openai, deepseek, gemini, local"
        )),
    }
}
