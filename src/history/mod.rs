//! Claude conversation history loading and parsing.
//!
//! This module provides functionality for:
//! - Loading conversations from Claude project directories
//! - Parsing JSONL conversation files
//! - Encoding/decoding project directory paths
//!
//! # Module Structure
//!
//! - `loader` - Loading conversations from directories
//! - `parser` - Parsing individual JSONL files
//! - `path` - Path encoding/decoding utilities

pub mod cache;
pub mod loader;
pub mod parser;
pub mod path;
pub mod prune;
mod rename;

use crate::error::{AppError, Result};
use chrono::{DateTime, Local};
use std::path::PathBuf;
use std::time::SystemTime;

// Re-export public API
pub use loader::{
    DeleteEmptyScope, delete_empty_transcripts, delete_session_by_uuid, find_jsonl_by_uuid,
    load_all_conversations, load_all_conversations_streaming,
};
pub(crate) use parser::{
    extract_skill_preview, is_clear_metadata_message, process_conversation_file,
};
pub use path::{convert_path_to_project_dir_name, format_short_name_from_path, is_same_project};
pub use prune::{PruneCandidate, PruneFilter, find_prune_candidates};
pub use rename::append_session_rename;

/// Represents a JSONL parsing error with context for debugging
#[derive(Clone, Debug)]
pub struct ParseError {
    pub line_number: usize,
    pub line_content: String,
    pub error_message: String,
    /// Lines before the error (up to 2)
    pub context_before: Vec<String>,
    /// Lines after the error (up to 2)
    pub context_after: Vec<String>,
}

#[derive(Clone)]
pub struct Conversation {
    pub path: PathBuf,
    pub index: usize,
    pub timestamp: DateTime<Local>,
    pub preview: String,
    /// Preview showing first 3 messages (used when show_last=false)
    pub preview_first: String,
    /// Preview showing last 3 messages (used when show_last=true)
    pub preview_last: String,
    pub full_text: String,
    pub agent_search_text: String,
    pub semantic_turns: Vec<String>,
    pub semantic_turn_ranges: Vec<crate::agent::refs::MessageRange>,
    /// Pre-normalized lowercase search text (avoids re-normalizing on every startup)
    pub search_text_lower: String,
    pub project_name: Option<String>,
    pub project_path: Option<PathBuf>,
    /// The working directory extracted from the JSONL file (the actual cwd)
    pub cwd: Option<PathBuf>,
    /// Number of user and assistant messages in the conversation
    pub message_count: usize,
    /// Parse errors encountered while processing this conversation file
    pub parse_errors: Vec<ParseError>,
    /// Summary/title of the conversation (from type=summary JSONL entry)
    pub summary: Option<String>,
    /// Custom session title set by user via /rename (from type=custom-title JSONL entry)
    pub custom_title: Option<String>,
    /// Model name from assistant messages (e.g., "claude-opus-4-5-20251101")
    pub model: Option<String>,
    /// Total tokens used in the conversation (input + output + cache)
    pub total_tokens: u64,
    /// Conversation duration in minutes (from first to last message)
    pub duration_minutes: Option<u64>,
    /// Which AI tool produced this conversation (e.g., "claude", "codex")
    pub provider: Option<String>,
}

impl Conversation {
    /// Extract the raw session ID or thread ID for this conversation
    pub fn session_id(&self) -> &str {
        let file_stem = self.path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

        if let Some(rest) = file_stem.strip_prefix("opencode:") {
            rest
        } else if let Some(rest) = file_stem.strip_prefix("mimo:") {
            rest
        } else if let Some(rest) = file_stem.strip_prefix("rollout-") {
            extract_codex_thread_id(rest)
        } else if file_stem == "transcript" {
            // brain/<uuid>/.system_generated/logs/transcript.jsonl
            let mut parent = self.path.parent();
            while let Some(p) = parent {
                if let Some(grandparent) = p.parent() {
                    if grandparent.file_name().and_then(|n| n.to_str()) == Some("brain") {
                        return p.file_name().and_then(|n| n.to_str()).unwrap_or(file_stem);
                    }
                }
                parent = p.parent();
            }
            file_stem
        } else {
            file_stem
        }
    }

    /// Extract a shortened session ID (first 8 characters) for concise display
    pub fn short_session_id(&self) -> &str {
        let id = self.session_id();
        if id.len() > 8 { &id[..8] } else { id }
    }
}

/// Extract the thread ID from a Codex rollout filename suffix.
///
/// Filenames look like `rollout-2026-10-01T10-15-34-01a0f53e-e31b-7723-8ec4-22ff8cf15443`:
/// the timestamp itself contains dashes, so the full trailing UUID (36 chars)
/// is used instead of the text after the last dash.
fn extract_codex_thread_id(rest: &str) -> &str {
    if rest.len() > 36 {
        let candidate = &rest[rest.len() - 36..];
        if is_uuid_like(candidate) {
            return candidate;
        }
    }
    rest.rsplit_once('-').map(|(_, uuid)| uuid).unwrap_or(rest)
}

fn is_uuid_like(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    for (i, c) in b.iter().enumerate() {
        let expected_dash = matches!(i, 8 | 13 | 18 | 23);
        if expected_dash {
            if *c != b'-' {
                return false;
            }
        } else if !c.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::Conversation;
    use std::path::PathBuf;

    fn conversation_with_path(path: &str) -> Conversation {
        Conversation {
            path: PathBuf::from(path),
            index: 0,
            timestamp: chrono::Local::now(),
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
            message_count: 0,
            parse_errors: Vec::new(),
            summary: None,
            custom_title: None,
            model: None,
            total_tokens: 0,
            duration_minutes: None,
            provider: None,
        }
    }

    #[test]
    fn session_id_extracts_full_codex_thread_id() {
        let conv = conversation_with_path(
            "/home/u/.codex/sessions/2026/10/01/rollout-2026-10-01T10-15-34-01a0f53e-e31b-7723-8ec4-22ff8cf15443.jsonl",
        );
        assert_eq!(conv.session_id(), "01a0f53e-e31b-7723-8ec4-22ff8cf15443");
        assert_eq!(conv.short_session_id(), "01a0f53e");
    }

    #[test]
    fn session_id_handles_sqlite_backed_providers() {
        let conv = conversation_with_path("opencode:ses_ed1507032ffeadwMPKhia9tmjM");
        assert_eq!(conv.session_id(), "ses_ed1507032ffeadwMPKhia9tmjM");

        let conv = conversation_with_path("mimo:ses_abc123");
        assert_eq!(conv.session_id(), "ses_abc123");
    }

    #[test]
    fn session_id_handles_plain_uuid_files() {
        let conv = conversation_with_path(
            "/home/u/.claude/projects/foo/12345678-1234-4234-9234-123456789abc.jsonl",
        );
        assert_eq!(conv.session_id(), "12345678-1234-4234-9234-123456789abc");
    }
}

pub struct Project {
    pub name: String,         // directory name (encoded)
    pub display_name: String, // heuristic decoded path
    pub modified: SystemTime,
}

/// Message sent from background loader to TUI
pub enum LoaderMessage {
    /// A fatal error occurred (e.g., projects root doesn't exist)
    Fatal(AppError),
    /// A non-fatal error occurred (project-level, error already logged)
    ProjectError,
    /// A batch of loaded conversations from one project
    Batch(Vec<Conversation>),
    /// Loading completed
    Done,
}

/// Get the root Claude projects directory (~/.claude/projects)
/// Respects CLAUDE_CONFIG_DIR env variable if set.
pub fn get_claude_projects_root() -> Result<PathBuf> {
    let claude_dir = if let Ok(config_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        PathBuf::from(config_dir)
    } else {
        let home_dir = home::home_dir().ok_or_else(|| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not determine home directory",
            ))
        })?;
        home_dir.join(".claude")
    };

    Ok(claude_dir.join("projects"))
}

/// Get the Claude projects directory for the current working directory
pub fn get_claude_projects_dir(current_dir: &std::path::Path) -> Result<PathBuf> {
    let converted = convert_path_to_project_dir_name(current_dir);
    Ok(get_claude_projects_root()?.join(converted))
}
