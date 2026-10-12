use crate::cli::DebugLevel;
use crate::error::{AppError, Result};
use crate::history::{Conversation, LoaderMessage, Project};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

use super::{ConversationParser, PathEncoder, Provider, ProviderKind, ResumeAction};

/// Codex has no project-based path encoding — it uses a flat date-based structure.
pub struct CodexPathEncoder;

impl PathEncoder for CodexPathEncoder {
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

pub struct CodexParser;

impl ConversationParser for CodexParser {
    fn parse_file(
        &self,
        _path: PathBuf,
        _modified: Option<SystemTime>,
        _debug_level: Option<DebugLevel>,
    ) -> Result<Option<Conversation>> {
        Err(AppError::ClaudeExecutionError(
            "Codex parser not yet implemented".to_string(),
        ))
    }

    fn format_name(&self) -> &str {
        "rollout-jsonl"
    }
}

pub struct CodexProvider {
    path_encoder: CodexPathEncoder,
    parser: CodexParser,
}

impl CodexProvider {
    pub fn try_new() -> Result<Self> {
        let root = codex_home()?;
        if !root.exists() {
            return Err(AppError::ProjectsDirNotFound(root.display().to_string()));
        }
        Ok(Self {
            path_encoder: CodexPathEncoder,
            parser: CodexParser,
        })
    }
}

impl Provider for CodexProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Codex
    }

    fn display_name(&self) -> &str {
        "Codex"
    }

    fn data_root(&self) -> Result<PathBuf> {
        codex_home()
    }

    fn projects_dir(&self, _cwd: &Path) -> Result<PathBuf> {
        let root = codex_home()?;
        Ok(root.join("sessions"))
    }

    fn list_projects(&self, root: &Path) -> Result<Vec<Project>> {
        // Codex uses date-based dirs: YYYY/MM/DD
        // We treat the sessions root as a single "project"
        let sessions_dir = root.join("sessions");
        if !sessions_dir.exists() {
            return Ok(Vec::new());
        }
        let modified = std::fs::metadata(&sessions_dir)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        Ok(vec![Project {
            name: "codex-sessions".to_string(),
            display_name: "Codex Sessions".to_string(),
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
        // Codex conversations are loaded via the streaming loader
        Ok(Vec::new())
    }

    fn load_all_conversations_streaming(
        &self,
        show_last: bool,
        debug_level: Option<DebugLevel>,
    ) -> Receiver<LoaderMessage> {
        // For now, use a simple synchronous approach
        // A proper implementation would scan ~/.codex/sessions/YYYY/MM/DD/ rollout files
        let (tx, rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            match load_codex_conversations(show_last, debug_level) {
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
        "codex"
    }

    fn build_resume_command(
        &self,
        conversation: &Conversation,
        _fork: bool,
        default_args: &[String],
    ) -> Result<ResumeAction> {
        let thread_id = conversation.session_id().to_string();

        let current_dir = conversation
            .cwd
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));

        let mut args = vec!["resume".to_string(), thread_id];
        args.extend(default_args.iter().cloned());

        Ok(ResumeAction::Run {
            command: "codex".to_string(),
            args,
            current_dir,
        })
    }

    fn delete_session(&self, uuid: &str) -> Result<usize> {
        let root = codex_home()?;
        let sessions_dir = root.join("sessions");
        if !sessions_dir.exists() {
            return Ok(0);
        }

        let mut deleted = 0;

        // Search through date directories for rollout files matching UUID
        for year_entry in std::fs::read_dir(&sessions_dir).map_err(AppError::Io)? {
            let year_dir = year_entry.map_err(AppError::Io)?.path();
            if !year_dir.is_dir() {
                continue;
            }
            for month_entry in std::fs::read_dir(&year_dir).map_err(AppError::Io)? {
                let month_dir = month_entry.map_err(AppError::Io)?.path();
                if !month_dir.is_dir() {
                    continue;
                }
                for day_entry in std::fs::read_dir(&month_dir).map_err(AppError::Io)? {
                    let day_dir = day_entry.map_err(AppError::Io)?.path();
                    if !day_dir.is_dir() {
                        continue;
                    }
                    for file_entry in std::fs::read_dir(&day_dir).map_err(AppError::Io)? {
                        let file_path = file_entry.map_err(AppError::Io)?.path();
                        let name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        if name.contains(uuid)
                            && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"))
                        {
                            std::fs::remove_file(&file_path).map_err(AppError::Io)?;
                            deleted += 1;
                        }
                    }
                }
            }
        }

        Ok(deleted)
    }

    fn find_jsonl_by_uuid(&self, uuid: &str) -> Result<Option<PathBuf>> {
        let root = codex_home()?;
        let sessions_dir = root.join("sessions");
        if !sessions_dir.exists() {
            return Ok(None);
        }

        // Search through date directories for rollout file matching UUID
        for year_dir in std::fs::read_dir(&sessions_dir).map_err(AppError::Io)? {
            let year_dir = year_dir.map_err(AppError::Io)?.path();
            if !year_dir.is_dir() {
                continue;
            }
            for month_dir in std::fs::read_dir(&year_dir).map_err(AppError::Io)? {
                let month_dir = month_dir.map_err(AppError::Io)?.path();
                if !month_dir.is_dir() {
                    continue;
                }
                for day_dir in std::fs::read_dir(&month_dir).map_err(AppError::Io)? {
                    let day_dir = day_dir.map_err(AppError::Io)?.path();
                    if !day_dir.is_dir() {
                        continue;
                    }
                    for file in std::fs::read_dir(&day_dir).map_err(AppError::Io)? {
                        let file = file.map_err(AppError::Io)?.path();
                        if let Some(name) = file.file_name().and_then(|n| n.to_str()) {
                            if name.contains(uuid) && name.ends_with(".jsonl") {
                                return Ok(Some(file));
                            }
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn supports_fork(&self) -> bool {
        false
    }
}

/// Get the Codex home directory (~/.codex)
pub(crate) fn codex_home() -> Result<PathBuf> {
    let home = home::home_dir().ok_or_else(|| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine home directory",
        ))
    })?;
    Ok(home.join(".codex"))
}

/// Load conversations from Codex rollout files.
///
/// Full transcript text (user/assistant messages + tool outputs) IS indexed
/// for deep search, but parsed results are cached on disk keyed by filename +
/// mtime + size: unchanged files never touch the JSON parser after the first
/// run. Only cache misses are fully parsed, in parallel.
fn load_codex_conversations(
    show_last: bool,
    _debug_level: Option<DebugLevel>,
) -> Result<Vec<Conversation>> {
    use crate::history::cache;
    use std::collections::{HashMap, HashSet};

    let root = codex_home()?;
    let sessions_dir = root.join("sessions");
    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }

    // Collect rollout files with metadata in one pass.
    let mut files_with_meta: Vec<(PathBuf, u64, Option<SystemTime>)> = Vec::new();
    // Walk date directories: YYYY/MM/DD/
    for year_entry in std::fs::read_dir(&sessions_dir).map_err(AppError::Io)? {
        let year_dir = year_entry.map_err(AppError::Io)?.path();
        if !year_dir.is_dir() {
            continue;
        }
        for month_entry in std::fs::read_dir(&year_dir).map_err(AppError::Io)? {
            let month_dir = month_entry.map_err(AppError::Io)?.path();
            if !month_dir.is_dir() {
                continue;
            }
            for day_entry in std::fs::read_dir(&month_dir).map_err(AppError::Io)? {
                let day_dir = day_entry.map_err(AppError::Io)?.path();
                if !day_dir.is_dir() {
                    continue;
                }
                for file_entry in std::fs::read_dir(&day_dir).map_err(AppError::Io)? {
                    let file_path = file_entry.map_err(AppError::Io)?.path();
                    if !file_path.is_file() {
                        continue;
                    }
                    let name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                        continue;
                    }
                    let metadata = std::fs::metadata(&file_path).ok();
                    let modified = metadata.as_ref().and_then(|m| m.modified().ok());
                    let file_size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                    files_with_meta.push((file_path, file_size, modified));
                }
            }
        }
    }

    let mut cached = cache::read_provider_cache("codex");
    let live_keys: HashSet<String> = files_with_meta
        .iter()
        .filter_map(|(p, _, _)| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
        .collect();

    let mut conversations: Vec<Conversation> = Vec::with_capacity(files_with_meta.len());
    let mut files_to_parse: Vec<(PathBuf, u64, Option<SystemTime>)> = Vec::new();
    let mut dirty = false;

    for (path, file_size, modified) in &files_with_meta {
        let filename = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("unknown");
        match (modified, cached.get(filename)) {
            (Some(mtime), Some(entry)) if cache::entry_matches(entry, *file_size, *mtime) => {
                if !entry.is_empty {
                    conversations.push(cache::conversation_from_entry(
                        entry,
                        path.clone(),
                        show_last,
                    ));
                }
            }
            _ => {
                dirty = true;
                files_to_parse.push((path.clone(), *file_size, *modified));
            }
        }
    }

    // Drop cache entries for deleted files.
    let stale_keys: Vec<String> = cached
        .keys()
        .filter(|k| !live_keys.contains(*k))
        .cloned()
        .collect();
    if !stale_keys.is_empty() {
        dirty = true;
        for key in stale_keys {
            cached.remove(&key);
        }
    }

    // Fully parse only cache misses, in parallel.
    let parse_results: Vec<(Option<Conversation>, String, u64, Option<SystemTime>)> =
        files_to_parse
            .into_par_iter()
            .map(|(path, file_size, modified)| {
                let filename = path
                    .file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or("unknown")
                    .to_owned();
                let conv = parse_rollout_file(&path, show_last);
                (conv, filename, file_size, modified)
            })
            .collect();

    let mut new_entries: HashMap<String, cache::CacheEntry> = HashMap::new();
    for (conv, filename, file_size, modified) in parse_results {
        match (conv, modified) {
            (Some(conv), Some(mtime)) => {
                new_entries.insert(
                    filename,
                    cache::entry_from_conversation(&conv, file_size, mtime),
                );
                conversations.push(conv);
            }
            (Some(conv), None) => {
                conversations.push(conv);
            }
            (None, Some(mtime)) => {
                new_entries.insert(filename, cache::empty_entry(file_size, mtime));
            }
            (None, None) => {}
        }
    }

    if dirty {
        for (key, entry) in new_entries {
            cached.insert(key, entry);
        }
        cache::write_provider_cache("codex", cached);
    }

    // Fill project display info (not stored in cache) from each conv's cwd.
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

/// Full parse of a single Codex rollout JSONL file.
///
/// Indexes user/assistant text AND tool call outputs for deep search. This is
/// only called for disk-cache misses; warm runs never execute this function.
fn parse_rollout_file(path: &Path, show_last: bool) -> Option<Conversation> {
    use std::io::{BufRead, BufReader};

    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut cwd: Option<PathBuf> = None;
    let mut model_provider: Option<String> = None;
    let mut preview_first: Option<String> = None;
    let mut preview_last: Option<String> = None;
    let mut all_text = Vec::new();
    let mut message_count = 0;
    let mut line_count = 0;

    for line in reader.lines() {
        let line = line.ok()?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        line_count += 1;

        let value: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Codex format: {"timestamp": "...", "type": "...", "payload": {...}}
        let msg_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let payload = value.get("payload");

        match msg_type {
            "session_meta" => {
                if let Some(p) = payload {
                    if let Some(c) = p.get("cwd").and_then(|v| v.as_str()) {
                        cwd = Some(PathBuf::from(c));
                    }
                    if let Some(p) = p.get("model_provider").and_then(|v| v.as_str()) {
                        model_provider = Some(p.to_string());
                    }
                }
            }
            "event_msg" => {
                if let Some(p) = payload {
                    let event_type = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if event_type == "user_message" {
                        // Codex uses "message" field, not "content"
                        let text = p
                            .get("message")
                            .or_else(|| p.get("content"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !text.trim().is_empty() {
                            if preview_first.is_none() {
                                preview_first = Some(text.clone());
                            }
                            preview_last = Some(text.clone());
                            all_text.push(text);
                            message_count += 1;
                        }
                    }
                }
            }
            "response_item" => {
                if let Some(p) = payload {
                    let role = p.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    let item_type = p.get("type").and_then(|v| v.as_str()).unwrap_or("");

                    // Handle assistant/model text responses
                    if role == "assistant" || role == "model" {
                        if let Some(content) = p.get("content").and_then(|v| v.as_array()) {
                            for block in content {
                                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                    all_text.push(text.to_string());
                                }
                            }
                            message_count += 1;
                        }
                    }

                    // Handle tool call outputs (custom_tool_call_output)
                    if item_type == "custom_tool_call_output" {
                        if let Some(output) = p.get("output").and_then(|v| v.as_array()) {
                            for block in output {
                                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                    all_text.push(text.to_string());
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if line_count == 0 {
        return None;
    }

    let file_metadata = std::fs::metadata(path).ok();
    let modified = file_metadata.as_ref().and_then(|m| m.modified().ok());

    let timestamp = modified
        .map(chrono::DateTime::<chrono::Local>::from)
        .unwrap_or_else(chrono::Local::now);

    let full_text = all_text.join(" ");
    let first = preview_first.clone().unwrap_or_default();
    let last = preview_last.clone().unwrap_or_default();
    let preview_str = if show_last {
        last.clone()
    } else {
        first.clone()
    };

    // Use the last component of cwd as the project name (e.g., "ekbuild", "ekm")
    let project_name = cwd
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(|s| s.to_string());

    Some(Conversation {
        path: path.to_path_buf(),
        index: 0,
        timestamp,
        preview: preview_str,
        preview_first: first,
        preview_last: last,
        full_text: full_text.clone(),
        agent_search_text: String::new(),
        semantic_turns: Vec::new(),
        semantic_turn_ranges: Vec::new(),
        search_text_lower: crate::search::normalize_for_search(&full_text),
        project_name,
        project_path: cwd.clone(),
        cwd,
        message_count,
        parse_errors: Vec::new(),
        summary: None,
        // Use first user message as session title instead of rollout UUID
        custom_title: preview_first,
        model: model_provider,
        total_tokens: 0,
        duration_minutes: None,
        provider: Some("codex".to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_provider_loads_sessions() {
        let provider = CodexProvider::try_new();
        if provider.is_err() {
            // Codex not installed, skip
            return;
        }
        let provider = provider.unwrap();
        let root = provider.data_root().unwrap();
        let sessions_dir = root.join("sessions");
        if !sessions_dir.exists() {
            return;
        }
        let rx = provider.load_all_conversations_streaming(false, None);
        let mut count = 0;
        let mut busmirror_count = 0;
        while let Ok(msg) = rx.recv() {
            match msg {
                LoaderMessage::Batch(convs) => {
                    for c in &convs {
                        if c.search_text_lower.contains("busmirror") {
                            busmirror_count += 1;
                        }
                    }
                    count += convs.len();
                }
                LoaderMessage::Done => break,
                _ => {}
            }
        }
        eprintln!(
            "Codex loaded {} conversations, {} contain busmirror",
            count, busmirror_count
        );
        assert!(count > 0, "Should load Codex conversations");
    }
}
