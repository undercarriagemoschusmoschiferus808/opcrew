# opcrew

A Rust CLI that assembles AI agent squads to diagnose and fix infrastructure problems. Give it a problem description, and it deploys specialist agents that run shell commands, read logs, edit files, and verify the fix — with a Guardian agent reviewing every command before execution.

Built for developers, devops engineers, and sysadmins.

## How it works

```
Problem (CLI)
  -> CEO Agent (analyzes problem, generates hypotheses, creates plan)
     -> Agent Factory (builds specialist agents with specific tools)
        -> Squad Runner (executes agents concurrently, respects task dependencies)
           -> Guardian Agent (reviews every command: allowlist -> AI review -> user approval)
              -> Tool Execution (shell, file ops, log reader, code writer)
           -> Verifier Agent (checks if the problem is actually resolved)
              -> If not resolved: CEO replans, new squad executes (up to 3 rounds)
        -> CEO Synthesis (combines all results into a final report)
```

## Quick start

```bash
# Build
cargo build --release

# Set your API key
export ANTHROPIC_API_KEY=sk-ant-...

# Solve a problem
opcrew --problem "nginx is returning 502 errors"

# Preview without executing
opcrew --problem "disk full on /var" --dry-run

# See all examples
opcrew examples
```

## CLI reference

| Flag | Default | Description |
|------|---------|-------------|
| `--problem`, `-p` | | Problem description (required unless `--file` or `--watch`) |
| `--file`, `-f` | | Read problem from a file |
| `--model`, `-m` | `claude-sonnet-4-20250514` | Claude model to use |
| `--max-tokens` | `4096` | Token limit per API call |
| `--dry-run` | off | Preview plan and Guardian decisions without executing |
| `--max-agents` | `5` | Maximum specialist agents in the squad |
| `--auto-approve` | off | Skip all approval prompts (Guardian still blocks dangerous ops) |
| `--session-budget` | `500000` | Total token cap for the session |
| `--target` | localhost | Remote host via SSH (`user@host`) |
| `--max-prompts` | `20` | Max user approval prompts before fail-closed |
| `--max-rounds` | `3` | Max verification/replan rounds |
| `--session-timeout` | `600` | Hard wall-clock limit in seconds |
| `--no-memory` | off | Disable persistent memory (past solutions) |
| `--verbose`, `-v` | off | DEBUG-level logging |
| `--json` | off | JSON output for programmatic consumption |
| `--watch` | off | Continuous monitoring mode |
| `--watch-interval` | `60` | Check interval in seconds (watch mode) |
| `--auto-fix` | off | Auto-trigger fixes on anomaly (watch mode) |
| `--watch-config` | | TOML file for watch checks |

Run `opcrew --help` for detailed explanations of each flag.

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `ANTHROPIC_API_KEY` | *required* | Claude API key |
| `CLAUDE_MODEL` | `claude-sonnet-4-20250514` | Override default model |
| `MAX_TOKENS` | `4096` | Override default token limit |
| `API_BASE_URL` | `https://api.anthropic.com` | API endpoint |
| `SESSION_TOKEN_BUDGET` | `500000` | Default session budget |
| `PER_AGENT_TOKEN_BUDGET` | `100000` | Per-agent token limit |
| `PER_AGENT_CONVERSATION_CAP` | `30` | Max conversation turns per agent |
| `LOG_LEVEL` | `info` | Logging level (`debug`, `info`, `warn`, `error`) |

You can also place these in a `.env` file in the working directory.

## Infrastructure discovery

Discover running services, ports, dependencies, and log paths on your infrastructure:

```bash
# Scan localhost
opcrew infra discover

# Scan a remote host
opcrew infra discover --host admin@prod-01

# View the graph
opcrew infra show

# JSON output
opcrew infra show --json

# Clear and re-scan
opcrew infra clear
opcrew infra discover
```

The graph is stored in `~/.opcrew/memory.db` and automatically loaded in future sessions. The CEO and Hypothesis agents use it to understand your infrastructure topology, log locations, and service dependencies.

If the graph is older than 24 hours, a staleness warning is printed.

## Watch mode

Continuously monitor infrastructure health and optionally auto-fix issues:

```bash
# Monitor with config file
opcrew --watch --watch-config checks.toml

# Auto-fix with caution (1 attempt, then escalate)
opcrew --watch --auto-fix --max-rounds 1

# Fast polling
opcrew --watch --watch-interval 15
```

Watch config example (`checks.toml`):

```toml
interval_secs = 30
auto_fix = false

[[checks]]
type = "DiskUsage"
path = "/var"
threshold_pct = 85

[[checks]]
type = "ServiceDown"
service_name = "nginx"
check_cmd = "systemctl is-active nginx"

[[checks]]
type = "MemoryUsage"
threshold_pct = 90

[[checks]]
type = "LogErrorRate"
log_path = "/var/log/app/app.log"
pattern = "ERROR"
max_per_minute = 10

[[checks]]
type = "PortUnreachable"
host = "localhost"
port = 5432

[[checks]]
type = "CustomCommand"
cmd = "curl -sf http://localhost:3000/health"
expected_exit = 0
```

If no config file is provided and an infra graph exists, checks are auto-generated from discovered services.

## Safety model

Every command an agent wants to execute passes through the Guardian agent:

1. **Shell composition block** — commands with `;`, `|`, `&&`, `||`, `` ` ``, `$()` are rejected (agents must issue atomic commands)
2. **Static allowlist** — read-only operations (`ls`, `cat`, `ps`, `df`, `grep`, `git status`, etc.) are auto-approved with zero latency
3. **AI review** — Claude analyzes the command in context, classifies as SAFE/RISKY/BLOCKED
4. **User approval** — risky commands prompt you: `[y]es / [n]o / [a]pprove-all-similar / [b]lock-all-similar`

Additional safety:
- **File path denylist** — writes to `/etc/`, `/boot/`, `/sys/`, `/proc/`, `/dev/`, `/root/` are blocked at the tool level (independent of Guardian)
- **Secret masking** — API keys, passwords, tokens are redacted in audit logs, agent conversations, and output
- **Token budget** — hard cap prevents runaway API costs
- **Prompt limit** — max 20 approval prompts per session (fail-closed)
- **Session timeout** — hard wall-clock limit
- **Circuit breaker** — if Claude API is down, Guardian fails closed (blocks all execution)

## Persistent memory

Sessions are stored in `~/.opcrew/memory.db` (SQLite):

- **Past solutions** — what worked and what failed for similar problems
- **Approach statistics** — success rates per approach per problem type
- **Hypothesis priors** — Bayesian-updated probabilities from real outcomes
- **Infrastructure graph** — discovered services and dependencies

The CEO uses this to avoid repeating failed approaches and to prioritize what's worked before. Use `--no-memory` to disable.

## Architecture

```
src/
  main.rs                  Entry point, pipeline orchestration
  cli.rs                   CLI arguments and subcommands
  config.rs                Config from environment
  error.rs                 Error types (thiserror)
  api/
    client.rs              Claude API client (streaming, rate limiting, retries)
    types.rs               API request/response types
    schema.rs              JSON Schema validation with retry
  domain/
    agent.rs               AgentBehavior trait, AgentConfig, signals
    plan.rs                Plan, PlannedRole, PlannedTask
    squad.rs               Squad (agents + tasks)
    task.rs                Task with dependencies
  agents/
    ceo.rs                 CEO agent (planning, synthesis, chain-of-thought)
    specialist.rs          Specialist agent (autonomous tool-use loop)
    factory.rs             Agent factory (builds squad from plan)
    verifier.rs            Verifier agent (checks resolution, detects regressions)
    hypothesis.rs          Hypothesis agent (pre-analysis, Bayesian priors)
  execution/
    runner.rs              Squad runner (topological sort, JoinSet, cancellation)
    budget.rs              Token budget (atomic, no TOCTOU)
    circuit_breaker.rs     Circuit breaker (fail-closed on Guardian)
  safety/
    guardian.rs             Multi-layer command review
    allowlist.rs            Read-only command allowlist
    approval.rs             User approval flow (rate-limited)
    audit.rs                Audit log (HMAC, rotation, secret masking)
    secrets.rs              Secret detection and masking
  tools/
    shell.rs                Shell command execution (no composition)
    file_ops.rs             File operations (path denylist)
    log_reader.rs           Log file reader/searcher
    code_writer.rs          Source code editor
    registry.rs             Tool registry
    target.rs               Local/remote (SSH) execution
  infra/
    graph.rs                Infrastructure graph (services, dependencies)
    discovery.rs            Auto-discovery agent
    commands.rs             Infra CLI handlers
  watch/
    monitor.rs              Health check types
    trigger.rs              Watch loop and TOML config
  memory/
    store.rs                SQLite persistence
    models.rs               Session, finding, solution records
  output/
    formatter.rs            CLI output formatting
  observability/
    logging.rs              Structured logging
    metrics.rs              Runtime metrics
    export.rs               Audit log export
```

## Requirements

- Rust 1.75+ (uses edition 2024)
- Linux (sandboxing features use Linux-specific APIs)
- Claude API key from [Anthropic](https://console.anthropic.com/)

## License

Apache License 2.0 — see [LICENSE](LICENSE).
