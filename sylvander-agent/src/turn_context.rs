//! Typed, budgeted context composition for one authenticated Agent turn.
//!
//! Static policy is kept separate from retrieved data. Relationship memory and
//! workspace knowledge enter the prompt only through bounded, relevance-based
//! retrieval, and every included item carries content-safe provenance and a
//! digest in the returned manifest.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::tools::memory::{
    Importance, MemoryExecutionContext, MemoryFilter, MemoryStore, MemoryStoreError,
};
use crate::workspace_executor::{
    WorkspaceExecutor, WorkspaceQueryLimits, WorkspaceSearchRequest, WorkspaceTarget,
};

pub const TURN_CONTEXT_SCHEMA_VERSION: u16 = 1;
pub const MAX_TURN_CONTEXT_BYTES: usize = 128 * 1024;
const TOKEN_ESTIMATE_BYTES: usize = 4;
const MAX_RELEVANCE_TERMS: usize = 4;
const MAX_RETRIEVAL_CANDIDATES: usize = 48;

/// Prompt order. A larger value has later, more specific precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TurnContextLayerKind {
    Safety,
    Agent,
    UserProfile,
    RelationshipMemory,
    WorkspaceKnowledge,
    Session,
}

impl TurnContextLayerKind {
    #[must_use]
    pub const fn precedence(self) -> u8 {
        match self {
            Self::Safety => 0,
            Self::Agent => 1,
            Self::UserProfile => 2,
            Self::RelationshipMemory => 3,
            Self::WorkspaceKnowledge => 4,
            Self::Session => 5,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Safety => "safety",
            Self::Agent => "agent",
            Self::UserProfile => "user_profile",
            Self::RelationshipMemory => "relationship_memory",
            Self::WorkspaceKnowledge => "workspace_knowledge",
            Self::Session => "session",
        }
    }
}

/// Hard per-layer limits. Both byte and conservative token estimates must fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnContextBudget {
    pub max_bytes: usize,
    pub max_estimated_tokens: usize,
    pub max_items: usize,
}

impl TurnContextBudget {
    pub const fn new(max_bytes: usize, max_estimated_tokens: usize, max_items: usize) -> Self {
        Self {
            max_bytes,
            max_estimated_tokens,
            max_items,
        }
    }

    fn is_valid(self) -> bool {
        self.max_bytes > 0 && self.max_estimated_tokens > 0 && self.max_items > 0
    }
}

/// Default budgets leave room for all six layers inside the provider limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnContextBudgets {
    pub safety: TurnContextBudget,
    pub agent: TurnContextBudget,
    pub user_profile: TurnContextBudget,
    pub relationship_memory: TurnContextBudget,
    pub workspace_knowledge: TurnContextBudget,
    pub session: TurnContextBudget,
}

impl Default for TurnContextBudgets {
    fn default() -> Self {
        Self {
            safety: TurnContextBudget::new(2 * 1024, 512, 1),
            agent: TurnContextBudget::new(60 * 1024, 15 * 1024, 2),
            user_profile: TurnContextBudget::new(3 * 1024, 768, 1),
            relationship_memory: TurnContextBudget::new(8 * 1024, 2 * 1024, 12),
            workspace_knowledge: TurnContextBudget::new(33 * 1024, 8_448, 24),
            session: TurnContextBudget::new(18 * 1024, 4_608, 1),
        }
    }
}

impl TurnContextBudgets {
    #[must_use]
    pub const fn for_layer(&self, kind: TurnContextLayerKind) -> TurnContextBudget {
        match kind {
            TurnContextLayerKind::Safety => self.safety,
            TurnContextLayerKind::Agent => self.agent,
            TurnContextLayerKind::UserProfile => self.user_profile,
            TurnContextLayerKind::RelationshipMemory => self.relationship_memory,
            TurnContextLayerKind::WorkspaceKnowledge => self.workspace_knowledge,
            TurnContextLayerKind::Session => self.session,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TurnContextSource {
    RuntimeSafety,
    AgentDefinition,
    ModelProfile,
    UserProfile,
    RelationshipMemory,
    WorkspaceInstructions,
    WorkspaceSearch,
    GuardianCurated,
    SessionOverride,
}

impl TurnContextSource {
    const fn label(self) -> &'static str {
        match self {
            Self::RuntimeSafety => "runtime_safety",
            Self::AgentDefinition => "agent_definition",
            Self::ModelProfile => "model_profile",
            Self::UserProfile => "user_profile",
            Self::RelationshipMemory => "relationship_memory",
            Self::WorkspaceInstructions => "workspace_instructions",
            Self::WorkspaceSearch => "workspace_search",
            Self::GuardianCurated => "guardian_curated",
            Self::SessionOverride => "session_override",
        }
    }
}

/// Content-safe source identity attached to every prompt item.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TurnContextProvenance {
    pub source: TurnContextSource,
    pub reference: String,
    pub revision: Option<u64>,
}

impl TurnContextProvenance {
    #[must_use]
    pub fn new(source: TurnContextSource, reference: impl Into<String>) -> Self {
        Self {
            source,
            reference: reference.into(),
            revision: None,
        }
    }

    #[must_use]
    pub const fn with_revision(mut self, revision: u64) -> Self {
        self.revision = Some(revision);
        self
    }
}

/// One authoritative or retrieved input to a typed layer.
#[derive(Clone, PartialEq, Eq)]
pub struct TurnContextCandidate {
    content: String,
    authoritative: bool,
    pub provenance: TurnContextProvenance,
    pub relevance: u32,
    pub expires_at_unix_secs: Option<i64>,
    pub superseded_by: Option<String>,
}

impl TurnContextCandidate {
    #[must_use]
    pub fn authoritative(content: impl Into<String>, provenance: TurnContextProvenance) -> Self {
        Self {
            content: content.into(),
            authoritative: true,
            provenance,
            relevance: u32::MAX,
            expires_at_unix_secs: None,
            superseded_by: None,
        }
    }

    #[must_use]
    pub fn retrieved(
        content: impl Into<String>,
        provenance: TurnContextProvenance,
        relevance: u32,
    ) -> Self {
        Self {
            content: content.into(),
            authoritative: false,
            provenance,
            relevance,
            expires_at_unix_secs: None,
            superseded_by: None,
        }
    }

    #[must_use]
    pub const fn with_expiry(mut self, expires_at_unix_secs: Option<i64>) -> Self {
        self.expires_at_unix_secs = expires_at_unix_secs;
        self
    }

    #[must_use]
    pub fn with_superseded_by(mut self, superseded_by: Option<String>) -> Self {
        self.superseded_by = superseded_by;
        self
    }

    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    fn is_active(&self, now_unix_secs: i64) -> bool {
        self.superseded_by.is_none()
            && self
                .expires_at_unix_secs
                .is_none_or(|expiry| expiry > now_unix_secs)
    }
}

impl fmt::Debug for TurnContextCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TurnContextCandidate")
            .field("content", &"[REDACTED]")
            .field("authoritative", &self.authoritative)
            .field("provenance", &self.provenance)
            .field("relevance", &self.relevance)
            .field("expires_at_unix_secs", &self.expires_at_unix_secs)
            .field("superseded_by", &self.superseded_by)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnContextItemDigest {
    pub provenance: TurnContextProvenance,
    pub sha256: String,
    pub byte_count: usize,
    pub estimated_tokens: usize,
    pub relevance: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnContextLayerManifest {
    pub kind: TurnContextLayerKind,
    pub precedence: u8,
    pub budget: TurnContextBudget,
    pub sha256: String,
    pub byte_count: usize,
    pub estimated_tokens: usize,
    pub included_items: Vec<TurnContextItemDigest>,
    pub omitted_items: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnContextManifest {
    pub schema_version: u16,
    pub layers: Vec<TurnContextLayerManifest>,
    pub aggregate_sha256: String,
    pub total_bytes: usize,
    pub total_estimated_tokens: usize,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ComposedTurnContext {
    system_prompt: String,
    pub manifest: TurnContextManifest,
}

impl ComposedTurnContext {
    #[must_use]
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }
}

impl fmt::Debug for ComposedTurnContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComposedTurnContext")
            .field("system_prompt", &"[REDACTED]")
            .field("manifest", &self.manifest)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TurnContextError {
    #[error("turn context configuration is invalid")]
    InvalidConfiguration,
    #[error("required turn context exceeds its layer budget")]
    RequiredLayerBudgetExceeded,
    #[error("turn context exceeds the provider prompt budget")]
    TotalBudgetExceeded,
    #[error("relationship context is unavailable")]
    RelationshipUnavailable,
    #[error("workspace context is unavailable")]
    WorkspaceUnavailable,
}

/// Inputs grouped by precedence. Required entries fail closed when oversized;
/// retrieved entries are ranked and omitted until they fit.
#[derive(Debug, Clone, Default)]
pub struct TurnContextInputs {
    layers: HashMap<TurnContextLayerKind, LayerInputs>,
}

#[derive(Debug, Clone, Default)]
struct LayerInputs {
    required: Vec<TurnContextCandidate>,
    retrieved: Vec<TurnContextCandidate>,
}

impl TurnContextInputs {
    pub fn push_required(&mut self, kind: TurnContextLayerKind, item: TurnContextCandidate) {
        self.layers.entry(kind).or_default().required.push(item);
    }

    pub fn extend_retrieved(
        &mut self,
        kind: TurnContextLayerKind,
        items: impl IntoIterator<Item = TurnContextCandidate>,
    ) {
        self.layers.entry(kind).or_default().retrieved.extend(items);
    }
}

/// Compose all layers in the canonical precedence order.
pub fn compose_turn_context(
    inputs: TurnContextInputs,
    budgets: &TurnContextBudgets,
    now_unix_secs: i64,
) -> Result<ComposedTurnContext, TurnContextError> {
    let mut rendered_layers = Vec::new();
    let mut manifests = Vec::new();
    for kind in [
        TurnContextLayerKind::Safety,
        TurnContextLayerKind::Agent,
        TurnContextLayerKind::UserProfile,
        TurnContextLayerKind::RelationshipMemory,
        TurnContextLayerKind::WorkspaceKnowledge,
        TurnContextLayerKind::Session,
    ] {
        let budget = budgets.for_layer(kind);
        if !budget.is_valid() {
            return Err(TurnContextError::InvalidConfiguration);
        }
        let layer_inputs = inputs.layers.get(&kind).cloned().unwrap_or_default();
        let (rendered, manifest) = compose_layer(kind, layer_inputs, budget, now_unix_secs)?;
        if !rendered.is_empty() {
            rendered_layers.push(rendered);
            manifests.push(manifest);
        }
    }
    let system_prompt = rendered_layers.join("\n\n");
    if system_prompt.len() > MAX_TURN_CONTEXT_BYTES {
        return Err(TurnContextError::TotalBudgetExceeded);
    }
    let total_estimated_tokens = estimated_tokens(&system_prompt);
    let aggregate_sha256 = manifest_digest(&manifests);
    Ok(ComposedTurnContext {
        manifest: TurnContextManifest {
            schema_version: TURN_CONTEXT_SCHEMA_VERSION,
            total_bytes: system_prompt.len(),
            total_estimated_tokens,
            layers: manifests,
            aggregate_sha256,
        },
        system_prompt,
    })
}

fn compose_layer(
    kind: TurnContextLayerKind,
    mut inputs: LayerInputs,
    budget: TurnContextBudget,
    now_unix_secs: i64,
) -> Result<(String, TurnContextLayerManifest), TurnContextError> {
    let retrieved_count = inputs.retrieved.len();
    inputs
        .retrieved
        .retain(|item| item.is_active(now_unix_secs));
    inputs
        .retrieved
        .retain(|item| valid_content(item.content()));
    let mut omitted = retrieved_count.saturating_sub(inputs.retrieved.len());
    inputs.retrieved.sort_by(|left, right| {
        right
            .relevance
            .cmp(&left.relevance)
            .then_with(|| left.provenance.reference.cmp(&right.provenance.reference))
            .then_with(|| digest(left.content()).cmp(&digest(right.content())))
    });
    let mut selected = Vec::new();
    for item in inputs.required {
        if !valid_content(item.content()) {
            return Err(TurnContextError::InvalidConfiguration);
        }
        if selected.len() == budget.max_items || !fits(&selected, &item, budget, kind) {
            return Err(TurnContextError::RequiredLayerBudgetExceeded);
        }
        selected.push(item);
    }
    for item in inputs.retrieved {
        if item.relevance == 0
            || selected.len() == budget.max_items
            || !fits(&selected, &item, budget, kind)
        {
            omitted += 1;
        } else {
            selected.push(item);
        }
    }
    if selected.is_empty() {
        return Ok((
            String::new(),
            TurnContextLayerManifest {
                kind,
                precedence: kind.precedence(),
                budget,
                sha256: digest(""),
                byte_count: 0,
                estimated_tokens: 0,
                included_items: Vec::new(),
                omitted_items: omitted,
            },
        ));
    }
    let rendered = render_layer(kind, &selected);
    let included_items = selected
        .iter()
        .map(|item| TurnContextItemDigest {
            provenance: item.provenance.clone(),
            sha256: digest(item.content()),
            byte_count: item.content().len(),
            estimated_tokens: estimated_tokens(item.content()),
            relevance: item.relevance,
        })
        .collect();
    Ok((
        rendered.clone(),
        TurnContextLayerManifest {
            kind,
            precedence: kind.precedence(),
            budget,
            sha256: digest(&rendered),
            byte_count: rendered.len(),
            estimated_tokens: estimated_tokens(&rendered),
            included_items,
            omitted_items: omitted,
        },
    ))
}

fn fits(
    selected: &[TurnContextCandidate],
    candidate: &TurnContextCandidate,
    budget: TurnContextBudget,
    kind: TurnContextLayerKind,
) -> bool {
    let mut items = selected.to_vec();
    items.push(candidate.clone());
    let rendered = render_layer(kind, &items);
    rendered.len() <= budget.max_bytes && estimated_tokens(&rendered) <= budget.max_estimated_tokens
}

fn render_layer(kind: TurnContextLayerKind, items: &[TurnContextCandidate]) -> String {
    let mut rendered = format!(
        "[SYLVANDER_CONTEXT_LAYER kind={} precedence={}]\n",
        kind.label(),
        kind.precedence()
    );
    for item in items {
        rendered.push_str(&format!(
            "source={}; reference={}; revision={}; sha256={}\n",
            item.provenance.source.label(),
            encode_reference(&item.provenance.reference),
            item.provenance
                .revision
                .map_or_else(|| "-".to_owned(), |revision| revision.to_string()),
            digest(item.content())
        ));
        if item.authoritative {
            rendered.push_str(item.content());
        } else {
            rendered.push_str("retrieved_content_json=");
            rendered.push_str(
                &serde_json::to_string(item.content()).unwrap_or_else(|_| "\"invalid\"".into()),
            );
        }
        if !rendered.ends_with('\n') {
            rendered.push('\n');
        }
    }
    rendered.push_str("[/SYLVANDER_CONTEXT_LAYER]");
    rendered
}

/// Retrieve only active relationship records that match bounded query terms.
pub async fn retrieve_relationship_context(
    store: &dyn MemoryStore,
    context: &MemoryExecutionContext,
    query: &str,
    budget: TurnContextBudget,
    now_unix_secs: i64,
) -> Result<Vec<TurnContextCandidate>, TurnContextError> {
    let terms = relevance_terms(query);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let per_term = budget.max_items.clamp(1, 12);
    let mut entries = HashMap::new();
    for term in &terms {
        for entry in store
            .search_relationship(
                context,
                term,
                MemoryFilter {
                    limit: Some(per_term),
                    ..MemoryFilter::default()
                },
            )
            .await
            .map_err(map_memory_error)?
        {
            if entry.superseded_by.is_none()
                && entry.expires_at.is_none_or(|expiry| expiry > now_unix_secs)
            {
                entries.entry(entry.id.clone()).or_insert(entry);
            }
        }
    }
    let mut candidates = entries
        .into_values()
        .filter_map(|entry| {
            let relevance = relevance_score(&entry.content, &terms)
                .saturating_add(importance_score(entry.importance));
            (relevance > 0).then(|| {
                TurnContextCandidate::retrieved(
                    entry.content,
                    TurnContextProvenance::new(
                        TurnContextSource::RelationshipMemory,
                        format!("relationship:{}", entry.id),
                    )
                    .with_revision(entry.revision),
                    relevance,
                )
                .with_expiry(entry.expires_at)
                .with_superseded_by(entry.superseded_by)
            })
        })
        .collect::<Vec<_>>();
    rank_and_bound(&mut candidates);
    Ok(candidates)
}

/// Retrieve bounded matching workspace lines through the selected executor.
pub async fn retrieve_workspace_context(
    executor: &dyn WorkspaceExecutor,
    target: &WorkspaceTarget,
    query: &str,
    budget: TurnContextBudget,
) -> Result<Vec<TurnContextCandidate>, TurnContextError> {
    let terms = relevance_terms(query);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut matches = HashMap::new();
    let searches = terms.iter().map(|term| {
        executor.search(
            target,
            WorkspaceSearchRequest {
                relative_path: ".".into(),
                query: term.clone(),
                limits: WorkspaceQueryLimits {
                    max_results: budget.max_items.clamp(1, 16),
                    max_line_chars: 512,
                    max_output_bytes: budget.max_bytes.saturating_mul(2).max(1024),
                    timeout: Duration::from_millis(750),
                },
            },
        )
    });
    for result in futures_util::future::join_all(searches).await {
        let Ok(result) = result else {
            // Workspace knowledge is an optional read path. Missing search
            // support, a timeout, or a temporarily absent workspace omits
            // dynamic candidates without changing tool authorization.
            continue;
        };
        for item in result.matches {
            matches
                .entry((item.relative_path.clone(), item.line_number))
                .or_insert(item);
        }
    }
    let mut candidates = matches
        .into_values()
        .filter_map(|item| {
            let content = format!("{}:{}: {}", item.relative_path, item.line_number, item.line);
            let relevance = relevance_score(&content, &terms);
            (relevance > 0).then(|| {
                TurnContextCandidate::retrieved(
                    content,
                    TurnContextProvenance::new(
                        TurnContextSource::WorkspaceSearch,
                        format!("{}:{}#{}", target.id, item.relative_path, item.line_number),
                    ),
                    relevance,
                )
            })
        })
        .collect::<Vec<_>>();
    rank_and_bound(&mut candidates);
    Ok(candidates)
}

fn rank_and_bound(candidates: &mut Vec<TurnContextCandidate>) {
    candidates.sort_by(|left, right| {
        right
            .relevance
            .cmp(&left.relevance)
            .then_with(|| left.provenance.reference.cmp(&right.provenance.reference))
    });
    candidates.truncate(MAX_RETRIEVAL_CANDIDATES);
}

fn relevance_terms(query: &str) -> Vec<String> {
    let primary = query
        .split(|character: char| {
            !character.is_alphanumeric() && character != '_' && character != '-'
        })
        .map(str::trim)
        .filter(|term| term.chars().count() >= 2)
        .filter(|term| !is_stop_term(term))
        .map(|term| term.chars().take(128).collect::<String>())
        .collect::<Vec<_>>();
    let mut terms = primary.clone();
    for term in primary {
        if term.chars().count() > 2 && term.chars().all(|character| !character.is_ascii()) {
            let characters = term.chars().collect::<Vec<_>>();
            terms.extend(
                characters
                    .windows(2)
                    .map(|pair| pair.iter().collect::<String>()),
            );
        }
    }
    terms.sort_by_key(|term| std::cmp::Reverse(term.chars().count()));
    let mut seen = HashSet::new();
    terms.retain(|term| seen.insert(term.to_lowercase()));
    terms.truncate(MAX_RELEVANCE_TERMS);
    terms
}

fn is_stop_term(term: &str) -> bool {
    matches!(
        term.to_ascii_lowercase().as_str(),
        "a" | "about"
            | "and"
            | "are"
            | "could"
            | "explain"
            | "for"
            | "how"
            | "please"
            | "show"
            | "tell"
            | "the"
            | "this"
            | "what"
            | "would"
            | "you"
            | "your"
    )
}

fn relevance_score(content: &str, terms: &[String]) -> u32 {
    let normalized = content.to_lowercase();
    terms.iter().fold(0_u32, |score, term| {
        if normalized.contains(&term.to_lowercase()) {
            score.saturating_add(u32::try_from(term.chars().count()).unwrap_or(u32::MAX))
        } else {
            score
        }
    })
}

const fn importance_score(importance: Importance) -> u32 {
    match importance {
        Importance::Low => 0,
        Importance::Medium => 1,
        Importance::High => 2,
        Importance::Critical => 3,
    }
}

fn estimated_tokens(value: &str) -> usize {
    let non_ascii = value
        .chars()
        .filter(|character| !character.is_ascii())
        .count();
    let ascii = value.len().saturating_sub(
        value
            .chars()
            .filter(|character| !character.is_ascii())
            .map(char::len_utf8)
            .sum::<usize>(),
    );
    ascii.div_ceil(TOKEN_ESTIMATE_BYTES) + non_ascii
}

fn valid_content(value: &str) -> bool {
    !value
        .chars()
        .any(|character| character <= '\u{1f}' && !matches!(character, '\n' | '\r' | '\t'))
}

fn encode_reference(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"invalid\"".into())
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn manifest_digest(layers: &[TurnContextLayerManifest]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"sylvander.turn-context.v1\0");
    for layer in layers {
        hasher.update([layer.kind.precedence()]);
        hasher.update((layer.byte_count as u64).to_be_bytes());
        hasher.update(layer.sha256.as_bytes());
        for item in &layer.included_items {
            hasher.update(item.sha256.as_bytes());
            hasher.update(item.provenance.reference.as_bytes());
            hasher.update(item.provenance.revision.unwrap_or_default().to_be_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

fn map_memory_error(_error: MemoryStoreError) -> TurnContextError {
    TurnContextError::RelationshipUnavailable
}

#[cfg(test)]
#[path = "../tests/unit/turn_context.rs"]
mod tests;
