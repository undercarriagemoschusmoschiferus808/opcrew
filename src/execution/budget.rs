use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::error::{AgentError, Result};

/// Token budget tracker with atomic operations to prevent TOCTOU races.
///
/// Each agent gets 85% of its budget for work, 15% reserved for internal ops
/// (conversation summarization, wrap-up messages).
pub struct TokenBudget {
    per_agent_limit: u32,
    session_limit: u32,
    /// 85% of per_agent_limit — the work budget
    per_agent_work_limit: u32,
    agent_usage: RwLock<HashMap<String, AtomicU32>>,
    session_usage: AtomicU32,
}

const WORK_BUDGET_RATIO: f32 = 0.85;

impl TokenBudget {
    pub fn new(per_agent_limit: u32, session_limit: u32) -> Self {
        let per_agent_work_limit = (per_agent_limit as f32 * WORK_BUDGET_RATIO) as u32;
        Self {
            per_agent_limit,
            session_limit,
            per_agent_work_limit,
            agent_usage: RwLock::new(HashMap::new()),
            session_usage: AtomicU32::new(0),
        }
    }

    /// Reserve tokens BEFORE an API call. Returns error if budget exceeded.
    /// Uses atomic fetch_add — no TOCTOU window.
    pub fn try_consume(
        &self,
        agent_id: &str,
        estimated_tokens: u32,
        is_internal: bool,
    ) -> Result<()> {
        let limit = if is_internal {
            self.per_agent_limit // internal ops use full budget
        } else {
            self.per_agent_work_limit // regular work uses 85%
        };

        // Check session budget first
        let prev_session = self
            .session_usage
            .fetch_add(estimated_tokens, Ordering::SeqCst);
        if prev_session + estimated_tokens > self.session_limit {
            self.session_usage
                .fetch_sub(estimated_tokens, Ordering::SeqCst);
            return Err(AgentError::BudgetExceeded {
                agent_role: "session".into(),
                limit: self.session_limit,
            });
        }

        // Check per-agent budget
        let usage_map = self.agent_usage.read().unwrap();
        if let Some(counter) = usage_map.get(agent_id) {
            let prev = counter.fetch_add(estimated_tokens, Ordering::SeqCst);
            if prev + estimated_tokens > limit {
                counter.fetch_sub(estimated_tokens, Ordering::SeqCst);
                self.session_usage
                    .fetch_sub(estimated_tokens, Ordering::SeqCst);
                return Err(AgentError::BudgetExceeded {
                    agent_role: agent_id.into(),
                    limit,
                });
            }
        }
        // If agent not registered, session budget was already checked — allow it

        Ok(())
    }

    /// Adjust after actual usage is known (estimated vs actual).
    pub fn adjust_actual(&self, agent_id: &str, estimated: u32, actual: u32) {
        let diff = estimated as i64 - actual as i64;
        if diff > 0 {
            // Over-estimated: return tokens
            let return_amount = diff as u32;
            self.session_usage
                .fetch_sub(return_amount, Ordering::SeqCst);
            let usage_map = self.agent_usage.read().unwrap();
            if let Some(counter) = usage_map.get(agent_id) {
                counter.fetch_sub(return_amount, Ordering::SeqCst);
            }
        } else if diff < 0 {
            // Under-estimated: consume more
            let extra = (-diff) as u32;
            self.session_usage.fetch_add(extra, Ordering::SeqCst);
            let usage_map = self.agent_usage.read().unwrap();
            if let Some(counter) = usage_map.get(agent_id) {
                counter.fetch_add(extra, Ordering::SeqCst);
            }
        }
    }

    /// Register an agent for budget tracking.
    pub fn register_agent(&self, agent_id: &str) {
        let mut usage_map = self.agent_usage.write().unwrap();
        usage_map
            .entry(agent_id.to_string())
            .or_insert_with(|| AtomicU32::new(0));
    }

    pub fn remaining_for_agent(&self, agent_id: &str) -> u32 {
        let usage_map = self.agent_usage.read().unwrap();
        if let Some(counter) = usage_map.get(agent_id) {
            self.per_agent_work_limit
                .saturating_sub(counter.load(Ordering::SeqCst))
        } else {
            self.per_agent_work_limit
        }
    }

    pub fn remaining_for_session(&self) -> u32 {
        self.session_limit
            .saturating_sub(self.session_usage.load(Ordering::SeqCst))
    }

    pub fn agent_approaching_limit(&self, agent_id: &str) -> bool {
        self.remaining_for_agent(agent_id) < (self.per_agent_work_limit / 10)
    }

    pub fn session_usage(&self) -> u32 {
        self.session_usage.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_consume_and_adjust() {
        let budget = TokenBudget::new(10000, 50000);
        budget.register_agent("agent1");

        assert!(budget.try_consume("agent1", 1000, false).is_ok());
        assert_eq!(budget.remaining_for_agent("agent1"), 7500); // 8500 - 1000

        budget.adjust_actual("agent1", 1000, 800);
        assert_eq!(budget.remaining_for_agent("agent1"), 7700); // got 200 back
    }

    #[test]
    fn agent_budget_exceeded() {
        let budget = TokenBudget::new(1000, 50000);
        budget.register_agent("agent1");

        // Work limit is 850 (85% of 1000)
        assert!(budget.try_consume("agent1", 800, false).is_ok());
        assert!(budget.try_consume("agent1", 100, false).is_err()); // 800 + 100 > 850
    }

    #[test]
    fn internal_ops_use_full_budget() {
        let budget = TokenBudget::new(1000, 50000);
        budget.register_agent("agent1");

        assert!(budget.try_consume("agent1", 800, false).is_ok());
        // Internal op can use remaining 200 (up to 1000 total)
        assert!(budget.try_consume("agent1", 150, true).is_ok());
    }

    #[test]
    fn session_budget_exceeded() {
        let budget = TokenBudget::new(100000, 500);
        budget.register_agent("agent1");

        assert!(budget.try_consume("agent1", 400, false).is_ok());
        assert!(budget.try_consume("agent1", 200, false).is_err()); // 400 + 200 > 500 session
    }

    #[test]
    fn approaching_limit_detection() {
        let budget = TokenBudget::new(10000, 50000);
        budget.register_agent("agent1");

        assert!(!budget.agent_approaching_limit("agent1"));
        budget.try_consume("agent1", 8000, false).unwrap();
        assert!(budget.agent_approaching_limit("agent1")); // < 10% remaining
    }
}
