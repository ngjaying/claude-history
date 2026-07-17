pub mod claude;
pub mod codex;
pub mod mimo;
pub mod registry;

use crate::cli::DebugLevel;
use crate::error::Result;
use crate::history::{Conversation, LoaderMessage, Project};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    Claude,
    Codex,
    Crush,
    MiMo,
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Claude => write!(f, "claude"),
            Self::Codex => write!(f, "codex"),
            Self::Crush => write!(f, "crush"),
            Self::MiMo => write!(f, "mimo"),
        }
    }
}

pub enum ResumeAction {
    Run {
        command: String,
        args: Vec<String>,
        current_dir: PathBuf,
    },
    CopyAndRun {
        command: String,
        args: Vec<String>,
        current_dir: PathBuf,
        source_path: PathBuf,
        target_path: PathBuf,
    },
    Unsupported,
}

pub trait PathEncoder: Send + Sync {
    fn encode_project_path(&self, path: &Path) -> String;
    fn decode_project_name(&self, encoded: &str) -> String;
    fn project_root<'a>(&self, encoded: &'a str) -> &'a str;
    fn is_same_project(&self, a: &str, b: &str) -> bool {
        self.project_root(a) == self.project_root(b)
    }
}

pub trait ConversationParser: Send + Sync {
    fn parse_file(
        &self,
        path: PathBuf,
        modified: Option<SystemTime>,
        debug_level: Option<DebugLevel>,
    ) -> Result<Option<Conversation>>;
    fn format_name(&self) -> &str;
}

pub trait Provider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn display_name(&self) -> &str;
    fn data_root(&self) -> Result<PathBuf>;
    fn projects_dir(&self, cwd: &Path) -> Result<PathBuf>;
    fn list_projects(&self, root: &Path) -> Result<Vec<Project>>;
    fn load_conversations(
        &self,
        project_dir: &Path,
        show_last: bool,
        project_dir_name: &str,
        debug_level: Option<DebugLevel>,
    ) -> Result<Vec<Conversation>>;
    fn load_all_conversations_streaming(
        &self,
        show_last: bool,
        debug_level: Option<DebugLevel>,
    ) -> Receiver<LoaderMessage>;
    fn path_encoder(&self) -> &dyn PathEncoder;
    fn parser(&self) -> &dyn ConversationParser;
    fn cli_command(&self) -> &str;
    fn build_resume_command(
        &self,
        conversation: &Conversation,
        fork: bool,
        default_args: &[String],
    ) -> Result<ResumeAction>;
    fn delete_session(&self, uuid: &str) -> Result<usize>;
    fn find_jsonl_by_uuid(&self, uuid: &str) -> Result<Option<PathBuf>>;
    fn agent_search_text(&self, conversation: &Conversation) -> String {
        conversation.agent_search_text.clone()
    }
    fn supports_resume(&self) -> bool {
        true
    }
    fn supports_fork(&self) -> bool {
        false
    }
}
