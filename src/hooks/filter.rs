use liquid::ValueView;
use liquid_core::{
    Display_filter, Expression, Filter, FilterReflection, ParseFilter, Result, Runtime, Value,
    parser::{FilterArguments, ParameterReflection},
};
use napi::bindgen_prelude::*;
use napi::threadsafe_function::ThreadsafeFunction;
use std::sync::Arc;

// Type Aliases
pub type LiquidFilterArgs = FnArgs<(serde_json::Value, Vec<serde_json::Value>)>;
pub type LiquidFilterReturn = Promise<serde_json::Value>;
pub type LiquidFilterCallback = ThreadsafeFunction<
    LiquidFilterArgs,
    LiquidFilterReturn,
    LiquidFilterArgs,
    Status,
    false,
    true,
    0,
>;

#[derive(Clone)]
pub struct LiquidFilter {
    pub name: &'static str,
    pub callback: Arc<LiquidFilterCallback>,
}

impl FilterReflection for LiquidFilter {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        "Custom JS filter registered via napi-rs"
    }

    fn positional_parameters(&self) -> &'static [ParameterReflection] {
        &[]
    }

    fn keyword_parameters(&self) -> &'static [ParameterReflection] {
        &[]
    }
}

impl ParseFilter for LiquidFilter {
    fn parse(&self, mut arguments: FilterArguments<'_>) -> Result<Box<dyn Filter>> {
        let mut positional = Vec::new();
        while let Some(arg) = arguments.positional.next() {
            positional.push(arg);
        }

        Ok(Box::new(LiquidFilterRuntime {
            positional,
            callback: Arc::clone(&self.callback),
        }))
    }

    fn reflection(&self) -> &dyn FilterReflection {
        self
    }
}

#[derive(Display_filter)]
#[name = "liquid_filter"]
struct LiquidFilterRuntime {
    positional: Vec<Expression>,
    callback: Arc<LiquidFilterCallback>,
}

impl Filter for LiquidFilterRuntime {
    fn evaluate(&self, input: &dyn ValueView, runtime: &dyn Runtime) -> Result<Value> {
        let input_json = serde_json::to_value(input.to_value())
            .map_err(|e| liquid_core::Error::with_msg(e.to_string()))?;

        let mut args_json = Vec::with_capacity(self.positional.len());
        for expr in &self.positional {
            let val = expr.evaluate(runtime)?;
            let json_val = serde_json::to_value(val.to_value())
                .map_err(|e| liquid_core::Error::with_msg(e.to_string()))?;
            args_json.push(json_val);
        }

        let result_json: serde_json::Value = tokio::runtime::Handle::current().block_on(async {
            // Await the promise returned by the JS callback.
            let promise: Promise<serde_json::Value> = self
                .callback
                .call_async(FnArgs::from((input_json, args_json)))
                .await
                .map_err(|e| liquid_core::Error::with_msg(e.to_string()))?;

            promise
                .await
                .map_err(|e| liquid_core::Error::with_msg(e.to_string()))
        })?;

        serde_json::from_value(result_json).map_err(|e| liquid_core::Error::with_msg(e.to_string()))
    }
}

impl std::fmt::Debug for LiquidFilterRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiquidFilterRuntime")
            .field("positional", &self.positional)
            .field("has_callback", &true)
            .finish()
    }
}
