# Multi-Provider Architecture Design

## Goal

Extend `claude-history` to support multiple AI coding tools (Claude Code, Codex, Crush, MiMo Code) through a pluggable `Provider` trait abstraction.

## Current State

The codebase is tightly coupled to Claude Code:
- `history::get_claude_projects_root()` → `~/.claude/projects/`
- `history::path::convert_path_to_project_dir_name()` → Claude's encoding scheme
- `claude::LogEntry` → Claude's JSONL format
- `resume_with_claude()` → `claude --resume <id>`
- `history::is_same_project()` → Claude's worktree detection

## Provider Trait

```rust
// src/provider/mod.rs

use crate::error::Result;
use crate::history::{Conversation, Project};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::SystemTime;

/// Unique identifier for a provider
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

/// Result of a resume operation
pub enum ResumeAction {
    /// Run the tool's CLI with the given working directory
    Run {
        command: String,
        args: Vec<String>,
        current_dir: PathBuf,
    },
    /// Copy session files then run (for cross-project fork)
    CopyAndRun {
        command: String,
        args: Vec<String>,
        current_dir: PathBuf,
        source_path: PathBuf,
        target_path: PathBuf,
    },
    /// Resume not supported for this provider
    Unsupported,
}

/// Provider-specific path encoding
pub trait PathEncoder: Send + Sync {
    /// Encode a filesystem path to the provider's directory naming scheme
    fn encode_project_path(&self, path: &Path) -> String;

    /// Decode a provider directory name back to a readable path (heuristic)
    fn decode_project_name(&self, encoded: &str) -> String;

    /// Extract the project root from an encoded name (strip worktree suffix etc.)
    fn project_root(&self, encoded: &str) -> &str;

    /// Check if two encoded names belong to the same project
    fn is_same_project(&self, a: &str, b: &str) -> bool {
        self.project_root(a) == self.project_root(b)
    }
}

/// Provider-specific conversation parser
pub trait ConversationParser: Send + Sync {
    /// Parse a single conversation file and extract metadata
    fn parse_file(
        &self,
        path: PathBuf,
        modified: Option<SystemTime>,
        debug_level: Option<crate::cli::DebugLevel>,
    ) -> Result<Option<Conversation>>;

    /// Get a human-readable description of the parse format
    fn format_name(&self) -> &str;
}

/// Provider-specific project discovery and session management
pub trait Provider: Send + Sync {
    /// Provider identifier
    fn kind(&self) -> ProviderKind;

    /// Human-readable name for display
    fn display_name(&self) -> &str;

    /// Get the root directory for this provider's data
    fn data_root(&self) -> Result<PathBuf>;

    /// Get the projects directory for a given working directory
    fn projects_dir(&self, cwd: &Path) -> Result<PathBuf>;

    /// List all projects that contain conversations
    fn list_projects(&self, root: &Path) -> Result<Vec<Project>>;

    /// Load conversations from a project directory
    fn load_conversations(
        &self,
        project_dir: &Path,
        show_last: bool,
        project_dir_name: &str,
        debug_level: Option<crate::cli::DebugLevel>,
    ) -> Result<Vec<Conversation>>;

    /// Stream-load all conversations (background thread)
    fn load_all_conversations_streaming(
        &self,
        show_last: bool,
        debug_level: Option<crate::cli::DebugLevel>,
    ) -> Sender<crate::history::LoaderMessage>;

    /// Get the path encoder for this provider
    fn path_encoder(&self) -> &dyn PathEncoder;

    /// Get the conversation parser for this provider
    fn parser(&self) -> &dyn ConversationParser;

    /// Get the CLI command name for this provider
    fn cli_command(&self) -> &str;

    /// Build a resume command for a conversation
    fn build_resume_command(
        &self,
        conversation: &Conversation,
        fork: bool,
        default_args: &[String],
    ) -> Result<ResumeAction>;

    /// Delete a session by UUID
    fn delete_session(&self, uuid: &str) -> Result<usize>;

    /// Find a JSONL file by UUID across all projects
    fn find_jsonl_by_uuid(&self, uuid: &str) -> Result<Option<PathBuf>>;

    /// Get provider-specific search text from a conversation
    /// (for agent protocol, semantic search, etc.)
    fn agent_search_text(&self, conversation: &Conversation) -> String {
        conversation.agent_search_text.clone()
    }

    /// Whether this provider supports resume
    fn supports_resume(&self) -> bool {
        true
    }

    /// Whether this provider supports fork
    fn supports_fork(&self) -> bool {
        false
    }
}
```

## Provider Registry

```rust
// src/provider/registry.rs

use std::sync::Arc;

pub struct ProviderRegistry {
    providers: Vec<Arc<dyn Provider>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        self.providers.push(provider);
    }

    pub fn providers(&self) -> &[Arc<dyn Provider>] {
        &self.providers
    }

    pub fn by_kind(&self, kind: ProviderKind) -> Option<&Arc<dyn Provider>> {
        self.providers.iter().find(|p| p.kind() == kind)
    }

    /// Detect which providers are installed and register them
    pub fn detect_all() -> Self {
        let mut registry = Self::new();

        // Try each provider, skip if not installed
        if let Ok(p) = ClaudeProvider::try_new() {
            registry.register(Arc::new(p));
        }
        if let Ok(p) = CodexProvider::try_new() {
            registry.register(Arc::new(p));
        }
        if let Ok(p) = CrushProvider::try_new() {
            registry.register(Arc::new(p));
        }
        if let Ok(p) = MiMoProvider::try_new() {
            registry.register(Arc::new(p));
        }

        registry
    }
}
```

## Data Directory Mapping

| Provider | Data Root | Projects Dir | Config |
|----------|-----------|--------------|--------|
| Claude | `~/.claude/` | `~/.claude/projects/<encoded>/` | `CLAUDE_CONFIG_DIR` env |
| Codex | `~/.codex/` | `~/.codex/` (SQLite-based) | — |
| Crush | `~/.local/share/crush/` | `~/.local/share/crush/crush.db` | `~/.config/crush/crush.json` |
| MiMo | `~/.local/share/mimocode/` | `~/.local/share/mimocode/` (SQLite+FTS5) | `.mimocode/mimocode.jsonc` |

## Storage Format Details

### Claude Code (JSONL)
- Files: `~/.claude/projects/<encoded>/<uuid>.jsonl`
- Format: One JSON object per line (`LogEntry` enum)
- Entry types: `user`, `assistant`, `summary`, `ai-title`, `custom-title`, `progress`
- Session metadata: cwd in user messages, model in assistant messages
- Resume: `claude --resume <uuid>` (requires cd to project dir)

### Codex (JSONL + SQLite)
- History: `codex-rollout` JSONL files (different schema from Claude)
- Metadata: SQLite state database in `~/.codex/`
- Thread model: `ThreadId` + `SessionId` (not UUID-based)
- Resume: `codex resume` CLI command
- Key types: `RolloutItem`, `StoredThread`, `ThreadMetadataPatch`

### Crush (SQLite)
- Database: `~/.local/share/crush/crush.db`
- Tables: sessions, messages (via sqlc queries)
- Session fields: id, title, message_count, prompt_tokens, completion_tokens, cost, todos
- No CLI resume command

### MiMo Code (SQLite + FTS5)
- Database: `~/.local/share/mimocode/` (SQLite)
- Inherits OpenCode's storage schema
- Adds: memory system (MEMORY.md, checkpoint.md, notes.md, tasks/)
- FTS5 for full-text search across memory
- No CLI resume command

## Migration Plan

### Phase 1: Trait Extraction (No behavior change)
1. Create `src/provider/mod.rs` with `Provider`, `PathEncoder`, `ConversationParser` traits
2. Create `src/provider/claude.rs` implementing traits for Claude Code
3. Move existing `history/` code into `provider/claude.rs` or keep as-is behind the trait
4. Create `src/provider/registry.rs` with `ProviderRegistry`
5. Update `main.rs` to use registry instead of direct function calls

### Phase 2: Codex Support
1. Create `src/provider/codex.rs` implementing traits for Codex
2. Implement `CodexParser` (different JSONL schema from Claude)
3. Implement `CodexPathEncoder` (if applicable, Codex uses SQLite not directory-based)
4. Implement resume via `codex resume` CLI

### Phase 3: SQLite Providers (Crush + MiMo)
1. Add `rusqlite` dependency
2. Create `src/provider/sqlite_base.rs` with shared SQLite query logic
3. Create `src/provider/crush.rs` implementing traits for Crush
4. Create `src/provider/mimo.rs` implementing traits for MiMo Code
5. Both share the OpenCode-derived schema

### Phase 4: Multi-Provider TUI
1. Add provider selector to TUI (Tab to switch scope, or new keybinding)
2. Show provider name in conversation list
3. Filter by provider in search

## Key Design Decisions

### Why not a single generic parser?
Each tool has fundamentally different storage:
- Claude: JSONL with custom encoding
- Codex: JSONL + SQLite (different schema)
- Crush/MiMo: Pure SQLite

A generic parser would need to handle all formats, becoming more complex than specialized parsers. Instead, each provider implements its own parser behind a common trait.

### Why keep `Conversation` as a unified type?
The TUI, search, and semantic features all operate on `Conversation`. Converting between provider-specific types and a unified type at the boundary keeps the rest of the codebase clean. Provider-specific fields (like Claude's `cwd` extraction) are handled during parsing.

### Why a registry pattern?
The registry allows:
- Runtime detection of installed providers
- Adding new providers without modifying existing code
- Filtering conversations by provider in the TUI
- Provider-specific CLI flags (e.g., `--provider codex`)

## Open Questions

1. **SQLite dependency**: Adding `rusqlite` increases binary size. Consider making SQLite providers optional via cargo features.
2. **Resume for non-Claude providers**: Crush/MiMo don't have CLI resume. Options:
   - Skip resume for these providers (show read-only)
   - Implement resume by spawning the tool with session file path
   - Add resume support upstream
3. **Session ID format**: Claude uses UUID, Codex uses ThreadId, Crush uses UUID. The `AgentConversationRef` system needs to handle all formats.
4. **Config merging**: Each provider has its own config format. The `claude-history` config should remain separate from provider configs.
