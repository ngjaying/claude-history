use crate::cli::DebugLevel;
use crate::error::{AppError, Result};
use crate::history::{Conversation, LoaderMessage, Project};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

use super::{ConversationParser, PathEncoder, Provider, ProviderKind, ResumeAction};

/// MiMo has no project-based path encoding — it uses SQLite.
pub struct MiMoPathEncoder;

impl PathEncoder for MiMoPathEncoder {
    fn encode_project_path(&self, _path: &Path) -> String {
        String::new()
    }

    fn decode_project_name(&self, encoded: &str) -> String {
        encoded.to_string()
    }

    fn project_root<'a>(&self, encoded: &'a str) -> &'a str {
        encoded
    }

    fn is_same_project(&self, _a: &str, _b: &str) -> bool {
        true
    }
}

pub struct MiMoParser;

impl ConversationParser for MiMoParser {
    fn parse_file(
        &self,
        _path: PathBuf,
        _modified: Option<SystemTime>,
        _debug_level: Option<DebugLevel>,
    ) -> Result<Option<Conversation>> {
        Err(AppError::ClaudeExecutionError(
            "MiMo parser uses SQLite, not file-based".to_string(),
        ))
    }

    fn format_name(&self) -> &str {
        "sqlite"
    }
}

pub struct MiMoProvider {
    path_encoder: MiMoPathEncoder,
    parser: MiMoParser,
}

impl MiMoProvider {
    pub fn try_new() -> Result<Self> {
        let db_path = mimocode_db_path()?;
        if !db_path.exists() {
            return Err(AppError::ProjectsDirNotFound(
                db_path.display().to_string(),
            ));
        }
        Ok(Self {
            path_encoder: MiMoPathEncoder,
            parser: MiMoParser,
        })
    }
}

impl Provider for MiMoProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::MiMo
    }

    fn display_name(&self) -> &str {
        "MiMo Code"
    }

    fn data_root(&self) -> Result<PathBuf> {
        let home = home::home_dir().ok_or_else(|| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not determine home directory",
            ))
        })?;
        Ok(home.join(".local").join("share").join("mimocode"))
    }

    fn projects_dir(&self, _cwd: &Path) -> Result<PathBuf> {
        // MiMo uses SQLite, not a projects directory
        self.data_root()
    }

    fn list_projects(&self, _root: &Path) -> Result<Vec<Project>> {
        // MiMo uses a single SQLite database, not per-project directories
        let data_root = self.data_root()?;
        let modified = std::fs::metadata(&data_root)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        Ok(vec![Project {
            name: "mimocode".to_string(),
            display_name: "MiMo Code".to_string(),
            modified,
        }])
    }

    fn load_conversations(
        &self,
        _project_dir: &Path,
        _show_last: bool,
        _project_dir_name: &str,
        _debug_level: Option<DebugLevel>,
    ) -> Result<Vec<Conversation>> {
        Ok(Vec::new())
    }

    fn load_all_conversations_streaming(
        &self,
        show_last: bool,
        _debug_level: Option<DebugLevel>,
    ) -> Receiver<LoaderMessage> {
        let (tx, rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            match load_mimocode_conversations(show_last) {
                Ok(conversations) => {
                    if !conversations.is_empty() {
                        let _ = tx.send(LoaderMessage::Batch(conversations));
                    }
                }
                Err(e) => {
                    let _ = tx.send(LoaderMessage::Fatal(e));
                }
            }
            let _ = tx.send(LoaderMessage::Done);
        });

        rx
    }

    fn path_encoder(&self) -> &dyn PathEncoder {
        &self.path_encoder
    }

    fn parser(&self) -> &dyn ConversationParser {
        &self.parser
    }

    fn cli_command(&self) -> &str {
        "mimo"
    }

    fn build_resume_command(
        &self,
        _conversation: &Conversation,
        _fork: bool,
        _default_args: &[String],
    ) -> Result<ResumeAction> {
        // MiMo doesn't have a CLI resume command
        Ok(ResumeAction::Unsupported)
    }

    fn delete_session(&self, _uuid: &str) -> Result<usize> {
        // Not implemented yet
        Ok(0)
    }

    fn find_jsonl_by_uuid(&self, _uuid: &str) -> Result<Option<PathBuf>> {
        // MiMo uses SQLite, not JSONL
        Ok(None)
    }

    fn supports_resume(&self) -> bool {
        false
    }

    fn supports_fork(&self) -> bool {
        false
    }
}

/// Get the MiMo database path
fn mimocode_db_path() -> Result<PathBuf> {
    let home = home::home_dir().ok_or_else(|| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine home directory",
        ))
    })?;
    Ok(home
        .join(".local")
        .join("share")
        .join("mimocode")
        .join("mimocode.db"))
}

/// Load conversations from MiMo SQLite database
fn load_mimocode_conversations(_show_last: bool) -> Result<Vec<Conversation>> {
    let db_path = mimocode_db_path()?;
    let conn = Connection::open(&db_path).map_err(|e| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to open MiMo database: {}", e),
        ))
    })?;

    let mut stmt = conn
        .prepare(
            "SELECT id, title, directory, time_created, time_updated FROM session ORDER BY time_updated DESC",
        )
        .map_err(|e| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to query sessions: {}", e),
            ))
        })?;

    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let title: String = row.get(1)?;
            let directory: String = row.get(2)?;
            let time_created: i64 = row.get(3)?;
            let time_updated: i64 = row.get(4)?;
            Ok((id, title, directory, time_created, time_updated))
        })
        .map_err(|e| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to query sessions: {}", e),
            ))
        })?;

    let mut conversations = Vec::new();

    for row in rows {
        let (id, title, directory, time_created, time_updated) = row.map_err(|e| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to read session row: {}", e),
            ))
        })?;

        // Get message count
        let message_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM message WHERE session_id = ?1",
                [&id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Get first user message as preview
        let preview: Option<String> = conn
            .query_row(
                "SELECT data FROM message WHERE session_id = ?1 AND json_extract(data, '$.role') = 'user' ORDER BY time_created ASC LIMIT 1",
                [&id],
                |row| {
                    let data: String = row.get(0)?;
                    Ok(data)
                },
            )
            .ok()
            .and_then(|data| {
                serde_json::from_str::<serde_json::Value>(&data)
                    .ok()
                    .and_then(|v| {
                        // Try to extract text content from the message data
                        v.get("content")
                            .or_else(|| v.get("text"))
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string())
                    })
            });

        let cwd = PathBuf::from(&directory);
        let project_name = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string());

        // Convert timestamps (milliseconds to seconds)
        let created_dt = chrono::DateTime::from_timestamp_millis(time_created)
            .map(|dt| dt.with_timezone(&chrono::Local))
            .unwrap_or_else(chrono::Local::now);

        let preview_str = preview.unwrap_or_default();

        conversations.push(Conversation {
            path: PathBuf::from(format!("mimo:{}", id)),
            index: 0,
            timestamp: created_dt,
            preview: preview_str.clone(),
            preview_first: preview_str.clone(),
            preview_last: preview_str,
            full_text: String::new(), // Would need to load all messages
            agent_search_text: String::new(),
            semantic_turns: Vec::new(),
            semantic_turn_ranges: Vec::new(),
            search_text_lower: String::new(),
            project_name,
            project_path: Some(cwd.clone()),
            cwd: Some(cwd),
            message_count: message_count as usize,
            parse_errors: Vec::new(),
            summary: None,
            custom_title: Some(title),
            model: None,
            total_tokens: 0,
            duration_minutes: None,
            provider: Some("mimo".to_string()),
        });
    }

    Ok(conversations)
}
