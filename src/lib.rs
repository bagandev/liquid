//! Liquid template rendering for Node.js, backed by the Rust `liquid` crate.
//!
//! [`LiquidEngine`] supports custom filters, tags, blocks, and partial sources.
//! Templates are parsed lazily and cached; changing the engine's registrations
//! invalidates the parser and template cache.

mod front_matter;
mod hooks;
pub mod split_matter;

use hooks::{
    block::{LiquidBlock, LiquidBlockArgs, LiquidBlockReturn},
    filter::{LiquidFilter, LiquidFilterArgs, LiquidFilterReturn},
    partial::{
        HybridPartialSource, SourceLoaderArgs, SourceLoaderCallback, SourceLoaderReturn,
        SourcePriority,
    },
    tag::{LiquidTag, LiquidTagArgs, LiquidTagReturn},
};

use front_matter::front_matter;
// use matter::extract_layout_and_body;

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};

use liquid::ParserBuilder;
use liquid::partials::LazyCompiler;
use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::LiquidExtension::Html;

#[napi(string_enum)]
#[derive(Debug, Clone)]
/// File extensions supported when resolving templates from the filesystem.
pub enum LiquidExtension {
    /// The `html` extension.
    Html,
    /// The `liquid` extension.
    Liquid,
}

impl LiquidExtension {
    /// Returns this extension without a leading dot.
    pub fn to_str(self) -> &'static str {
        match self {
            Html => "html",
            LiquidExtension::Liquid => "liquid",
        }
    }
}

#[napi]
/// A Liquid renderer that can be extended with JavaScript callbacks.
///
/// Create an engine, optionally register custom behavior or partial sources,
/// then call [`render`](LiquidEngine::render) with a template and JSON context.
pub struct LiquidEngine {
    config: RwLock<EngineConfig>,

    // Lazily compiled parser and template cache
    parser: Arc<RwLock<Option<Arc<liquid::Parser>>>>,
    cache: Arc<RwLock<HashMap<u64, Arc<liquid::Template>>>>,
}

struct EngineConfig {
    filters: Vec<LiquidFilter>,
    tags: Vec<LiquidTag>,
    blocks: Vec<LiquidBlock>,
    // Layouts
    layouts_dir: Option<String>,
    layouts_extensions: Vec<String>,
    layouts_loader: Option<Arc<SourceLoaderCallback>>,
    layouts_priority: SourcePriority,

    // Includes
    partials_dir: Option<String>,
    partial_extensions: Vec<String>,
    partial_loader: Option<Arc<SourceLoaderCallback>>,
    partial_priority: SourcePriority,
}

impl EngineConfig {
    fn new(priority: SourcePriority) -> Self {
        Self {
            filters: Vec::new(),
            tags: Vec::new(),
            blocks: Vec::new(),
            // Layouts
            layouts_dir: None,
            layouts_loader: None,
            layouts_priority: priority,
            layouts_extensions: vec!["liquid".to_string(), "html".to_string()],
            // Includes
            partials_dir: None,
            partial_loader: None,
            partial_priority: priority,
            partial_extensions: vec!["liquid".to_string(), "html".to_string()],
        }
    }
}

impl LiquidEngine {
    fn try_layout_loader(
        loader: &Option<Arc<SourceLoaderCallback>>,
        layout_name: &str,
    ) -> Option<String> {
        let loader_fn = loader.as_ref()?;
        let name_owned = layout_name.to_string();

        tokio::runtime::Handle::current().block_on(async {
            let promise = loader_fn
                .call_async(FnArgs::from((name_owned,)))
                .await
                .ok()?;
            promise.await.ok().flatten()
        })
    }
    fn try_layout_fs(
        layouts_dir: &Option<String>,
        extensions: &[String],
        layout_name: &str,
    ) -> napi::Result<Option<String>> {
        let Some(base_dir) = layouts_dir.as_ref() else {
            return Ok(None);
        };

        let rel_path = Path::new(layout_name);
        if rel_path.is_absolute()
            || rel_path
                .components()
                .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(napi::Error::from_reason(format!(
                "Invalid layout path: {}",
                layout_name
            )));
        }

        let base_path = Path::new(base_dir).join(rel_path);

        let resolved_path = if base_path.extension().is_some() {
            base_path
        } else {
            extensions
                .iter()
                .map(|ext| base_path.with_extension(ext))
                .find(|candidate| candidate.is_file())
                .unwrap_or_else(|| {
                    let fallback_ext = extensions.first().map(String::as_str).unwrap_or("liquid");
                    base_path.with_extension(fallback_ext)
                })
        };

        if !resolved_path.is_file() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&resolved_path).map_err(|e| {
            napi::Error::from_reason(format!(
                "Failed to read layout '{}': {}",
                resolved_path.display(),
                e
            ))
        })?;

        Ok(Some(content))
    }
    fn load_layout_source(
        layouts_dir: &Option<String>,
        layout_loader: &Option<Arc<SourceLoaderCallback>>,
        priority: SourcePriority,
        extensions: &[String],
        layout_name: &str,
    ) -> napi::Result<String> {
        if layouts_dir.is_none() && layout_loader.is_none() {
            return Err(napi::Error::from_reason(format!(
                "Template specifies layout '{}', but neither setLayoutsDir() nor registerLayoutLoader() was called",
                layout_name
            )));
        }

        let content = match priority {
            SourcePriority::LoaderFirst => {
                if let Some(src) = Self::try_layout_loader(layout_loader, layout_name) {
                    Some(src)
                } else {
                    Self::try_layout_fs(layouts_dir, extensions, layout_name)?
                }
            }
            SourcePriority::FileSystemFirst => {
                if let Some(src) = Self::try_layout_fs(layouts_dir, extensions, layout_name)? {
                    Some(src)
                } else {
                    Self::try_layout_loader(layout_loader, layout_name)
                }
            }
        };

        content.ok_or_else(|| {
            napi::Error::from_reason(format!("Layout '{}' could not be found", layout_name))
        })
    }
    fn render_cached(
        parser: &liquid::Parser,
        cache: &RwLock<HashMap<u64, Arc<liquid::Template>>>,
        source: &str,
        globals: &liquid::Object,
    ) -> napi::Result<String> {
        let cache_key = Self::hash_template(source);

        let template = {
            let read_guard = cache
                .read()
                .map_err(|_| napi::Error::from_reason("Template cache lock poisoned"))?;
            read_guard.get(&cache_key).cloned()
        };

        let template = match template {
            Some(cached) => cached,
            None => {
                let parsed = parser
                    .parse(source)
                    .map_err(|e| napi::Error::from_reason(e.to_string()))?;
                let arc_template = Arc::new(parsed);

                let mut write_guard = cache
                    .write()
                    .map_err(|_| napi::Error::from_reason("Template cache lock poisoned"))?;
                write_guard.insert(cache_key, Arc::clone(&arc_template));
                arc_template
            }
        };

        template
            .render(globals)
            .map_err(|e| napi::Error::from_reason(e.to_string()))
    }
    /// Invalidates the compiled parser and template cache when plugins change.
    fn invalidate(&self) -> napi::Result<()> {
        let mut parser_guard = self
            .parser
            .write()
            .map_err(|_| napi::Error::from_reason("Parser lock poisoned"))?;
        *parser_guard = None;
        drop(parser_guard);

        self.cache
            .write()
            .map_err(|_| napi::Error::from_reason("Template cache lock poisoned"))?
            .clear();
        Ok(())
    }
    /// Returns the active parser, building it lazily if needed.
    fn get_or_build_parser(&self) -> napi::Result<Arc<liquid::Parser>> {
        let config = self
            .config
            .read()
            .map_err(|_| napi::Error::from_reason("Engine config lock poisoned"))?;

        // 1. Fast path: return already-built parser
        {
            let read_guard = self
                .parser
                .read()
                .map_err(|_| napi::Error::from_reason("Parser lock poisoned"))?;
            if let Some(existing) = read_guard.as_ref() {
                return Ok(Arc::clone(existing));
            }
        }

        // 2. Slow path: build parser from current plugins
        let mut builder = ParserBuilder::with_stdlib();

        for filter in &config.filters {
            builder = builder.filter(filter.clone());
        }
        for tag in &config.tags {
            builder = builder.tag(tag.clone());
        }
        for block in &config.blocks {
            builder = builder.block(block.clone());
        }

        // Build with or without partials without reassigning across different generic types
        let parser = if config.partials_dir.is_some() || config.partial_loader.is_some() {
            let source = HybridPartialSource {
                loader: config.partial_loader.clone(),
                partials_dir: config.partials_dir.as_ref().map(PathBuf::from),
                priority: config.partial_priority,
                extensions: config.partial_extensions.clone(),
            };

            builder
                .partials(LazyCompiler::new(source))
                .build()
                .map_err(|e| napi::Error::from_reason(e.to_string()))?
        } else {
            builder
                .build()
                .map_err(|e| napi::Error::from_reason(e.to_string()))?
        };

        let built = Arc::new(parser);

        let mut write_guard = self
            .parser
            .write()
            .map_err(|_| napi::Error::from_reason("Parser lock poisoned"))?;
        *write_guard = Some(Arc::clone(&built));

        Ok(built)
    }

    fn hash_template(template_str: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        template_str.hash(&mut hasher);
        hasher.finish()
    }
}

#[napi]
impl LiquidEngine {
    /// Creates an engine with the standard Liquid filters and tags.
    ///
    /// # Arguments
    ///
    /// * `include_priority` - Determines whether the partial loader or the
    ///   filesystem is checked first when both are configured. Defaults to
    ///   `LoaderFirst`.
    #[napi(constructor)]
    pub fn new(
        #[napi(ts_arg_type = "\"LoaderFirst\" | \"FileSystemFirst\"")] include_priority: Option<
            SourcePriority,
        >,
    ) -> Self {
        Self {
            config: RwLock::new(EngineConfig::new(
                include_priority.unwrap_or(SourcePriority::LoaderFirst),
            )),
            parser: Arc::new(RwLock::new(None)),
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    /// Registers a JavaScript callback as a Liquid filter.
    ///
    /// The callback receives the input value followed by an array of
    /// positional arguments, and may return a value or a promise resolving to
    /// a value. Registering the filter invalidates the parser and template
    /// cache.
    ///
    /// # Arguments
    ///
    /// * `this` - Engine reference returned for chaining.
    /// * `name` - Name used to invoke the filter in a Liquid template.
    /// * `callback` - Function that transforms the input value.
    ///
    /// # Errors
    ///
    /// Returns an error if the callback cannot be converted to a
    /// thread-safe function.
    #[napi]
    pub fn register_filter(
        &self,
        this: Reference<LiquidEngine>,
        name: String,
        #[napi(ts_arg_type = "(input:any,args:any[]) => Promise<any>")] callback: Function<
            'static,
            LiquidFilterArgs,
            LiquidFilterReturn,
        >,
    ) -> napi::Result<Reference<LiquidEngine>> {
        let tsfn = callback
            .build_threadsafe_function()
            .callee_handled::<false>()
            .weak::<true>()
            .build()?;

        let static_name: &'static str = Box::leak(name.into_boxed_str());
        let mut config = self
            .config
            .write()
            .map_err(|_| napi::Error::from_reason("Engine config lock poisoned"))?;
        config.filters.push(LiquidFilter {
            name: static_name,
            callback: Arc::new(tsfn),
        });

        self.invalidate()?;
        Ok(this)
    }
    /// Registers a JavaScript callback as a Liquid tag.
    ///
    /// The callback receives an array of evaluated tag arguments and may
    /// return a string or a promise resolving to one. Registering the tag
    /// invalidates the parser and template cache.
    ///
    /// # Arguments
    ///
    /// * `this` - Engine reference returned for chaining.
    /// * `name` - Tag name used in Liquid templates.
    /// * `callback` - Function that produces the tag's rendered output.
    ///
    /// # Errors
    ///
    /// Returns an error if the callback cannot be converted to a
    /// thread-safe function.
    #[napi]
    pub fn register_tag(
        &self,
        this: Reference<LiquidEngine>,
        name: String,
        #[napi(ts_arg_type = "(args:any[]) => Promise<string> | string")] callback: Function<
            'static,
            LiquidTagArgs,
            LiquidTagReturn,
        >,
    ) -> napi::Result<Reference<LiquidEngine>> {
        let tsfn = callback
            .build_threadsafe_function()
            .callee_handled::<false>()
            .weak::<true>()
            .build()?;

        let static_name: &'static str = Box::leak(name.into_boxed_str());
        let mut config = self
            .config
            .write()
            .map_err(|_| napi::Error::from_reason("Engine config lock poisoned"))?;
        config.tags.push(LiquidTag {
            name: static_name,
            callback: Arc::new(tsfn),
        });

        self.invalidate()?;
        Ok(this)
    }
    /// Registers a JavaScript callback as a Liquid block.
    ///
    /// The callback receives the rendered block body and an array of evaluated
    /// arguments, and may return a string or a promise resolving to one. The
    /// closing tag is named `end{name}`. Registering the block invalidates the
    /// parser and template cache.
    ///
    /// # Arguments
    ///
    /// * `this` - Engine reference returned for chaining.
    /// * `name` - Opening block tag name.
    /// * `callback` - Function that transforms the rendered body.
    ///
    /// # Errors
    ///
    /// Returns an error if the callback cannot be converted to a
    /// thread-safe function.
    #[napi]
    pub fn register_block(
        &self,
        this: Reference<LiquidEngine>,
        name: String,
        #[napi(ts_arg_type = "(body:string,args:any[]) => Promise<string> | string")]
        callback: Function<'static, LiquidBlockArgs, LiquidBlockReturn>,
    ) -> napi::Result<Reference<LiquidEngine>> {
        let tsfn = callback
            .build_threadsafe_function()
            .callee_handled::<false>()
            .weak::<true>()
            .build()?;

        let end_name = format!("end{}", name);
        let start_tag: &'static str = Box::leak(name.into_boxed_str());
        let end_tag: &'static str = Box::leak(end_name.into_boxed_str());

        let mut config = self
            .config
            .write()
            .map_err(|_| napi::Error::from_reason("Engine config lock poisoned"))?;
        config.blocks.push(LiquidBlock {
            start_tag,
            end_tag,
            callback: Arc::new(tsfn),
        });

        self.invalidate()?;
        Ok(this)
    }
    /// Sets the directory used to load partial templates from the filesystem.
    ///
    /// Partial names are resolved relative to this directory. If a custom
    /// partial loader is also registered, the configured include priority
    /// determines which source is tried first. This invalidates the parser and
    /// template cache.
    ///
    /// # Arguments
    ///
    /// * `this` - Engine reference returned for chaining.
    /// * `dir` - Filesystem directory containing partial templates.
    #[napi]
    pub fn set_partials_dir(
        &self,
        this: Reference<LiquidEngine>,
        dir: String,
    ) -> napi::Result<Reference<LiquidEngine>> {
        let mut config = self
            .config
            .write()
            .map_err(|_| napi::Error::from_reason("Engine config lock poisoned"))?;
        config.partials_dir = Some(dir);
        self.invalidate()?;
        Ok(this)
    }
    /// Sets the directory used to load layout templates from the filesystem.
    ///
    /// Layout names are resolved relative to this directory. If a custom
    /// layout loader is also registered, the layout priority determines which
    /// source is tried first.
    ///
    /// # Arguments
    ///
    /// * `this` - Engine reference returned for chaining.
    /// * `dir` - Filesystem directory containing layout templates.
    #[napi]
    pub fn set_layouts_dir(
        &self,
        this: Reference<LiquidEngine>,
        dir: String,
    ) -> napi::Result<Reference<LiquidEngine>> {
        let mut config = self
            .config
            .write()
            .map_err(|_| napi::Error::from_reason("Engine config lock poisoned"))?;
        config.layouts_dir = Some(dir);
        Ok(this)
    }
    /// Sets the extensions tried when resolving layouts without an extension.
    ///
    /// Leading dots are removed from each extension. The default order is
    /// `liquid`, then `html`. An empty list leaves no fallback extension.
    ///
    /// # Arguments
    ///
    /// * `this` - Engine reference returned for chaining.
    /// * `extensions` - Extensions to try, in priority order.
    #[napi]
    pub fn set_layout_extensions(
        &self,
        this: Reference<LiquidEngine>,
        extensions: Vec<String>,
    ) -> napi::Result<Reference<LiquidEngine>> {
        // Strip leading dots if user passes ".html" instead of "html"
        let cleaned: Vec<String> = extensions
            .into_iter()
            .map(|ext| ext.trim_start_matches('.').to_string())
            .filter(|ext| !ext.is_empty())
            .collect();

        let mut config = self
            .config
            .write()
            .map_err(|_| napi::Error::from_reason("Engine config lock poisoned"))?;

        config.layouts_extensions = cleaned;

        Ok(this)
    }
    /// Sets the extensions tried when resolving partials without an extension.
    ///
    /// Leading dots are removed from each extension. The default order is
    /// `liquid`, then `html`. Changing the extensions invalidates the parser
    /// and template cache.
    ///
    /// # Arguments
    ///
    /// * `this` - Engine reference returned for chaining.
    /// * `extensions` - Extensions to try, in priority order.
    #[napi]
    pub fn set_partial_extensions(
        &self,
        this: Reference<LiquidEngine>,
        extensions: Vec<String>,
    ) -> napi::Result<Reference<LiquidEngine>> {
        let cleaned: Vec<String> = extensions
            .into_iter()
            .map(|ext| ext.trim_start_matches('.').to_string())
            .filter(|ext| !ext.is_empty())
            .collect();

        let mut config = self
            .config
            .write()
            .map_err(|_| napi::Error::from_reason("Engine config lock poisoned"))?;

        config.partial_extensions = cleaned;

        self.invalidate()?;
        Ok(this)
    }
    /// Registers a JavaScript callback to load partial templates.
    ///
    /// The callback receives a partial name and may resolve to the partial's
    /// source text or `None` if it is not available. If a filesystem directory
    /// is also configured, the include priority determines which source is
    /// tried first. Registering the loader invalidates the parser and template
    /// cache.
    ///
    /// # Arguments
    ///
    /// * `this` - Engine reference returned for chaining.
    /// * `callback` - Async function that looks up partial source by name.
    ///
    /// # Errors
    ///
    /// Returns an error if the callback cannot be converted to a
    /// thread-safe function.
    #[napi]
    pub fn register_partial_loader(
        &self,
        this: Reference<LiquidEngine>,
        #[napi(ts_arg_type = "(name:string) => Promise<string> | null | undefined")]
        callback: Function<'static, SourceLoaderArgs, SourceLoaderReturn>,
    ) -> napi::Result<Reference<LiquidEngine>> {
        let tsfn = callback
            .build_threadsafe_function()
            .callee_handled::<false>()
            .weak::<true>()
            .build()?;

        let mut config = self
            .config
            .write()
            .map_err(|_| napi::Error::from_reason("Engine config lock poisoned"))?;
        config.partial_loader = Some(Arc::new(tsfn));
        self.invalidate()?;
        Ok(this)
    }
    /// Registers a JavaScript callback to load layout templates.
    ///
    /// The callback receives a layout name and may resolve to the layout's
    /// source text or `None` if it is not available. If a filesystem directory
    /// is also configured, the layout priority determines which source is
    /// tried first.
    ///
    /// # Arguments
    ///
    /// * `this` - Engine reference returned for chaining.
    /// * `callback` - Async function that looks up layout source by name.
    ///
    /// # Errors
    ///
    /// Returns an error if the callback cannot be converted to a
    /// thread-safe function.
    #[napi]
    pub fn register_layout_loader(
        &self,
        this: Reference<LiquidEngine>,
        #[napi(ts_arg_type = "(name:string) => Promise<string> | null | undefined")]
        callback: Function<'static, SourceLoaderArgs, SourceLoaderReturn>,
    ) -> napi::Result<Reference<LiquidEngine>> {
        let tsfn = callback
            .build_threadsafe_function()
            .callee_handled::<false>()
            .weak::<true>()
            .build()?;

        let mut config = self
            .config
            .write()
            .map_err(|_| napi::Error::from_reason("Engine config lock poisoned"))?;
        config.layouts_loader = Some(Arc::new(tsfn));
        self.invalidate()?;
        Ok(this)
    }
    /// Sets whether the loader or filesystem is tried first for layouts.
    ///
    /// The default is `LoaderFirst`.
    ///
    /// # Arguments
    ///
    /// * `this` - Engine reference returned for chaining.
    /// * `priority` - Source to check first when both are configured.
    #[napi]
    pub fn set_layout_priority(
        &self,
        this: Reference<LiquidEngine>,
        #[napi(ts_arg_type = "\"LoaderFirst\" | \"FileSystemFirst\"")] priority: SourcePriority,
    ) -> napi::Result<Reference<LiquidEngine>> {
        let mut config = self
            .config
            .write()
            .map_err(|_| napi::Error::from_reason("Layout priority lock poisoned"))?;
        config.layouts_priority = priority;
        self.invalidate()?;
        Ok(this)
    }
    /// Sets whether the loader or filesystem is tried first for partials.
    ///
    /// The default is `LoaderFirst`.
    ///
    /// # Arguments
    ///
    /// * `this` - Engine reference returned for chaining.
    /// * `priority` - Source to check first when both are configured.
    #[napi]
    pub fn set_partial_priority(
        &self,
        this: Reference<LiquidEngine>,
        #[napi(ts_arg_type = "\"LoaderFirst\" | \"FileSystemFirst\"")] priority: SourcePriority,
    ) -> napi::Result<Reference<LiquidEngine>> {
        let mut config = self
            .config
            .write()
            .map_err(|_| napi::Error::from_reason("Partial priority lock poisoned"))?;
        config.partial_priority = priority;
        self.invalidate()?;
        Ok(this)
    }
    /// Applies a plugin function to this engine and returns the engine.
    ///
    /// The plugin receives a reference to this engine and can register filters,
    /// tags, blocks, or partial sources on it.
    ///
    /// # Arguments
    ///
    /// * `env` - JavaScript environment used to clone the engine reference.
    /// * `this` - Engine reference passed to the plugin and returned afterward.
    /// * `plugin` - Function that configures the engine.
    ///
    /// # Errors
    ///
    /// Returns an error if the engine reference cannot be cloned or the plugin
    /// call fails.
    #[napi(js_name = "use")]
    pub fn use_plugin(
        &self,
        env: Env,
        this: Reference<LiquidEngine>,
        #[napi(ts_arg_type = "LiquidPlugin")] plugin: Function<'_, Reference<LiquidEngine>, ()>,
    ) -> napi::Result<Reference<LiquidEngine>> {
        // Clone the Reference so we can pass one to the plugin and return the other
        let plugin_arg = this.clone(env)?;
        plugin.call(plugin_arg)?;
        Ok(this)
    }
    /// Renders a Liquid template using the supplied JSON context.
    ///
    /// The parser is built lazily and reused until registrations change.
    /// Parsed templates are cached by template source. Rendering runs on a
    /// blocking worker thread.
    ///
    /// # Arguments
    ///
    /// * `template_str` - Liquid template source.
    /// * `context` - JSON object providing the template's variables.
    ///
    /// # Errors
    ///
    /// Returns an error if the parser or template cannot be built, the context
    /// cannot be converted, rendering fails, a synchronization lock is
    /// poisoned, or the worker thread panics.
    #[napi]
    pub async fn render(
        &self,
        template_str: String,
        #[napi(ts_arg_type = "Record<string,any>")] context: serde_json::Value,
    ) -> napi::Result<String> {
        // Lazily build (or reuse) the parser before entering spawn_blocking
        let parser = self.get_or_build_parser()?;
        let cache = Arc::clone(&self.cache);
        let (layouts_dir, layouts_extensions, layouts_loader, layouts_priority) = {
            let config = self
                .config
                .read()
                .map_err(|_| napi::Error::from_reason("Layouts lock poisoned"))
                .unwrap();
            (
                config.layouts_dir.clone(),
                config.layouts_extensions.clone(),
                config.layouts_loader.clone(),
                config.layouts_priority.clone(),
            )
        };
        // 2. Execute YAML extraction, caching, and layout wrapping off the main V8 thread
        tokio::task::spawn_blocking(move || {
            // A. Extract YAML front matter (or {% layout %}) and clean body
            let parsed_page = front_matter(template_str)?;

            // B. Merge front matter keys into `page` (accessible as {{ page.* }})
            let mut ctx_obj = match context {
                serde_json::Value::Object(map) => map,
                _ => serde_json::Map::new(),
            };
            if matches!(&parsed_page.front_matter, serde_json::Value::Object(map) if !map.is_empty()) {
                ctx_obj.insert("page".to_string(), parsed_page.front_matter);
            }

            let mut globals: liquid::Object =
                serde_json::from_value(serde_json::Value::Object(ctx_obj))
                    .map_err(|e| napi::Error::from_reason(e.to_string()))?;

            // C. Render the page body using the cached AST helper
            let mut current_html =
                Self::render_cached(&parser, &cache, &parsed_page.body, &globals)?;
            let mut next_layout = parsed_page.layout;

            // D. Wrap in parent layouts if `layout` was specified (supports nested layouts)
            let mut depth = 0;
            while let Some(layout_name) = next_layout {
                depth += 1;
                if depth > 10 {
                    return Err(napi::Error::from_reason(
                        "Max layout nesting depth (10) exceeded — check for circular layouts",
                    ));
                }

                let raw_layout = Self::load_layout_source(
                    &layouts_dir,
                    &layouts_loader,
                    layouts_priority,
                    &layouts_extensions,
                    &layout_name,
                )?;
                let parsed_layout = front_matter(raw_layout)?;

                // Expose the layout's own YAML front matter as {{ layout.* }}
                if matches!(&parsed_layout.front_matter, serde_json::Value::Object(map) if !map.is_empty()) {
                    let layout_val: liquid::model::Value =
                        serde_json::from_value(parsed_layout.front_matter)
                            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
                    globals.insert("layout".into(), layout_val);
                }

                // Inject the rendered inner content into {{ content }} and {{ content_for_layout }}
                globals.insert(
                    "content".into(),
                    liquid::model::Value::scalar(current_html.clone()),
                );
                globals.insert(
                    "content_for_layout".into(),
                    liquid::model::Value::scalar(current_html),
                );

                current_html = Self::render_cached(&parser, &cache, &parsed_layout.body, &globals)?;
                next_layout = parsed_layout.layout;
            }
            Ok(current_html)
        })
        .await
        .map_err(|e| napi::Error::from_reason(format!("Worker thread panicked: {}", e)))?
    }
}
