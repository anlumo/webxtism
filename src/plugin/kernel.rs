use std::sync::Arc;

use extism_convert::{FromBytes, FromBytesOwned, MemoryHandle, ToBytes};
use thiserror::Error;
use wasmer::{
    AsStoreMut, AsStoreRef, FunctionEnvMut, Instance, Memory, MemoryAccessError, MemoryType,
    RuntimeError,
};

use crate::{Context, EXTISM_ENV_MODULE};

use super::{PluginIdentifier, PluginMetadata};

#[cfg(target_arch = "wasm32")]
thread_local! {
    static THREAD_LOCAL_MAP: std::cell::RefCell<std::collections::HashMap<usize, wasmer::Instance>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

pub struct Kernel {
    #[allow(unused)]
    id: usize,
    #[cfg(not(target_arch = "wasm32"))]
    runtime: wasmer::Instance,
}

impl Kernel {
    pub(super) fn new(context: &Arc<Context>, id: usize) -> Self {
        let mut store = context.store.write().unwrap();
        let memory = Memory::new(&mut store, MemoryType::new(0, None, false)).unwrap();
        let runtime = Instance::new(
            &mut store,
            &context.runtime,
            &wasmer::imports! {
                EXTISM_ENV_MODULE => {
                    "memory" => memory,
                }
            },
        )
        .unwrap();

        #[cfg(target_arch = "wasm32")]
        {
            THREAD_LOCAL_MAP.with(|map| {
                map.borrow_mut().insert(id, runtime);
            });
            Self { id }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self { id, runtime }
        }
    }

    // WARNING: On wasm32, this only works on the thread that created this kernel.
    // On other threads, `f` will not be called and None is returned.
    fn with_runtime<R>(&self, f: impl FnOnce(&wasmer::Instance) -> R) -> Option<R> {
        #[cfg(target_arch = "wasm32")]
        {
            THREAD_LOCAL_MAP.with(|map| map.borrow().get(&self.id).map(f))
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            Some(f(&self.runtime))
        }
    }

    pub(super) fn memory(&self) -> Result<Memory, KernelError> {
        self.with_runtime(|runtime| {
            runtime
                .exports
                .get_memory("memory")
                .cloned()
                .map_err(|err| err.into())
        })
        .unwrap_or(Err(KernelError::RuntimeNotFound))
    }

    pub(super) fn exports(&self) -> Vec<(String, wasmer::Extern)> {
        self.with_runtime(|runtime| {
            runtime
                .exports
                .iter()
                .map(|(name, f)| (name.clone(), f.clone()))
                .collect()
        })
        .unwrap_or_default()
    }

    pub fn memory_length(&self, mut store: impl AsStoreMut, offs: u64) -> Result<u64, KernelError> {
        self.with_runtime(|runtime| {
            Ok(runtime
                .exports
                .get_typed_function::<i64, i64>(&store, "length")
                .unwrap()
                .call(&mut store, offs as i64)? as u64)
        })
        .ok_or(KernelError::RuntimeNotFound)?
    }

    /// Allocate a handle large enough for the encoded Rust type and copy it into Extism memory
    pub fn memory_new<'a, T: ToBytes<'a>>(
        &self,
        mut store: impl AsStoreMut,
        t: T,
    ) -> Result<MemoryHandle, KernelError> {
        let data = t.to_bytes()?;
        let data = data.as_ref();
        if data.is_empty() {
            return Ok(MemoryHandle::null());
        }
        let handle = self.memory_alloc(&mut store, data.len() as u64)?;

        self.with_runtime::<Result<(), KernelError>>(|runtime| {
            let mem = runtime.exports.get_memory("memory")?.view(&store);

            mem.write(handle.offset, data.as_ref())?;

            Ok(())
        })
        .ok_or(KernelError::RuntimeNotFound)??;

        Ok(handle)
    }

    pub fn memory_alloc(
        &self,
        mut store: impl AsStoreMut,
        n: u64,
    ) -> Result<MemoryHandle, KernelError> {
        if n == 0 {
            return Ok(MemoryHandle {
                offset: 0,
                length: 0,
            });
        }

        let offs = self
            .with_runtime::<Result<_, RuntimeError>>(|runtime| {
                Ok(runtime
                    .exports
                    .get_typed_function::<i64, i64>(&store, "alloc")
                    .unwrap()
                    .call(&mut store, n as i64)? as u64)
            })
            .ok_or(KernelError::RuntimeNotFound)??;

        if offs == 0 {
            return Err(KernelError::OutOfMemory);
        }
        tracing::trace!("memory_alloc({}) = {}", offs, n);
        Ok(MemoryHandle {
            offset: offs,
            length: n,
        })
    }

    pub fn memory_handle(&self, store: impl AsStoreMut, offs: u64) -> Option<MemoryHandle> {
        if offs == 0 {
            return Some(MemoryHandle::null());
        }
        let len = self.memory_length(store, offs).unwrap_or_default();
        if len == 0 {
            tracing::trace!("memory handle not found: offs = {offs}",);
            return None;
        }

        tracing::trace!("memory handle found: offs = {offs}, length = {len}",);
        Some(MemoryHandle {
            offset: offs,
            length: len,
        })
    }

    /// Free a block of Extism plugin memory
    pub fn memory_free(
        &self,
        mut store: impl AsStoreMut,
        handle: MemoryHandle,
    ) -> Result<(), KernelError> {
        self.with_runtime(|runtime| {
            runtime
                .exports
                .get_typed_function::<i64, ()>(&store, "free")
                .unwrap()
                .call(&mut store, handle.offset as i64)
                .map_err(|e| e.into())
        })
        .ok_or(KernelError::RuntimeNotFound)?
    }

    pub fn memory_bytes(
        &self,
        store: impl AsStoreRef,
        handle: MemoryHandle,
    ) -> Result<Vec<u8>, KernelError> {
        self.with_runtime(|runtime| {
            let mem = runtime.exports.get_memory("memory")?.view(&store);
            mem.copy_range_to_vec(handle.offset..(handle.length + handle.offset))
                .map_err(|e| e.into())
        })
        .ok_or(KernelError::RuntimeNotFound)?
    }

    pub fn memory_get<T: FromBytesOwned>(
        &self,
        store: impl AsStoreRef,
        handle: MemoryHandle,
    ) -> Result<T, extism_convert::Error> {
        let bytes = self.memory_bytes(store, handle)?;
        T::from_bytes(&bytes)
    }

    pub fn memory_string(
        &self,
        store: impl AsStoreRef,
        handle: MemoryHandle,
    ) -> Result<String, KernelError> {
        let bytes = self.memory_bytes(store, handle)?;
        Ok(String::from_utf8(bytes)?)
    }

    pub fn set_input(
        &self,
        mut store: impl AsStoreMut,
        handle: MemoryHandle,
    ) -> Result<(), KernelError> {
        self.with_runtime(|runtime| {
            runtime
                .exports
                .get_typed_function::<(i64, i64), ()>(&store, "input_set")
                .unwrap()
                .call(&mut store, handle.offset as i64, handle.length as i64)
                .map_err(|e| e.into())
        })
        .ok_or(KernelError::RuntimeNotFound)?
    }

    pub fn set_output(
        &self,
        mut store: impl AsStoreMut,
        handle: MemoryHandle,
    ) -> Result<(), KernelError> {
        self.with_runtime(|runtime| {
            runtime
                .exports
                .get_typed_function::<(i64, i64), ()>(&store, "output_set")
                .unwrap()
                .call(&mut store, handle.offset as i64, handle.length as i64)
                .map_err(|e| e.into())
        })
        .ok_or(KernelError::RuntimeNotFound)?
    }

    pub fn get_output(&self, mut store: impl AsStoreMut) -> Result<MemoryHandle, KernelError> {
        let (offset, length) = self
            .with_runtime::<Result<_, RuntimeError>>(|runtime| {
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
                Ok((offset, length))
            })
            .ok_or(KernelError::RuntimeNotFound)??;
        Ok(MemoryHandle { offset, length })
    }

    pub fn get_input(&self, mut store: impl AsStoreMut) -> Result<MemoryHandle, KernelError> {
        let (offset, length) = self
            .with_runtime::<Result<_, RuntimeError>>(|runtime| {
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
                Ok((offset, length))
            })
            .ok_or(KernelError::RuntimeNotFound)??;
        Ok(MemoryHandle { offset, length })
    }

    fn log(
        &self,
        mut store: impl AsStoreMut,
        level: tracing::Level,
        id: impl AsRef<str>,
        text: i64,
    ) {
        let span = tracing::span!(tracing::Level::TRACE, "log", plugin = id.as_ref());
        let _enter = span.enter();

        let Some(handle) = self.memory_handle(&mut store, text as _) else {
            return;
        };
        let text = match self.memory_string(&store, handle) {
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

        if let Err(err) = self.memory_free(store, handle) {
            tracing::error!("Failed freeing extism memory: {err:?}");
        }
    }

    pub(super) fn log_warn<ID: PluginIdentifier>(
        mut env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>,
        text: i64,
    ) {
        let ((plugin, metadata), store) = env.data_and_store_mut();
        plugin.log(store, tracing::Level::WARN, &metadata.id, text);
    }
    pub(super) fn log_info<ID: PluginIdentifier>(
        mut env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>,
        text: i64,
    ) {
        let ((plugin, metadata), store) = env.data_and_store_mut();
        plugin.log(store, tracing::Level::INFO, &metadata.id, text);
    }
    pub(super) fn log_debug<ID: PluginIdentifier>(
        mut env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>,
        text: i64,
    ) {
        let ((plugin, metadata), store) = env.data_and_store_mut();
        plugin.log(store, tracing::Level::DEBUG, &metadata.id, text);
    }
    pub(super) fn log_error<ID: PluginIdentifier>(
        mut env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>,
        text: i64,
    ) {
        let ((plugin, metadata), store) = env.data_and_store_mut();
        plugin.log(store, tracing::Level::ERROR, &metadata.id, text);
    }
    pub(super) fn log_trace<ID: PluginIdentifier>(
        mut env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>,
        text: i64,
    ) {
        let ((plugin, metadata), store) = env.data_and_store_mut();
        plugin.log(store, tracing::Level::TRACE, &metadata.id, text);
    }
    pub(super) fn get_log_level() -> i32 {
        let level = tracing::level_filters::LevelFilter::current();
        if level == tracing::level_filters::LevelFilter::OFF {
            i32::MAX
        } else {
            log_level_to_int(level.into_level().unwrap_or(tracing::Level::ERROR))
        }
    }

    /// Get a configuration value
    /// Params: i64 (offset)
    /// Returns: i64 (offset)
    /// **Note**: this function takes ownership of the handle passed in
    /// the caller should not `free` this value
    pub(super) fn config_get<ID: PluginIdentifier>(
        mut env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>,
        key: i64,
    ) -> i64 {
        let ((context, metadata), mut store) = env.data_and_store_mut();
        let span = tracing::span!(
            tracing::Level::TRACE,
            "config_get",
            plugin = metadata.id.as_ref()
        );
        let _enter = span.enter();

        let Some(handle) = context.memory_handle(&mut store, key as _) else {
            tracing::warn!("Memory handle not found: {key}");
            return 0;
        };
        let key = match context.memory_string(&mut store, handle) {
            Ok(text) => text,
            Err(err) => {
                tracing::error!("Memory access error: {err}");
                return 0;
            }
        };
        if let Err(err) = context.memory_free(&mut store, handle) {
            tracing::error!("Failed freeing extism memory: {err:?}");
        }

        let Some(value) = metadata.config.get(&key) else {
            tracing::warn!("Config key not found: {key}");
            return 0;
        };

        let handle = match context.memory_new(store, value.as_bytes()) {
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
    pub(super) fn var_get<ID: PluginIdentifier>(
        mut env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>,
        key: i64,
    ) -> i64 {
        let ((context, metadata), mut store) = env.data_and_store_mut();
        let span = tracing::span!(
            tracing::Level::TRACE,
            "var_get",
            plugin = metadata.id.as_ref()
        );
        let _enter = span.enter();

        let Some(handle) = context.memory_handle(&mut store, key as _) else {
            tracing::warn!("Memory handle not found: {key}");
            return 0;
        };
        let key = match context.memory_string(&mut store, handle) {
            Ok(text) => text,
            Err(err) => {
                tracing::error!("Memory access error: {err}");
                return 0;
            }
        };
        if let Err(err) = context.memory_free(&mut store, handle) {
            tracing::error!("Failed freeing extism memory: {err:?}");
        }

        let Some(entry) = metadata.vars.get_var(&key) else {
            tracing::warn!("Var key not found: {key}");
            return 0;
        };

        let handle = match context.memory_new(store, entry) {
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
    pub(super) fn var_set<ID: PluginIdentifier>(
        mut env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>,
        key: i64,
        value: i64,
    ) {
        let ((context, metadata), mut store) = env.data_and_store_mut();
        let span = tracing::span!(
            tracing::Level::TRACE,
            "var_set",
            plugin = metadata.id.as_ref()
        );
        let _enter = span.enter();

        let Some(key_handle) = context.memory_handle(&mut store, key as _) else {
            tracing::warn!("Memory handle not found: {key}");
            let Some(value_handle) = context.memory_handle(&mut store, value as _) else {
                tracing::warn!("Memory handle not found: {value}");
                return;
            };
            if let Err(err) = context.memory_free(&mut store, value_handle) {
                tracing::error!("Failed freeing extism memory: {err:?}");
            }
            return;
        };
        let Some(value_handle) = context.memory_handle(&mut store, value as _) else {
            tracing::warn!("Memory handle not found: {value}");
            return;
        };
        let key = match context.memory_string(&mut store, key_handle) {
            Ok(text) => text,
            Err(err) => {
                tracing::error!("Memory access error: {err}");
                if let Err(err) = context.memory_free(&mut store, value_handle) {
                    tracing::error!("Failed freeing extism memory: {err:?}");
                }
                return;
            }
        };
        if let Err(err) = context.memory_free(&mut store, key_handle) {
            tracing::error!("Failed freeing extism memory: {err:?}");
        }

        let Some(value_handle) = context.memory_handle(&mut store, value as _) else {
            tracing::warn!("Memory handle not found: {key}");
            return;
        };
        let value = match context.memory_bytes(&mut store, value_handle) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::error!("Memory access error: {err}");
                return;
            }
        };

        if let Err(err) = context.memory_free(store, value_handle) {
            tracing::error!("Failed freeing extism memory: {err:?}");
        }

        metadata.vars.set_var(key, value);
    }

    /// Make an HTTP request
    /// Params: i64 (offset to JSON encoded HttpRequest), i64 (offset to body or 0)
    /// Returns: i64 (offset)
    /// **Note**: this function takes ownership of the handles passed in
    /// the caller should not `free` these values, the result will need to
    /// be freed.
    pub(super) fn http_request<ID: PluginIdentifier>(
        _env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>,
        _url: i64,
        _body: i64,
    ) -> i64 {
        todo!()
    }

    /// Get the status code of the last HTTP request
    /// Params: none
    /// Returns: i32 (status code)
    pub(super) fn http_status_code<ID: PluginIdentifier>(
        _env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>,
    ) -> i64 {
        todo!()
    }

    /// Get the HTTP response headers from the last HTTP request
    /// Params: none
    /// Returns: i64 (offset)
    pub(super) fn http_headers<ID: PluginIdentifier>(
        _env: FunctionEnvMut<(Arc<Self>, Arc<PluginMetadata<ID>>)>,
    ) -> i64 {
        todo!()
    }
}

#[cfg(target_arch = "wasm32")]
impl Drop for Kernel {
    fn drop(&mut self) {
        THREAD_LOCAL_MAP.with(|map| {
            map.borrow_mut().remove(&self.id);
        });
    }
}

#[derive(Debug, Error)]
pub enum KernelError {
    #[error("Out of memory")]
    OutOfMemory,
    #[error("Memory access")]
    MemoryAccess(#[from] MemoryAccessError),
    #[error("Runtime: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("Extism: {0}")]
    Extism(#[from] extism_convert::Error),
    #[error("Runtime not found")]
    RuntimeNotFound,
    #[error("UTF8 conversion failed: {0}")]
    FromUtf8Error(#[from] std::string::FromUtf8Error),
    #[error("Export: {0}")]
    Export(#[from] wasmer::ExportError),
}

/// Convert log level to integer
const fn log_level_to_int(level: tracing::Level) -> i32 {
    match level {
        tracing::Level::TRACE => 0,
        tracing::Level::DEBUG => 1,
        tracing::Level::INFO => 2,
        tracing::Level::WARN => 3,
        tracing::Level::ERROR => 4,
    }
}
