use crate::cli::DebugLevel;
use crate::error::{AppError, Result};
use crate::history::{Conversation, LoaderMessage, Project};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

use super::{ConversationParser, PathEncoder, Provider, ProviderKind, ResumeAction};

pub struct OpenCodePathEncoder;

impl PathEncoder for OpenCodePathEncoder {
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

pub struct OpenCodeParser;

impl ConversationParser for OpenCodeParser {
    fn parse_file(
        &self,
        _path: PathBuf,
        _modified: Option<SystemTime>,
        _debug_level: Option<DebugLevel>,
    ) -> Result<Option<Conversation>> {
        Err(AppError::ClaudeExecutionError(
            "OpenCode parser uses SQLite, not file-based".to_string(),
        ))
    }
    fn format_name(&self) -> &str {
        "sqlite"
    }
}

pub struct OpenCodeProvider {
    path_encoder: OpenCodePathEncoder,
    parser: OpenCodeParser,
    db_path: PathBuf,
}

impl OpenCodeProvider {
    pub fn try_new() -> Result<Self> {
        // Try Linux path first, then Windows path via WSL
        let db_path = opencode_db_path().or_else(|_| opencode_db_path_windows())?;
        if !db_path.exists() {
            return Err(AppError::ProjectsDirNotFound(
                db_path.display().to_string(),
            ));
        }
        Ok(Self {
            path_encoder: OpenCodePathEncoder,
            parser: OpenCodeParser,
            db_path,
        })
    }
}

impl Provider for OpenCodeProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenCode
    }

    fn display_name(&self) -> &str {
        "OpenCode"
    }

    fn data_root(&self) -> Result<PathBuf> {
        let home = home::home_dir().ok_or_else(|| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not determine home directory",
            ))
        })?;
        Ok(home.join(".local").join("share").join("opencode"))
    }

    fn projects_dir(&self, _cwd: &Path) -> Result<PathBuf> {
        self.data_root()
    }

    fn list_projects(&self, _root: &Path) -> Result<Vec<Project>> {
        let data_root = self.data_root()?;
        let modified = std::fs::metadata(&data_root)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        Ok(vec![Project {
            name: "opencode".to_string(),
            display_name: "OpenCode".to_string(),
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
        let db_path = self.db_path.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // Copy database to temp file to avoid locking issues on Windows
            let temp_db = if cfg!(target_os = "linux") && db_path.to_string_lossy().contains("/mnt/c/") {
                let temp = std::env::temp_dir().join("opencode_copy.db");
                match std::fs::copy(&db_path, &temp) {
                    Ok(_) => Some(temp),
                    Err(_) => None,
                }
            } else {
                None
            };

            let actual_path = temp_db.as_ref().unwrap_or(&db_path);

            match load_opencode_conversations(actual_path, show_last) {
                Ok(conversations) => {
                    if !conversations.is_empty() {
                        let _ = tx.send(LoaderMessage::Batch(conversations));
                    }
                }
                Err(e) => {
                    let _ = tx.send(LoaderMessage::Fatal(e));
                }
            }

            // Clean up temp file
            if let Some(temp) = temp_db {
                let _ = std::fs::remove_file(temp);
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
        "opencode"
    }

    fn build_resume_command(
        &self,
        _conversation: &Conversation,
        _fork: bool,
        _default_args: &[String],
    ) -> Result<ResumeAction> {
        Ok(ResumeAction::Unsupported)
    }

    fn delete_session(&self, _uuid: &str) -> Result<usize> {
        Ok(0)
    }

    fn find_jsonl_by_uuid(&self, _uuid: &str) -> Result<Option<PathBuf>> {
        Ok(None)
    }

    fn supports_resume(&self) -> bool {
        false
    }

    fn supports_fork(&self) -> bool {
        false
    }
}

fn opencode_db_path() -> Result<PathBuf> {
    let home = home::home_dir().ok_or_else(|| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine home directory",
        ))
    })?;
    Ok(home
        .join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db"))
}

/// Try to find OpenCode database on Windows via WSL
fn opencode_db_path_windows() -> Result<PathBuf> {
    // Try common Windows user directories via WSL
    let mnt_c = PathBuf::from("/mnt/c/Users");
    if !mnt_c.exists() {
        return Err(AppError::ProjectsDirNotFound("No /mnt/c".to_string()));
    }

    for entry in std::fs::read_dir(&mnt_c).map_err(AppError::Io)? {
        let entry = entry.map_err(AppError::Io)?;
        if !entry.file_type().map_err(AppError::Io)?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Skip system directories
        if matches!(name_str.as_ref(), "All Users" | "Default" | "Default User" | "Public" | "WsiAccount") {
            continue;
        }
        let db_path = entry.path().join(".local").join("share").join("opencode").join("opencode.db");
        if db_path.exists() {
            return Ok(db_path);
        }
    }

    Err(AppError::ProjectsDirNotFound("OpenCode not found on Windows".to_string()))
}

/// Load conversations from OpenCode SQLite database
fn load_opencode_conversations(db_path: &Path, _show_last: bool) -> Result<Vec<Conversation>> {
    let conn = Connection::open(db_path).map_err(|e| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to open OpenCode database: {}", e),
        ))
    })?;

    let mut stmt = conn
        .prepare(
            "SELECT id, title, directory, time_created, time_updated FROM session
             WHERE title NOT LIKE 'checkpoint-writer:%'
               AND title NOT LIKE 'goal:%'
               AND parent_id IS NULL
             ORDER BY time_updated DESC",
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
        let (id, title, directory, time_created, _time_updated) = row.map_err(|e| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to read session row: {}", e),
            ))
        })?;

        let message_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM message WHERE session_id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let mut all_text = Vec::new();
        let mut preview = None;

        if let Ok(mut part_stmt) = conn.prepare(
            "SELECT p.data, json_extract(m.data, '$.role') FROM part p
             JOIN message m ON p.message_id = m.id
             WHERE p.session_id = ?1
             ORDER BY p.time_created ASC",
        ) {
            if let Ok(part_rows) = part_stmt.query_map(rusqlite::params![id], |row| {
                let data: String = row.get(0)?;
                let role: String = row.get(1).unwrap_or_default();
                Ok((data, role))
            }) {
                for part_row in part_rows {
                    if let Ok((data, role)) = part_row {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                            let part_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            let text = match part_type {
                                "text" => v.get("text").and_then(|t| t.as_str()).unwrap_or(""),
                                "subtask" => v.get("prompt").and_then(|t| t.as_str()).unwrap_or(""),
                                _ => "",
                            };
                            if !text.is_empty() {
                                all_text.push(text.to_string());
                                if preview.is_none() && role == "user" {
                                    preview = Some(text.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        let cwd = PathBuf::from(&directory);
        let project_name = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string());

        let created_dt = chrono::DateTime::from_timestamp_millis(time_created)
            .map(|dt| dt.with_timezone(&chrono::Local))
            .unwrap_or_else(chrono::Local::now);

        let preview_str = preview.unwrap_or_default();
        let full_text = all_text.join(" ");

        conversations.push(Conversation {
            path: PathBuf::from(format!("opencode:{}", id)),
            index: 0,
            timestamp: created_dt,
            preview: preview_str.clone(),
            preview_first: preview_str.clone(),
            preview_last: preview_str,
            full_text: full_text.clone(),
            agent_search_text: String::new(),
            semantic_turns: Vec::new(),
            semantic_turn_ranges: Vec::new(),
            search_text_lower: crate::search::normalize_for_search(&full_text),
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
            provider: Some("opencode".to_string()),
        });
    }

    Ok(conversations)
}
