use std::sync::{Arc, Weak};

use extism_convert::{FromBytesOwned, ToBytes};
use thiserror::Error;
use wasmer::{Extern, Function, FunctionEnv, FunctionEnvMut};

use crate::{Context, PluginIdentifier};

#[derive(Debug)]
pub struct HostExport {
    pub name: String,
    namespace: Option<String>,
    pub wasmer_extern: Extern,
}

impl HostExport {
    pub fn new_with_in_out<'o, IN, OUT, ENV, ID: PluginIdentifier>(
        context: &Arc<Context<ID>>,
        namespace: Option<&str>,
        name: &str,
        f: impl Fn(&ENV, IN) -> Result<OUT, ()> + Send + Sync + 'static,
        env: ENV,
    ) -> Self
    where
        IN: FromBytesOwned,
        OUT: ToBytes<'o>,
        ENV: Send + 'static,
    {
        let mut store = context.store.write().unwrap();
        let function_env = FunctionEnv::new(&mut store, (Arc::downgrade(context), env));
        let wasmer_extern = Extern::Function(Function::new_typed_with_env(
            &mut store,
            &function_env,
            move |env: FunctionEnvMut<'_, (Weak<Context<ID>>, ENV)>, input: i64| {
                let (context, env) = env.data();
                if let Some(context) = context.upgrade() {
                    let input = match Self::fetch_in(&context, input) {
                        Ok(input) => input,
                        Err(err) => {
                            tracing::error!("{err}");
                            return 0;
                        }
                    };
                    if let Ok(output) = f(env, input) {
                        Self::store_out(&context, output).unwrap_or_else(|err| {
                            tracing::error!("{err}");
                            0
                        })
                    } else {
                        tracing::warn!("Host function error");
                        0
                    }
                } else {
                    0
                }
            },
        ));
        Self {
            name: name.to_string(),
            namespace: namespace.map(|s| s.to_string()),
            wasmer_extern,
        }
    }

    pub fn new_with_in<IN, ENV, ID: PluginIdentifier>(
        context: &Arc<Context<ID>>,
        namespace: Option<&str>,
        name: &str,
        f: impl Fn(&ENV, IN) + Send + Sync + 'static,
        env: ENV,
    ) -> Self
    where
        IN: FromBytesOwned,
        ENV: Send + 'static,
    {
        let mut store = context.store.write().unwrap();
        let function_env = FunctionEnv::new(&mut store, (Arc::downgrade(context), env));
        let wasmer_extern = Extern::Function(Function::new_typed_with_env(
            &mut store,
            &function_env,
            move |env: FunctionEnvMut<'_, (Weak<Context<ID>>, ENV)>, input: i64| {
                let (context, env) = env.data();
                if let Some(context) = context.upgrade() {
                    let input = match Self::fetch_in(&context, input) {
                        Ok(input) => input,
                        Err(err) => {
                            tracing::error!("{err}");
                            return;
                        }
                    };
                    f(env, input);
                }
            },
        ));
        Self {
            name: name.to_string(),
            namespace: namespace.map(|s| s.to_string()),
            wasmer_extern,
        }
    }

    pub fn new_with_out<'o, OUT, ENV, ID: PluginIdentifier>(
        context: &Arc<Context<ID>>,
        namespace: Option<&str>,
        name: &str,
        f: impl Fn(&ENV) -> Result<OUT, ()> + Send + Sync + 'static,
        env: ENV,
    ) -> Self
    where
        OUT: ToBytes<'o>,
        ENV: Send + 'static,
    {
        let mut store = context.store.write().unwrap();
        let function_env = FunctionEnv::new(&mut store, (Arc::downgrade(context), env));
        let wasmer_extern = Extern::Function(Function::new_typed_with_env(
            &mut store,
            &function_env,
            move |env: FunctionEnvMut<'_, (Weak<Context<ID>>, ENV)>| {
                let (context, env) = env.data();
                if let Some(context) = context.upgrade() {
                    let Ok(output) = f(env) else {
                        tracing::warn!("Host function error");
                        return 0;
                    };
                    Self::store_out(&context, output).unwrap_or_else(|err| {
                        tracing::error!("{err}");
                        0
                    })
                } else {
                    0
                }
            },
        ));
        Self {
            name: name.to_string(),
            namespace: namespace.map(|s| s.to_string()),
            wasmer_extern,
        }
    }

    pub fn new<ENV, ID: PluginIdentifier>(
        context: &Arc<Context<ID>>,
        namespace: Option<&str>,
        name: &str,
        f: impl Fn(&ENV) + Send + Sync + 'static,
        env: ENV,
    ) -> Self
    where
        ENV: Send + 'static,
    {
        let mut store = context.store.write().unwrap();
        let function_env = FunctionEnv::new(&mut store, env);
        let wasmer_extern = Extern::Function(Function::new_typed_with_env(
            &mut store,
            &function_env,
            move |env: FunctionEnvMut<'_, ENV>| {
                f(env.data());
            },
        ));
        Self {
            name: name.to_string(),
            namespace: namespace.map(|s| s.to_string()),
            wasmer_extern,
        }
    }

    fn fetch_in<IN: FromBytesOwned, ID: PluginIdentifier>(
        context: &Arc<Context<ID>>,
        input: i64,
    ) -> Result<IN, FetchInError> {
        let handle = context
            .memory_handle(input as u64)
            .ok_or(FetchInError::HandleNotFound(input))?;
        let input_message: IN = context.memory_get(handle)?;
        context.memory_free(handle)?;
        Ok(input_message)
    }

    fn store_out<'o, OUT: ToBytes<'o>, ID: PluginIdentifier>(
        context: &Arc<Context<ID>>,
        output: OUT,
    ) -> Result<i64, StoreOutError> {
        Ok(context.memory_new(output)?.offset as i64)
    }

    pub fn namespace(&self) -> &str {
        self.namespace
            .as_deref()
            .unwrap_or(crate::EXTISM_USER_MODULE)
    }
}

#[derive(Debug, Error)]
enum FetchInError {
    #[error("Memory handle not found: {0}")]
    HandleNotFound(i64),
    #[error("Convert: {0}")]
    Access(#[from] extism_convert::Error),
    #[error("Runtime: {0}")]
    Runtime(#[from] wasmer::RuntimeError),
}

#[derive(Debug, Error)]
enum StoreOutError {
    #[error("Memory allocation: {0}")]
    Memory(#[from] crate::context::ContextError),
}
