use std::collections::HashMap;
use std::sync::Mutex;

use crate::config::Dialect;
use crate::store::{RateLimitRow, RouteSet, TargetDef};

/// One concrete forwarding attempt (a target + a chosen key), with the reason
/// it was selected — recorded for later analysis.
#[derive(Debug, Clone)]
pub struct Attempt {
    pub provider: String,
    pub base_url: String,
    pub dialect: Dialect,
    pub real_model: String,
    pub token: Option<String>,
    pub inject_usage: bool,
    pub rewrite_model: bool,
    /// primary | load_balance | fallback
    pub reason: &'static str,
}

/// In-memory round-robin counters (shared across requests).
#[derive(Default)]
pub struct LbState {
    per_model: Mutex<HashMap<String, u64>>,
    per_provider: Mutex<HashMap<String, u64>>,
}

impl LbState {
    fn next(map: &Mutex<HashMap<String, u64>>, key: &str) -> u64 {
        let mut m = map.lock().unwrap();
        let c = m.entry(key.to_string()).or_insert(0);
        let v = *c;
        *c = c.wrapping_add(1);
        v
    }
}

fn exhausted(provider: &str, rl: &[RateLimitRow]) -> bool {
    rl.iter().any(|r| {
        r.provider == provider && (r.requests_remaining == Some(0) || r.tokens_remaining == Some(0))
    })
}

/// Build the ordered attempt list for a request (rules-first):
/// priority tiers → rate-limit skip → weighted round-robin within the top tier.
pub fn plan(rs: &RouteSet, model_id: &str, rl: &[RateLimitRow], lb: &LbState) -> Vec<Attempt> {
    // Drop rate-limit-exhausted providers; if that leaves nothing, keep all.
    let mut pool: Vec<&TargetDef> = rs
        .targets
        .iter()
        .filter(|t| !exhausted(&t.provider, rl))
        .collect();
    if pool.is_empty() {
        pool = rs.targets.iter().collect();
    }
    if pool.is_empty() {
        return Vec::new();
    }

    // Group by priority, preserving ascending priority order.
    let mut tiers: Vec<(i64, Vec<&TargetDef>)> = Vec::new();
    for t in &pool {
        match tiers.last_mut() {
            Some((p, v)) if *p == t.priority => v.push(t),
            _ => tiers.push((t.priority, vec![t])),
        }
    }

    let mut attempts = Vec::new();
    for (ti, (_prio, tier)) in tiers.iter().enumerate() {
        // Weighted round-robin: pick a primary within the tier, rest follow as fallback.
        let ordered = weighted_order(model_id, tier, lb);
        for (i, t) in ordered.iter().enumerate() {
            let reason = if ti == 0 && i == 0 {
                if tier.len() > 1 {
                    "load_balance"
                } else {
                    "primary"
                }
            } else {
                "fallback"
            };
            attempts.push(Attempt {
                token: pick_key(&t.provider, &t.keys, lb),
                provider: t.provider.clone(),
                base_url: t.base_url.clone(),
                dialect: t.dialect,
                rewrite_model: t.real_model != model_id,
                real_model: t.real_model.clone(),
                inject_usage: rs.inject_usage,
                reason,
            });
        }
    }
    attempts
}

/// Rotate a tier so the weighted-RR primary comes first; the rest follow in order.
fn weighted_order<'a>(model_id: &str, tier: &[&'a TargetDef], lb: &LbState) -> Vec<&'a TargetDef> {
    if tier.len() <= 1 {
        return tier.to_vec();
    }
    let total: u64 = tier.iter().map(|t| t.weight.max(1) as u64).sum();
    let mut idx = LbState::next(&lb.per_model, model_id) % total.max(1);
    let mut primary = 0usize;
    for (i, t) in tier.iter().enumerate() {
        let w = t.weight.max(1) as u64;
        if idx < w {
            primary = i;
            break;
        }
        idx -= w;
    }
    let mut out = Vec::with_capacity(tier.len());
    out.push(tier[primary]);
    for (i, t) in tier.iter().enumerate() {
        if i != primary {
            out.push(t);
        }
    }
    out
}

fn pick_key(provider: &str, keys: &[String], lb: &LbState) -> Option<String> {
    if keys.is_empty() {
        return None;
    }
    let i = (LbState::next(&lb.per_provider, provider) as usize) % keys.len();
    Some(keys[i].clone())
}

#[cfg(test)]
#[path = "../tests/unit/select.rs"]
mod tests;
