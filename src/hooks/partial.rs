//! Hybrid partials hooks
//!
//! How Partials Work in liquid 0.26
//!
//! `ParserBuilder::partials()`` expects a PartialCompiler,
//! such as `EagerCompiler<T>`` or `LazyCompiler<T>``, where T implements `liquid::partials::PartialSource`.
//!
//! - `EagerCompiler`: Compiles partials once when `build()` is called (fast, ideal for known folders).
//! - `LazyCompiler`: Loads and compiles partials on demand during `parser.parse()` (ideal for dynamic JS callbacks or databases).

use liquid::partials::PartialSource;
use napi::bindgen_prelude::*;
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;
use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[napi(string_enum)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourcePriority {
    /// Check the custom JS partial loader first, then fall back to disk.
    LoaderFirst,
    /// Check the local file system directory first, then fall back to the JS loader.
    FileSystemFirst,
}

pub type SourceLoaderArgs = FnArgs<(String,)>;
pub type SourceLoaderReturn = Promise<Option<String>>;
pub type SourceLoaderCallback = ThreadsafeFunction<
    SourceLoaderArgs,
    SourceLoaderReturn,
    SourceLoaderArgs,
    Status,
    false,
    true,
    0,
>;

pub struct HybridPartialSource {
    pub loader: Option<Arc<SourceLoaderCallback>>,
    pub partials_dir: Option<PathBuf>,
    pub priority: SourcePriority,
    pub extensions: Vec<String>,
}

impl std::fmt::Debug for HybridPartialSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HybridPartialSource")
            .field("has_loader", &self.loader.is_some())
            .field("partials_dir", &self.partials_dir)
            .field("priority", &self.priority)
            .field("extensions", &self.extensions)
            .finish()
    }
}

impl HybridPartialSource {
    fn try_loader(&self, name: &str) -> Option<String> {
        let loader = self.loader.as_ref()?;
        let name_owned = name.to_string();

        tokio::runtime::Handle::current().block_on(async {
            let promise = loader.call_async(FnArgs::from((name_owned,))).await.ok()?;
            promise.await.ok().flatten()
        })
    }

    fn resolve_safe_path(&self, base_dir: &Path, name: &str) -> Option<PathBuf> {
        let rel_path = Path::new(name);
        // Block path traversal attacks (e.g., {% include "../secret" %})
        if rel_path.is_absolute()
            || rel_path
                .components()
                .any(|c| matches!(c, Component::ParentDir))
        {
            return None;
        }

        let base_path = base_dir.join(rel_path);

        // 1. If the include already has an extension (e.g., "header.html"), use it directly
        if base_path.extension().is_some() {
            return Some(base_path);
        }

        // 2. Otherwise check configured extensions in order (e.g., ["liquid", "html"])
        self.extensions
            .iter()
            .map(|ext| base_path.with_extension(ext))
            .find(|candidate| candidate.is_file())
            .or_else(|| {
                let fallback_ext = self
                    .extensions
                    .first()
                    .map(String::as_str)
                    .unwrap_or("html");
                Some(base_path.with_extension(fallback_ext))
            })
    }
    fn try_fs(&self, name: &str) -> Option<String> {
        let base_dir = self.partials_dir.as_ref()?;
        let full_path = self.resolve_safe_path(base_dir, name)?;
        std::fs::read_to_string(full_path).ok()
    }
}

impl PartialSource for HybridPartialSource {
    fn contains(&self, name: &str) -> bool {
        if self.loader.is_some() {
            return true;
        }
        if let Some(base_dir) = &self.partials_dir {
            if let Some(full_path) = self.resolve_safe_path(base_dir, name) {
                return full_path.is_file();
            }
        }
        false
    }

    fn names(&self) -> Vec<&str> {
        Vec::new()
    }

    fn try_get<'a>(&'a self, name: &str) -> Option<Cow<'a, str>> {
        let content = match self.priority {
            SourcePriority::LoaderFirst => self.try_loader(name).or_else(|| self.try_fs(name)),
            SourcePriority::FileSystemFirst => self.try_fs(name).or_else(|| self.try_loader(name)),
        }?;

        Some(Cow::Owned(content))
    }
}
