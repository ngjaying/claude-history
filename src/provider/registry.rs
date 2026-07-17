use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use crate::cli::DebugLevel;
use crate::error::Result;
use crate::history::{Conversation, LoaderMessage};

use super::{
    Provider, ProviderKind,
    agy::AgyProvider,
    claude::ClaudeProvider,
    codex::CodexProvider,
    mimo::MiMoProvider,
    opencode::OpenCodeProvider,
};

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

    pub fn detect_all() -> Self {
        let mut registry = Self::new();

        // Codex first (user prefers to see Codex sessions first)
        if let Ok(p) = CodexProvider::try_new() {
            registry.register(Arc::new(p));
        }
        if let Ok(p) = ClaudeProvider::try_new() {
            registry.register(Arc::new(p));
        }
        if let Ok(p) = MiMoProvider::try_new() {
            registry.register(Arc::new(p));
        }
        if let Ok(p) = OpenCodeProvider::try_new() {
            registry.register(Arc::new(p));
        }
        if let Ok(p) = AgyProvider::try_new() {
            registry.register(Arc::new(p));
        }

        registry
    }

    /// Load all conversations from all providers synchronously.
    pub fn load_all(
        &self,
        show_last: bool,
        debug_level: Option<DebugLevel>,
    ) -> Result<Vec<Conversation>> {
        let mut all = Vec::new();
        for provider in &self.providers {
            match provider.load_all_conversations_streaming(show_last, debug_level) {
                rx => {
                    // Drain the receiver
                    while let Ok(msg) = rx.recv() {
                        match msg {
                            LoaderMessage::Batch(convs) => all.extend(convs),
                            LoaderMessage::Fatal(e) => return Err(e),
                            LoaderMessage::Done => break,
                            LoaderMessage::ProjectError => {}
                        }
                    }
                }
            }
        }
        all.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        Ok(all)
    }

    /// Load all conversations from all providers via streaming (background threads).
    /// Returns a merged receiver that combines messages from all providers.
    pub fn load_all_streaming(
        &self,
        show_last: bool,
        debug_level: Option<DebugLevel>,
    ) -> Receiver<LoaderMessage> {
        let (merged_tx, merged_rx) = std::sync::mpsc::channel();

        for provider in &self.providers {
            let rx = provider.load_all_conversations_streaming(show_last, debug_level);
            let tx = merged_tx.clone();
            std::thread::spawn(move || {
                while let Ok(msg) = rx.recv() {
                    if tx.send(msg).is_err() {
                        break;
                    }
                }
            });
        }

        // Drop the original sender so merged_rx closes when all threads finish
        drop(merged_tx);

        merged_rx
    }

    /// Delete a session across all providers.
    pub fn delete_session(&self, uuid: &str) -> Result<usize> {
        let mut total = 0;
        for provider in &self.providers {
            match provider.delete_session(uuid) {
                Ok(count) => total += count,
                Err(crate::error::AppError::SessionNotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(total)
    }

    /// Find a JSONL file by UUID across all providers.
    pub fn find_jsonl_by_uuid(&self, uuid: &str) -> Result<Option<PathBuf>> {
        for provider in &self.providers {
            if let Some(path) = provider.find_jsonl_by_uuid(uuid)? {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    /// Get the projects directory for a given working directory from the first provider.
    pub fn projects_dir(&self, cwd: &Path) -> Result<PathBuf> {
        self.providers
            .first()
            .ok_or_else(|| crate::error::AppError::NoHistoryFound("no providers installed".to_string()))?
            .projects_dir(cwd)
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::detect_all()
    }
}
