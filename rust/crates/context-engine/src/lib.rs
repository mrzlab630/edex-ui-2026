use core_domain::{HistoryEntry, HistoryEntryId, HistoryEntryKind, SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextQuery {
    pub workspace_id: Option<WorkspaceId>,
    pub session_id: Option<SessionId>,
    pub text: String,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextResult {
    pub history_entry_id: HistoryEntryId,
    pub workspace_id: WorkspaceId,
    pub session_id: Option<SessionId>,
    pub kind: HistoryEntryKind,
    pub at_unix_ms: u64,
    pub score: u32,
    pub preview: String,
}

#[derive(Debug, Clone, Default)]
pub struct ContextEngine {
    entries: HashMap<HistoryEntryId, HistoryEntry>,
    inverted_index: HashMap<String, HashSet<HistoryEntryId>>,
}

impl ContextEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_history(entries: &[HistoryEntry]) -> Self {
        let mut engine = Self::new();
        for entry in entries {
            engine.append(entry.clone());
        }
        engine
    }

    pub fn append(&mut self, entry: HistoryEntry) {
        for token in tokenize(&entry.content) {
            self.inverted_index
                .entry(token)
                .or_default()
                .insert(entry.id);
        }
        self.entries.insert(entry.id, entry);
    }

    pub fn search(&self, query: &ContextQuery) -> Vec<ContextResult> {
        let limit = query.limit.max(1);
        let tokens = tokenize(&query.text);
        if tokens.is_empty() {
            return Vec::new();
        }

        let mut scores: HashMap<HistoryEntryId, u32> = HashMap::new();
        for token in tokens {
            if let Some(entry_ids) = self.inverted_index.get(&token) {
                for entry_id in entry_ids {
                    *scores.entry(*entry_id).or_default() += 1;
                }
            }
        }

        let mut results: Vec<_> = scores
            .into_iter()
            .filter_map(|(entry_id, score)| {
                let entry = self.entries.get(&entry_id)?;
                if query
                    .workspace_id
                    .map(|workspace_id| entry.workspace_id == workspace_id)
                    .unwrap_or(true)
                    && query
                        .session_id
                        .map(|session_id| entry.session_id == Some(session_id))
                        .unwrap_or(true)
                {
                    Some(ContextResult {
                        history_entry_id: entry.id,
                        workspace_id: entry.workspace_id,
                        session_id: entry.session_id,
                        kind: entry.kind,
                        at_unix_ms: entry.at_unix_ms,
                        score,
                        preview: preview(&entry.content),
                    })
                } else {
                    None
                }
            })
            .collect();

        results.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then(right.at_unix_ms.cmp(&left.at_unix_ms))
                .then(right.history_entry_id.cmp(&left.history_entry_id))
        });
        results.truncate(limit);
        results
    }
}

fn tokenize(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric())
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| token.len() >= 2)
        .collect()
}

fn preview(content: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 160;
    let mut chars = content.chars();
    let preview: String = chars.by_ref().take(MAX_PREVIEW_CHARS).collect();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn search_returns_ranked_matching_entries() {
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let entries = vec![
            HistoryEntry::new(
                Uuid::new_v4(),
                workspace_id,
                Some(session_id),
                HistoryEntryKind::TerminalOutput,
                1,
                "cargo test --workspace failed with sqlite error",
            )
            .expect("entry should be valid"),
            HistoryEntry::new(
                Uuid::new_v4(),
                workspace_id,
                Some(session_id),
                HistoryEntryKind::ChatAgent,
                2,
                "Try cargo check first, then rerun tests.",
            )
            .expect("entry should be valid"),
        ];

        let engine = ContextEngine::from_history(&entries);
        let results = engine.search(&ContextQuery {
            workspace_id: Some(workspace_id),
            session_id: Some(session_id),
            text: "cargo test sqlite".into(),
            limit: 10,
        });

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].history_entry_id, entries[0].id);
        assert!(results[0].score >= results[1].score);
    }
}
