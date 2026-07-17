use crate::cli::DebugLevel;
use crate::error::{AppError, Result};
use crate::history::{Conversation, LoaderMessage, Project};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

use super::{ConversationParser, PathEncoder, Provider, ProviderKind, ResumeAction};

pub struct AgyPathEncoder;

impl PathEncoder for AgyPathEncoder {
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

pub struct AgyParser;

impl ConversationParser for AgyParser {
    fn parse_file(
        &self,
        _path: PathBuf,
        _modified: Option<SystemTime>,
        _debug_level: Option<DebugLevel>,
    ) -> Result<Option<Conversation>> {
        Err(AppError::ClaudeExecutionError(
            "Agy parser uses SQLite, not file-based".to_string(),
        ))
    }
    fn format_name(&self) -> &str {
        "sqlite"
    }
}

pub struct AgyProvider {
    path_encoder: AgyPathEncoder,
    parser: AgyParser,
}

impl AgyProvider {
    pub fn try_new() -> Result<Self> {
        // Try Linux path first, then Windows path via WSL
        let db_path = agy_db_path().or_else(|_| agy_db_path_windows())?;
        if !db_path.exists() {
            return Err(AppError::ProjectsDirNotFound(
                db_path.display().to_string(),
            ));
        }
        Ok(Self {
            path_encoder: AgyPathEncoder,
            parser: AgyParser,
        })
    }
}

impl Provider for AgyProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Agy
    }

    fn display_name(&self) -> &str {
        "Agy"
    }

    fn data_root(&self) -> Result<PathBuf> {
        let home = home::home_dir().ok_or_else(|| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not determine home directory",
            ))
        })?;
        Ok(home.join(".gemini").join("antigravity-cli"))
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
            name: "agy".to_string(),
            display_name: "Agy".to_string(),
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
            match load_agy_conversations(show_last) {
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
        "agy"
    }

    fn build_resume_command(
        &self,
        conversation: &Conversation,
        _fork: bool,
        default_args: &[String],
    ) -> Result<ResumeAction> {
        let conversation_id = conversation
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                AppError::ClaudeExecutionError(
                    "Cannot extract conversation ID".to_string(),
                )
            })?
            .to_string();

        let cwd = std::env::current_dir().map_err(|e| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Failed to get current directory: {}", e),
            ))
        })?;

        let mut args = vec!["--conversation".to_string(), conversation_id];
        args.extend(default_args.iter().cloned());

        Ok(ResumeAction::Run {
            command: "agy".to_string(),
            args,
            current_dir: cwd,
        })
    }

    fn delete_session(&self, uuid: &str) -> Result<usize> {
        let db_path = agy_db_path().or_else(|_| agy_db_path_windows())?;
        if !db_path.exists() {
            return Ok(0);
        }

        let conn = Connection::open(&db_path).map_err(|e| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to open Agy database: {}", e),
            ))
        })?;

        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM conversation_summaries WHERE conversation_id = ?1",
                rusqlite::params![uuid],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !exists {
            return Ok(0);
        }

        // Delete from summary database
        conn.execute(
            "DELETE FROM conversation_summaries WHERE conversation_id = ?1",
            rusqlite::params![uuid],
        )
        .map_err(|e| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to delete session: {}", e),
            ))
        })?;

        // Delete the session .db file
        let conversations_dir = agy_conversations_dir()?;
        let session_db = conversations_dir.join(format!("{}.db", uuid));
        if session_db.exists() {
            let _ = std::fs::remove_file(&session_db);
        }

        // Delete the session .pb file if exists
        let session_pb = conversations_dir.join(format!("{}.pb", uuid));
        if session_pb.exists() {
            let _ = std::fs::remove_file(&session_pb);
        }

        // Delete the workspace directory
        let home = home::home_dir().ok_or_else(|| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not determine home directory",
            ))
        })?;
        let brain_dir = home
            .join(".gemini")
            .join("antigravity-cli")
            .join("brain")
            .join(uuid);
        if brain_dir.exists() {
            let _ = std::fs::remove_dir_all(&brain_dir);
        }

        Ok(1)
    }

    fn find_jsonl_by_uuid(&self, _uuid: &str) -> Result<Option<PathBuf>> {
        Ok(None)
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn supports_fork(&self) -> bool {
        false
    }
}

fn agy_db_path() -> Result<PathBuf> {
    let home = home::home_dir().ok_or_else(|| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine home directory",
        ))
    })?;
    Ok(home
        .join(".gemini")
        .join("antigravity-cli")
        .join("conversation_summaries.db"))
}

/// Try to find Agy database on Windows via WSL
fn agy_db_path_windows() -> Result<PathBuf> {
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
        if matches!(name_str.as_ref(), "All Users" | "Default" | "Default User" | "Public" | "WsiAccount") {
            continue;
        }
        let db_path = entry.path().join(".gemini").join("antigravity-cli").join("conversation_summaries.db");
        if db_path.exists() {
            return Ok(db_path);
        }
    }

    Err(AppError::ProjectsDirNotFound("Agy not found on Windows".to_string()))
}

fn agy_conversations_dir() -> Result<PathBuf> {
    let home = home::home_dir().ok_or_else(|| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine home directory",
        ))
    })?;
    Ok(home
        .join(".gemini")
        .join("antigravity-cli")
        .join("conversations"))
}

fn load_agy_conversations(_show_last: bool) -> Result<Vec<Conversation>> {
    let db_path = agy_db_path()?;
    let conn = Connection::open(&db_path).map_err(|e| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to open Agy database: {}", e),
        ))
    })?;

    let conversations_dir = agy_conversations_dir()?;

    let mut stmt = conn
        .prepare(
            "SELECT conversation_id, title, preview, step_count,
                    last_modified_time, workspace_uris, status, source
             FROM conversation_summaries
             WHERE killed = 0
             ORDER BY last_modified_time DESC",
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
            let preview: String = row.get(2)?;
            let step_count: i64 = row.get(3)?;
            let last_modified: String = row.get(4)?;
            let workspace_uris: String = row.get(5)?;
            let status: String = row.get(6)?;
            let source: String = row.get(7)?;
            Ok((
                id,
                title,
                preview,
                step_count,
                last_modified,
                workspace_uris,
                status,
                source,
            ))
        })
        .map_err(|e| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to query sessions: {}", e),
            ))
        })?;

    let mut conversations = Vec::new();

    for row in rows {
        let (id, title, preview, step_count, last_modified, workspace_uris, _status, _source) =
            row.map_err(|e| {
                AppError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Failed to read session row: {}", e),
                ))
            })?;

        // Extract workspace path from JSON array
        let cwd = serde_json::from_str::<Vec<String>>(&workspace_uris)
            .ok()
            .and_then(|uris| {
                uris.first()
                    .and_then(|uri| uri.strip_prefix("file://"))
                    .map(PathBuf::from)
            });

        let project_name = cwd
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|s| s.to_string());

        // Parse the timestamp
        let created_dt = chrono::DateTime::parse_from_rfc3339(&last_modified)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Local))
            .unwrap_or_else(chrono::Local::now);

        // Use title if available, otherwise use preview or generate from ID
        let display_title = if !title.is_empty() {
            title
        } else if !preview.is_empty() {
            preview.clone()
        } else {
            format!("Session {}", &id[..8])
        };

        // Try to load first step content for search
        let mut full_text = String::new();
        let session_db = conversations_dir.join(format!("{}.db", id));
        if session_db.exists() {
            if let Ok(session_conn) = Connection::open(&session_db) {
                if let Ok(mut step_stmt) = session_conn.prepare(
                    "SELECT step_payload FROM steps WHERE step_payload IS NOT NULL ORDER BY idx LIMIT 10",
                ) {
                    if let Ok(step_rows) = step_stmt.query_map([], |row| {
                        let payload: Vec<u8> = row.get(0)?;
                        Ok(payload)
                    }) {
                        for step_row in step_rows {
                            if let Ok(_payload) = step_row {
                                // Step payloads are protobuf, we can't easily parse them
                                // but we can try to extract text if it's UTF-8
                            }
                        }
                    }
                }
            }
        }

        // Use preview as fallback text content
        if full_text.is_empty() && !preview.is_empty() {
            full_text = preview;
        }

        conversations.push(Conversation {
            path: conversations_dir.join(format!("{}.db", id)),
            index: 0,
            timestamp: created_dt,
            preview: display_title.clone(),
            preview_first: display_title.clone(),
            preview_last: display_title,
            full_text: full_text.clone(),
            agent_search_text: String::new(),
            semantic_turns: Vec::new(),
            semantic_turn_ranges: Vec::new(),
            search_text_lower: crate::search::normalize_for_search(&full_text),
            project_name,
            project_path: cwd.clone(),
            cwd,
            message_count: step_count as usize,
            parse_errors: Vec::new(),
            summary: None,
            custom_title: None,
            model: None,
            total_tokens: 0,
            duration_minutes: None,
            provider: Some("agy".to_string()),
        });
    }

    Ok(conversations)
}
