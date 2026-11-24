use napi::bindgen_prelude::*;
use napi::threadsafe_function::ThreadsafeFunction;
use std::io::Write;
use std::sync::Arc;

use liquid::ValueView;
use liquid_core::{
    Expression, Language, ParseTag, Renderable, Result, Runtime, TagReflection, TagTokenIter,
};

pub type LiquidTagArgs = FnArgs<(Vec<serde_json::Value>,)>;
pub type LiquidTagReturn = Either<String, Promise<String>>;
pub type LiquidTagCallback =
    ThreadsafeFunction<LiquidTagArgs, LiquidTagReturn, LiquidTagArgs, Status, false, true, 0>;

#[derive(Clone)]
pub struct LiquidTag {
    pub name: &'static str,
    pub callback: Arc<LiquidTagCallback>,
}

impl TagReflection for LiquidTag {
    fn tag(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        "Custom JS tag registered via napi-rs"
    }
}

impl ParseTag for LiquidTag {
    fn parse(
        &self,
        mut arguments: TagTokenIter<'_>,
        _options: &Language,
    ) -> Result<Box<dyn Renderable>> {
        let mut args = Vec::new();
        while let Some(token) = arguments.next() {
            args.push(token.expect_value().into_result()?);
        }

        Ok(Box::new(LiquidTagRuntime {
            args,
            callback: Arc::clone(&self.callback),
        }))
    }

    fn reflection(&self) -> &dyn TagReflection {
        self
    }
}

struct LiquidTagRuntime {
    args: Vec<Expression>,
    callback: Arc<LiquidTagCallback>,
}

impl std::fmt::Debug for LiquidTagRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiquidTagRuntime")
            .field("args", &self.args)
            .field("has_callback", &true)
            .finish()
    }
}

impl Renderable for LiquidTagRuntime {
    fn render_to(&self, writer: &mut dyn Write, runtime: &dyn Runtime) -> Result<()> {
        let mut args_json = Vec::with_capacity(self.args.len());
        for expr in &self.args {
            let val = expr.evaluate(runtime)?;
            let json_val = serde_json::to_value(val.to_value())
                .map_err(|e| liquid_core::Error::with_msg(e.to_string()))?;
            args_json.push(json_val);
        }

        let output: String = tokio::runtime::Handle::current().block_on(async {
            let res = self
                .callback
                .call_async(FnArgs::from((args_json,)))
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
