mod context;
mod function;
mod plugin;
mod wasm_input;

pub use context::{Context, ContextSettings};
pub use function::{HostExportBuilder, HostExportBuilderWithFunction};
pub use plugin::{
    kernel::{Kernel, KernelError},
    vars::{InMemoryVars, PluginVars},
    Plugin, PluginIdentifier, PluginInstantiationError, PluginLoadError, PluginRunError,
};
pub use wasm_input::WasmInput;

pub use extism_convert;
pub use extism_manifest;
pub use wasmer;
#[cfg(feature = "wasix")]
pub use wasmer_wasix;

pub const EXTISM_ENV_MODULE: &str = "extism:host/env";
pub const EXTISM_USER_MODULE: &str = "extism:host/user";
