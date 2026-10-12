//! Prune candidates: old, small sessions across all providers.
//!
//! A session is a prune candidate when it is BOTH:
//! - older than `older_than_days` (by last-activity timestamp), AND
//! - small (`message_count <= max_messages`).
//!
//! Deletion itself goes through each provider's `delete_session`, so no
//! provider-specific file handling lives here.

use super::Conversation;
use chrono::{DateTime, Local};

#[derive(Debug, Clone)]
pub struct PruneFilter {
    /// Sessions strictly older than this many days are eligible.
    pub older_than_days: u64,
    /// Sessions with at most this many messages are eligible.
    pub max_messages: usize,
    /// If non-empty, only these providers (case-insensitive) are eligible.
    pub providers: Vec<String>,
}

#[derive(Clone)]
pub struct PruneCandidate<'a> {
    pub conversation: &'a Conversation,
    /// Whole days since the session's last activity.
    pub age_days: i64,
}

/// Select prune candidates, oldest first.
pub fn find_prune_candidates<'a>(
    conversations: &'a [Conversation],
    filter: &PruneFilter,
    now: DateTime<Local>,
) -> Vec<PruneCandidate<'a>> {
    let mut candidates: Vec<PruneCandidate<'a>> = conversations
        .iter()
        .filter_map(|conv| {
            if !filter.providers.is_empty() {
                let provider = conv.provider.as_deref().unwrap_or("");
                if !filter
                    .providers
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case(provider))
                {
                    return None;
                }
            }
            let age_days = now.signed_duration_since(conv.timestamp).num_days();
            if age_days <= filter.older_than_days as i64 {
                return None;
            }
            if conv.message_count > filter.max_messages {
                return None;
            }
            Some(PruneCandidate {
                conversation: conv,
                age_days,
            })
        })
        .collect();
    candidates.sort_by(|a, b| {
        b.age_days
            .cmp(&a.age_days)
            .then_with(|| a.conversation.path.cmp(&b.conversation.path))
    });
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn conversation(provider: Option<&str>, age_days: i64, message_count: usize) -> Conversation {
        let timestamp = Local::now()
            - chrono::Duration::from_std(Duration::from_secs((age_days.max(0) as u64) * 24 * 3600))
                .unwrap();
        Conversation {
            path: PathBuf::from(format!(
                "/tmp/{}-{}-{}.jsonl",
                provider.unwrap_or("none"),
                age_days,
                message_count
            )),
            index: 0,
            timestamp,
            preview: String::new(),
            preview_first: String::new(),
            preview_last: String::new(),
            full_text: String::new(),
            agent_search_text: String::new(),
            semantic_turns: Vec::new(),
            semantic_turn_ranges: Vec::new(),
            search_text_lower: String::new(),
            project_name: None,
            project_path: None,
            cwd: None,
            message_count,
            parse_errors: Vec::new(),
            summary: None,
            custom_title: None,
            model: None,
            total_tokens: 0,
            duration_minutes: None,
            provider: provider.map(str::to_string),
        }
    }

    fn filter() -> PruneFilter {
        PruneFilter {
            older_than_days: 30,
            max_messages: 5,
            providers: Vec::new(),
        }
    }

    #[test]
    fn selects_old_and_small_sessions() {
        let convs = vec![conversation(Some("codex"), 45, 3)];
        let found = find_prune_candidates(&convs, &filter(), Local::now());
        assert_eq!(found.len(), 1);
        assert!(found[0].age_days >= 45);
    }

    #[test]
    fn excludes_recent_sessions() {
        let convs = vec![conversation(Some("codex"), 10, 1)];
        assert!(find_prune_candidates(&convs, &filter(), Local::now()).is_empty());
    }

    #[test]
    fn excludes_boundary_age() {
        let convs = vec![conversation(Some("codex"), 30, 1)];
        assert!(find_prune_candidates(&convs, &filter(), Local::now()).is_empty());
    }

    #[test]
    fn excludes_large_sessions() {
        let convs = vec![conversation(Some("opencode"), 90, 50)];
        assert!(find_prune_candidates(&convs, &filter(), Local::now()).is_empty());
    }

    #[test]
    fn provider_filter_is_case_insensitive() {
        let convs = vec![
            conversation(Some("codex"), 60, 2),
            conversation(Some("agy"), 60, 2),
        ];
        let mut f = filter();
        f.providers = vec!["Codex".to_string()];
        let found = find_prune_candidates(&convs, &f, Local::now());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].conversation.provider.as_deref(), Some("codex"));
    }

    #[test]
    fn sorts_oldest_first() {
        let convs = vec![
            conversation(Some("codex"), 40, 1),
            conversation(Some("codex"), 80, 1),
        ];
        let found = find_prune_candidates(&convs, &filter(), Local::now());
        assert_eq!(found.len(), 2);
        assert!(found[0].age_days >= found[1].age_days);
    }
}
