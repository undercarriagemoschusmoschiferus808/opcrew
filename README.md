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

## Multi-provider LLM support

opcrew works with multiple LLM providers — not just Claude:

| Provider | Flag | API Key Env Var | Default Model |
|----------|------|-----------------|---------------|
| Claude | `--provider claude` | `ANTHROPIC_API_KEY` | claude-sonnet-4-20250514 |
| OpenAI | `--provider openai` | `OPENAI_API_KEY` | gpt-4o |
| DeepSeek | `--provider deepseek` | `DEEPSEEK_API_KEY` | deepseek-chat |
| Gemini | `--provider gemini` | `GEMINI_API_KEY` | gemini-2.5-flash |
| Local (Ollama) | `--provider local` | None needed | llama3 |

```bash
# Use DeepSeek
export DEEPSEEK_API_KEY=sk-...
opcrew --provider deepseek --problem "nginx 502"

# Use OpenAI with a specific model
export OPENAI_API_KEY=sk-...
opcrew --provider openai --model gpt-4o-mini --problem "disk full"

# Use a local Ollama instance
opcrew --provider local --model codestral --problem "check services"
```

Override the default model for any provider with `--model`.

## Quick start

```bash
# Build
cargo build --release

# Set your API key (pick your provider)
export ANTHROPIC_API_KEY=sk-ant-...    # or OPENAI_API_KEY, DEEPSEEK_API_KEY, etc.

# Solve a problem
opcrew --problem "nginx is returning 502 errors"

# Use a different provider
opcrew --provider deepseek --problem "disk full on /var"

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
| `--provider` | `claude` | LLM provider: `claude`, `openai`, `deepseek`, `gemini`, `local` |
| `--model`, `-m` | per provider | Override the default model for the selected provider |
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
| `ANTHROPIC_API_KEY` | | Claude API key (required for `--provider claude`) |
| `OPENAI_API_KEY` | | OpenAI API key (required for `--provider openai`) |
| `DEEPSEEK_API_KEY` | | DeepSeek API key (required for `--provider deepseek`) |
| `GEMINI_API_KEY` | | Gemini API key (required for `--provider gemini`) |
| `LOCAL_LLM_URL` | `http://localhost:11434/v1/chat/completions` | Ollama/local endpoint |
| `LOCAL_LLM_MODEL` | `llama3` | Default model for local provider |
| `MAX_TOKENS` | `4096` | Override default token limit |
| `API_BASE_URL` | `https://api.anthropic.com` | Claude API endpoint |
| `SESSION_TOKEN_BUDGET` | `500000` | Default session budget |
| `PER_AGENT_TOKEN_BUDGET` | `100000` | Per-agent token limit |
| `PER_AGENT_CONVERSATION_CAP` | `30` | Max conversation turns per agent |
| `LOG_LEVEL` | `info` | Logging level (`debug`, `info`, `warn`, `error`) |

You can also place these in a `.env` file in the working directory.

## Infrastructure discovery

opcrew uses adaptive discovery — it fingerprints your server first, then generates tailored commands based on what's actually installed.

```
Phase 1: Fingerprint (hardcoded probes, no LLM)
  → Detects: OS, Docker, Podman, K8s, nginx, haproxy, Apache, Caddy, etc.

Phase 2: LLM generates commands (up to 30, tailored to your stack)
  → Docker detected? → docker ps, docker inspect, docker network ls
  → K8s detected? → kubectl get pods/svc/ingress
  → nginx detected? → nginx -T (dump config)

Phase 3: Execute with failure classification
  → Success / PermissionDenied / NotFound / Timeout

Phase 4: LLM extracts service graph from all collected data
  → Post-processing resolves any remaining unknowns from raw ss/docker data
```

```bash
# Scan localhost
opcrew infra discover

# Scan with elevated access (retries permission-denied commands with sudo)
opcrew infra discover --sudo

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

Without `--sudo`, permission-denied commands are skipped and gaps are noted. The graph is stored in `~/.opcrew/memory.db` and automatically loaded in future sessions.

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
    provider.rs            LlmProvider trait (multi-provider abstraction)
    client.rs              Claude provider (streaming, rate limiting, retries)
    openai.rs              OpenAI/DeepSeek provider (OpenAI-compatible API)
    gemini.rs              Google Gemini provider
    local.rs               Local LLM provider (Ollama, llama.cpp)
    types.rs               Shared request/response types
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
    graph.rs                Infrastructure graph (services, dependencies, gaps)
    discovery.rs            Adaptive discovery (fingerprint → LLM commands → execute → extract)
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
- An API key for at least one provider: Claude, OpenAI, DeepSeek, or Gemini. Or a local Ollama instance (no key needed).

## License

Apache License 2.0 — see [LICENSE](LICENSE).
