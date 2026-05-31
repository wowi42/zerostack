use std::collections::HashMap;
use std::path::{Path, PathBuf};

use include_dir::Dir;
use smallvec::SmallVec;

use crate::session::storage;

pub mod prompts;
pub mod themes;

pub(crate) fn load_embedded_files(embedded: &Dir, ext: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    for file in embedded.files() {
        if file.path().extension().is_some_and(|e| e == ext)
            && let Some(name) = file.path().file_stem().and_then(|s| s.to_str())
            && let Some(content) = file.contents_utf8()
        {
            results.push((name.to_string(), content.to_string()));
        }
    }
    results
}

pub(crate) fn load_dir_files(dir: &Path, ext: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    if dir.exists()
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == ext)
                && let Some(name) = path.file_stem().and_then(|s| s.to_str())
                && let Ok(content) = std::fs::read_to_string(&path)
            {
                results.push((name.to_string(), content));
            }
        }
    }
    results
}

pub(crate) fn copy_embedded_to(embedded: &Dir, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    for file in embedded.files() {
        if let Some(name) = file.path().file_name().and_then(|s| s.to_str()) {
            let dest_path = dest.join(name);
            if let Some(content) = file.contents_utf8() {
                std::fs::write(&dest_path, content)?;
            }
        }
    }
    Ok(())
}

pub struct ContextFiles {
    pub agents: Option<String>,
    pub prompts: HashMap<String, String>,
    pub current_prompt: Option<String>,
    pub current_prompt_name: Option<String>,
    pub themes: HashMap<String, String>,
    pub current_theme_name: Option<String>,
    #[cfg(feature = "memory")]
    pub memory: Option<String>,
}

impl ContextFiles {
    #[allow(dead_code)]
    pub fn reload(&mut self) {
        self.agents = load_agents();
        self.prompts = prompts::load();
        if let Some(name) = &self.current_prompt_name {
            self.current_prompt = self.prompts.get(name).cloned();
        }
        self.themes = themes::load();
        // Reload persisted theme name from disk
        self.current_theme_name = crate::session::storage::load_theme_name();
        #[cfg(feature = "memory")]
        {
            self.memory = crate::agent::memory::Mem::open().context_block();
        }
    }
}

pub fn load(no_context_files: bool) -> ContextFiles {
    let _ = prompts::ensure_global();
    let _ = themes::ensure_global();
    let agents = if no_context_files {
        None
    } else {
        load_agents()
    };
    let prompt_map = prompts::load();
    let theme_map = themes::load();
    let theme_name = crate::session::storage::load_theme_name();
    #[cfg(feature = "memory")]
    let memory = crate::agent::memory::Mem::open().context_block();
    ContextFiles {
        agents,
        prompts: prompt_map,
        current_prompt: None,
        current_prompt_name: None,
        themes: theme_map,
        current_theme_name: theme_name,
        #[cfg(feature = "memory")]
        memory,
    }
}

fn load_file(path: &PathBuf) -> Option<String> {
    if path.exists() {
        std::fs::read_to_string(path).ok()
    } else {
        None
    }
}

fn load_agents() -> Option<String> {
    let mut parts: SmallVec<[String; 4]> = SmallVec::new();

    let global = storage::agents_path();
    if let Some(content) = load_file(&global)
        && !content.trim().is_empty()
    {
        parts.push(format!("# Global AGENTS.md\n{}", content));
    }

    let cwd = std::env::current_dir().ok();
    if let Some(cwd) = cwd {
        let mut current = Some(cwd.as_path());
        while let Some(dir) = current {
            for name in &["AGENTS.md", "CLAUDE.md"] {
                let path = dir.join(name);
                if let Some(content) = load_file(&path)
                    && !content.trim().is_empty()
                {
                    parts.push(format!("# {} ({})\n{}", name, dir.display(), content));
                }
            }
            current = dir.parent();
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}
