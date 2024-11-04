use std::{
    collections::BTreeMap,
    marker::PhantomData,
    sync::{Arc, RwLock},
};

use extism_convert::{FromBytes, FromBytesOwned, MemoryHandle, ToBytes};
use thiserror::Error;
use tracing::trace;
use wasmer::{
    imports, Extern, Function, FunctionEnv, FunctionEnvMut, Instance, Memory, MemoryAccessError,
    MemoryType, Module, RuntimeError, Store,
};

use crate::{
    plugin::{PluginIdentifier, PluginMetadata},
    EXTISM_ENV_MODULE,
};

const RUNTIME: &[u8] = include_bytes!("../extism-runtime.wasm");

pub struct Context<ID: PluginIdentifier> {
    pub(crate) store: RwLock<Store>,
    pub(crate) runtime: RwLock<Instance>,
    _phantom: PhantomData<ID>,
}

impl<ID: PluginIdentifier> Default for Context<ID> {
    fn default() -> Self {
        // TODO: allow for configuration on non-web targets
        let mut store = Store::default();
        let memory = Memory::new(&mut store, MemoryType::new(0, None, false)).unwrap();

        let module = Module::new(&store, RUNTIME).unwrap();
        let runtime = Instance::new(
            &mut store,
            &module,
            &imports! {
                EXTISM_ENV_MODULE => {
                    "memory" => memory,
                }
            },
        )
        .unwrap();

        Self {
            store: RwLock::new(store),
            runtime: RwLock::new(runtime),
            _phantom: Default::default(),
        }
    }
}

impl<ID: PluginIdentifier> Context<ID> {
    pub(crate) fn host_env(
        self: &Arc<Self>,
        metadata: Arc<PluginMetadata<ID>>,
    ) -> BTreeMap<String, Extern> {
        let mut store = self.store.write().unwrap();
        let runtime = self.runtime.read().unwrap();

        let function_env = FunctionEnv::new(&mut store, (self.clone(), metadata));

        [
            (
                "memory",
                Extern::Memory(runtime.exports.get_memory("memory").unwrap().clone()),
            ),
            (
                "log_warn",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Self::log_warn,
                )),
            ),
            (
                "log_info",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Self::log_info,
                )),
            ),
            (
                "log_debug",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Self::log_debug,
                )),
            ),
            (
                "log_error",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Self::log_error,
                )),
            ),
            (
                "log_trace",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Self::log_trace,
                )),
            ),
            (
                "get_log_level",
                Extern::Function(Function::new_typed(&mut store, Self::get_log_level)),
            ),
            (
                "config_get",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Self::config_get,
                )),
            ),
            (
                "var_get",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Self::var_get,
                )),
            ),
            (
                "var_set",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Self::var_set,
                )),
            ),
            (
                "http_request",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Self::http_request,
                )),
            ),
            (
                "http_status_code",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Self::http_status_code,
                )),
            ),
            (
                "http_headers",
                Extern::Function(Function::new_typed_with_env(
                    &mut store,
                    &function_env,
                    Self::http_headers,
                )),
            ),
        ]
        .into_iter()
        .map(|(name, ext)| (name.to_string(), ext))
        .chain(
            runtime
                .exports
                .iter()
                .map(|(name, f)| (name.clone(), f.clone())),
        )
        .collect()
    }

    pub fn memory_length(&self, offs: u64) -> Result<u64, RuntimeError> {
        let mut store = self.store.write().unwrap();
        let runtime = self.runtime.read().unwrap();

        let len = runtime
            .exports
            .get_typed_function::<i64, i64>(&store, "length")
            .unwrap()
            .call(&mut store, offs as i64)? as u64;

        Ok(len)
    }

    /// Allocate a handle large enough for the encoded Rust type and copy it into Extism memory
    pub fn memory_new<'a, T: ToBytes<'a>>(&self, t: T) -> Result<MemoryHandle, ContextError> {
        let data = t.to_bytes()?;
        let data = data.as_ref();
        if data.is_empty() {
            return Ok(MemoryHandle::null());
        }
        let handle = self.memory_alloc(data.len() as u64)?;

        let store = self.store.read().unwrap();
        let runtime = self.runtime.read().unwrap();
        let mem = runtime.exports.get_memory("memory").unwrap().view(&store);

        mem.write(handle.offset, data.as_ref())?;

        Ok(handle)
    }

    pub fn memory_alloc(&self, n: u64) -> Result<MemoryHandle, ContextError> {
        if n == 0 {
            return Ok(MemoryHandle {
                offset: 0,
                length: 0,
            });
        }

        let mut store = self.store.write().unwrap();
        let runtime = self.runtime.read().unwrap();

        let offs = runtime
            .exports
            .get_typed_function::<i64, i64>(&store, "alloc")
            .unwrap()
            .call(&mut store, n as i64)? as u64;

        if offs == 0 {
            return Err(ContextError::OutOfMemory);
        }
        trace!("memory_alloc({}) = {}", offs, n);
        Ok(MemoryHandle {
            offset: offs,
            length: n,
        })
    }

    pub fn memory_handle(&self, offs: u64) -> Option<MemoryHandle> {
        if offs == 0 {
            return Some(MemoryHandle::null());
        }
        let len = self.memory_length(offs).unwrap_or_default();
        if len == 0 {
            trace!("memory handle not found: offs = {offs}",);
            return None;
        }

        trace!("memory handle found: offs = {offs}, length = {len}",);
        Some(MemoryHandle {
            offset: offs,
            length: len,
        })
    }

    /// Free a block of Extism plugin memory
    pub fn memory_free(&self, handle: MemoryHandle) -> Result<(), RuntimeError> {
        let mut store = self.store.write().unwrap();
        let runtime = self.runtime.read().unwrap();

        runtime
            .exports
            .get_typed_function::<i64, ()>(&store, "free")
            .unwrap()
            .call(&mut store, handle.offset as i64)
    }

    pub fn memory_bytes(&self, handle: MemoryHandle) -> Result<Vec<u8>, MemoryAccessError> {
        let store = self.store.read().unwrap();
        let runtime = self.runtime.read().unwrap();

        let mem = runtime.exports.get_memory("memory").unwrap().view(&store);

        mem.copy_range_to_vec(handle.offset..(handle.length + handle.offset))
    }

    pub fn memory_get<T: FromBytesOwned>(
        &self,
        handle: MemoryHandle,
    ) -> Result<T, extism_convert::Error> {
        let bytes = self.memory_bytes(handle)?;
        T::from_bytes(&bytes)
    }

    pub fn memory_string(&self, handle: MemoryHandle) -> Result<String, MemoryAccessError> {
        let bytes = self.memory_bytes(handle)?;
        Ok(String::from_utf8(bytes)?)
    }

    pub fn set_input(&self, handle: MemoryHandle) -> Result<(), RuntimeError> {
        let mut store = self.store.write().unwrap();
        let runtime = self.runtime.read().unwrap();

        runtime
            .exports
            .get_typed_function::<(i64, i64), ()>(&store, "input_set")
            .unwrap()
            .call(&mut store, handle.offset as i64, handle.length as i64)
    }

    pub fn set_output(&self, handle: MemoryHandle) -> Result<(), RuntimeError> {
        let mut store = self.store.write().unwrap();
        let runtime = self.runtime.read().unwrap();

        runtime
            .exports
            .get_typed_function::<(i64, i64), ()>(&store, "output_set")
            .unwrap()
            .call(&mut store, handle.offset as i64, handle.length as i64)
    }

    pub fn get_output(&self) -> Result<MemoryHandle, RuntimeError> {
        let mut store = self.store.write().unwrap();
        let runtime = self.runtime.read().unwrap();

        let offset = runtime
            .exports
            .get_typed_function::<(), i64>(&store, "output_offset")
            .unwrap()
            .call(&mut store)? as u64;
        let length = runtime
            .exports
            .get_typed_function::<(), i64>(&store, "output_length")
            .unwrap()
            .call(&mut store)? as u64;
        Ok(MemoryHandle { offset, length })
    }

    pub fn get_input(&self) -> Result<MemoryHandle, RuntimeError> {
        let mut store = self.store.write().unwrap();
        let runtime = self.runtime.read().unwrap();

        let offset = runtime
            .exports
            .get_typed_function::<(), i64>(&store, "input_offset")
            .unwrap()
            .call(&mut store)? as u64;
        let length = runtime
            .exports
            .get_typed_function::<(), i64>(&store, "input_length")
            .unwrap()
            .call(&mut store)? as u64;
        Ok(MemoryHandle { offset, length })
    }

    fn log(&self, level: tracing::Level, id: impl AsRef<str>, text: i64) {
        let span = tracing::span!(tracing::Level::TRACE, "log", plugin = id.as_ref());
        let _enter = span.enter();

        let Some(handle) = self.memory_handle(text as _) else {
            return;
        };
        let text = match self.memory_string(handle) {
            Ok(text) => text,
            Err(err) => {
                tracing::error!("Memory access error: {err}");
                return;
            }
        };

        match level {
            tracing::Level::TRACE => tracing::trace!("{text}"),
            tracing::Level::DEBUG => tracing::debug!("{text}"),
            tracing::Level::INFO => tracing::info!("{text}"),
            tracing::Level::WARN => tracing::warn!("{text}"),
            tracing::Level::ERROR => tracing::error!("{text}"),
        }

        if let Err(err) = self.memory_free(handle) {
            tracing::error!("Failed freeing extism memory: {err:?}");
        }
    }

    fn log_warn(env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>, text: i64) {
        let (context, metadata) = env.data();
        context.log(tracing::Level::WARN, &metadata.id, text);
    }
    fn log_info(env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>, text: i64) {
        let (context, metadata) = env.data();
        context.log(tracing::Level::INFO, &metadata.id, text);
    }
    fn log_debug(env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>, text: i64) {
        let (context, metadata) = env.data();
        context.log(tracing::Level::DEBUG, &metadata.id, text);
    }
    fn log_error(env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>, text: i64) {
        let (context, metadata) = env.data();
        context.log(tracing::Level::ERROR, &metadata.id, text);
    }
    fn log_trace(env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>, text: i64) {
        let (context, metadata) = env.data();
        context.log(tracing::Level::TRACE, &metadata.id, text);
    }
    fn get_log_level() -> i64 {
        let level = tracing::level_filters::LevelFilter::current();
        if level == tracing::level_filters::LevelFilter::OFF {
            i64::MAX
        } else {
            log_level_to_int(level.into_level().unwrap_or(tracing::Level::ERROR))
        }
    }

    /// Get a configuration value
    /// Params: i64 (offset)
    /// Returns: i64 (offset)
    /// **Note**: this function takes ownership of the handle passed in
    /// the caller should not `free` this value
    fn config_get(env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>, key: i64) -> i64 {
        let (context, metadata) = env.data();
        let span = tracing::span!(
            tracing::Level::TRACE,
            "config_get",
            plugin = metadata.id.as_ref()
        );
        let _enter = span.enter();

        let Some(handle) = context.memory_handle(key as _) else {
            tracing::warn!("Memory handle not found: {key}");
            return 0;
        };
        let key = match context.memory_string(handle) {
            Ok(text) => text,
            Err(err) => {
                tracing::error!("Memory access error: {err}");
                return 0;
            }
        };
        if let Err(err) = context.memory_free(handle) {
            tracing::error!("Failed freeing extism memory: {err:?}");
        }

        let Some(value) = metadata.config.get(&key) else {
            tracing::warn!("Config key not found: {key}");
            return 0;
        };

        let handle = match context.memory_new(value.as_bytes()) {
            Ok(handle) => handle,
            Err(err) => {
                tracing::error!("Memory access error: {err}");
                return 0;
            }
        };

        handle.offset as i64
    }

    /// Get a variable
    /// Params: i64 (offset)
    /// Returns: i64 (offset)
    /// **Note**: this function takes ownership of the handle passed in
    /// the caller should not `free` this value, but the return value
    /// will need to be freed
    fn var_get(env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>, key: i64) -> i64 {
        let (context, metadata) = env.data();
        let span = tracing::span!(
            tracing::Level::TRACE,
            "var_get",
            plugin = metadata.id.as_ref()
        );
        let _enter = span.enter();

        let Some(handle) = context.memory_handle(key as _) else {
            tracing::warn!("Memory handle not found: {key}");
            return 0;
        };
        let key = match context.memory_string(handle) {
            Ok(text) => text,
            Err(err) => {
                tracing::error!("Memory access error: {err}");
                return 0;
            }
        };
        if let Err(err) = context.memory_free(handle) {
            tracing::error!("Failed freeing extism memory: {err:?}");
        }

        let Some(entry) = metadata.vars.get(&key) else {
            tracing::warn!("Var key not found: {key}");
            return 0;
        };

        let handle = match context.memory_new(entry.value()) {
            Ok(handle) => handle,
            Err(err) => {
                tracing::error!("Memory access error: {err}");
                return 0;
            }
        };

        handle.offset as i64
    }

    /// Set a variable, if the value offset is 0 then the provided key will be removed
    /// Params: i64 (key offset), i64 (value offset)
    /// Returns: none
    /// **Note**: this function takes ownership of the handles passed in
    /// the caller should not `free` these values
    fn var_set(env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>, key: i64, value: i64) {
        let (context, metadata) = env.data();
        let span = tracing::span!(
            tracing::Level::TRACE,
            "var_set",
            plugin = metadata.id.as_ref()
        );
        let _enter = span.enter();

        let Some(key_handle) = context.memory_handle(key as _) else {
            tracing::warn!("Memory handle not found: {key}");
            let Some(value_handle) = context.memory_handle(value as _) else {
                tracing::warn!("Memory handle not found: {value}");
                return;
            };
            if let Err(err) = context.memory_free(value_handle) {
                tracing::error!("Failed freeing extism memory: {err:?}");
            }
            return;
        };
        let Some(value_handle) = context.memory_handle(value as _) else {
            tracing::warn!("Memory handle not found: {value}");
            return;
        };
        let key = match context.memory_string(key_handle) {
            Ok(text) => text,
            Err(err) => {
                tracing::error!("Memory access error: {err}");
                if let Err(err) = context.memory_free(value_handle) {
                    tracing::error!("Failed freeing extism memory: {err:?}");
                }
                return;
            }
        };
        if let Err(err) = context.memory_free(key_handle) {
            tracing::error!("Failed freeing extism memory: {err:?}");
        }

        let Some(value_handle) = context.memory_handle(value as _) else {
            tracing::warn!("Memory handle not found: {key}");
            return;
        };
        let value = match context.memory_bytes(value_handle) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::error!("Memory access error: {err}");
                return;
            }
        };

        if let Err(err) = context.memory_free(value_handle) {
            tracing::error!("Failed freeing extism memory: {err:?}");
        }

        metadata.vars.insert(key, value);
    }

    /// Make an HTTP request
    /// Params: i64 (offset to JSON encoded HttpRequest), i64 (offset to body or 0)
    /// Returns: i64 (offset)
    /// **Note**: this function takes ownership of the handles passed in
    /// the caller should not `free` these values, the result will need to
    /// be freed.
    fn http_request(
        _env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>,
        _url: i64,
        _body: i64,
    ) -> i64 {
        todo!()
    }

    /// Get the status code of the last HTTP request
    /// Params: none
    /// Returns: i32 (status code)
    fn http_status_code(_env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>) -> i64 {
        todo!()
    }

    /// Get the HTTP response headers from the last HTTP request
    /// Params: none
    /// Returns: i64 (offset)
    fn http_headers(_env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>) -> i64 {
        todo!()
    }
}

/// Convert log level to integer
const fn log_level_to_int(level: tracing::Level) -> i64 {
    match level {
        tracing::Level::TRACE => 0,
        tracing::Level::DEBUG => 1,
        tracing::Level::INFO => 2,
        tracing::Level::WARN => 3,
        tracing::Level::ERROR => 4,
    }
}

#[derive(Debug, Error)]
pub enum ContextError {
    #[error("Out of memory")]
    OutOfMemory,
    #[error("Memory access")]
    MemoryAccess(#[from] MemoryAccessError),
    #[error("Runtime: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("Extism: {0}")]
    Extism(#[from] extism_convert::Error),
}
