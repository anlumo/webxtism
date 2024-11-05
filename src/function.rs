use std::sync::{Arc, Weak};

use extism_convert::{FromBytesOwned, ToBytes};
use thiserror::Error;
use wasmer::{AsStoreMut, Extern, Function, FunctionEnv, FunctionEnvMut, StoreMut};

use crate::{plugin::kernel::KernelError, Context, Plugin, PluginIdentifier};

trait ToExtern<ID: PluginIdentifier> {
    fn to_extern(self: Box<Self>, context: &Arc<Context>, plugin: &Arc<Plugin<ID>>) -> Extern;
}

#[allow(clippy::type_complexity)]
pub enum ExportDefinition<IN, OUT, ENV> {
    InOut {
        function:
            Box<dyn Fn(&mut StoreMut<'_>, &ENV, IN) -> Result<OUT, ()> + Send + Sync + 'static>,
        env: ENV,
    },
    In {
        function: Box<dyn Fn(&mut StoreMut<'_>, &ENV, IN) + Send + Sync + 'static>,
        env: ENV,
    },
    Out {
        function: Box<dyn Fn(&mut StoreMut<'_>, &ENV) -> Result<OUT, ()> + Send + Sync + 'static>,
        env: ENV,
    },
    Bare {
        function: Box<dyn Fn(&mut StoreMut<'_>, &ENV) + Send + Sync + 'static>,
        env: ENV,
    },
}

fn fetch_in<IN: FromBytesOwned, ID: PluginIdentifier>(
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

fn store_out<'o, OUT: ToBytes<'o>, ID: PluginIdentifier>(
    mut store: impl AsStoreMut,
    plugin: &Plugin<ID>,
    output: OUT,
) -> Result<i64, StoreOutError> {
    Ok(plugin.kernel.memory_new(&mut store, output)?.offset as i64)
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
                            let input = match fetch_in(&mut store, &plugin, input) {
                                Ok(input) => input,
                                Err(err) => {
                                    tracing::error!("{err}");
                                    return 0;
                                }
                            };
                            if let Ok(output) = function(&mut store, env, input) {
                                store_out(store, &plugin, output).unwrap_or_else(|err| {
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
                ))
            }
            ExportDefinition::In { function, env } => {
                let mut store = context.store.write().unwrap();
                let function_env = FunctionEnv::new(&mut store, (Arc::downgrade(plugin), env));
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    move |mut fenv: FunctionEnvMut<'_, (Weak<Plugin<ID>>, ENV)>, input: i64| {
                        let ((plugin, env), mut store) = fenv.data_and_store_mut();
                        if let Some(plugin) = plugin.upgrade() {
                            let input = match fetch_in(&mut store, &plugin, input) {
                                Ok(input) => input,
                                Err(err) => {
                                    tracing::error!("{err}");
                                    return;
                                }
                            };
                            function(&mut store, env, input);
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
                    move |mut fenv: FunctionEnvMut<'_, (Weak<Plugin<ID>>, ENV)>| {
                        let ((plugin, env), mut store) = fenv.data_and_store_mut();
                        if let Some(plugin) = plugin.upgrade() {
                            let Ok(output) = function(&mut store, env) else {
                                tracing::warn!("Host function error");
                                return 0;
                            };
                            store_out(fenv, &plugin, output).unwrap_or_else(|err| {
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
                    move |mut fenv: FunctionEnvMut<'_, ENV>| {
                        let (env, mut store) = fenv.data_and_store_mut();
                        function(&mut store, env);
                    },
                ))
            }
        }
    }
}

pub struct HostExportBuilder {
    pub name: String,
    pub namespace: Option<String>,
}

impl HostExportBuilder {
    pub fn new(name: impl AsRef<str>) -> Self {
        Self {
            name: name.as_ref().to_owned(),
            namespace: None,
        }
    }
    pub fn namespace(mut self, namespace: impl AsRef<str>) -> Self {
        self.namespace = Some(namespace.as_ref().to_owned());
        self
    }

    pub fn function_in_out<'o, IN, OUT, ENV, ID>(
        self,
        f: impl Fn(&mut StoreMut<'_>, &ENV, IN) -> Result<OUT, ()> + Send + Sync + 'static,
        env: ENV,
    ) -> HostExportBuilderWithFunction<ID>
    where
        IN: FromBytesOwned + 'static,
        OUT: ToBytes<'o> + 'static,
        ENV: Send + 'static,
        ID: PluginIdentifier,
    {
        HostExportBuilderWithFunction {
            name: self.name,
            namespace: self.namespace,
            definition: Box::new(ExportDefinition::InOut {
                function: Box::new(f),
                env,
            }),
        }
    }

    pub fn function_in<IN, ENV, ID>(
        self,
        f: impl Fn(&mut StoreMut<'_>, &ENV, IN) + Send + Sync + 'static,
        env: ENV,
    ) -> HostExportBuilderWithFunction<ID>
    where
        IN: FromBytesOwned + 'static,
        ENV: Send + 'static,
        ID: PluginIdentifier,
    {
        HostExportBuilderWithFunction {
            name: self.name,
            namespace: self.namespace,
            definition: Box::new(ExportDefinition::<_, (), _>::In {
                function: Box::new(f),
                env,
            }),
        }
    }

    pub fn function_out<'o, OUT, ENV, ID>(
        self,
        f: impl Fn(&mut StoreMut<'_>, &ENV) -> Result<OUT, ()> + Send + Sync + 'static,
        env: ENV,
    ) -> HostExportBuilderWithFunction<ID>
    where
        OUT: ToBytes<'o> + 'static,
        ENV: Send + 'static,
        ID: PluginIdentifier,
    {
        HostExportBuilderWithFunction {
            name: self.name,
            namespace: self.namespace,
            definition: Box::new(ExportDefinition::<(), _, _>::Out {
                function: Box::new(f),
                env,
            }),
        }
    }

    pub fn function<ENV, ID>(
        self,
        f: impl Fn(&mut StoreMut<'_>, &ENV) + Send + Sync + 'static,
        env: ENV,
    ) -> HostExportBuilderWithFunction<ID>
    where
        ENV: Send + 'static,
        ID: PluginIdentifier,
    {
        HostExportBuilderWithFunction {
            name: self.name,
            namespace: self.namespace,
            definition: Box::new(ExportDefinition::<(), (), _>::Bare {
                function: Box::new(f),
                env,
            }),
        }
    }
}

pub struct HostExportBuilderWithFunction<ID> {
    pub(crate) name: String,
    pub(crate) namespace: Option<String>,
    definition: Box<dyn ToExtern<ID>>,
}

impl<ID: PluginIdentifier> HostExportBuilderWithFunction<ID> {
    pub fn get_namespace(&self) -> &str {
        self.namespace
            .as_deref()
            .unwrap_or(crate::EXTISM_USER_MODULE)
    }
    pub fn namespace(mut self, namespace: impl AsRef<str>) -> Self {
        self.namespace = Some(namespace.as_ref().to_owned());
        self
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
