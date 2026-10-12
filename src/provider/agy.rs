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
            return Err(AppError::ProjectsDirNotFound(db_path.display().to_string()));
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
        let conversation_id = conversation.session_id().to_string();

        let current_dir = conversation
            .cwd
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));

        let mut args = vec!["--conversation".to_string(), conversation_id];
        args.extend(default_args.iter().cloned());

        Ok(ResumeAction::Run {
            command: "agy".to_string(),
            args,
            current_dir,
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

    fn find_jsonl_by_uuid(&self, uuid: &str) -> Result<Option<PathBuf>> {
        find_agy_transcript_by_uuid(uuid)
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn supports_fork(&self) -> bool {
        false
    }
}

pub(crate) fn find_agy_transcript_by_uuid(uuid: &str) -> Result<Option<PathBuf>> {
    let home = home::home_dir().ok_or_else(|| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine home directory",
        ))
    })?;
    let transcript_path = home
        .join(".gemini")
        .join("antigravity-cli")
        .join("brain")
        .join(uuid)
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl");

    if transcript_path.exists() {
        Ok(Some(transcript_path))
    } else {
        Ok(None)
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
        if matches!(
            name_str.as_ref(),
            "All Users" | "Default" | "Default User" | "Public" | "WsiAccount"
        ) {
            continue;
        }
        let db_path = entry
            .path()
            .join(".gemini")
            .join("antigravity-cli")
            .join("conversation_summaries.db");
        if db_path.exists() {
            return Ok(db_path);
        }
    }

    Err(AppError::ProjectsDirNotFound(
        "Agy not found on Windows".to_string(),
    ))
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

/// Load Agy conversations: summaries are instant, full text is cached.
///
/// Summary rows (title/preview/cwd/timestamps) come from
/// `conversation_summaries.db` in one query. Deep-search text is extracted per
/// session (transcript.jsonl > session .db protobuf blobs > .pb file) and
/// cached on disk: unchanged sessions never re-read detail files. Sessions
/// with no detail files (~70%) are title+preview only with zero file IO.
fn load_agy_conversations(_show_last: bool) -> Result<Vec<Conversation>> {
    use crate::history::cache;
    use rayon::prelude::*;
    use std::collections::HashSet;
    use std::time::UNIX_EPOCH;

    let db_path = agy_db_path()?;
    let conn = Connection::open(&db_path).map_err(|e| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to open Agy database: {}", e),
        ))
    })?;

    let conversations_dir = agy_conversations_dir()?;
    let home_dir = home::home_dir();

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

    // Collect summary rows first so the cache can be consulted per session.
    let summaries: Vec<(String, String, String, i64, String, String, String, String)> = stmt
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
        })?
        .filter_map(|row| row.ok())
        .collect();
    drop(stmt);

    let mut cached = cache::read_provider_cache("agy");
    let live_ids: HashSet<String> = summaries
        .iter()
        .map(|(id, _, _, _, _, _, _, _)| id.clone())
        .collect();

    // Cheap per-row fields + text source detection (metadata stats only).
    let mut pending: Vec<AgySessionParts> = Vec::with_capacity(summaries.len());
    for (id, title, preview, step_count, last_modified, workspace_uris, _status, _source) in
        summaries
    {
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

        // Detail files, in preference order: transcript > session db > pb.
        let transcript = home_dir.as_ref().map(|h| {
            h.join(".gemini")
                .join("antigravity-cli")
                .join("brain")
                .join(&id)
                .join(".system_generated")
                .join("logs")
                .join("transcript.jsonl")
        });
        let session_db = conversations_dir.join(format!("{id}.db"));
        let session_pb = conversations_dir.join(format!("{id}.pb"));

        let file_meta = |p: &PathBuf| {
            std::fs::metadata(p)
                .ok()
                .and_then(|m| m.modified().ok().map(|t| (m.len(), t)))
        };
        let (source, cache_size, cache_mtime) = if let Some(tp) = transcript
            && let Some((len, t)) = file_meta(&tp)
        {
            (TextSource::Transcript(tp.clone()), len, t)
        } else if let Some((len, t)) = file_meta(&session_db) {
            (TextSource::SessionDb(session_db.clone()), len, t)
        } else if let Some((len, t)) = file_meta(&session_pb) {
            (TextSource::PbFile(session_pb.clone()), len, t)
        } else {
            let t = parse_agy_time(&last_modified).unwrap_or(UNIX_EPOCH);
            (TextSource::RowOnly, step_count.max(0) as u64, t)
        };

        let path = match &source {
            TextSource::Transcript(tp) => tp.clone(),
            _ => conversations_dir.join(format!("{id}.db")),
        };

        pending.push(AgySessionParts {
            id,
            title,
            preview,
            step_count,
            last_modified,
            cwd,
            project_name,
            path,
            source,
            cache_size,
            cache_mtime,
        });
    }

    let mut conversations: Vec<Conversation> = Vec::with_capacity(pending.len());
    let mut misses: Vec<AgySessionParts> = Vec::new();
    let mut dirty = false;

    for p in pending {
        match cached.get(p.id.as_str()) {
            Some(entry)
                if cache::entry_matches(entry, p.cache_size, p.cache_mtime) && !entry.is_empty =>
            {
                let mut conv = cache::conversation_from_entry(entry, p.path.clone(), false);
                conv.project_name = p.project_name.clone();
                conv.project_path = p.cwd.clone();
                conversations.push(conv);
            }
            _ => {
                dirty = true;
                misses.push(p);
            }
        }
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

    // Extract deep-search text for misses only, in parallel.
    let built: Vec<(String, Conversation, u64, SystemTime)> = misses
        .into_par_iter()
        .map(|p| {
            let extra = match &p.source {
                TextSource::Transcript(tp) => extract_agy_transcript_text(tp),
                TextSource::SessionDb(db) => extract_agy_session_db_text(db),
                TextSource::PbFile(pb) => std::fs::read(pb)
                    .map(|data| printable_runs(&data))
                    .unwrap_or_default(),
                TextSource::RowOnly => Vec::new(),
            };
            let conv = agy_conversation_from_parts(&p, extra);
            (p.id.clone(), conv, p.cache_size, p.cache_mtime)
        })
        .collect();

    for (id, conv, size, mtime) in built {
        cached.insert(id, cache::entry_from_conversation(&conv, size, mtime));
        conversations.push(conv);
    }

    if dirty {
        cache::write_provider_cache("agy", cached);
    }

    conversations.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    for (idx, conv) in conversations.iter_mut().enumerate() {
        conv.index = idx;
    }

    Ok(conversations)
}

/// Where a session's deep-search text comes from, cheapest first.
enum TextSource {
    /// brain/<id>/.system_generated/logs/transcript.jsonl (structured JSONL)
    Transcript(PathBuf),
    /// conversations/<id>.db (protobuf blobs in `steps`)
    SessionDb(PathBuf),
    /// conversations/<id>.pb (raw protobuf file)
    PbFile(PathBuf),
    /// No detail files: title+preview only.
    RowOnly,
}

/// Leniently parse Agy datetime strings ("... 03:45:30.819+00:00" or RFC3339).
fn parse_agy_time(s: &str) -> Option<SystemTime> {
    use std::time::{Duration, UNIX_EPOCH};
    let dt = chrono::DateTime::parse_from_rfc3339(s)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(&s.replacen(' ', "T", 1)))
        .ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(dt.timestamp().max(0) as u64))
}

/// Build a Conversation from summary fields + extracted deep-search text.
/// Display fields (preview/title) are unchanged from summary-only behavior.
fn agy_conversation_from_parts(p: &AgySessionParts, extra_text: Vec<String>) -> Conversation {
    // Parse the timestamp
    let created_dt = chrono::DateTime::parse_from_rfc3339(&p.last_modified)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Local))
        .unwrap_or_else(chrono::Local::now);

    // Use title if available, otherwise use preview or generate from ID
    let display_title = if !p.title.is_empty() {
        p.title.clone()
    } else if !p.preview.is_empty() {
        p.preview.clone()
    } else {
        format!("Session {}", &p.id[..8.min(p.id.len())])
    };

    // Title + preview first (summary-level), then deep-search text.
    let mut full_text = String::new();
    if !p.title.is_empty() {
        full_text.push_str(&p.title);
    }
    if !p.preview.is_empty() {
        if !full_text.is_empty() {
            full_text.push(' ');
        }
        full_text.push_str(&p.preview);
    }
    for chunk in extra_text {
        if chunk.trim().is_empty() {
            continue;
        }
        if !full_text.is_empty() {
            full_text.push('\n');
        }
        full_text.push_str(&chunk);
    }
    if full_text.is_empty() {
        full_text = display_title.clone();
    }

    Conversation {
        path: p.path.clone(),
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
        project_name: p.project_name.clone(),
        project_path: p.cwd.clone(),
        cwd: p.cwd.clone(),
        message_count: p.step_count.max(0) as usize,
        parse_errors: Vec::new(),
        summary: if !p.preview.is_empty() && !p.title.is_empty() {
            Some(p.preview.clone())
        } else {
            None
        },
        custom_title: if !p.title.is_empty() {
            Some(p.title.clone())
        } else {
            None
        },
        model: None,
        total_tokens: 0,
        duration_minutes: None,
        provider: Some("agy".to_string()),
    }
}

/// Summary fields needed to build a Conversation.
struct AgySessionParts {
    id: String,
    title: String,
    preview: String,
    step_count: i64,
    last_modified: String,
    cwd: Option<PathBuf>,
    project_name: Option<String>,
    path: PathBuf,
    source: TextSource,
    cache_size: u64,
    cache_mtime: SystemTime,
}

/// Extract searchable text from an Agy transcript.jsonl.
/// Mirrors the viewer's field handling (USER_INPUT / PLANNER_RESPONSE / GENERIC).
fn extract_agy_transcript_text(path: &Path) -> Vec<String> {
    use std::io::{BufRead, BufReader};

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let step_type = v.get("type").and_then(|s| s.as_str()).unwrap_or("");
        match step_type {
            "USER_INPUT" => {
                let mut content = v
                    .get("content")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                if content.starts_with("<USER_REQUEST>")
                    && let Some(end) = content.find("</USER_REQUEST>")
                {
                    content = content["<USER_REQUEST>".len()..end].trim().to_string();
                }
                if !content.trim().is_empty() {
                    out.push(content);
                }
            }
            "PLANNER_RESPONSE" => {
                if let Some(thinking) = v.get("thinking").and_then(|s| s.as_str())
                    && !thinking.trim().is_empty()
                {
                    out.push(thinking.to_string());
                }
                if let Some(tools) = v.get("tool_calls").and_then(|t| t.as_array()) {
                    for t in tools {
                        let name = t.get("name").and_then(|s| s.as_str()).unwrap_or("");
                        let args = t.get("args").map(|a| a.to_string()).unwrap_or_default();
                        let entry = format!("{name} {args}").trim().to_string();
                        if !entry.is_empty() {
                            out.push(entry);
                        }
                    }
                }
                if let Some(content) = v.get("content").and_then(|s| s.as_str())
                    && !content.trim().is_empty()
                {
                    out.push(content.to_string());
                }
            }
            "GENERIC" => {
                if let Some(content) = v.get("content").and_then(|s| s.as_str())
                    && !content.trim().is_empty()
                {
                    out.push(content.to_string());
                }
            }
            _ => {}
        }
    }
    out
}

/// Extract searchable text from a session .db's protobuf blobs.
///
/// Blobs are length-delimited protobuf fields; human text (prompts, answers,
/// tool I/O, file paths) is embedded as UTF-8 and recovered by scanning for
/// printable runs. Total text per session is bounded.
fn extract_agy_session_db_text(db_path: &Path) -> Vec<String> {
    /// Bound text extracted from one session (chars).
    const MAX_SESSION_CHARS: usize = 1_000_000;

    let conn = match Connection::open(db_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut stmt = match conn
        .prepare("SELECT step_payload, metadata, task_details, render_info FROM steps ORDER BY idx")
    {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = match stmt.query_map([], |row| {
        let payload: Option<Vec<u8>> = row.get(0).unwrap_or(None);
        let metadata: Option<Vec<u8>> = row.get(1).unwrap_or(None);
        let task: Option<Vec<u8>> = row.get(2).unwrap_or(None);
        let render: Option<Vec<u8>> = row.get(3).unwrap_or(None);
        Ok((payload, metadata, task, render))
    }) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    let mut budget = MAX_SESSION_CHARS;
    for row in rows.flatten() {
        for blob in [row.0, row.1, row.2, row.3].into_iter().flatten() {
            if budget == 0 {
                return out;
            }
            for run in printable_runs(&blob) {
                let len = run.chars().count();
                if len > budget {
                    out.push(run.chars().take(budget).collect());
                    return out;
                }
                budget -= len;
                out.push(run);
            }
        }
    }
    out
}

/// Scan bytes for human-readable runs (ASCII printable + valid UTF-8 text).
/// Protobuf field tags and binary payloads act as natural separators.
fn printable_runs(data: &[u8]) -> Vec<String> {
    /// Minimum run length (chars) to keep.
    const MIN_RUN_CHARS: usize = 4;
    /// Truncate pathological runs (e.g. long base64) per run.
    const MAX_RUN_CHARS: usize = 500;

    String::from_utf8_lossy(data)
        .split(|c: char| c == '\u{FFFD}' || c.is_control())
        .filter_map(|seg| {
            let seg = seg.trim();
            let n = seg.chars().count();
            if n < MIN_RUN_CHARS {
                return None;
            }
            if n > MAX_RUN_CHARS {
                Some(seg.chars().take(MAX_RUN_CHARS).collect())
            } else {
                Some(seg.to_string())
            }
        })
        .collect()
}

#[cfg(test)]
mod agy_text_tests {
    use super::{extract_agy_transcript_text, printable_runs};
    use std::io::Write;

    #[test]
    fn printable_runs_recovers_mixed_text() {
        // Protobuf-ish bytes: field tags + embedded UTF-8 (ASCII + CJK).
        let mut data = vec![0x0Au8, 0x0C, 0x08, 0xD4, 0x06];
        data.extend_from_slice("sessionID".as_bytes());
        data.extend_from_slice(&[0x12, 0x24]);
        data.extend_from_slice("当前共享stream需要压缩".as_bytes());
        data.extend_from_slice(&[0x00, 0x01, 0x02, 0xFF, 0xFE]);
        data.extend_from_slice("/home/u/repo/main.go".as_bytes());
        data.push(0x0A);

        let runs = printable_runs(&data);
        assert!(runs.iter().any(|r| r.contains("sessionID")), "{runs:?}");
        assert!(
            runs.iter().any(|r| r.contains("当前共享stream需要压缩")),
            "{runs:?}"
        );
        assert!(
            runs.iter().any(|r| r.contains("/home/u/repo/main.go")),
            "{runs:?}"
        );
        // Control bytes and replacement chars must split runs, not leak in.
        assert!(runs.iter().all(|r| !r.contains('\u{FFFD}')), "{runs:?}");
    }

    #[test]
    fn printable_runs_drops_short_noise() {
        let runs = printable_runs(b"\x08\x0Fab\xe2\x82");
        assert!(runs.is_empty(), "{runs:?}");
    }

    #[test]
    fn transcript_extraction_collects_all_step_types() {
        let mut file = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({
                "type": "USER_INPUT",
                "created_at": "2026-01-01T00:00:00Z",
                "content": "<USER_REQUEST>\nFix the flaky sampler test\n</USER_REQUEST>"
            })
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({
                "type": "PLANNER_RESPONSE",
                "created_at": "2026-01-01T00:00:01Z",
                "thinking": "Need to inspect sampler precision config",
                "tool_calls": [{"name": "read_file", "args": {"path": "sampler.go"}}],
                "content": "I will check the sampler"
            })
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({"type": "GENERIC", "content": "read 42 lines"})
        )
        .unwrap();

        let texts = extract_agy_transcript_text(file.path());
        let joined = texts.join("\n");
        assert!(joined.contains("Fix the flaky sampler test"), "{joined:?}");
        assert!(joined.contains("sampler precision config"), "{joined:?}");
        assert!(joined.contains("read_file"), "{joined:?}");
        assert!(joined.contains("sampler.go"), "{joined:?}");
        assert!(joined.contains("read 42 lines"), "{joined:?}");
    }
}
