use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "opcrew",
    version,
    about = "AI Agent Squad Creator — solve devops and sysadmin problems with dynamic AI agent squads",
    long_about = "opcrew assembles a squad of AI agents to diagnose and fix infrastructure problems.\n\n\
                  It works like a senior SRE: analyzes the problem, generates hypotheses, deploys \
                  specialist agents to investigate and fix, then verifies the result. A Guardian agent \
                  reviews every command before execution — dangerous operations require your approval.\n\n\
                  Requires ANTHROPIC_API_KEY in environment or .env file.",
    after_long_help = "QUICK START:\n\
                       \n  1. export ANTHROPIC_API_KEY=sk-ant-...\
                       \n  2. opcrew --problem \"nginx is returning 502 errors\"\
                       \n  3. opcrew --problem \"disk full on /var\" --dry-run\
                       \n\n  Run `opcrew examples` for more usage patterns."
)]
pub struct Cli {
    /// The problem to solve
    #[arg(
        short,
        long,
        long_help = "\
        Describe the infrastructure problem you want the agent squad to solve.\n\
        Be specific: include service names, error messages, and what you expect.\n\n\
        Example: --problem \"nginx returns 502 after deploying v2.3\"\n\
        Example: --problem \"PostgreSQL replication lag exceeds 30s on replica-02\""
    )]
    pub problem: Option<String>,

    /// Read problem from a file
    #[arg(
        short = 'f',
        long,
        long_help = "\
        Read the problem description from a text file instead of the command line.\n\
        Useful for complex problems that need multiple lines of context.\n\n\
        Example: --file incident-report.txt"
    )]
    pub file: Option<PathBuf>,

    /// LLM provider
    #[arg(
        long,
        default_value = "claude",
        long_help = "\
        Which LLM provider to use. Each provider requires its own API key env var.\n\n\
        Providers:\n\
        - claude:   Anthropic Claude (ANTHROPIC_API_KEY)\n\
        - openai:   OpenAI GPT (OPENAI_API_KEY)\n\
        - deepseek: DeepSeek (DEEPSEEK_API_KEY)\n\
        - gemini:   Google Gemini (GEMINI_API_KEY)\n\
        - local:    Ollama/llama.cpp (LOCAL_LLM_URL, no key needed)\n\n\
        Example: --provider openai"
    )]
    pub provider: String,

    /// Model name to use
    #[arg(
        short,
        long,
        long_help = "\
        Override the default model for the selected provider.\n\
        If omitted, each provider uses a sensible default:\n\
        - claude: claude-sonnet-4-20250514\n\
        - openai: gpt-4o\n\
        - deepseek: deepseek-chat\n\
        - gemini: gemini-2.5-flash\n\
        - local: llama3\n\n\
        Example: --model gpt-4o-mini"
    )]
    pub model: Option<String>,

    /// Maximum tokens per API call
    #[arg(
        long,
        default_value = "4096",
        long_help = "\
        Token limit for each individual Claude API call. Higher values allow longer \
        responses but cost more. Most tasks work fine with the default.\n\n\
        Example: --max-tokens 8192"
    )]
    pub max_tokens: u32,

    /// Preview the plan without executing any commands
    #[arg(
        long,
        long_help = "\
        Show what the CEO agent would plan and what the Guardian would approve or block, \
        without actually running any commands on your system.\n\n\
        The full pipeline runs (hypothesis generation, CEO planning, Guardian review) \
        but tool execution is skipped. Useful for reviewing the approach before committing.\n\n\
        Example: opcrew --problem \"high CPU usage\" --dry-run"
    )]
    pub dry_run: bool,

    /// Maximum number of agents in a squad
    #[arg(
        long,
        default_value = "5",
        long_help = "\
        Cap the number of specialist agents the CEO can create. More agents means \
        more parallelism but higher API cost. The CEO adapts its plan to this limit.\n\n\
        Example: --max-agents 3"
    )]
    pub max_agents: u8,

    /// Skip all approval prompts and clarity questions
    #[arg(
        long,
        long_help = "\
        Automatically approve all Guardian prompts and skip CEO clarity questions.\n\
        The Guardian still blocks clearly dangerous commands (rm -rf /, DROP DATABASE), \
        but risky operations (service restarts, config changes) proceed without asking.\n\n\
        WARNING: Use only in environments where automated changes are acceptable.\n\n\
        Example: opcrew --problem \"restart crashed workers\" --auto-approve"
    )]
    pub auto_approve: bool,

    /// Session token budget (hard cap)
    #[arg(
        long,
        default_value = "2000000",
        long_help = "\
        Maximum total tokens (input + output) across all API calls in this session.\n\
        When the budget is exhausted, agents wrap up with partial results.\n\
        Each agent gets a proportional share (session_budget / max_agents).\n\n\
        Example: --session-budget 500000"
    )]
    pub session_budget: u32,

    /// Target host for remote execution (user@host)
    #[arg(
        long,
        long_help = "\
        Run shell commands on a remote host via SSH instead of localhost.\n\
        Format: user@hostname (e.g., deploy@prod-01, root@192.168.1.10).\n\
        Requires SSH key-based auth configured for the target host.\n\
        Uses strict host key checking and a 10-second connection timeout.\n\n\
        Example: --target admin@web-server-03"
    )]
    pub target: Option<String>,

    /// Maximum user approval prompts per session
    #[arg(
        long,
        default_value = "20",
        long_help = "\
        Safety limit: if agents request more than this many user approvals, \
        remaining risky operations are blocked (fail-closed). Prevents pathological \
        loops where an agent keeps requesting the same operation.\n\n\
        Example: --max-prompts 10"
    )]
    pub max_prompts: u16,

    /// Enable verbose/debug logging
    #[arg(
        short,
        long,
        long_help = "\
        Show DEBUG-level log output including agent conversations, tool call details, \
        Guardian decisions, and token usage. Default level is INFO.\n\n\
        Example: opcrew --problem \"slow queries\" --verbose"
    )]
    pub verbose: bool,

    /// Output as JSON (for piping to other tools)
    #[arg(
        long,
        long_help = "\
        All output (plan, progress, results, alerts) is formatted as JSON objects, \
        one per line. Logging also switches to structured JSON format.\n\
        Useful for piping to jq, feeding into dashboards, or programmatic consumption.\n\n\
        Example: opcrew --problem \"check health\" --json | jq '.result'"
    )]
    pub json: bool,

    /// Disable persistent memory
    #[arg(
        long,
        long_help = "\
        Skip reading past solutions from the memory database (~/.opcrew/memory.db) \
        and do not save this session's results. Useful for one-off investigations where \
        you don't want past context influencing the approach.\n\n\
        Example: opcrew --problem \"investigate from scratch\" --no-memory"
    )]
    pub no_memory: bool,

    /// Maximum verification/re-planning rounds
    #[arg(
        long,
        default_value = "3",
        long_help = "\
        After each execution round, a Verifier agent checks if the problem is resolved.\n\
        If not, the CEO replans and a new squad executes. This flag limits the number of \
        attempt rounds. If the problem isn't resolved after max-rounds, an escalation \
        report is printed with recommended manual steps.\n\n\
        Example: --max-rounds 1  (try once, escalate immediately if it fails)"
    )]
    pub max_rounds: u8,

    /// Global session timeout in seconds
    #[arg(
        long,
        default_value = "1800",
        long_help = "\
        Hard wall-clock limit for the entire session. When reached, all agents are \
        cancelled, partial results are saved to memory, and a report is printed.\n\
        Independent of --max-rounds (both are safety nets).\n\n\
        Example: --session-timeout 600  (10 minutes)"
    )]
    pub session_timeout: u64,

    /// Enable watch mode (continuous monitoring)
    #[arg(
        long,
        long_help = "\
        Instead of solving a single problem, continuously monitor infrastructure health.\n\
        Runs checks (disk, memory, services, ports, log error rates) on an interval.\n\
        When an anomaly is detected, prints an alert and optionally triggers the full \
        diagnostic pipeline (with --auto-fix).\n\n\
        Configure checks via --watch-config or let it auto-generate from the infra graph.\n\n\
        Example: opcrew --watch --watch-config checks.toml"
    )]
    pub watch: bool,

    /// Watch check interval in seconds
    #[arg(
        long,
        default_value = "60",
        long_help = "\
        How often to run health checks in watch mode. Lower values detect issues faster \
        but generate more load.\n\n\
        Example: --watch --watch-interval 30"
    )]
    pub watch_interval: u64,

    /// Automatically fix detected issues in watch mode
    #[arg(
        long,
        long_help = "\
        When watch mode detects a critical anomaly, automatically trigger the full \
        diagnostic pipeline (hypothesis -> CEO plan -> squad -> verifier) without \
        prompting. Requires --watch.\n\n\
        WARNING: This means the system will execute commands on your infrastructure \
        without confirmation. Use with --max-rounds 1 for cautious auto-remediation.\n\n\
        Example: opcrew --watch --auto-fix --max-rounds 1"
    )]
    pub auto_fix: bool,

    /// Path to watch config TOML file
    #[arg(
        long,
        long_help = "\
        TOML file defining which health checks to run in watch mode.\n\
        If omitted and an infra graph exists, checks are auto-generated from discovered services.\n\n\
        Example TOML:\n  interval_secs = 30\n  auto_fix = false\n\n  \
        [[checks]]\n  type = \"DiskUsage\"\n  path = \"/var\"\n  threshold_pct = 85\n\n  \
        [[checks]]\n  type = \"ServiceDown\"\n  service_name = \"nginx\"\n  \
        check_cmd = \"systemctl is-active nginx\"\n\n\
        Example: --watch --watch-config /etc/opcrew/checks.toml"
    )]
    pub watch_config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Manage the infrastructure knowledge graph
    #[command(subcommand)]
    Infra(InfraAction),

    /// Show usage examples
    #[command(about = "Show usage examples for common workflows")]
    Examples,
}

#[derive(Subcommand, Debug)]
pub enum InfraAction {
    /// Scan the local machine (or a remote host) and build a service dependency graph
    #[command(long_about = "\
        Runs read-only system commands (systemctl, ss, ps, find) to discover running services,\n\
        listening ports, config files, and Docker containers. Sends the raw output to Claude\n\
        to extract a structured service dependency graph.\n\n\
        The graph is saved to ~/.opcrew/memory.db and automatically loaded in future sessions\n\
        to give the CEO and Hypothesis agents infrastructure context.\n\n\
        Example:\n  opcrew infra discover\n  opcrew infra discover --host admin@prod-01")]
    Discover {
        /// Remote host to scan (user@host). Omit for localhost.
        #[arg(
            long,
            long_help = "\
            SSH target for remote discovery. Requires key-based SSH auth.\n\
            Format: user@hostname\n\n\
            Example: --host deploy@web-01"
        )]
        host: Option<String>,

        /// Retry permission-denied commands with sudo
        #[arg(
            long,
            long_help = "\
            If some commands fail with permission denied, retry them with sudo.\n\
            Without this flag, those commands are skipped and gaps are noted.\n\n\
            Example: opcrew infra discover --sudo"
        )]
        sudo: bool,
    },

    /// Display the current infrastructure graph
    #[command(long_about = "\
        Shows all discovered services, their ports, log paths, and dependencies.\n\
        Warns if the graph is stale (>24 hours old).\n\n\
        Example:\n  opcrew infra show\n  opcrew infra show --json")]
    Show {
        /// Output as JSON instead of formatted text
        #[arg(long)]
        json: bool,
    },

    /// Register a remote host for future discovery
    Add {
        /// Host address (hostname or IP)
        #[arg(long)]
        host: String,
        /// SSH user for the host
        #[arg(long)]
        user: String,
    },

    /// Re-scan a specific service
    Update {
        /// Service name to re-scan (as shown in `infra show`)
        #[arg(long)]
        service: String,
    },

    /// Delete the entire infrastructure graph
    #[command(
        long_about = "Removes all discovered services and dependencies from the database.\nThis cannot be undone — you will need to run `infra discover` again."
    )]
    Clear,
}

impl Cli {
    pub fn read_problem(&self) -> anyhow::Result<String> {
        match (&self.problem, &self.file) {
            (Some(p), _) => Ok(p.clone()),
            (_, Some(f)) => {
                std::fs::read_to_string(f).map_err(|e| anyhow::anyhow!("Failed to read file: {e}"))
            }
            _ => Err(anyhow::anyhow!("Provide a problem via --problem or --file")),
        }
    }
}

pub fn print_examples() {
    println!(
        r#"BASIC USAGE:
  opcrew --problem "nginx is returning 502 errors"
  opcrew --problem "app server crashed, check logs" --verbose
  opcrew -f incident-report.txt

DRY RUN (preview without executing):
  opcrew --problem "disk full on /var" --dry-run
  opcrew --problem "high memory usage" --dry-run --json

REMOTE HOST:
  opcrew --problem "app not responding" --target deploy@prod-01
  opcrew --problem "database slow" --target dba@db-replica-02

WATCH MODE (continuous monitoring):
  opcrew --watch --watch-config checks.toml
  opcrew --watch --auto-fix --watch-interval 30
  opcrew --watch --auto-fix --max-rounds 1  # cautious auto-fix

INFRASTRUCTURE DISCOVERY:
  opcrew infra discover                        # scan localhost
  opcrew infra discover --host admin@web-01    # scan remote host
  opcrew infra show                            # display graph
  opcrew infra show --json                     # JSON output
  opcrew infra clear                           # wipe graph

COST CONTROL:
  opcrew --problem "check health" --session-budget 100000 --max-agents 2
  opcrew --problem "investigate" --max-rounds 1 --session-timeout 120

AUTOMATION:
  opcrew --problem "fix nginx" --auto-approve --json | jq '.result'
  opcrew --problem "restart workers" --auto-approve --no-memory

ENVIRONMENT:
  export ANTHROPIC_API_KEY=sk-ant-...    # required
  export CLAUDE_MODEL=claude-opus-4-20250514  # optional: override model
  export LOG_LEVEL=debug                 # optional: verbose logging
"#
    );
}
