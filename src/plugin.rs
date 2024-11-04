use std::{
    collections::BTreeMap,
    str::FromStr,
    sync::{atomic::AtomicUsize, Arc, Weak},
};

use dashmap::DashMap;
use extism_convert::{FromBytesOwned, ToBytes};
use extism_manifest::{Manifest, Wasm};
use reqwest::{header::HeaderValue, Method};
use thiserror::Error;
use tracing::{event, Level};
use wasmer::{
    CompileError, ExportError, Extern, ExternType, Function, FunctionEnv, Imports, Instance,
    InstantiationError, Memory, Module, RuntimeError,
};

pub mod kernel;

use crate::{Context, WasmInput, EXTISM_ENV_MODULE};
use kernel::Kernel;

pub trait PluginIdentifier: AsRef<str> + Send + Sync + 'static {}
impl<T> PluginIdentifier for T where T: AsRef<str> + Send + Sync + 'static {}

const DEFAULT_MODULE_NAME: &str = "main";

static PLUGIN_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_arch = "wasm32")]
thread_local! {
    static THREAD_LOCAL_MAP: std::cell::RefCell<std::collections::HashMap<usize, BTreeMap<String, Instance>>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

#[cfg(not(target_arch = "wasm32"))]
static GLOBAL_MAP: std::sync::LazyLock<DashMap<usize, BTreeMap<String, Instance>>> =
    std::sync::LazyLock::new(Default::default);

fn new_plugin_id() -> usize {
    PLUGIN_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[allow(unused)]
pub struct Plugin<ID> {
    id: usize,
    context: Weak<Context>,
    modules: BTreeMap<String, Module>,
    metadata: Arc<PluginMetadata<ID>>,
    pub kernel: Arc<Kernel>,
}

impl<ID: PluginIdentifier> Plugin<ID> {
    pub async fn new<'a>(
        context: &Arc<Context>,
        id: ID,
        input: impl Into<WasmInput<'a>>,
        imports: impl IntoIterator<Item = crate::HostExport>,
    ) -> Result<Self, PluginLoadError> {
        let plugin_id = new_plugin_id();
        let kernel = Arc::new(Kernel::new(context, plugin_id));

        let (modules, metadata) = match input.into() {
            WasmInput::Data(data) => {
                let store = context.store.read().unwrap();
                let mut module = Module::new(&store, data)?;
                let name = if let Some(name) = module.name() {
                    name.to_owned()
                } else {
                    module.set_name(DEFAULT_MODULE_NAME);
                    DEFAULT_MODULE_NAME.to_owned()
                };

                (
                    BTreeMap::from([(name, module)]),
                    Arc::new(PluginMetadata {
                        id,
                        config: Default::default(),
                        vars: Default::default(),
                    }),
                )
            }
            WasmInput::Manifest(manifest) => (
                Self::load_manifest(context, &manifest).await?,
                Arc::new(PluginMetadata {
                    id,
                    config: match manifest {
                        std::borrow::Cow::Borrowed(manifest) => manifest.config.clone(),
                        std::borrow::Cow::Owned(manifest) => manifest.config,
                    },
                    vars: Default::default(),
                }),
            ),
        };
        let external_imports = imports
            .into_iter()
            .map(|e| ((e.namespace().to_owned(), e.name), e.wasmer_extern))
            .chain(
                Self::host_env(context, &kernel, metadata.clone())
                    .into_iter()
                    .map(|(name, external)| ((EXTISM_ENV_MODULE.to_owned(), name), external)),
            )
            .collect();
        let mut instances = BTreeMap::new();
        for module in modules.values() {
            Self::instantiate_module(
                context,
                module,
                &mut instances,
                &modules,
                &external_imports,
                &[],
            )?;
        }

        #[cfg(target_arch = "wasm32")]
        {
            THREAD_LOCAL_MAP.with(|map| {
                map.borrow_mut().insert(plugin_id, instances);
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            GLOBAL_MAP.insert(plugin_id, instances);
        }

        Ok(Self {
            id: plugin_id,
            context: Arc::downgrade(context),
            modules,
            metadata,
            kernel,
        })
    }

    pub fn context(&self) -> Result<Arc<Context>, ContextGoneError> {
        Ok(self
            .context
            .upgrade()
            .ok_or(ContextGoneError::ContextGone)?
            .clone())
    }

    fn with_instances<R>(
        &self,
        f: impl FnOnce(&BTreeMap<String, Instance>) -> R,
    ) -> Result<R, PluginRunError> {
        #[cfg(target_arch = "wasm32")]
        {
            THREAD_LOCAL_MAP.with(|map| {
                let plugins = map.borrow();
                let Some(instances) = plugins.get(&self.id) else {
                    return Err(PluginRunError::ContextGone);
                };
                Ok(f(instances))
            })
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Some(instances) = GLOBAL_MAP.get(&self.id) else {
                return Err(PluginRunError::ContextGone);
            };
            Ok(f(&instances))
        }
    }

    #[allow(clippy::result_large_err)]
    fn instantiate_module(
        context: &Context,
        module: &Module,
        loaded: &mut BTreeMap<String, Instance>,
        all_modules: &BTreeMap<String, Module>,
        external_imports: &BTreeMap<(String, String), Extern>,
        dependency_tree: &[&str],
    ) -> Result<(), PluginInstantiationError> {
        if loaded.contains_key(module.name().unwrap()) {
            return Ok(());
        }
        if dependency_tree.contains(&module.name().unwrap()) {
            return Err(PluginInstantiationError::CyclicDependency);
        }

        let mut imports = Imports::new();

        for import in module.imports() {
            let dependency_name = import.module();
            if dependency_name == crate::EXTISM_ENV_MODULE
                && !matches!(import.ty(), ExternType::Function(_))
            {
                return Err(PluginInstantiationError::NonFunctionKernelAccess);
            }
            if let Some(memory_type) = import.ty().memory() {
                if dependency_name == "env" {
                    // TODO: check for some limits there
                    let plugin_memory =
                        Memory::new(&mut context.store.write().unwrap(), *memory_type)?;
                    imports.define(dependency_name, import.name(), plugin_memory);
                } else {
                    return Err(PluginInstantiationError::MissingExport(
                        dependency_name.to_owned(),
                        import.name().to_owned(),
                    ));
                }
            } else if let Some(external) =
                external_imports.get(&(dependency_name.to_owned(), import.name().to_owned()))
            {
                let store = context.store.read().unwrap();
                if !external.ty(&store).is_compatible_with(import.ty(), None) {
                    return Err(PluginInstantiationError::IncompatibleType(
                        dependency_name.to_owned(),
                        import.name().to_owned(),
                    ));
                }
                imports.define(dependency_name, import.name(), external.clone());
            } else {
                if !loaded.contains_key(dependency_name) {
                    if let Some(dependency) = all_modules.get(dependency_name) {
                        let mut tree = dependency_tree.to_vec();
                        tree.push(module.name().unwrap());

                        Self::instantiate_module(
                            context,
                            dependency,
                            loaded,
                            all_modules,
                            external_imports,
                            &tree,
                        )?;
                    } else {
                        return Err(PluginInstantiationError::Export(ExportError::Missing(
                            dependency_name.to_owned(),
                        )));
                    }
                }
                if let Some(dependency) = loaded.get(dependency_name) {
                    let store = context.store.read().unwrap();
                    let Some(export) = dependency.exports.get_extern(import.name()) else {
                        return Err(PluginInstantiationError::MissingExport(
                            dependency_name.to_owned(),
                            import.name().to_owned(),
                        ));
                    };
                    if !export.ty(&store).is_compatible_with(import.ty(), None) {
                        return Err(PluginInstantiationError::IncompatibleType(
                            dependency_name.to_owned(),
                            import.name().to_owned(),
                        ));
                    }
                    imports.define(dependency_name, import.name(), export.clone());
                }
            }
        }

        let mut store = context.store.write().unwrap();
        let instance = Instance::new(&mut store, module, &imports)?;

        loaded.insert(module.name().unwrap().to_owned(), instance);

        Ok(())
    }

    async fn load_manifest(
        context: &Arc<Context>,
        manifest: &Manifest,
    ) -> Result<BTreeMap<String, Module>, PluginLoadError> {
        let tuples =
            futures_util::future::try_join_all(manifest.wasm.iter().map(|wasm| async move {
                match wasm {
                    Wasm::File { path, meta } => {
                        #[cfg(not(target_arch = "wasm32"))]
                        {
                            let data = tokio::fs::read(path).await?;
                            let store = context.store.read().unwrap();
                            let mut module = Module::new(&store, data)?;
                            if let Some(name) = &meta.name {
                                module.set_name(name);
                            } else if module.name().is_none() {
                                module.set_name(DEFAULT_MODULE_NAME);
                            }
                            Ok::<_, PluginLoadError>((
                                meta.name
                                    .as_ref()
                                    .cloned()
                                    .unwrap_or_else(|| DEFAULT_MODULE_NAME.to_owned()),
                                module,
                            ))
                        }
                        #[cfg(target_arch = "wasm32")]
                        {
                            let _ = (path, meta);
                            Err(PluginLoadError::NotSupportedOnWeb)
                        }
                    }
                    Wasm::Data { data, meta } => {
                        let store = context.store.read().unwrap();
                        let mut module = Module::new(&store, data)?;
                        event!(
                            Level::DEBUG,
                            "Module name from metadata: {:?}, from wasm: {:?}",
                            meta.name,
                            module.name()
                        );
                        if let Some(name) = &meta.name {
                            let success = module.set_name(name);
                            event!(
                                Level::DEBUG,
                                "module name set to {name:?} success = {success:?}"
                            );
                        } else if module.name().is_none() {
                            module.set_name(DEFAULT_MODULE_NAME);
                        }
                        Ok((
                            meta.name
                                .as_ref()
                                .cloned()
                                .unwrap_or_else(|| DEFAULT_MODULE_NAME.to_owned()),
                            module,
                        ))
                    }
                    Wasm::Url { req, meta } => {
                        let client = reqwest::Client::new();
                        let request = client
                            .request(
                                Method::from_str(req.method.as_deref().unwrap_or("GET"))
                                    .map_err(|_| PluginLoadError::InvalidHttpMethod)?,
                                &req.url,
                            )
                            .headers(
                                req.headers
                                    .iter()
                                    .map(|(name, value)| {
                                        Ok((name.try_into()?, HeaderValue::from_str(value)?))
                                    })
                                    .collect::<Result<_, PluginLoadError>>()?,
                            );
                        let response =
                            client.execute(request.build()?).await?.error_for_status()?;
                        let data = response.bytes().await?;

                        let store = context.store.read().unwrap();
                        let mut module = Module::new(&store, data)?;
                        if let Some(name) = &meta.name {
                            module.set_name(name);
                        } else if module.name().is_none() {
                            module.set_name(DEFAULT_MODULE_NAME);
                        }
                        Ok((
                            meta.name
                                .as_ref()
                                .cloned()
                                .unwrap_or_else(|| DEFAULT_MODULE_NAME.to_owned()),
                            module,
                        ))
                    }
                }
            }))
            .await?;

        Ok(tuples.into_iter().collect())
    }

    /// MARK: Call functions

    pub fn call_in_out<'i, IN: ToBytes<'i>, OUT: FromBytesOwned>(
        &self,
        name: &str,
        arg: IN,
    ) -> Result<OUT, PluginRunError> {
        let Some(context) = self.context.upgrade() else {
            return Err(PluginRunError::ContextGone);
        };

        let handle = self.kernel.memory_new(arg)?;
        if let Err(err) = self.kernel.set_input(handle) {
            if self.kernel.memory_free(handle).is_err() {
                tracing::error!("Failed to free memory {}", handle.offset);
            }
            return Err(err.into());
        }

        let err_handle = self.with_instances(|instances| {
            let Some(instance) = instances.get(DEFAULT_MODULE_NAME) else {
                tracing::event!(
                    Level::ERROR,
                    "No main module: {:?}",
                    instances.keys().collect::<Vec<_>>()
                );
                return Err(PluginRunError::NoMainModule);
            };

            let err_handle = {
                let mut store = context.store.write().unwrap();
                let function = instance.exports.get_function(name)?;
                tracing::event!(Level::DEBUG, "function type = {:?}", function.ty(&store));

                instance
                    .exports
                    .get_typed_function::<(), i32>(&store, name)?
                    .call(&mut store)?
            };
            Ok(err_handle)
        })??;

        if err_handle != 0 {
            todo!()
        }

        let output_handle = self.kernel.get_output()?;
        let output: OUT = self.kernel.memory_get(output_handle)?;

        if self.kernel.memory_free(output_handle).is_err() {
            tracing::error!("Failed to free memory {}", output_handle.offset);
        }

        Ok(output)
    }

    pub fn call_in<'i, IN: ToBytes<'i>>(&self, name: &str, arg: IN) -> Result<(), PluginRunError> {
        let Some(context) = self.context.upgrade() else {
            return Err(PluginRunError::ContextGone);
        };

        let mut store = context.store.write().unwrap();
        let handle = self.kernel.memory_new(arg)?;

        let err_handle = self.with_instances(|instances| {
            let Some(instance) = instances.get(DEFAULT_MODULE_NAME) else {
                tracing::event!(
                    Level::ERROR,
                    "No main module: {:?}",
                    instances.keys().collect::<Vec<_>>()
                );
                return Err(PluginRunError::NoMainModule);
            };

            self.kernel.set_input(handle)?;

            let err_handle = instance
                .exports
                .get_typed_function::<(), i32>(&store, name)?
                .call(&mut store)?;

            Ok(err_handle)
        })??;

        if err_handle != 0 {
            todo!()
        }

        Ok(())
    }

    pub fn call_out<OUT: FromBytesOwned>(&self, name: &str) -> Result<OUT, PluginRunError> {
        let Some(context) = self.context.upgrade() else {
            return Err(PluginRunError::ContextGone);
        };

        let mut store = context.store.write().unwrap();
        let err_handle = self.with_instances(|instances| {
            let Some(instance) = instances.get(DEFAULT_MODULE_NAME) else {
                tracing::event!(
                    Level::ERROR,
                    "No main module: {:?}",
                    instances.keys().collect::<Vec<_>>()
                );
                return Err(PluginRunError::NoMainModule);
            };
            let err_handle = instance
                .exports
                .get_typed_function::<(), i32>(&store, name)?
                .call(&mut store)?;

            Ok(err_handle)
        })??;

        if err_handle != 0 {
            todo!()
        }

        let output_handle = self.kernel.get_output()?;
        let output: OUT = self.kernel.memory_get(output_handle)?;

        if self.kernel.memory_free(output_handle).is_err() {
            tracing::error!("Failed to free memory {}", output_handle.offset);
        }

        Ok(output)
    }

    pub fn call(&self, name: &str) -> Result<(), PluginRunError> {
        let Some(context) = self.context.upgrade() else {
            return Err(PluginRunError::ContextGone);
        };

        let mut store = context.store.write().unwrap();
        let err_handle = self.with_instances(|instances| {
            let Some(instance) = instances.get(DEFAULT_MODULE_NAME) else {
                return Err(PluginRunError::NoMainModule);
            };
            let err_handle = instance
                .exports
                .get_typed_function::<(), i32>(&store, name)?
                .call(&mut store)?;

            Ok(err_handle)
        })??;

        if err_handle != 0 {
            todo!()
        }

        Ok(())
    }

    /// MARK: Runtime

    pub(crate) fn host_env(
        context: &Arc<Context>,
        kernel: &Arc<Kernel>,
        metadata: Arc<PluginMetadata<ID>>,
    ) -> BTreeMap<String, Extern> {
        let mut store = context.store.write().unwrap();

        let function_env = FunctionEnv::new(&mut store, (kernel.clone(), metadata));

        [
            ("memory", Extern::Memory(kernel.memory().unwrap())),
            (
                "log_warn",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Kernel::log_warn,
                )),
            ),
            (
                "log_info",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Kernel::log_info,
                )),
            ),
            (
                "log_debug",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Kernel::log_debug,
                )),
            ),
            (
                "log_error",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Kernel::log_error,
                )),
            ),
            (
                "log_trace",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Kernel::log_trace,
                )),
            ),
            (
                "get_log_level",
                Extern::Function(Function::new_typed(&mut store, Kernel::get_log_level)),
            ),
            (
                "config_get",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Kernel::config_get,
                )),
            ),
            (
                "var_get",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Kernel::var_get,
                )),
            ),
            (
                "var_set",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Kernel::var_set,
                )),
            ),
            (
                "http_request",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Kernel::http_request,
                )),
            ),
            (
                "http_status_code",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Kernel::http_status_code,
                )),
            ),
            (
                "http_headers",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Kernel::http_headers,
                )),
            ),
        ]
        .into_iter()
        .map(|(name, ext)| (name.to_string(), ext))
        .chain(kernel.exports())
        .collect()
    }
}

impl<ID> Drop for Plugin<ID> {
    fn drop(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            THREAD_LOCAL_MAP.with(|map| {
                map.borrow_mut().remove(&self.id);
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            GLOBAL_MAP.remove(&self.id);
        }
    }
}

#[derive(Debug, Error)]
pub enum PluginLoadError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Reqwest error: {0}")]
    ReqwestError(#[from] reqwest::Error),
    #[error("Invalid header name")]
    InvalidHeaderName,
    #[error("Invalid header value")]
    InvalidHeaderValue,
    #[error("Invalid HTTP method")]
    InvalidHttpMethod,
    #[error("Compiler: {0}")]
    CompileError(#[from] CompileError),
    #[error("Instantiation: {0}")]
    Instantiation(#[from] PluginInstantiationError),
    #[error("Not supported on the web platform")]
    NotSupportedOnWeb,
}

impl From<reqwest::header::InvalidHeaderName> for PluginLoadError {
    fn from(_err: reqwest::header::InvalidHeaderName) -> Self {
        PluginLoadError::InvalidHeaderName
    }
}

impl From<reqwest::header::InvalidHeaderValue> for PluginLoadError {
    fn from(_err: reqwest::header::InvalidHeaderValue) -> Self {
        PluginLoadError::InvalidHeaderValue
    }
}

#[derive(Debug, Error)]
pub enum PluginInstantiationError {
    #[error("Wasmer: {0}")]
    Wasmer(#[from] InstantiationError),
    #[error("Linked modules cannot access non-function exports of extism kernel")]
    NonFunctionKernelAccess,
    #[error("Export: {0}")]
    Export(#[from] ExportError),
    #[error("Dependency {0} is missing export {1}")]
    MissingExport(String, String),
    #[error("Cyclic dependency detected")]
    CyclicDependency,
    #[error("Incompatible type on namespace {0} extern {1}")]
    IncompatibleType(String, String),
    #[error("Memory: {0}")]
    Memory(#[from] wasmer::MemoryError),
}

#[derive(Debug, Clone)]
pub struct PluginMetadata<ID> {
    pub id: ID,
    pub config: BTreeMap<String, String>,
    pub vars: DashMap<String, Vec<u8>>,
}

#[derive(Debug, Error)]
pub enum PluginRunError {
    #[error("Context gone")]
    ContextGone,
    #[error("Context: {0}")]
    Context(#[from] kernel::KernelError),
    #[error("No main module")]
    NoMainModule,
    #[error("Export: {0}")]
    Export(#[from] ExportError),
    #[error("Runtime: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("Output handle not found")]
    OutputHandleNotFound,
    #[error("Extism convert: {0}")]
    Extism(#[from] extism_convert::Error),
}

#[derive(Debug, Error)]
pub enum ContextGoneError {
    #[error("Context gone")]
    ContextGone,
}

impl From<ContextGoneError> for PluginRunError {
    fn from(_err: ContextGoneError) -> Self {
        PluginRunError::ContextGone
    }
}
