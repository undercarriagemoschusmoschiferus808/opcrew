use crate::agents::hypothesis::{Complexity, HypothesisReport};
use crate::memory::models::ApproachOutcome;
use crate::tools::shell::ShellTool;

/// Routing decision: fast-path (1 agent) or full pipeline (CEO → squad → verifier).
#[derive(Debug)]
pub enum RouteDecision {
    /// Known solution from memory — replay directly, skip everything.
    MemoryReplay {
        approach: String,
        solution: String,
        score: u32,
    },
    /// Simple problem — 1 agent, top hypothesis, confirm + fix.
    FastPath { score: u32, reasons: Vec<String> },
    /// Complex problem — full CEO → squad → verifier pipeline.
    FullPipeline { score: u32, reasons: Vec<String> },
}

impl RouteDecision {
    pub fn is_fast(&self) -> bool {
        matches!(self, Self::FastPath { .. } | Self::MemoryReplay { .. })
    }
}

impl std::fmt::Display for RouteDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MemoryReplay {
                approach, score, ..
            } => {
                write!(f, "MemoryReplay (score: {score}) — replaying: {approach}")
            }
            Self::FastPath { score, reasons } => {
                write!(f, "FastPath (score: {score}) — {}", reasons.join(", "))
            }
            Self::FullPipeline { score, reasons } => {
                write!(f, "FullPipeline (score: {score}) — {}", reasons.join(", "))
            }
        }
    }
}

/// Compute the routing decision from multiple objective signals.
///
/// Signals (scored 0-100, weighted):
/// - LLM complexity (25%): Simple=100, Moderate=40, Complex=0
/// - Top hypothesis confidence (25%): H1.probability * 100
/// - Single service (15%): problem mentions 1 service keyword = 100, 2+ = 40, 0 = 60
/// - Simple confirm command (15%): no pipes/&& = 100, has composition = 0
/// - Memory hit (20%): success rate > 60% = 100, no data = 50
///
/// Score >= 60 → FastPath, else FullPipeline.
pub fn compute_route(
    problem: &str,
    hypothesis_report: Option<&HypothesisReport>,
    approach_stats: &[ApproachOutcome],
    past_worked: bool,
) -> RouteDecision {
    // Memory replay: if a solution worked before with high success rate, replay it
    if past_worked {
        for stat in approach_stats {
            if stat.success_rate() > 0.7 && stat.total_tries() >= 2 {
                return RouteDecision::MemoryReplay {
                    approach: stat.approach.clone(),
                    solution: format!(
                        "Previously worked {}/{} times ({:.0}%)",
                        stat.times_succeeded,
                        stat.total_tries(),
                        stat.success_rate() * 100.0
                    ),
                    score: 95,
                };
            }
        }
    }

    let report = match hypothesis_report {
        Some(r) => r,
        None => {
            return RouteDecision::FullPipeline {
                score: 30,
                reasons: vec!["No hypothesis report available".into()],
            };
        }
    };

    let mut score: f32 = 0.0;
    let mut reasons = Vec::new();

    // Signal 1: LLM complexity (25%)
    let complexity_score = match report.estimated_complexity {
        Complexity::Simple => 100.0,
        Complexity::Moderate => 40.0,
        Complexity::Complex => 0.0,
    };
    score += complexity_score * 0.25;
    reasons.push(format!(
        "complexity={:?}({:.0})",
        report.estimated_complexity, complexity_score
    ));

    // Signal 2: Top hypothesis confidence (25%)
    let confidence_score = report
        .hypotheses
        .first()
        .map(|h| h.probability * 100.0)
        .unwrap_or(30.0);
    score += confidence_score * 0.25;
    reasons.push(format!("H1={:.0}%", confidence_score));

    // Signal 3: Single service detection (15%)
    let service_keywords = count_service_keywords(problem);
    let service_score = match service_keywords {
        0 => 60.0,  // Generic problem, neutral
        1 => 100.0, // Single service — focused
        _ => 40.0,  // Multiple services — complex
    };
    score += service_score * 0.15;
    reasons.push(format!("services={service_keywords}"));

    // Signal 4: Simple confirm command (15%)
    let confirm_score = report
        .hypotheses
        .first()
        .map(|h| {
            if ShellTool::has_composition(&h.confirm_by) {
                0.0 // Needs pipes/&& → complex
            } else {
                100.0 // Single atomic command → simple
            }
        })
        .unwrap_or(50.0);
    score += confirm_score * 0.15;
    if confirm_score < 50.0 {
        reasons.push("complex confirm cmd".into());
    }

    // Signal 5: Memory hit (20%)
    let memory_score = if approach_stats.is_empty() {
        50.0 // No data, neutral
    } else {
        let best = approach_stats
            .iter()
            .map(|s| s.success_rate())
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(0.0);
        if best > 0.6 { 100.0 } else { 30.0 }
    };
    score += memory_score * 0.20;
    if memory_score > 80.0 {
        reasons.push("memory: known approach".into());
    }

    let final_score = score as u32;

    if final_score >= 60 {
        RouteDecision::FastPath {
            score: final_score,
            reasons,
        }
    } else {
        RouteDecision::FullPipeline {
            score: final_score,
            reasons,
        }
    }
}

/// Count how many distinct service-related keywords appear in the problem text.
fn count_service_keywords(problem: &str) -> usize {
    let lower = problem.to_lowercase();
    let keywords = [
        "nginx",
        "apache",
        "haproxy",
        "caddy",
        "traefik",
        "postgres",
        "mysql",
        "redis",
        "mongo",
        "elasticsearch",
        "docker",
        "container",
        "pod",
        "deployment",
        "service",
        "kubelet",
        "node",
        "k8s",
        "kubernetes",
    ];
    keywords.iter().filter(|kw| lower.contains(*kw)).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::hypothesis::{Complexity, Hypothesis, HypothesisReport};

    fn make_report(complexity: Complexity, h1_prob: f32, confirm_by: &str) -> HypothesisReport {
        HypothesisReport {
            hypotheses: vec![Hypothesis {
                id: "H1".into(),
                description: "test".into(),
                probability: h1_prob,
                confirm_by: confirm_by.into(),
                deny_by: "test".into(),
                fix_approach: "test".into(),
                category: "test".into(),
            }],
            recommended_first_checks: vec![],
            estimated_complexity: complexity,
        }
    }

    #[test]
    fn simple_high_confidence_routes_fast() {
        let report = make_report(Complexity::Simple, 0.8, "docker logs myapp");
        let decision = compute_route("container myapp is crashing", Some(&report), &[], false);
        assert!(decision.is_fast());
    }

    #[test]
    fn complex_low_confidence_routes_full() {
        let report = make_report(Complexity::Complex, 0.2, "check multiple services");
        let decision = compute_route(
            "everything is slow across all services",
            Some(&report),
            &[],
            false,
        );
        assert!(!decision.is_fast());
    }

    #[test]
    fn memory_replay_on_known_solution() {
        let stats = vec![ApproachOutcome {
            problem_hash: "h".into(),
            approach: "restart nginx".into(),
            times_succeeded: 5,
            times_failed: 1,
        }];
        let decision = compute_route("nginx 502", None, &stats, true);
        assert!(matches!(decision, RouteDecision::MemoryReplay { .. }));
    }

    #[test]
    fn no_hypothesis_routes_full() {
        let decision = compute_route("something broken", None, &[], false);
        assert!(!decision.is_fast());
    }

    #[test]
    fn service_keyword_count() {
        assert_eq!(count_service_keywords("nginx returns 502"), 1);
        assert_eq!(count_service_keywords("nginx and redis are down"), 2);
        assert_eq!(count_service_keywords("something is slow"), 0);
        assert_eq!(count_service_keywords("docker container postgres"), 3);
    }
}
