use napi::bindgen_prelude::*;
use napi::threadsafe_function::ThreadsafeFunction;
use std::io::Write;
use std::sync::Arc;

use liquid::ValueView;
use liquid_core::{
    BlockReflection, Expression, Language, ParseBlock, Renderable, Result, Runtime, TagBlock,
    TagTokenIter, Template,
};

pub type LiquidBlockArgs = FnArgs<(String, Vec<serde_json::Value>)>;
pub type LiquidBlockReturn = Either<String, Promise<String>>;
pub type LiquidBlockCallback =
    ThreadsafeFunction<LiquidBlockArgs, LiquidBlockReturn, LiquidBlockArgs, Status, false, true, 0>;

#[derive(Clone)]
pub struct LiquidBlock {
    pub start_tag: &'static str,
    pub end_tag: &'static str,
    pub callback: Arc<LiquidBlockCallback>,
}

impl BlockReflection for LiquidBlock {
    fn start_tag(&self) -> &'static str {
        self.start_tag
    }

    fn end_tag(&self) -> &'static str {
        self.end_tag
    }

    fn description(&self) -> &'static str {
        "Custom JS block tag registered via napi-rs"
    }
}

impl ParseBlock for LiquidBlock {
    fn parse(
        &self,
        mut arguments: TagTokenIter<'_>,
        mut tokens: TagBlock<'_, '_>,
        options: &Language,
    ) -> Result<Box<dyn Renderable>> {
        let mut args = Vec::new();
        while let Some(token) = arguments.next() {
            args.push(token.expect_value().into_result()?);
        }

        let inner_template = Template::new(tokens.parse_all(options)?);

        Ok(Box::new(LiquidBlockRuntime {
            args,
            inner_template,
            callback: Arc::clone(&self.callback),
        }))
    }

    fn reflection(&self) -> &dyn BlockReflection {
        self
    }
}

struct LiquidBlockRuntime {
    args: Vec<Expression>,
    inner_template: Template,
    callback: Arc<LiquidBlockCallback>,
}

impl std::fmt::Debug for LiquidBlockRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiquidBlockRuntime")
            .field("args", &self.args)
            .field("inner_template", &self.args)
            .field("has_callback", &true)
            .finish()
    }
}

impl Renderable for LiquidBlockRuntime {
    fn render_to(&self, writer: &mut dyn Write, runtime: &dyn Runtime) -> Result<()> {
        let mut args_json = Vec::with_capacity(self.args.len());
        for expr in &self.args {
            let val = expr.evaluate(runtime)?;
            let json_val = serde_json::to_value(val.to_value())
                .map_err(|e| liquid_core::Error::with_msg(e.to_string()))?;
            args_json.push(json_val);
        }

        let mut inner_buf = Vec::new();
        self.inner_template.render_to(&mut inner_buf, runtime)?;
        let body_str = String::from_utf8(inner_buf)
            .map_err(|e| liquid_core::Error::with_msg(e.to_string()))?;

        let output: String = tokio::runtime::Handle::current().block_on(async {
            let res = self
                .callback
                .call_async(FnArgs::from((body_str, args_json)))
                .await
                .map_err(|e| liquid_core::Error::with_msg(e.to_string()))?;

            match res {
                Either::A(sync_str) => Ok(sync_str),
                Either::B(promise) => promise
                    .await
                    .map_err(|e| liquid_core::Error::with_msg(e.to_string())),
            }
        })?;

        write!(writer, "{}", output).map_err(|e| liquid_core::Error::with_msg(e.to_string()))?;
        Ok(())
    }
}
