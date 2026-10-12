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
            return Err(AppError::ProjectsDirNotFound(db_path.display().to_string()));
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
            match load_opencode_conversations(&db_path, show_last) {
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
        "opencode"
    }

    fn build_resume_command(
        &self,
        conversation: &Conversation,
        _fork: bool,
        default_args: &[String],
    ) -> Result<ResumeAction> {
        let session_id = conversation.session_id().to_string();

        let current_dir = conversation
            .cwd
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));

        let mut args = vec!["-s".to_string(), session_id];
        args.extend(default_args.iter().cloned());

        Ok(ResumeAction::Run {
            command: "opencode".to_string(),
            args,
            current_dir,
        })
    }

    fn delete_session(&self, uuid: &str) -> Result<usize> {
        let conn = Connection::open(&self.db_path).map_err(|e| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to open OpenCode database: {}", e),
            ))
        })?;

        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM session WHERE id = ?1",
                rusqlite::params![uuid],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !exists {
            return Ok(0);
        }

        conn.execute("DELETE FROM session WHERE id = ?1", rusqlite::params![uuid])
            .map_err(|e| {
                AppError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Failed to delete session: {}", e),
                ))
            })?;

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

pub(crate) fn opencode_db_path() -> Result<PathBuf> {
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
pub(crate) fn opencode_db_path_windows() -> Result<PathBuf> {
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
        if matches!(
            name_str.as_ref(),
            "All Users" | "Default" | "Default User" | "Public" | "WsiAccount"
        ) {
            continue;
        }
        let db_path = entry
            .path()
            .join(".local")
            .join("share")
            .join("opencode")
            .join("opencode.db");
        if db_path.exists() {
            return Ok(db_path);
        }
    }

    Err(AppError::ProjectsDirNotFound(
        "OpenCode not found on Windows".to_string(),
    ))
}

/// Load conversations from OpenCode SQLite database.
///
/// Full message text IS indexed for deep search, but results are cached on
/// disk keyed by session id + last-update time + message count: unchanged
/// sessions never touch the `part` table after the first run. Only cache
/// misses run the full part scan.
fn load_opencode_conversations(db_path: &Path, show_last: bool) -> Result<Vec<Conversation>> {
    use crate::history::cache;
    use std::collections::HashSet;
    use std::time::{Duration, UNIX_EPOCH};

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

    // Collect session rows first so other statements can reuse the connection.
    let sessions: Vec<(String, String, String, i64, i64)> = stmt
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
        })?
        .filter_map(|row| row.ok())
        .collect();
    drop(stmt);

    let mut count_stmt = conn
        .prepare("SELECT COUNT(*) FROM message WHERE session_id = ?1")
        .map_err(|e| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to prepare count query: {}", e),
            ))
        })?;
    let mut parts_stmt = conn
        .prepare(
            "SELECT p.data, json_extract(m.data, '$.role') FROM part p
             JOIN message m ON p.message_id = m.id
             WHERE p.session_id = ?1
             ORDER BY p.time_created ASC",
        )
        .map_err(|e| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to prepare parts query: {}", e),
            ))
        })?;

    let mut cached = cache::read_provider_cache("opencode");
    let live_ids: HashSet<&str> = sessions
        .iter()
        .map(|(id, _, _, _, _)| id.as_str())
        .collect();

    let mut conversations: Vec<Conversation> = Vec::with_capacity(sessions.len());
    let mut dirty = false;

    for (id, title, directory, time_created, time_updated) in &sessions {
        let message_count: i64 = count_stmt
            .query_row(rusqlite::params![id], |row| row.get(0))
            .unwrap_or(0);
        let updated_time = UNIX_EPOCH + Duration::from_millis((*time_updated).max(0) as u64);

        if let Some(entry) = cached.get(id.as_str())
            && cache::entry_matches(entry, message_count.max(0) as u64, updated_time)
            && !entry.is_empty
        {
            let path = PathBuf::from(format!("opencode:{id}"));
            conversations.push(cache::conversation_from_entry(entry, path, show_last));
            continue;
        }
        dirty = true;

        // Cache miss: full part scan for deep search indexing.
        let mut all_text = Vec::new();
        let mut preview_first: Option<String> = None;
        let mut preview_last: Option<String> = None;
        if let Ok(part_rows) = parts_stmt.query_map(rusqlite::params![id], |row| {
            let data: String = row.get(0)?;
            let role: String = row.get(1).unwrap_or_default();
            Ok((data, role))
        }) {
            for part_row in part_rows.flatten() {
                let (data, role) = part_row;
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                    let part_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    let text = match part_type {
                        "text" => v.get("text").and_then(|t| t.as_str()).unwrap_or(""),
                        "subtask" => v.get("prompt").and_then(|t| t.as_str()).unwrap_or(""),
                        _ => "",
                    };
                    if text.trim().is_empty() {
                        continue;
                    }
                    all_text.push(text.to_string());
                    if role == "user" {
                        if preview_first.is_none() {
                            preview_first = Some(text.to_string());
                        }
                        preview_last = Some(text.to_string());
                    }
                }
            }
        }

        let cwd = PathBuf::from(directory);
        let project_name = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string());

        // Prefer last activity time so stale sessions sort (and prune) by recency.
        let created_dt = chrono::DateTime::from_timestamp_millis(*time_updated)
            .or_else(|| chrono::DateTime::from_timestamp_millis(*time_created))
            .map(|dt| dt.with_timezone(&chrono::Local))
            .unwrap_or_else(chrono::Local::now);

        let first = preview_first.clone().unwrap_or_default();
        let last = preview_last.clone().unwrap_or_default();
        let preview_str = if show_last {
            last.clone()
        } else {
            first.clone()
        };
        let mut full_text = if title.trim().is_empty() {
            all_text.join(" ")
        } else if all_text.is_empty() {
            title.clone()
        } else {
            format!("{title} {}", all_text.join(" "))
        };
        if full_text.trim().is_empty() {
            full_text = title.clone();
        }

        let conv = Conversation {
            path: PathBuf::from(format!("opencode:{id}")),
            index: 0,
            timestamp: created_dt,
            preview: preview_str,
            preview_first: first,
            preview_last: last,
            full_text: full_text.clone(),
            agent_search_text: String::new(),
            semantic_turns: Vec::new(),
            semantic_turn_ranges: Vec::new(),
            search_text_lower: crate::search::normalize_for_search(&full_text),
            project_name,
            project_path: Some(cwd.clone()),
            cwd: Some(cwd),
            message_count: message_count.max(0) as usize,
            parse_errors: Vec::new(),
            summary: None,
            custom_title: Some(title.clone()),
            model: None,
            total_tokens: 0,
            duration_minutes: None,
            provider: Some("opencode".to_string()),
        };
        cached.insert(
            id.clone(),
            cache::entry_from_conversation(&conv, message_count.max(0) as u64, updated_time),
        );
        conversations.push(conv);
    }

    // Drop cache entries for deleted sessions.
    let stale_ids: Vec<String> = cached
        .keys()
        .filter(|k| !live_ids.contains(k.as_str()))
        .cloned()
        .collect();
    if !stale_ids.is_empty() {
        dirty = true;
        for key in stale_ids {
            cached.remove(&key);
        }
    }

    if dirty {
        cache::write_provider_cache("opencode", cached);
    }

    // Fill project display info for cache hits (not stored in cache).
    for conv in &mut conversations {
        if conv.project_name.is_none()
            && let Some(ref cwd) = conv.cwd
        {
            conv.project_name = cwd
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string());
            conv.project_path = Some(cwd.clone());
        }
    }

    conversations.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    for (idx, conv) in conversations.iter_mut().enumerate() {
        conv.index = idx;
    }

    Ok(conversations)
}
