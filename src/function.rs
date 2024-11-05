use std::sync::{Arc, Weak};

use extism_convert::{FromBytesOwned, ToBytes};
use thiserror::Error;
use wasmer::{AsStoreMut, Extern, Function, FunctionEnv, FunctionEnvMut};

use crate::{plugin::kernel::KernelError, Context, Plugin, PluginIdentifier};

pub(crate) trait ToExtern<ID: PluginIdentifier> {
    fn to_extern(self: Box<Self>, context: &Arc<Context>, plugin: &Arc<Plugin<ID>>) -> Extern;
}

pub enum ExportDefinition<IN, OUT, ENV> {
    InOut {
        function: Box<dyn Fn(&ENV, IN) -> Result<OUT, ()> + Send + Sync + 'static>,
        env: ENV,
    },
    In {
        function: Box<dyn Fn(&ENV, IN) + Send + Sync + 'static>,
        env: ENV,
    },
    Out {
        function: Box<dyn Fn(&ENV) -> Result<OUT, ()> + Send + Sync + 'static>,
        env: ENV,
    },
    Bare {
        function: Box<dyn Fn(&ENV) + Send + Sync + 'static>,
        env: ENV,
    },
}

impl<'o, IN, OUT, ENV, ID> ToExtern<ID> for ExportDefinition<IN, OUT, ENV>
where
    ID: PluginIdentifier,
    IN: FromBytesOwned + 'static,
    OUT: ToBytes<'o> + 'static,
    ENV: Send + 'static,
{
    fn to_extern(self: Box<Self>, context: &Arc<Context>, plugin: &Arc<Plugin<ID>>) -> Extern {
        match *self {
            ExportDefinition::InOut { function, env } => {
                let mut store = context.store.write().unwrap();
                let function_env = FunctionEnv::new(&mut store, (Arc::downgrade(plugin), env));
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    move |mut fenv: FunctionEnvMut<'_, (Weak<Plugin<ID>>, ENV)>, input: i64| {
                        let ((plugin, env), mut store) = fenv.data_and_store_mut();
                        if let Some(plugin) = plugin.upgrade() {
                            let input = match HostExport::fetch_in(&mut store, &plugin, input) {
                                Ok(input) => input,
                                Err(err) => {
                                    tracing::error!("{err}");
                                    return 0;
                                }
                            };
                            if let Ok(output) = function(env, input) {
                                HostExport::store_out(store, &plugin, output).unwrap_or_else(
                                    |err| {
                                        tracing::error!("{err}");
                                        0
                                    },
                                )
                            } else {
                                tracing::warn!("Host function error");
                                0
                            }
                        } else {
                            0
                        }
                    },
                ))
            }
            ExportDefinition::In { function, env } => {
                let mut store = context.store.write().unwrap();
                let function_env = FunctionEnv::new(&mut store, (Arc::downgrade(plugin), env));
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    move |mut fenv: FunctionEnvMut<'_, (Weak<Plugin<ID>>, ENV)>, input: i64| {
                        let ((plugin, env), store) = fenv.data_and_store_mut();
                        if let Some(plugin) = plugin.upgrade() {
                            let input = match HostExport::fetch_in(store, &plugin, input) {
                                Ok(input) => input,
                                Err(err) => {
                                    tracing::error!("{err}");
                                    return;
                                }
                            };
                            function(env, input);
                        }
                    },
                ))
            }
            ExportDefinition::Out { function, env } => {
                let mut store = context.store.write().unwrap();
                let function_env = FunctionEnv::new(&mut store, (Arc::downgrade(plugin), env));
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    move |fenv: FunctionEnvMut<'_, (Weak<Plugin<ID>>, ENV)>| {
                        let (plugin, env) = fenv.data();
                        if let Some(plugin) = plugin.upgrade() {
                            let Ok(output) = function(env) else {
                                tracing::warn!("Host function error");
                                return 0;
                            };
                            HostExport::store_out(fenv, &plugin, output).unwrap_or_else(|err| {
                                tracing::error!("{err}");
                                0
                            })
                        } else {
                            0
                        }
                    },
                ))
            }
            ExportDefinition::Bare { function, env } => {
                let mut store = context.store.write().unwrap();
                let function_env = FunctionEnv::new(&mut store, env);
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    move |env: FunctionEnvMut<'_, ENV>| {
                        function(env.data());
                    },
                ))
            }
        }
    }
}

pub struct HostExport<ID> {
    pub name: String,
    pub namespace: Option<String>,
    definition: Box<dyn ToExtern<ID>>,
}

impl<ID: PluginIdentifier> HostExport<ID> {
    pub fn new_with_in_out<'o, IN, OUT, ENV>(
        namespace: Option<&str>,
        name: &str,
        f: impl Fn(&ENV, IN) -> Result<OUT, ()> + Send + Sync + 'static,
        env: ENV,
    ) -> Self
    where
        IN: FromBytesOwned + 'static,
        OUT: ToBytes<'o> + 'static,
        ENV: Send + 'static,
    {
        Self {
            name: name.to_owned(),
            namespace: namespace.map(|ns| ns.to_owned()),
            definition: Box::new(ExportDefinition::InOut {
                function: Box::new(f),
                env,
            }),
        }
    }

    pub fn new_with_in<IN, ENV>(
        namespace: Option<&str>,
        name: &str,
        f: impl Fn(&ENV, IN) + Send + Sync + 'static,
        env: ENV,
    ) -> Self
    where
        IN: FromBytesOwned + 'static,
        ENV: Send + 'static,
    {
        Self {
            name: name.to_owned(),
            namespace: namespace.map(|ns| ns.to_owned()),
            definition: Box::new(ExportDefinition::<_, (), _>::In {
                function: Box::new(f),
                env,
            }),
        }
    }

    pub fn new_with_out<'o, OUT, ENV>(
        namespace: Option<&str>,
        name: &str,
        f: impl Fn(&ENV) -> Result<OUT, ()> + Send + Sync + 'static,
        env: ENV,
    ) -> Self
    where
        OUT: ToBytes<'o> + 'static,
        ENV: Send + 'static,
    {
        Self {
            name: name.to_owned(),
            namespace: namespace.map(|ns| ns.to_owned()),
            definition: Box::new(ExportDefinition::<(), _, _>::Out {
                function: Box::new(f),
                env,
            }),
        }
    }

    pub fn new<ENV>(
        namespace: Option<&str>,
        name: &str,
        f: impl Fn(&ENV) + Send + Sync + 'static,
        env: ENV,
    ) -> Self
    where
        ENV: Send + 'static,
    {
        Self {
            name: name.to_owned(),
            namespace: namespace.map(|ns| ns.to_owned()),
            definition: Box::new(ExportDefinition::<(), (), _>::Bare {
                function: Box::new(f),
                env,
            }),
        }
    }

    fn fetch_in<IN: FromBytesOwned>(
        mut store: impl AsStoreMut,
        plugin: &Plugin<ID>,
        input: i64,
    ) -> Result<IN, FetchInError> {
        let handle = plugin
            .kernel
            .memory_handle(&mut store, input as u64)
            .ok_or(FetchInError::HandleNotFound(input))?;
        let input_message: IN = plugin.kernel.memory_get(&mut store, handle)?;
        plugin.kernel.memory_free(&mut store, handle)?;
        Ok(input_message)
    }

    fn store_out<'o, OUT: ToBytes<'o>>(
        mut store: impl AsStoreMut,
        plugin: &Plugin<ID>,
        output: OUT,
    ) -> Result<i64, StoreOutError> {
        Ok(plugin.kernel.memory_new(&mut store, output)?.offset as i64)
    }

    pub fn namespace(&self) -> &str {
        self.namespace
            .as_deref()
            .unwrap_or(crate::EXTISM_USER_MODULE)
    }

    pub(crate) fn build(self, context: &Arc<Context>, plugin: &Arc<Plugin<ID>>) -> Extern {
        self.definition.to_extern(context, plugin)
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
