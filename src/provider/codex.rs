use crate::cli::DebugLevel;
use crate::error::{AppError, Result};
use crate::history::{Conversation, LoaderMessage, Project};
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
            return Err(AppError::ProjectsDirNotFound(
                root.display().to_string(),
            ));
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
        let thread_id = conversation
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("rollout-"))
            .and_then(|s| {
                // Extract UUID from filename: rollout-YYYY-MM-DDThh-mm-ss-<uuid>
                s.rsplit_once('-').map(|(_, uuid)| uuid.to_string())
            })
            .ok_or_else(|| {
                AppError::ClaudeExecutionError(
                    "Cannot extract thread ID from Codex rollout filename".to_string(),
                )
            })?;

        let cwd = std::env::current_dir().map_err(|e| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Failed to get current directory: {}", e),
            ))
        })?;

        let mut args = vec!["resume".to_string(), thread_id];
        args.extend(default_args.iter().cloned());

        Ok(ResumeAction::Run {
            command: "codex".to_string(),
            args,
            current_dir: cwd,
        })
    }

    fn delete_session(&self, _uuid: &str) -> Result<usize> {
        // Codex delete not implemented yet
        Ok(0)
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
fn codex_home() -> Result<PathBuf> {
    let home = home::home_dir().ok_or_else(|| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine home directory",
        ))
    })?;
    Ok(home.join(".codex"))
}

/// Load conversations from Codex rollout files
fn load_codex_conversations(
    _show_last: bool,
    _debug_level: Option<DebugLevel>,
) -> Result<Vec<Conversation>> {
    let root = codex_home()?;
    let sessions_dir = root.join("sessions");
    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }

    let mut conversations = Vec::new();

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
                    let name = file_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("");
                    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                        continue;
                    }

                    if let Some(conv) = parse_rollout_file(&file_path) {
                        conversations.push(conv);
                    }
                }
            }
        }
    }

    Ok(conversations)
}

/// Parse a single Codex rollout JSONL file
fn parse_rollout_file(path: &Path) -> Option<Conversation> {
    use std::io::{BufRead, BufReader};

    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut cwd: Option<PathBuf> = None;
    let mut thread_id: Option<String> = None;
    let mut model_provider: Option<String> = None;
    let mut preview: Option<String> = None;
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
                    if let Some(id) = p.get("id").and_then(|v| v.as_str()) {
                        thread_id = Some(id.to_string());
                    }
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
                        let text = p.get("message")
                            .or_else(|| p.get("content"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !text.is_empty() {
                            if preview.is_none() {
                                preview = Some(text.clone());
                            }
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
    let modified = file_metadata
        .as_ref()
        .and_then(|m| m.modified().ok());

    let timestamp = modified
        .map(|m| chrono::DateTime::<chrono::Local>::from(m))
        .unwrap_or_else(chrono::Local::now);

    let full_text = all_text.join(" ");
    let preview_str = preview.clone().unwrap_or_default();

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
        preview: preview_str.clone(),
        preview_first: preview_str.clone(),
        preview_last: preview_str,
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
        custom_title: preview,
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
        eprintln!("Codex loaded {} conversations, {} contain busmirror", count, busmirror_count);
        assert!(count > 0, "Should load Codex conversations");
    }
}
