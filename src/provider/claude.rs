use crate::cli::DebugLevel;
use crate::error::{AppError, Result};
use crate::history::{
    Conversation, LoaderMessage, Project,
    loader, parser, path,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

use super::{ConversationParser, PathEncoder, Provider, ProviderKind, ResumeAction};

pub struct ClaudePathEncoder;

impl PathEncoder for ClaudePathEncoder {
    fn encode_project_path(&self, path: &Path) -> String {
        path::convert_path_to_project_dir_name(path)
    }

    fn decode_project_name(&self, encoded: &str) -> String {
        path::decode_project_dir_name(encoded)
    }

    fn project_root<'a>(&self, encoded: &'a str) -> &'a str {
        path::encoded_project_root(encoded)
    }

    fn is_same_project(&self, a: &str, b: &str) -> bool {
        path::is_same_project(a, b)
    }
}

pub struct ClaudeParser;

impl ConversationParser for ClaudeParser {
    fn parse_file(
        &self,
        path: PathBuf,
        modified: Option<SystemTime>,
        debug_level: Option<DebugLevel>,
    ) -> Result<Option<Conversation>> {
        parser::process_conversation_file(path, modified, debug_level)
    }

    fn format_name(&self) -> &str {
        "jsonl"
    }
}

pub struct ClaudeProvider {
    path_encoder: ClaudePathEncoder,
    parser: ClaudeParser,
}

impl ClaudeProvider {
    pub fn try_new() -> Result<Self> {
        let root = get_claude_projects_root()?;
        if !root.exists() {
            return Err(AppError::ProjectsDirNotFound(
                root.display().to_string(),
            ));
        }
        Ok(Self {
            path_encoder: ClaudePathEncoder,
            parser: ClaudeParser,
        })
    }
}

impl Provider for ClaudeProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Claude
    }

    fn display_name(&self) -> &str {
        "Claude Code"
    }

    fn data_root(&self) -> Result<PathBuf> {
        get_claude_projects_root()
    }

    fn projects_dir(&self, cwd: &Path) -> Result<PathBuf> {
        let converted = path::convert_path_to_project_dir_name(cwd);
        Ok(get_claude_projects_root()?.join(converted))
    }

    fn list_projects(&self, root: &Path) -> Result<Vec<Project>> {
        loader::list_projects(root)
    }

    fn load_conversations(
        &self,
        project_dir: &Path,
        show_last: bool,
        project_dir_name: &str,
        debug_level: Option<DebugLevel>,
    ) -> Result<Vec<Conversation>> {
        loader::load_conversations(project_dir, show_last, project_dir_name, debug_level)
    }

    fn load_all_conversations_streaming(
        &self,
        show_last: bool,
        debug_level: Option<DebugLevel>,
    ) -> Receiver<LoaderMessage> {
        loader::load_all_conversations_streaming(show_last, debug_level)
    }

    fn path_encoder(&self) -> &dyn PathEncoder {
        &self.path_encoder
    }

    fn parser(&self) -> &dyn ConversationParser {
        &self.parser
    }

    fn cli_command(&self) -> &str {
        "claude"
    }

    fn build_resume_command(
        &self,
        conversation: &Conversation,
        fork: bool,
        default_args: &[String],
    ) -> Result<ResumeAction> {
        let conversation_id = conversation
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                AppError::ClaudeExecutionError(
                    "Conversation filename is not valid Unicode".to_string(),
                )
            })?
            .to_owned();

        let cwd = std::env::current_dir().map_err(|e| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Failed to get current directory: {}", e),
            ))
        })?;

        let conv_projects_dir = conversation.path.parent().ok_or_else(|| {
            AppError::ClaudeExecutionError(
                "Cannot determine conversation's project directory".to_string(),
            )
        })?;

        let mut args = vec!["--resume".to_string(), conversation_id];
        if fork {
            args.push("--fork-session".to_string());
        }
        args.extend(default_args.iter().cloned());

        // Determine if we need to copy files (cross-project fork)
        let project_path = conversation.project_path.as_ref();
        let cwd_projects_dir = self.projects_dir(&cwd)?;
        let project_dir = project_path.filter(|p| p.exists() && p.is_dir());

        if project_dir.is_none() || (fork && cwd_projects_dir != conv_projects_dir) {
            return Ok(ResumeAction::CopyAndRun {
                command: "claude".to_string(),
                args,
                current_dir: cwd,
                source_path: conversation.path.clone(),
                target_path: cwd_projects_dir.join(conversation.path.file_name().unwrap()),
            });
        }

        let current_dir = if cwd_projects_dir == conv_projects_dir {
            cwd
        } else {
            project_dir.unwrap().clone()
        };

        Ok(ResumeAction::Run {
            command: "claude".to_string(),
            args,
            current_dir,
        })
    }

    fn delete_session(&self, uuid: &str) -> Result<usize> {
        loader::delete_session_by_uuid(uuid)
    }

    fn find_jsonl_by_uuid(&self, uuid: &str) -> Result<Option<PathBuf>> {
        loader::find_jsonl_by_uuid(uuid)
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn supports_fork(&self) -> bool {
        true
    }
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
