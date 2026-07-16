use std::collections::BTreeSet;
use std::fmt::Write;

use rusqlite::params;
use sha2::{Digest, Sha256};

use super::analysis_types::{
    AnalysisPrivacyScope, AnalysisWarning, CohortAnalysis, CohortQuery, CohortTurn, FailureClass,
};
use super::{EvidenceError, EvidenceStore};

type AnalysisRow = (
    String,
    String,
    Option<String>,
    i64,
    Option<i64>,
    String,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    Option<bool>,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    bool,
    i64,
);

impl EvidenceStore {
    pub async fn analyze_cohort(
        &self,
        query: CohortQuery,
    ) -> Result<CohortAnalysis, EvidenceError> {
        if query.limit == 0
            || query.limit > 1000
            || query.started_at_inclusive >= query.started_before_exclusive
            || query.agent_id.as_ref().is_some_and(String::is_empty)
        {
            return Err(EvidenceError::InvalidAnalysisQuery);
        }
        let include_private = matches!(query.privacy_scope, AnalysisPrivacyScope::IncludePrivate);
        let limit = query.limit;
        let mut rows = self
            .run(move |connection| {
                let mut statement = connection
                    .prepare(
                        "WITH step_metrics AS (
                           SELECT turn_id, COUNT(*) AS tools,
                                  SUM(CASE WHEN status='failed' THEN 1 ELSE 0 END) AS failed_tools
                           FROM evidence_steps GROUP BY turn_id
                         ), event_metrics AS (
                           SELECT turn_id,
                             SUM(CASE WHEN event_type='stream_tool_approval_required' THEN 1 ELSE 0 END) AS approvals,
                             SUM(CASE WHEN event_type='system_approve_tool' THEN 1 ELSE 0 END) AS decisions,
                             SUM(CASE WHEN event_type='stream_model_retry' THEN 1 ELSE 0 END) AS retries,
                             SUM(CASE WHEN event_type='stream_interaction_timed_out' THEN 1 ELSE 0 END) AS timeouts
                           FROM evidence_events WHERE turn_id IS NOT NULL GROUP BY turn_id
                         ), feedback_metrics AS (
                           SELECT turn_id,
                             SUM(CASE WHEN rating='positive' THEN 1 ELSE 0 END) AS positive,
                             SUM(CASE WHEN rating='negative' THEN 1 ELSE 0 END) AS negative,
                             COUNT(DISTINCT privacy_class) AS privacy_classes
                           FROM evidence_feedback
                           WHERE turn_id IS NOT NULL
                             AND (?4 OR privacy_class!='private')
                           GROUP BY turn_id
                         )
                         SELECT t.id, t.run_id, t.agent_id, t.started_at, t.ended_at, t.status,
                                t.input_tokens, t.output_tokens, t.cost_nano_usd,
                                t.priced_iteration_count, t.unpriced_iteration_count,
                                COALESCE(s.tools, 0), COALESCE(s.failed_tools, 0),
                                MAX(o.success),
                                COALESCE(e.approvals, 0), COALESCE(e.decisions, 0),
                                COALESCE(e.retries, 0), COALESCE(e.timeouts, 0),
                                COALESCE(f.positive, 0), COALESCE(f.negative, 0),
                                COALESCE(f.privacy_classes, 0),
                                EXISTS(SELECT 1 FROM evidence_feedback rf
                                       WHERE rf.run_id=t.run_id AND rf.turn_id IS NULL
                                         AND (?4 OR rf.privacy_class!='private')),
                                (SELECT COUNT(DISTINCT candidate.agent_id)
                                 FROM evidence_turns candidate
                                 WHERE candidate.started_at>=?1 AND candidate.started_at<?2
                                   AND (?3 IS NULL OR candidate.agent_id=?3))
                         FROM evidence_turns t
                         LEFT JOIN step_metrics s ON s.turn_id=t.id
                         LEFT JOIN event_metrics e ON e.turn_id=t.id
                         LEFT JOIN feedback_metrics f ON f.turn_id=t.id
                         LEFT JOIN evidence_outcomes o ON o.turn_id=t.id
                         WHERE t.started_at>=?1 AND t.started_at<?2
                           AND (?3 IS NULL OR t.agent_id=?3)
                         GROUP BY t.id
                         ORDER BY t.started_at ASC, t.id ASC
                         LIMIT ?5",
                    )
                    .map_err(EvidenceError::sqlite)?;
                let mapped = statement
                    .query_map(
                        params![
                            query.started_at_inclusive,
                            query.started_before_exclusive,
                            query.agent_id,
                            include_private,
                            i64::from(limit) + 1
                        ],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                                row.get(6)?,
                                row.get(7)?,
                                row.get(8)?,
                                row.get(9)?,
                                row.get(10)?,
                                row.get(11)?,
                                row.get(12)?,
                                row.get(13)?,
                                row.get(14)?,
                                row.get(15)?,
                                row.get(16)?,
                                row.get(17)?,
                                row.get(18)?,
                                row.get(19)?,
                                row.get(20)?,
                                row.get(21)?,
                                row.get(22)?,
                            ))
                        },
                    )
                    .map_err(EvidenceError::sqlite)?;
                mapped
                    .collect::<Result<Vec<AnalysisRow>, _>>()
                    .map_err(EvidenceError::sqlite)
            })
            .await?;
        let truncated = rows.len() > usize::from(limit);
        if truncated {
            rows.pop();
        }
        analyze(rows, truncated)
    }
}

fn analyze(rows: Vec<AnalysisRow>, truncated: bool) -> Result<CohortAnalysis, EvidenceError> {
    let mut warnings = BTreeSet::new();
    if truncated {
        warnings.insert(AnalysisWarning::LimitReached);
    }
    let mut turns = Vec::with_capacity(rows.len());
    let mut digest = Sha256::new();
    let mut totals = Totals::default();
    for row in rows {
        if row.22 > 1 {
            warnings.insert(AnalysisWarning::MixedAgents);
        }
        if row.20 > 1 {
            warnings.insert(AnalysisWarning::MixedFeedbackPrivacy);
        }
        if row.21 {
            warnings.insert(AnalysisWarning::RunLevelFeedbackExcluded);
        }
        let turn = decode_turn(row)?;
        totals.add(&turn)?;
        let mut digest_line = String::new();
        let _ = writeln!(
            digest_line,
            "{}|{}|{:?}|{}|{:?}|{}|{:?}|{}|{}|{:?}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            turn.id,
            turn.run_id,
            turn.agent_id,
            turn.started_at,
            turn.ended_at,
            turn.status,
            turn.successful_outcome,
            turn.input_tokens,
            turn.output_tokens,
            turn.cost_nano_usd,
            turn.iteration_count,
            turn.tool_count,
            turn.failed_tool_count,
            turn.approval_request_count,
            turn.approval_decision_count,
            turn.retry_count,
            turn.timeout_count,
            turn.positive_feedback_count,
            turn.negative_feedback_count
        );
        digest.update(digest_line.as_bytes());
        turns.push(turn);
    }
    Ok(totals.finish(turns, digest, warnings))
}

fn decode_turn(row: AnalysisRow) -> Result<CohortTurn, EvidenceError> {
    let number = |value| u64::try_from(value).map_err(|_| EvidenceError::InvalidAnalysisData);
    let priced = number(row.9)?;
    let unpriced = number(row.10)?;
    let cost = number(row.8)?;
    let latency_secs = row
        .4
        .map(|ended| {
            ended
                .checked_sub(row.3)
                .ok_or(EvidenceError::InvalidAnalysisData)
        })
        .transpose()?
        .map(number)
        .transpose()?;
    let failed_tools = number(row.12)?;
    let timeouts = number(row.17)?;
    let negative = number(row.19)?;
    let failure_class = if row.13 == Some(true) && negative == 0 {
        FailureClass::None
    } else if negative > 0 {
        FailureClass::UserReported
    } else if failed_tools > 0 {
        FailureClass::Tool
    } else if timeouts > 0 {
        FailureClass::InteractionTimeout
    } else if row.5 == "interrupted" {
        FailureClass::Interrupted
    } else if row.13 == Some(false) || row.5 == "failed" {
        FailureClass::RuntimeOrModel
    } else {
        FailureClass::Incomplete
    };
    Ok(CohortTurn {
        id: row.0,
        run_id: row.1,
        agent_id: row.2,
        started_at: row.3,
        ended_at: row.4,
        latency_secs,
        status: row.5,
        successful_outcome: row.13,
        failure_class,
        input_tokens: number(row.6)?,
        output_tokens: number(row.7)?,
        iteration_count: priced + unpriced,
        cost_nano_usd: (priced > 0 && unpriced == 0).then_some(cost),
        tool_count: number(row.11)?,
        failed_tool_count: failed_tools,
        approval_request_count: number(row.14)?,
        approval_decision_count: number(row.15)?,
        retry_count: number(row.16)?,
        timeout_count: timeouts,
        positive_feedback_count: number(row.18)?,
        negative_feedback_count: negative,
    })
}

struct Totals {
    succeeded: u64,
    failed: u64,
    incomplete: u64,
    input_tokens: u64,
    output_tokens: u64,
    cost: u64,
    fully_priced: bool,
    has_iterations: bool,
    tools: u64,
    failed_tools: u64,
    approvals: u64,
    decisions: u64,
    retries: u64,
    timeouts: u64,
    positive: u64,
    negative: u64,
    feedback_turns: u64,
    failure_breakdown: super::analysis_types::FailureBreakdown,
    latencies: Vec<u64>,
}

impl Default for Totals {
    fn default() -> Self {
        Self {
            succeeded: 0,
            failed: 0,
            incomplete: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost: 0,
            fully_priced: true,
            has_iterations: false,
            tools: 0,
            failed_tools: 0,
            approvals: 0,
            decisions: 0,
            retries: 0,
            timeouts: 0,
            positive: 0,
            negative: 0,
            feedback_turns: 0,
            failure_breakdown: super::analysis_types::FailureBreakdown::default(),
            latencies: Vec::new(),
        }
    }
}

impl Totals {
    fn add(&mut self, turn: &CohortTurn) -> Result<(), EvidenceError> {
        match turn.successful_outcome {
            Some(true) => self.succeeded += 1,
            Some(false) => self.failed += 1,
            None => self.incomplete += 1,
        }
        match turn.failure_class {
            FailureClass::None => {}
            FailureClass::UserReported => self.failure_breakdown.user_reported += 1,
            FailureClass::Tool => self.failure_breakdown.tool += 1,
            FailureClass::InteractionTimeout => self.failure_breakdown.interaction_timeout += 1,
            FailureClass::Interrupted => self.failure_breakdown.interrupted += 1,
            FailureClass::RuntimeOrModel => self.failure_breakdown.runtime_or_model += 1,
            FailureClass::Incomplete => self.failure_breakdown.incomplete += 1,
        }
        if let Some(latency) = turn.latency_secs {
            self.latencies.push(latency);
        }
        self.input_tokens = checked(self.input_tokens, turn.input_tokens)?;
        self.output_tokens = checked(self.output_tokens, turn.output_tokens)?;
        if turn.iteration_count > 0 {
            self.has_iterations = true;
            if let Some(cost) = turn.cost_nano_usd {
                self.cost = checked(self.cost, cost)?;
            } else {
                self.fully_priced = false;
            }
        }
        self.tools = checked(self.tools, turn.tool_count)?;
        self.failed_tools = checked(self.failed_tools, turn.failed_tool_count)?;
        self.approvals = checked(self.approvals, turn.approval_request_count)?;
        self.decisions = checked(self.decisions, turn.approval_decision_count)?;
        self.retries = checked(self.retries, turn.retry_count)?;
        self.timeouts = checked(self.timeouts, turn.timeout_count)?;
        self.positive = checked(self.positive, turn.positive_feedback_count)?;
        self.negative = checked(self.negative, turn.negative_feedback_count)?;
        if turn.positive_feedback_count + turn.negative_feedback_count > 0 {
            self.feedback_turns += 1;
        }
        Ok(())
    }

    fn finish(
        mut self,
        turns: Vec<CohortTurn>,
        digest: Sha256,
        mut warnings: BTreeSet<AnalysisWarning>,
    ) -> CohortAnalysis {
        if self.incomplete > 0 {
            warnings.insert(AnalysisWarning::IncompleteOutcomes);
        }
        if self.has_iterations && !self.fully_priced {
            warnings.insert(AnalysisWarning::IncompletePricing);
        }
        if self.feedback_turns < turns.len() as u64 {
            warnings.insert(AnalysisWarning::SparseFeedback);
        }
        let terminal = self.succeeded + self.failed;
        self.latencies.sort_unstable();
        let latency_sum = self.latencies.iter().copied().map(u128::from).sum::<u128>();
        let latency_count = self.latencies.len() as u64;
        CohortAnalysis {
            cohort_digest_sha256: hex_digest(&digest.finalize()),
            success_rate_basis_points: rate(self.succeeded, terminal),
            positive_feedback_rate_basis_points: rate(self.positive, self.positive + self.negative),
            turns,
            succeeded_turns: self.succeeded,
            failed_turns: self.failed,
            incomplete_turns: self.incomplete,
            failure_breakdown: self.failure_breakdown,
            latency_sample_count: latency_count,
            mean_latency_secs: (latency_count > 0)
                .then_some((latency_sum / u128::from(latency_count.max(1))) as u64),
            p50_latency_secs: percentile(&self.latencies, 50),
            p95_latency_secs: percentile(&self.latencies, 95),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            fully_priced_cost_nano_usd: (self.has_iterations && self.fully_priced)
                .then_some(self.cost),
            tool_count: self.tools,
            failed_tool_count: self.failed_tools,
            approval_request_count: self.approvals,
            approval_decision_count: self.decisions,
            retry_count: self.retries,
            timeout_count: self.timeouts,
            positive_feedback_count: self.positive,
            negative_feedback_count: self.negative,
            warnings: warnings.into_iter().collect(),
        }
    }
}

fn checked(left: u64, right: u64) -> Result<u64, EvidenceError> {
    left.checked_add(right).ok_or(EvidenceError::CountTooLarge)
}

fn rate(numerator: u64, denominator: u64) -> Option<u16> {
    (denominator > 0).then(|| ((u128::from(numerator) * 10_000) / u128::from(denominator)) as u16)
}

fn percentile(sorted: &[u64], percentile: usize) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (percentile * sorted.len()).div_ceil(100);
    sorted.get(rank.saturating_sub(1)).copied()
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests;
