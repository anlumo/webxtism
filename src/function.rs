use std::sync::{Arc, Weak};

use extism_convert::{FromBytesOwned, ToBytes};
use thiserror::Error;
use wasmer::{Extern, Function, FunctionEnv, FunctionEnvMut};

use crate::{
    plugin::{kernel::KernelError, ContextGoneError},
    Plugin, PluginIdentifier,
};

#[derive(Debug)]
pub struct HostExport {
    pub name: String,
    namespace: Option<String>,
    pub wasmer_extern: Extern,
}

impl HostExport {
    pub fn new_with_in_out<'o, IN, OUT, ENV, ID: PluginIdentifier>(
        plugin: &Arc<Plugin<ID>>,
        namespace: Option<&str>,
        name: &str,
        f: impl Fn(&ENV, IN) -> Result<OUT, ()> + Send + Sync + 'static,
        env: ENV,
    ) -> Result<Self, ContextGoneError>
    where
        IN: FromBytesOwned,
        OUT: ToBytes<'o>,
        ENV: Send + 'static,
    {
        let context = plugin.context()?;
        let mut store = context.store.write().unwrap();
        let function_env = FunctionEnv::new(&mut store, (Arc::downgrade(plugin), env));
        let wasmer_extern = Extern::Function(Function::new_typed_with_env(
            &mut store,
            &function_env,
            move |env: FunctionEnvMut<'_, (Weak<Plugin<ID>>, ENV)>, input: i64| {
                let (plugin, env) = env.data();
                if let Some(plugin) = plugin.upgrade() {
                    let input = match Self::fetch_in(&plugin, input) {
                        Ok(input) => input,
                        Err(err) => {
                            tracing::error!("{err}");
                            return 0;
                        }
                    };
                    if let Ok(output) = f(env, input) {
                        Self::store_out(&plugin, output).unwrap_or_else(|err| {
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
        Ok(Self {
            name: name.to_string(),
            namespace: namespace.map(|s| s.to_string()),
            wasmer_extern,
        })
    }

    pub fn new_with_in<IN, ENV, ID: PluginIdentifier>(
        plugin: &Arc<Plugin<ID>>,
        namespace: Option<&str>,
        name: &str,
        f: impl Fn(&ENV, IN) + Send + Sync + 'static,
        env: ENV,
    ) -> Result<Self, ContextGoneError>
    where
        IN: FromBytesOwned,
        ENV: Send + 'static,
    {
        let context = plugin.context()?;
        let mut store = context.store.write().unwrap();
        let function_env = FunctionEnv::new(&mut store, (Arc::downgrade(plugin), env));
        let wasmer_extern = Extern::Function(Function::new_typed_with_env(
            &mut store,
            &function_env,
            move |env: FunctionEnvMut<'_, (Weak<Plugin<ID>>, ENV)>, input: i64| {
                let (plugin, env) = env.data();
                if let Some(plugin) = plugin.upgrade() {
                    let input = match Self::fetch_in(&plugin, input) {
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
        Ok(Self {
            name: name.to_string(),
            namespace: namespace.map(|s| s.to_string()),
            wasmer_extern,
        })
    }

    pub fn new_with_out<'o, OUT, ENV, ID: PluginIdentifier>(
        plugin: &Arc<Plugin<ID>>,
        namespace: Option<&str>,
        name: &str,
        f: impl Fn(&ENV) -> Result<OUT, ()> + Send + Sync + 'static,
        env: ENV,
    ) -> Result<Self, ContextGoneError>
    where
        OUT: ToBytes<'o>,
        ENV: Send + 'static,
    {
        let context = plugin.context()?;
        let mut store = context.store.write().unwrap();
        let function_env = FunctionEnv::new(&mut store, (Arc::downgrade(plugin), env));
        let wasmer_extern = Extern::Function(Function::new_typed_with_env(
            &mut store,
            &function_env,
            move |env: FunctionEnvMut<'_, (Weak<Plugin<ID>>, ENV)>| {
                let (plugin, env) = env.data();
                if let Some(plugin) = plugin.upgrade() {
                    let Ok(output) = f(env) else {
                        tracing::warn!("Host function error");
                        return 0;
                    };
                    Self::store_out(&plugin, output).unwrap_or_else(|err| {
                        tracing::error!("{err}");
                        0
                    })
                } else {
                    0
                }
            },
        ));
        Ok(Self {
            name: name.to_string(),
            namespace: namespace.map(|s| s.to_string()),
            wasmer_extern,
        })
    }

    pub fn new<ENV, ID: PluginIdentifier>(
        plugin: &Arc<Plugin<ID>>,
        namespace: Option<&str>,
        name: &str,
        f: impl Fn(&ENV) + Send + Sync + 'static,
        env: ENV,
    ) -> Result<Self, ContextGoneError>
    where
        ENV: Send + 'static,
    {
        let context = plugin.context()?;
        let mut store = context.store.write().unwrap();
        let function_env = FunctionEnv::new(&mut store, env);
        let wasmer_extern = Extern::Function(Function::new_typed_with_env(
            &mut store,
            &function_env,
            move |env: FunctionEnvMut<'_, ENV>| {
                f(env.data());
            },
        ));
        Ok(Self {
            name: name.to_string(),
            namespace: namespace.map(|s| s.to_string()),
            wasmer_extern,
        })
    }

    fn fetch_in<IN: FromBytesOwned, ID: PluginIdentifier>(
        plugin: &Plugin<ID>,
        input: i64,
    ) -> Result<IN, FetchInError> {
        let handle = plugin
            .kernel
            .memory_handle(input as u64)
            .ok_or(FetchInError::HandleNotFound(input))?;
        let input_message: IN = plugin.kernel.memory_get(handle)?;
        plugin.kernel.memory_free(handle)?;
        Ok(input_message)
    }

    fn store_out<'o, OUT: ToBytes<'o>, ID: PluginIdentifier>(
        plugin: &Plugin<ID>,
        output: OUT,
    ) -> Result<i64, StoreOutError> {
        Ok(plugin.kernel.memory_new(output)?.offset as i64)
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
    #[error("Kernel: {0}")]
    Runtime(#[from] KernelError),
}

#[derive(Debug, Error)]
enum StoreOutError {
    #[error("Kernel: {0}")]
    Memory(#[from] KernelError),
}
