use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{AgentError, Result};
use crate::memory::models::*;

pub struct MemoryStore {
    conn: Arc<Mutex<Connection>>,
}

impl MemoryStore {
    pub fn open() -> Result<Self> {
        let db_dir = dirs_db_path();
        if let Some(parent) = db_dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AgentError::MemoryError(format!("Failed to create dir: {e}"))
            })?;
        }

        let conn = Connection::open(&db_dir)
            .map_err(|e| AgentError::MemoryError(format!("DB open: {e}")))?;

        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.run_migrations()?;
        Ok(store)
    }

    /// Open an in-memory database (for testing).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| AgentError::MemoryError(format!("DB open: {e}")))?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.run_migrations()?;
        Ok(store)
    }

    fn run_migrations(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(MIGRATIONS)
            .map_err(|e| AgentError::MemoryError(format!("Migrations: {e}")))?;

        // Safe column additions (ignore if already exists)
        let _ = conn.execute("ALTER TABLE infra_services ADD COLUMN execution_context TEXT NOT NULL DEFAULT '{}'", []);

        Ok(())
    }

    // --- Sessions ---

    pub fn save_session(&self, session: &SessionRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO sessions (id, problem_hash, problem, outcome, created_at, duration_secs) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![session.id, session.problem_hash, session.problem, session.outcome, session.created_at, session.duration_secs],
        ).map_err(|e| AgentError::MemoryError(format!("Save session: {e}")))?;
        Ok(())
    }

    pub fn update_outcome(&self, session_id: &str, outcome: &str, duration_secs: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET outcome = ?1, duration_secs = ?2 WHERE id = ?3",
            rusqlite::params![outcome, duration_secs, session_id],
        ).map_err(|e| AgentError::MemoryError(format!("Update outcome: {e}")))?;
        Ok(())
    }

    pub fn find_recent_sessions(&self, limit: usize) -> Result<Vec<SessionRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, problem_hash, problem, outcome, created_at, duration_secs FROM sessions ORDER BY created_at DESC LIMIT ?1"
        ).map_err(|e| AgentError::MemoryError(format!("Prepare: {e}")))?;

        let rows = stmt.query_map(rusqlite::params![limit], |row| {
            Ok(SessionRecord {
                id: row.get(0)?,
                problem_hash: row.get(1)?,
                problem: row.get(2)?,
                outcome: row.get(3)?,
                created_at: row.get(4)?,
                duration_secs: row.get(5)?,
            })
        }).map_err(|e| AgentError::MemoryError(format!("Query: {e}")))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| AgentError::MemoryError(format!("Row: {e}")))?);
        }
        Ok(results)
    }

    // --- Findings ---

    pub fn save_finding(&self, finding: &FindingRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO findings (id, session_id, agent_role, finding, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![finding.id, finding.session_id, finding.agent_role, finding.finding, finding.created_at],
        ).map_err(|e| AgentError::MemoryError(format!("Save finding: {e}")))?;
        Ok(())
    }

    // --- Solutions ---

    pub fn save_solution(&self, solution: &SolutionRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO solutions (id, session_id, problem_hash, solution, commands, worked, failure_reason, approach_summary, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![solution.id, solution.session_id, solution.problem_hash, solution.solution, solution.commands, solution.worked, solution.failure_reason, solution.approach_summary, solution.created_at],
        ).map_err(|e| AgentError::MemoryError(format!("Save solution: {e}")))?;
        Ok(())
    }

    pub fn find_similar_solutions(&self, problem_hash: &str) -> Result<Vec<SolutionRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, problem_hash, solution, commands, worked, failure_reason, approach_summary, created_at FROM solutions WHERE problem_hash = ?1 ORDER BY created_at DESC"
        ).map_err(|e| AgentError::MemoryError(format!("Prepare: {e}")))?;

        let rows = stmt.query_map(rusqlite::params![problem_hash], |row| {
            Ok(SolutionRecord {
                id: row.get(0)?,
                session_id: row.get(1)?,
                problem_hash: row.get(2)?,
                solution: row.get(3)?,
                commands: row.get(4)?,
                worked: row.get(5)?,
                failure_reason: row.get(6)?,
                approach_summary: row.get(7)?,
                created_at: row.get(8)?,
            })
        }).map_err(|e| AgentError::MemoryError(format!("Query: {e}")))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| AgentError::MemoryError(format!("Row: {e}")))?);
        }
        Ok(results)
    }

    pub fn find_failed_approaches(&self, problem_hash: &str) -> Result<Vec<SolutionRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, problem_hash, solution, commands, worked, failure_reason, approach_summary, created_at FROM solutions WHERE problem_hash = ?1 AND worked = 0 ORDER BY created_at DESC"
        ).map_err(|e| AgentError::MemoryError(format!("Prepare: {e}")))?;

        let rows = stmt.query_map(rusqlite::params![problem_hash], |row| {
            Ok(SolutionRecord {
                id: row.get(0)?,
                session_id: row.get(1)?,
                problem_hash: row.get(2)?,
                solution: row.get(3)?,
                commands: row.get(4)?,
                worked: row.get(5)?,
                failure_reason: row.get(6)?,
                approach_summary: row.get(7)?,
                created_at: row.get(8)?,
            })
        }).map_err(|e| AgentError::MemoryError(format!("Query: {e}")))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| AgentError::MemoryError(format!("Row: {e}")))?);
        }
        Ok(results)
    }

    // --- Approach Outcomes ---

    pub fn update_approach_outcome(&self, problem_hash: &str, approach: &str, worked: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339();

        if worked {
            conn.execute(
                "INSERT INTO approach_outcomes (id, problem_hash, approach, times_succeeded, times_failed, last_used) VALUES (?1, ?2, ?3, 1, 0, ?4) ON CONFLICT(problem_hash, approach) DO UPDATE SET times_succeeded = times_succeeded + 1, last_used = ?4",
                rusqlite::params![Uuid::new_v4().to_string(), problem_hash, approach, now],
            )
        } else {
            conn.execute(
                "INSERT INTO approach_outcomes (id, problem_hash, approach, times_succeeded, times_failed, last_used) VALUES (?1, ?2, ?3, 0, 1, ?4) ON CONFLICT(problem_hash, approach) DO UPDATE SET times_failed = times_failed + 1, last_used = ?4",
                rusqlite::params![Uuid::new_v4().to_string(), problem_hash, approach, now],
            )
        }.map_err(|e| AgentError::MemoryError(format!("Update approach: {e}")))?;
        Ok(())
    }

    pub fn get_approach_stats(&self, problem_hash: &str) -> Result<Vec<ApproachOutcome>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT problem_hash, approach, times_succeeded, times_failed FROM approach_outcomes WHERE problem_hash = ?1"
        ).map_err(|e| AgentError::MemoryError(format!("Prepare: {e}")))?;

        let rows = stmt.query_map(rusqlite::params![problem_hash], |row| {
            Ok(ApproachOutcome {
                problem_hash: row.get(0)?,
                approach: row.get(1)?,
                times_succeeded: row.get(2)?,
                times_failed: row.get(3)?,
            })
        }).map_err(|e| AgentError::MemoryError(format!("Query: {e}")))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| AgentError::MemoryError(format!("Row: {e}")))?);
        }
        Ok(results)
    }

    // --- Hypothesis Outcomes (Bayesian) ---

    pub fn update_hypothesis_outcome(&self, problem_hash: &str, category: &str, confirmed: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339();

        if confirmed {
            conn.execute(
                "INSERT INTO hypothesis_outcomes (id, problem_hash, hypothesis_description, hypothesis_category, times_confirmed, times_denied, last_updated) VALUES (?1, ?2, ?3, ?3, 1, 0, ?4) ON CONFLICT(problem_hash, hypothesis_category) DO UPDATE SET times_confirmed = times_confirmed + 1, last_updated = ?4",
                rusqlite::params![Uuid::new_v4().to_string(), problem_hash, category, now],
            )
        } else {
            conn.execute(
                "INSERT INTO hypothesis_outcomes (id, problem_hash, hypothesis_description, hypothesis_category, times_confirmed, times_denied, last_updated) VALUES (?1, ?2, ?3, ?3, 0, 1, ?4) ON CONFLICT(problem_hash, hypothesis_category) DO UPDATE SET times_denied = times_denied + 1, last_updated = ?4",
                rusqlite::params![Uuid::new_v4().to_string(), problem_hash, category, now],
            )
        }.map_err(|e| AgentError::MemoryError(format!("Update hypothesis: {e}")))?;
        Ok(())
    }

    pub fn get_hypothesis_priors(&self, problem_hash: &str) -> Result<Vec<HypothesisOutcome>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT problem_hash, hypothesis_category, times_confirmed, times_denied FROM hypothesis_outcomes WHERE problem_hash = ?1"
        ).map_err(|e| AgentError::MemoryError(format!("Prepare: {e}")))?;

        let rows = stmt.query_map(rusqlite::params![problem_hash], |row| {
            Ok(HypothesisOutcome {
                problem_hash: row.get(0)?,
                hypothesis_category: row.get(1)?,
                times_confirmed: row.get(2)?,
                times_denied: row.get(3)?,
            })
        }).map_err(|e| AgentError::MemoryError(format!("Query: {e}")))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| AgentError::MemoryError(format!("Row: {e}")))?);
        }
        Ok(results)
    }

    pub fn connection(&self) -> &Arc<Mutex<Connection>> {
        &self.conn
    }
}

pub fn problem_hash(problem: &str) -> String {
    let normalized = problem.trim().to_lowercase();
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    hex::encode(hasher.finalize())
}

fn dirs_db_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home)
        .join(".opcrew")
        .join("memory.db")
}

const MIGRATIONS: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    problem_hash TEXT NOT NULL,
    problem TEXT NOT NULL,
    outcome TEXT,
    created_at TEXT NOT NULL,
    duration_secs INTEGER
);

CREATE TABLE IF NOT EXISTS findings (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    agent_role TEXT NOT NULL,
    finding TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id)
);

CREATE TABLE IF NOT EXISTS solutions (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    problem_hash TEXT NOT NULL,
    solution TEXT NOT NULL,
    commands TEXT NOT NULL,
    worked BOOLEAN NOT NULL,
    failure_reason TEXT,
    approach_summary TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id)
);

CREATE INDEX IF NOT EXISTS idx_solutions_problem ON solutions(problem_hash);
CREATE INDEX IF NOT EXISTS idx_solutions_worked ON solutions(worked);

CREATE TABLE IF NOT EXISTS approach_outcomes (
    id TEXT PRIMARY KEY,
    problem_hash TEXT NOT NULL,
    approach TEXT NOT NULL,
    times_succeeded INTEGER DEFAULT 0,
    times_failed INTEGER DEFAULT 0,
    last_used TEXT NOT NULL,
    UNIQUE(problem_hash, approach)
);

CREATE TABLE IF NOT EXISTS hypothesis_outcomes (
    id TEXT PRIMARY KEY,
    problem_hash TEXT NOT NULL,
    hypothesis_description TEXT NOT NULL,
    hypothesis_category TEXT NOT NULL,
    times_confirmed INTEGER DEFAULT 0,
    times_denied INTEGER DEFAULT 0,
    last_updated TEXT NOT NULL,
    UNIQUE(problem_hash, hypothesis_category)
);

CREATE TABLE IF NOT EXISTS infra_services (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    host TEXT NOT NULL,
    port INTEGER,
    process_name TEXT,
    log_paths TEXT NOT NULL DEFAULT '[]',
    config_paths TEXT NOT NULL DEFAULT '[]',
    health_check TEXT,
    service_type TEXT NOT NULL,
    discovered_via TEXT NOT NULL,
    discovered_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    execution_context TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS infra_dependencies (
    id TEXT PRIMARY KEY,
    from_service TEXT NOT NULL,
    to_service TEXT NOT NULL,
    dep_type TEXT NOT NULL,
    discovered_via TEXT NOT NULL,
    -- No FK: infra_services are cleared and re-inserted during discovery
    CHECK(from_service != ''),
    CHECK(to_service != '')
);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_find_sessions() {
        let store = MemoryStore::open_in_memory().unwrap();
        let session = SessionRecord {
            id: Uuid::new_v4().to_string(),
            problem_hash: problem_hash("nginx 502"),
            problem: "nginx 502".into(),
            outcome: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            duration_secs: None,
        };
        store.save_session(&session).unwrap();
        let sessions = store.find_recent_sessions(10).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].problem, "nginx 502");
    }

    fn create_test_session(store: &MemoryStore, id: &str) {
        let session = SessionRecord {
            id: id.into(),
            problem_hash: "test".into(),
            problem: "test".into(),
            outcome: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            duration_secs: None,
        };
        store.save_session(&session).unwrap();
    }

    #[test]
    fn save_and_find_solutions() {
        let store = MemoryStore::open_in_memory().unwrap();
        create_test_session(&store, "s1");
        let hash = problem_hash("nginx 502");
        let solution = SolutionRecord {
            id: Uuid::new_v4().to_string(),
            session_id: "s1".into(),
            problem_hash: hash.clone(),
            solution: "Restarted upstream".into(),
            commands: "systemctl restart app".into(),
            worked: true,
            failure_reason: None,
            approach_summary: "restart upstream".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        store.save_solution(&solution).unwrap();

        let found = store.find_similar_solutions(&hash).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].worked);
    }

    #[test]
    fn find_failed_approaches() {
        let store = MemoryStore::open_in_memory().unwrap();
        create_test_session(&store, "s1");
        create_test_session(&store, "s2");
        let hash = problem_hash("disk full");

        let worked = SolutionRecord {
            id: Uuid::new_v4().to_string(),
            session_id: "s1".into(),
            problem_hash: hash.clone(),
            solution: "Cleaned logs".into(),
            commands: "rm old logs".into(),
            worked: true,
            failure_reason: None,
            approach_summary: "clean logs".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        let failed = SolutionRecord {
            id: Uuid::new_v4().to_string(),
            session_id: "s2".into(),
            problem_hash: hash.clone(),
            solution: "Expanded partition".into(),
            commands: "resize2fs".into(),
            worked: false,
            failure_reason: Some("No free space on disk".into()),
            approach_summary: "expand partition".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        store.save_solution(&worked).unwrap();
        store.save_solution(&failed).unwrap();

        let failures = store.find_failed_approaches(&hash).unwrap();
        assert_eq!(failures.len(), 1);
        assert!(!failures[0].worked);
    }

    #[test]
    fn approach_outcomes_tracking() {
        let store = MemoryStore::open_in_memory().unwrap();
        let hash = problem_hash("nginx 502");

        store.update_approach_outcome(&hash, "restart nginx", true).unwrap();
        store.update_approach_outcome(&hash, "restart nginx", true).unwrap();
        store.update_approach_outcome(&hash, "restart nginx", false).unwrap();

        let stats = store.get_approach_stats(&hash).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].times_succeeded, 2);
        assert_eq!(stats[0].times_failed, 1);
        assert!((stats[0].success_rate() - 0.6667).abs() < 0.01);
    }

    #[test]
    fn hypothesis_bayesian_priors() {
        let store = MemoryStore::open_in_memory().unwrap();
        let hash = problem_hash("nginx 502");

        for _ in 0..8 {
            store.update_hypothesis_outcome(&hash, "upstream_down", true).unwrap();
        }
        for _ in 0..2 {
            store.update_hypothesis_outcome(&hash, "upstream_down", false).unwrap();
        }

        let priors = store.get_hypothesis_priors(&hash).unwrap();
        assert_eq!(priors.len(), 1);
        assert!((priors[0].prior_probability() - 0.8).abs() < 0.01);
    }

    #[test]
    fn empty_db_returns_empty() {
        let store = MemoryStore::open_in_memory().unwrap();
        assert!(store.find_similar_solutions("nonexistent").unwrap().is_empty());
        assert!(store.find_recent_sessions(10).unwrap().is_empty());
        assert!(store.get_approach_stats("nonexistent").unwrap().is_empty());
        assert!(store.get_hypothesis_priors("nonexistent").unwrap().is_empty());
    }

    #[test]
    fn problem_hash_deterministic() {
        let h1 = problem_hash("nginx 502");
        let h2 = problem_hash("  Nginx 502  ");
        assert_eq!(h1, h2); // Normalized: lowercase + trim
    }
}
