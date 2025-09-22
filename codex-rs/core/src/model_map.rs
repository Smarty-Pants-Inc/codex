use once_cell::sync::Lazy;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use model_id::{family_or_slug, normalize};

static EMBEDDED: &str = include_str!("../models_map.toml");

static MODEL_MAP: Lazy<ModelMap> = Lazy::new(ModelMap::load);

#[derive(Debug, Clone, Deserialize, Default)]
struct RawModelMap {
    #[serde(default)]
    family: HashMap<String, ModelMetadata>,
    #[serde(default)]
    slug: HashMap<String, ModelMetadata>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ModelMetadata {
    pub context_window: u64,
    pub max_output_tokens: u64,
}

#[derive(Debug, Clone, Default)]
pub struct ModelMap {
    family: HashMap<String, ModelMetadata>,
    slug: HashMap<String, ModelMetadata>,
}

impl ModelMap {
    fn load() -> Self {
        let mut map = Self::from_toml_str(EMBEDDED);

        if let Some(path) = env_override_path() {
            if let Ok(contents) = fs::read_to_string(&path) {
                let override_map = Self::from_toml_str(&contents);
                map.merge(override_map);
            }
        } else if let Some(home) = dirs::home_dir() {
            let path = home.join(".codex/models.toml");
            if let Ok(contents) = fs::read_to_string(path) {
                let override_map = Self::from_toml_str(&contents);
                map.merge(override_map);
            }
        }

        map
    }

    fn from_toml_str(s: &str) -> Self {
        match toml::from_str::<RawModelMap>(s) {
            Ok(raw) => Self {
                family: raw.family,
                slug: raw.slug,
            },
            Err(err) => {
                tracing::warn!(error = %err, "failed to parse model metadata map");
                Self::default()
            }
        }
    }

    fn merge(&mut self, other: Self) {
        self.family.extend(other.family);
        self.slug.extend(other.slug);
    }

    pub fn metadata_for_slug(&self, slug: &str) -> Option<&ModelMetadata> {
        let normalized = normalize(slug);
        if let Some(meta) = self.slug.get(&normalized) {
            return Some(meta);
        }
        let family = family_or_slug(&normalized);
        self.family.get(family.as_ref())
    }

    pub fn get() -> &'static Self {
        &MODEL_MAP
    }
}

fn env_override_path() -> Option<std::path::PathBuf> {
    for key in ["CODEX_MODELS_TOML", "CODE_MODELS_TOML"] {
        if let Ok(value) = std::env::var(key) {
            let path = Path::new(&value);
            if path.exists() {
                return Some(path.to_path_buf());
            }
        }
    }
    None
}

pub fn context_window_for_slug(slug: &str) -> Option<u64> {
    ModelMap::get().metadata_for_slug(slug).map(|m| m.context_window)
}

pub fn max_output_tokens_for_slug(slug: &str) -> Option<u64> {
    ModelMap::get()
        .metadata_for_slug(slug)
        .map(|m| m.max_output_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_map_loads() {
        let map = ModelMap::get();
        assert!(map.metadata_for_slug("gpt-5").is_some());
    }

    #[test]
    fn slug_lookup_falls_back_to_family() {
        let map = ModelMap::get();
        assert!(map.metadata_for_slug("gpt-4o-2024-08-06").is_some());
    }
}
