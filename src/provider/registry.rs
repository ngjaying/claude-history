use std::sync::Arc;

use super::{Provider, ProviderKind, claude::ClaudeProvider};

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

        if let Ok(p) = ClaudeProvider::try_new() {
            registry.register(Arc::new(p));
        }

        registry
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::detect_all()
    }
}
