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
use std::time::Duration;

use clap::Parser;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::ceo::CeoAgent;
use crate::agents::factory::AgentFactory;
use crate::agents::hypothesis::HypothesisAgent;
use crate::agents::verifier::{VerifierAgent, VerificationResult};
use crate::api::client::ClaudeClient;
use crate::api::gemini::GeminiClient;
use crate::api::local::LocalClient;
use crate::api::openai::OpenAiClient;
use crate::api::provider::LlmProvider;
use crate::cli::{Cli, Command};
use crate::domain::agent::{AgentBehavior, ClarityAssessment};
use crate::execution::budget::TokenBudget;
use crate::execution::runner::SquadRunner;
use crate::infra::graph::InfraGraph;
use crate::memory::models::{FindingRecord, SessionRecord, SolutionRecord};
use crate::memory::store::{problem_hash, MemoryStore};
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
use crate::watch::trigger::{WatchConfig, WatchLoop};

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
            let client = create_provider(&cli.provider, &config, cli.model.clone())?;
            let memory = MemoryStore::open()?;
            infra::commands::handle_infra_command(action, &memory, &client).await?;
            return Ok(());
        }
        None => {}
    }

    // =========================================================
    // Setup
    // =========================================================
    let session = SessionContext::new();
    let session_id = session.session_id.to_string();
    tracing::info!(session_id = %session.session_id, "Session started");

    let config = Arc::new(config::Config::from_env()?);
    let formatter = OutputFormatter::new(cli.json);
    let metrics = Arc::new(Metrics::new());
    let masker = Arc::new(SecretMasker::new());
    let audit_log = Arc::new(AuditLog::new(
        PathBuf::from("./audit.log"),
        session.session_id,
        SecretMasker::new(),
        50,
    ));
    let client = create_provider(&cli.provider, &config, cli.model.clone())?;
    tracing::info!(provider = client.provider_name(), model = client.model_name(), "LLM provider initialized");

    let budget = Arc::new(TokenBudget::new(
        cli.session_budget / cli.max_agents as u32,
        cli.session_budget,
    ));

    let target = cli.target.as_ref()
        .and_then(|t| TargetHost::parse_target(t))
        .unwrap_or(TargetHost::Local);
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Arc::new(ShellTool::new(target)));
    tool_registry.register(Arc::new(FileOpsTool::new(None)));
    tool_registry.register(Arc::new(LogReaderTool::new()));
    tool_registry.register(Arc::new(CodeWriterTool::new()));
    let tool_registry = Arc::new(tool_registry);

    let guardian = Arc::new(GuardianAgent::new(
        Arc::clone(&client),
        Arc::clone(&audit_log),
        cli.max_prompts,
        cli.auto_approve,
    ));

    let cancellation = CancellationToken::new();
    let cancel_clone = cancellation.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::warn!("Ctrl+C received, shutting down gracefully...");
        cancel_clone.cancel();
    });

    // Memory store
    let memory = if cli.no_memory {
        None
    } else {
        match MemoryStore::open() {
            Ok(m) => Some(Arc::new(m)),
            Err(e) => {
                tracing::warn!(error = %e, "Memory store unavailable, continuing without memory");
                None
            }
        }
    };

    // Load infra graph (if exists)
    let infra_context = if let Some(mem) = memory.as_ref() {
        let conn = mem.connection().lock().unwrap();
        match InfraGraph::load_from_db(&conn) {
            Ok(Some(graph)) => {
                if graph.is_stale(24) {
                    eprintln!("⚠ Infra graph is stale. Run `opcrew infra discover` to refresh.");
                }
                graph.to_context_string()
            }
            _ => String::new(),
        }
    } else {
        String::new()
    };

    // =========================================================
    // Watch mode
    // =========================================================
    if cli.watch {
        tracing::info!("Entering watch mode...");
        let watch_config = if let Some(ref path) = cli.watch_config {
            WatchConfig::from_toml(&path.to_string_lossy())?
        } else {
            WatchConfig::default()
        };

        // Create problem channel for auto-fix
        let (problem_tx, mut problem_rx) = tokio::sync::mpsc::channel::<String>(4);
        let watch_loop = WatchLoop::new(watch_config, cancellation.clone(), cli.json)
            .with_problem_sender(problem_tx);

        // Clone deps for the auto-fix handler
        let fix_client = Arc::clone(&client);
        let fix_tools = Arc::clone(&tool_registry);
        let fix_guardian = Arc::clone(&guardian);
        let fix_budget = Arc::clone(&budget);
        let fix_masker = Arc::clone(&masker);
        let fix_metrics = Arc::clone(&metrics);
        let _fix_cancel = cancellation.clone();

        // Spawn auto-fix handler: receives problems from watch, runs fast-path
        tokio::spawn(async move {
            while let Some(problem) = problem_rx.recv().await {
                tracing::info!(problem_len = problem.len(), "Auto-fix triggered by watch mode");
                let fast_config = crate::domain::agent::AgentConfig {
                    id: crate::domain::agent::AgentId::new(),
                    role: "Watch Auto-Fix".into(),
                    expertise: vec!["diagnostics".into(), "remediation".into()],
                    system_prompt: crate::agents::factory::SPECIALIST_SYSTEM_PROMPT.to_string(),
                    goal: "Fix detected issue".into(),
                    allowed_tools: vec!["shell".into(), "file_ops".into(), "log_reader".into()],
                    token_budget: 400_000,
                    max_conversation_turns: 15,
                };
                let agent = crate::agents::specialist::SpecialistAgent::new(
                    fast_config,
                    Arc::clone(&fix_client), Arc::clone(&fix_tools),
                    Arc::clone(&fix_guardian), Arc::clone(&fix_budget),
                    Arc::clone(&fix_masker), Arc::clone(&fix_metrics),
                );
                match crate::domain::agent::AgentBehavior::execute(&agent, &problem).await {
                    Ok(output) => {
                        eprintln!("\n  >>> Auto-fix result:\n  {}\n",
                            &output.content[..output.content.len().min(500)]);
                    }
                    Err(e) => {
                        eprintln!("\n  >>> Auto-fix failed: {e}\n");
                    }
                }
            }
        });

        watch_loop.run().await?;
        return Ok(());
    }

    // Read problem
    let problem = cli.read_problem()?;
    let p_hash = problem_hash(&problem);
    let start_time = std::time::Instant::now();
    tracing::info!(problem_len = problem.len(), "Problem loaded");

    // Log session start
    let mut session_entry = audit_log.create_entry(AuditAction::SessionStarted);
    session_entry.result_output = Some(problem.clone());
    audit_log.log(session_entry)?;

    // Save session to memory
    if let Some(mem) = memory.as_ref() {
        let _ = mem.save_session(&SessionRecord {
            id: session_id.clone(),
            problem_hash: p_hash.clone(),
            problem: problem.clone(),
            outcome: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            duration_secs: None,
        });
    }

    if !cli.json {
        println!("\nopcrew v{}", env!("CARGO_PKG_VERSION"));
        println!("Session: {}", session.session_id);
        println!("Provider: {} ({})", client.provider_name(), client.model_name());
        println!("Problem: {}...\n", &problem[..problem.len().min(100)]);
    }

    // =========================================================
    // Session timeout wrapper
    // =========================================================
    let timeout_duration = Duration::from_secs(cli.session_timeout);
    let ctx = PipelineCtx {
        client: Arc::clone(&client),
        budget: Arc::clone(&budget),
        tool_registry: Arc::clone(&tool_registry),
        guardian: Arc::clone(&guardian),
        masker: Arc::clone(&masker),
        audit_log: Arc::clone(&audit_log),
        cancellation: cancellation.clone(),
        metrics: Arc::clone(&metrics),
        memory: memory.clone(),
    };
    let pipeline_result = tokio::time::timeout(
        timeout_duration,
        run_pipeline(&cli, &problem, &p_hash, &session_id, &ctx, &formatter, &infra_context),
    )
    .await;

    match pipeline_result {
        Ok(result) => result?,
        Err(_) => {
            eprintln!("\n⚠ Session timeout ({} seconds). Partial results saved.", cli.session_timeout);
            if let Some(mem) = memory.as_ref() {
                let _ = mem.update_outcome(&session_id, "timeout", start_time.elapsed().as_secs() as i64);
            }
        }
    }

    // Final metrics
    if !cli.json {
        println!("\n{}", metrics.summary());
    }

    let mut end_entry = audit_log.create_entry(AuditAction::SessionCompleted);
    end_entry.tokens_used = Some(metrics.total_tokens());
    audit_log.log(end_entry)?;

    tracing::info!(tokens = metrics.total_tokens(), "Session completed");
    Ok(())
}

/// All shared state for the pipeline.
#[allow(clippy::too_many_arguments)]
struct PipelineCtx {
    client: Arc<dyn LlmProvider>,
    budget: Arc<TokenBudget>,
    tool_registry: Arc<ToolRegistry>,
    guardian: Arc<GuardianAgent>,
    masker: Arc<SecretMasker>,
    audit_log: Arc<AuditLog>,
    cancellation: CancellationToken,
    metrics: Arc<Metrics>,
    memory: Option<Arc<MemoryStore>>,
}

/// The main pipeline: clarity → hypothesis → CEO plan → squad → verify → replan loop → memory save
async fn run_pipeline(
    cli: &Cli,
    problem: &str,
    p_hash: &str,
    session_id: &str,
    ctx: &PipelineCtx,
    formatter: &OutputFormatter,
    infra_context: &str,
) -> anyhow::Result<()> {
    let client = &ctx.client;
    let budget = &ctx.budget;
    let tool_registry = &ctx.tool_registry;
    let guardian = &ctx.guardian;
    let masker = &ctx.masker;
    let audit_log = &ctx.audit_log;
    let cancellation = &ctx.cancellation;
    let metrics = &ctx.metrics;
    let memory = &ctx.memory;

    // =========================================================
    // TURBO: Pre-fetch + Triage (before any LLM call)
    // =========================================================
    if !cli.dry_run {
        use crate::execution::prefetch::prefetch_system_context;
        use crate::execution::triage::triage;
        use crate::tools::traits::Tool;

        // Load infra graph for context-aware pre-fetch
        let infra_graph = if let Some(mem) = memory.as_ref() {
            let conn = mem.connection().lock().unwrap();
            crate::infra::graph::InfraGraph::load_from_db(&conn).ok().flatten()
        } else {
            None
        };

        // Determine target
        let target = cli.target.as_ref()
            .and_then(|t| crate::tools::target::TargetHost::parse_target(t))
            .unwrap_or_default();

        // Phase T1: Pre-fetch system context
        if !cli.json {
            eprintln!("  {} Collecting system context...", colored::Colorize::dimmed("⚡"));
        }
        let system_context = prefetch_system_context(problem, &target, infra_graph.as_ref()).await;
        if !cli.json {
            eprintln!("  {} {} data points in {}ms",
                colored::Colorize::green("⚡"),
                system_context.data.len(),
                system_context.fetch_duration_ms);
        }

        // Phase T2: Triage — single LLM call with all context
        if !system_context.data.is_empty() {
            if !cli.json {
                eprintln!("  {} Analyzing...", colored::Colorize::dimmed("⚡"));
            }
            match triage(client, problem, &system_context).await {
                Ok(result) => {
                    if !cli.json {
                        eprintln!("  {} Triage: confidence {:.0}% — {}",
                            if result.is_confident() { colored::Colorize::green("⚡") } else { colored::Colorize::yellow("⚡") },
                            result.confidence * 100.0,
                            &result.root_cause[..result.root_cause.len().min(80)]);
                    }

                    // Phase T3: If confident, execute fix directly
                    if result.is_confident() && cli.auto_approve {
                        tracing::info!(confidence = result.confidence, "Turbo: applying fix directly");

                        let shell = crate::tools::shell::ShellTool::new(target.clone());
                        let mut all_ok = true;

                        for cmd in &result.fix_commands {
                            if crate::tools::shell::ShellTool::has_composition(cmd) {
                                tracing::warn!(cmd = %cmd, "Skipping: shell composition");
                                continue;
                            }

                            // Guardian review
                            let tool_params = crate::tools::traits::ToolParams {
                                tool_name: "shell".into(),
                                action: "run".into(),
                                args: [("command".into(), cmd.clone())].into(),
                            };
                            let decision = guardian.review(&tool_params, "Turbo Fix", "turbo", &result.diagnostic).await?;

                            match decision {
                                crate::safety::guardian::ReviewDecision::Approved { .. } => {
                                    metrics.record_guardian_approval();
                                    if !cli.json {
                                        eprintln!("  {} {}",
                                            colored::Colorize::cyan("→"),
                                            colored::Colorize::dimmed(cmd.as_str()));
                                    }
                                    let exec_result = shell.execute(&tool_params, std::time::Duration::from_secs(30)).await;
                                    match exec_result {
                                        Ok(r) if r.success => {
                                            tracing::info!(cmd = %cmd, "Fix command succeeded");
                                        }
                                        Ok(r) => {
                                            tracing::warn!(cmd = %cmd, error = ?r.error, "Fix command failed");
                                            all_ok = false;
                                        }
                                        Err(e) => {
                                            tracing::warn!(cmd = %cmd, error = %e, "Fix command error");
                                            all_ok = false;
                                        }
                                    }
                                }
                                _ => {
                                    metrics.record_guardian_block();
                                    tracing::warn!(cmd = %cmd, "Guardian blocked fix command");
                                    all_ok = false;
                                }
                            }
                        }

                        // Phase T4: Verify
                        let mut verified = false;
                        for cmd in &result.verify_commands {
                            if crate::tools::shell::ShellTool::has_composition(cmd) { continue; }
                            let params = crate::tools::traits::ToolParams {
                                tool_name: "shell".into(),
                                action: "run".into(),
                                args: [("command".into(), cmd.clone())].into(),
                            };
                            if let Ok(r) = shell.execute(&params, std::time::Duration::from_secs(15)).await
                                && r.success {
                                    if !cli.json {
                                        eprintln!("  {} Verify: {}",
                                            colored::Colorize::green("✓"),
                                            &r.output[..r.output.len().min(100)].trim());
                                    }
                                    verified = true;
                                }
                        }

                        // Report
                        let report = format!(
                            "## Turbo Diagnostic\n\n\
                             **Root cause**: {}\n\n\
                             **Diagnostic**: {}\n\n\
                             **Fix applied**: {}\n\n\
                             **Verified**: {}",
                            result.root_cause,
                            result.diagnostic,
                            result.fix_commands.join(", "),
                            if verified { "Yes" } else { "Partial" },
                        );
                        println!("{}", formatter.format_final_result(&report, metrics.total_tokens()));

                        // Save to memory
                        if let Some(mem) = memory.as_ref() {
                            let _ = mem.save_finding(&FindingRecord {
                                id: Uuid::new_v4().to_string(),
                                session_id: session_id.to_string(),
                                agent_role: "Turbo".into(),
                                finding: result.diagnostic.clone(),
                                created_at: chrono::Utc::now().to_rfc3339(),
                            });
                            if all_ok && verified {
                                let _ = mem.save_solution(&SolutionRecord {
                                    id: Uuid::new_v4().to_string(),
                                    session_id: session_id.to_string(),
                                    problem_hash: p_hash.to_string(),
                                    solution: result.diagnostic,
                                    commands: result.fix_commands.join("; "),
                                    worked: true,
                                    failure_reason: None,
                                    approach_summary: result.root_cause,
                                    created_at: chrono::Utc::now().to_rfc3339(),
                                });
                            }
                        }

                        return Ok(());
                    }

                    // If not confident enough, fall through to full pipeline
                    // but the triage context will be available
                    tracing::info!("Triage confidence too low, falling through to full pipeline");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Triage failed, falling through to full pipeline");
                }
            }
        }
    }

    // =========================================================
    // Phase 0: CEO clarity assessment
    // =========================================================
    if !cli.auto_approve && !cli.dry_run {
        tracing::info!("CEO assessing problem clarity...");
        let clarity_prompt = format!(
            "Given this problem, can you create a concrete plan?\n\
             If YES: respond with {{\"clear\": \"true\", \"reasoning\": \"...\"}}\n\
             If NO: respond with {{\"clear\": \"false\", \"questions\": [\"...\"], \"reasoning\": \"...\"}}\n\
             Respond NO if the problem is vague, missing critical details, or could mean multiple things.\n\n\
             Problem: {problem}"
        );
        let messages = vec![crate::api::types::ChatMessage {
            role: crate::api::types::MessageRole::User,
            content: clarity_prompt,
        }];
        if let Ok((response, _)) = client.send_message(
            "You evaluate whether a problem description is clear enough to create an action plan. Be concise.",
            &messages,
        ).await {
            let assessment = ClarityAssessment::parse(&response);
            if let ClarityAssessment::NeedsClarification { questions, reasoning } = assessment {
                println!("\n{}", formatter.format_progress("CEO", "Needs more information before planning:"));
                println!("  Reason: {reasoning}");
                for (i, q) in questions.iter().enumerate() {
                    println!("  {}. {q}", i + 1);
                }
                print!("\nYour answers (or press Enter to continue anyway): ");
                use std::io::Write;
                std::io::stdout().flush().ok();
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer).ok();
                let answer = answer.trim();
                if !answer.is_empty() {
                    // Append answer to problem for the rest of the pipeline
                    let enriched = format!("{problem}\n\nAdditional context: {answer}");
                    return Box::pin(run_pipeline(
                        cli, &enriched, p_hash, session_id, ctx, formatter, infra_context,
                    )).await;
                }
            }
        }
    }

    // =========================================================
    // Phase 1: Hypothesis generation
    // =========================================================
    let hypothesis_report = {
        let hypothesis_agent = HypothesisAgent::new(Arc::clone(client));

        // Get memory context
        let memory_context = if let Some(mem) = memory.as_ref() {
            let solutions = mem.find_similar_solutions(p_hash).unwrap_or_default();
            let _failed = mem.find_failed_approaches(p_hash).unwrap_or_default();
            let stats = mem.get_approach_stats(p_hash).unwrap_or_default();
            let mut ctx = String::new();
            for s in &solutions {
                let status = if s.worked { "WORKED" } else { "FAILED" };
                ctx.push_str(&format!("- [{status}] {}: {}\n", s.approach_summary, s.solution));
            }
            if !stats.is_empty() {
                ctx.push_str("\nApproach success rates:\n");
                for s in &stats {
                    ctx.push_str(&format!("- \"{}\": {:.0}% ({}/{})\n",
                        s.approach, s.success_rate() * 100.0, s.times_succeeded, s.total_tries()));
                }
            }
            ctx
        } else {
            String::new()
        };

        // Get Bayesian priors
        let priors = if let Some(mem) = memory.as_ref() {
            mem.get_hypothesis_priors(p_hash).unwrap_or_default()
        } else {
            Vec::new()
        };

        tracing::info!("Generating hypotheses...");
        match hypothesis_agent.generate(problem, &memory_context, infra_context, &priors).await {
            Ok(report) => {
                if !cli.json {
                    println!("{}", formatter.format_progress("Hypothesis", &format!(
                        "{} hypotheses generated (complexity: {:?})",
                        report.hypotheses.len(), report.estimated_complexity
                    )));
                }
                Some(report)
            }
            Err(e) => {
                tracing::warn!(error = %e, "Hypothesis generation failed, continuing without");
                None
            }
        }
    };

    let hypothesis_context = hypothesis_report.as_ref()
        .map(HypothesisAgent::format_for_ceo)
        .unwrap_or_default();

    // =========================================================
    // Smart routing: decide fast-path vs full pipeline
    // =========================================================
    let approach_stats = if let Some(mem) = memory.as_ref() {
        mem.get_approach_stats(p_hash).unwrap_or_default()
    } else {
        Vec::new()
    };
    let past_worked = if let Some(mem) = memory.as_ref() {
        mem.find_similar_solutions(p_hash).unwrap_or_default().iter().any(|s| s.worked)
    } else {
        false
    };

    let route = crate::execution::routing::compute_route(
        problem, hypothesis_report.as_ref(), &approach_stats, past_worked,
    );
    tracing::info!(route = %route, "Routing decision");
    if !cli.json {
        println!("{}", formatter.format_progress("Router", &format!("{route}")));
    }

    // Handle memory replay
    if let crate::execution::routing::RouteDecision::MemoryReplay { approach, solution, .. } = &route
        && !cli.dry_run {
            tracing::info!(approach = %approach, "Memory replay: applying known solution");
            if !cli.json {
                println!("{}", formatter.format_progress("Memory", &format!("Replaying known fix: {approach}")));
            }
            // Run a single agent with the known approach
            let replay_prompt = format!(
                "A previous session solved this same problem. Apply the known fix:\n\n\
                 Approach: {approach}\n\
                 Previous result: {solution}\n\n\
                 Problem: {problem}\n\n\
                 Apply the fix and verify it worked."
            );
            let replay_config = crate::domain::agent::AgentConfig {
                id: crate::domain::agent::AgentId::new(),
                role: "Memory Replay".into(),
                expertise: vec!["remediation".into()],
                system_prompt: crate::agents::factory::SPECIALIST_SYSTEM_PROMPT.to_string(),
                goal: "Replay known fix".into(),
                allowed_tools: vec!["shell".into(), "file_ops".into(), "log_reader".into()],
                token_budget: 400_000,
                max_conversation_turns: 15,
            };
            let replay_agent = crate::agents::specialist::SpecialistAgent::new(
                replay_config, Arc::clone(client), Arc::clone(tool_registry),
                Arc::clone(guardian), Arc::clone(budget), Arc::clone(masker), Arc::clone(metrics),
            );
            match replay_agent.execute(&replay_prompt).await {
                Ok(output) => {
                    metrics.record_tokens(output.tokens_used);
                    println!("{}", formatter.format_final_result(&output.content, metrics.total_tokens()));
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Memory replay failed, falling back");
                }
            }
        }

    // Handle fast-path
    if route.is_fast() && !cli.dry_run
        && let Some(report) = &hypothesis_report
            && let Some(top) = report.hypotheses.first() {
                tracing::info!(hypothesis = %top.id, "Fast-path: running direct diagnostic + fix");
                if !cli.json {
                    println!("{}", formatter.format_progress("Fast-path",
                        &format!("Testing {}: {}", top.id, &top.description[..top.description.len().min(60)])));
                }

                let fast_prompt = format!(
                    "Do these steps IN ORDER:\n\
                     1. Run: {} (to confirm the hypothesis)\n\
                     2. If confirmed, apply fix: {}\n\
                     3. Verify the fix worked\n\n\
                     Hypothesis: {}\nProblem: {problem}",
                    top.confirm_by, top.fix_approach, top.description,
                );
                let fast_config = crate::domain::agent::AgentConfig {
                    id: crate::domain::agent::AgentId::new(),
                    role: "Fast Diagnostic".into(),
                    expertise: vec!["diagnostics".into(), "remediation".into()],
                    system_prompt: crate::agents::factory::SPECIALIST_SYSTEM_PROMPT.to_string(),
                    goal: "Confirm hypothesis and apply fix".into(),
                    allowed_tools: vec!["shell".into(), "file_ops".into(), "log_reader".into()],
                    token_budget: 400_000,
                    max_conversation_turns: 20,
                };
                let fast_agent = crate::agents::specialist::SpecialistAgent::new(
                    fast_config, Arc::clone(client), Arc::clone(tool_registry),
                    Arc::clone(guardian), Arc::clone(budget), Arc::clone(masker), Arc::clone(metrics),
                );
                match fast_agent.execute(&fast_prompt).await {
                    Ok(output) => {
                        metrics.record_tokens(output.tokens_used);
                        println!("{}", formatter.format_final_result(&output.content, metrics.total_tokens()));
                        if let Some(mem) = memory.as_ref() {
                            let _ = mem.save_finding(&FindingRecord {
                                id: Uuid::new_v4().to_string(),
                                session_id: session_id.to_string(),
                                agent_role: "Fast Diagnostic".into(),
                                finding: output.content[..output.content.len().min(2000)].to_string(),
                                created_at: chrono::Utc::now().to_rfc3339(),
                            });
                        }
                        return Ok(());
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Fast-path failed, falling back to full pipeline");
                    }
                }
            }

    // =========================================================
    // Phase 2: CEO creates plan
    // =========================================================
    tracing::info!("CEO analyzing problem...");
    let ceo = Arc::new(CeoAgent::new(Arc::clone(client)));

    // Build enriched problem with all context
    let enriched_problem = {
        let mut p = problem.to_string();
        if !hypothesis_context.is_empty() {
            p.push_str(&format!("\n\n{hypothesis_context}"));
        }
        if !infra_context.is_empty() {
            p.push_str(&format!("\n\n{infra_context}"));
        }
        p
    };

    let plan = ceo.create_plan(&enriched_problem).await?;

    let mut plan_entry = audit_log.create_entry(AuditAction::PlanCreated);
    plan_entry.result_output = Some(serde_json::to_string(&plan)?);
    audit_log.log(plan_entry)?;

    println!("{}", formatter.format_plan(&plan));

    if cli.dry_run {
        println!("{}", formatter.format_dry_run_header());
        let factory = AgentFactory::new(
            Arc::clone(client), Arc::clone(tool_registry), Arc::clone(guardian),
            Arc::clone(budget), Arc::clone(masker), Arc::clone(metrics),
        );
        let squad = factory.create_squad_from_plan(&plan, cli.max_agents)?;
        let runner = SquadRunner::new(Arc::clone(&ceo), cancellation.clone());
        let (_, summaries) = runner.execute(&squad, true).await?;
        for s in &summaries { println!("{s}"); }
        return Ok(());
    }

    // =========================================================
    // Phase 3: Execute + Verify + Replan loop
    // =========================================================
    let factory = AgentFactory::new(
        Arc::clone(client), Arc::clone(tool_registry), Arc::clone(guardian),
        Arc::clone(budget), Arc::clone(masker), Arc::clone(metrics),
    );
    let verifier = VerifierAgent::new(
        Arc::clone(client), Arc::clone(tool_registry),
        Arc::clone(guardian), Arc::clone(metrics),
    );
    let runner = SquadRunner::new(Arc::clone(&ceo), cancellation.clone());

    let mut current_plan = plan;
    let mut all_summaries: Vec<String> = Vec::new();
    let mut rounds_info: Vec<(u8, String)> = Vec::new();
    let mut resolved = false;

    for round in 1..=cli.max_rounds {
        tracing::info!(round, max = cli.max_rounds, "Execution round");

        let squad = factory.create_squad_from_plan(&current_plan, cli.max_agents)?;
        if !cli.json {
            println!("{}", formatter.format_progress("Squad",
                &format!("Round {round}: {} agents, {} tasks", squad.agent_count(), squad.task_count())));
        }

        let (outputs, level_summaries) = runner.execute(&squad, false).await?;
        let round_tokens: u32 = outputs.iter().map(|o| o.tokens_used).sum();
        metrics.record_tokens(round_tokens);

        all_summaries.extend(level_summaries.clone());

        // Save findings to memory
        if let Some(mem) = memory.as_ref() {
            for output in &outputs {
                let _ = mem.save_finding(&FindingRecord {
                    id: Uuid::new_v4().to_string(),
                    session_id: session_id.to_string(),
                    agent_role: output.role.clone(),
                    finding: output.content[..output.content.len().min(2000)].to_string(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                });
            }
        }

        // Verify
        tracing::info!("Verifying result...");
        let verification = verifier.verify(problem, &outputs, &level_summaries).await?;
        let round_summary = format!("{:?}", &verification);
        rounds_info.push((round, round_summary.clone()));

        match &verification {
            VerificationResult::Resolved { confidence, evidence } => {
                tracing::info!(confidence, "Problem resolved!");
                if !cli.json {
                    println!("{}", formatter.format_progress("Verifier",
                        &format!("RESOLVED (confidence: {:.0}%)", confidence * 100.0)));
                }
                resolved = true;

                // Save to memory
                if let Some(mem) = memory.as_ref() {
                    let _ = mem.save_solution(&SolutionRecord {
                        id: Uuid::new_v4().to_string(),
                        session_id: session_id.to_string(),
                        problem_hash: p_hash.to_string(),
                        solution: evidence.clone(),
                        commands: String::new(),
                        worked: true,
                        failure_reason: None,
                        approach_summary: current_plan.analysis.clone(),
                        created_at: chrono::Utc::now().to_rfc3339(),
                    });
                    let _ = mem.update_approach_outcome(p_hash, &current_plan.analysis, true);
                }
                break;
            }
            VerificationResult::PartiallyResolved { what_remains, .. }
            | VerificationResult::Failed { reason: what_remains, .. } => {
                tracing::warn!(round, "Not fully resolved, replanning...");
                if !cli.json {
                    let label = if matches!(verification, VerificationResult::PartiallyResolved { .. }) {
                        "PARTIALLY RESOLVED"
                    } else {
                        "FAILED"
                    };
                    println!("{}", formatter.format_progress("Verifier",
                        &format!("{label} — {}", &what_remains[..what_remains.len().min(100)])));
                }

                if round < cli.max_rounds {
                    // CEO replans
                    tracing::info!("CEO replanning...");
                    let replan_context = format!(
                        "Previous attempt summary:\n{}\n\nVerification result: {:?}\n\nDO NOT repeat the same approach.",
                        level_summaries.join("\n"), verification
                    );
                    let replan_problem = format!("{problem}\n\n{replan_context}");
                    current_plan = ceo.create_plan(&replan_problem).await?;
                    println!("{}", formatter.format_plan(&current_plan));

                    if let Some(mem) = memory.as_ref() {
                        let _ = mem.update_approach_outcome(p_hash, &current_plan.analysis, false);
                    }
                }
            }
            VerificationResult::Regressed { original_fixed, new_issue, .. } => {
                tracing::warn!(round, "Regression detected!");
                if !cli.json {
                    println!("{}", formatter.format_progress("Verifier",
                        &format!("REGRESSED — fixed: {}, but caused: {}",
                            &original_fixed[..original_fixed.len().min(50)],
                            &new_issue[..new_issue.len().min(50)])));
                }
                if round < cli.max_rounds {
                    let replan_problem = format!(
                        "{problem}\n\nREGRESSION: The previous fix resolved the original issue but caused: {new_issue}\n\
                         Fix the regression WITHOUT reverting the original fix."
                    );
                    current_plan = ceo.create_plan(&replan_problem).await?;
                    println!("{}", formatter.format_plan(&current_plan));
                }
            }
            VerificationResult::Inconclusive { reason } => {
                tracing::warn!(round, reason = %reason, "Verification inconclusive");
                if !cli.json {
                    println!("{}", formatter.format_progress("Verifier", "INCONCLUSIVE"));
                }
                break;
            }
        }
    }

    // =========================================================
    // Phase 4: Synthesis or Escalation
    // =========================================================
    if resolved {
        tracing::info!("CEO synthesizing final report...");
        let synthesis = ceo.synthesize(problem, &all_summaries).await?;
        println!("{}", formatter.format_final_result(&synthesis, metrics.total_tokens()));

        if let Some(mem) = memory.as_ref() {
            let _ = mem.update_outcome(session_id, "resolved", 0);
        }
    } else {
        // Escalation
        let ceo_recommendation = match ceo.synthesize(problem, &all_summaries).await {
            Ok(s) => s,
            Err(_) => "Unable to generate recommendations.".into(),
        };
        let verifier_summary = rounds_info.last()
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| "No verification data".into());

        println!("{}", formatter.format_escalation(
            problem, &rounds_info, &verifier_summary, &ceo_recommendation,
        ));

        if let Some(mem) = memory.as_ref() {
            let _ = mem.save_solution(&SolutionRecord {
                id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                problem_hash: p_hash.to_string(),
                solution: ceo_recommendation,
                commands: String::new(),
                worked: false,
                failure_reason: Some(verifier_summary),
                approach_summary: current_plan.analysis,
                created_at: chrono::Utc::now().to_rfc3339(),
            });
            let _ = mem.update_outcome(session_id, "escalated", 0);
        }
    }

    Ok(())
}

fn create_provider(
    provider_name: &str,
    config: &Arc<config::Config>,
    model_override: Option<String>,
) -> anyhow::Result<Arc<dyn LlmProvider>> {
    let provider: Arc<dyn LlmProvider> = match provider_name {
        "claude" => Arc::new(ClaudeClient::new(Arc::clone(config))),
        "openai" => Arc::new(OpenAiClient::new_openai(config, model_override)?),
        "deepseek" => Arc::new(OpenAiClient::new_deepseek(config, model_override)?),
        "gemini" => Arc::new(GeminiClient::new(config, model_override)?),
        "local" => Arc::new(LocalClient::new(config, model_override)),
        other => return Err(anyhow::anyhow!(
            "Unknown provider: '{other}'. Use: claude, openai, deepseek, gemini, local"
        )),
    };
    Ok(provider)
}
