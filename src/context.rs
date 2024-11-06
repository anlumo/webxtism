use std::sync::RwLock;

use typed_builder::TypedBuilder;
use wasmer::{Module, Store};

#[cfg(not(target_arch = "wasm32"))]
use wasmer::{NativeEngineExt, TrapHandlerFn};

const RUNTIME: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/extism-runtime.wasm"));

pub struct Context {
    pub(crate) store: RwLock<Store>,
    pub(crate) runtime: Module,
}

impl Default for Context {
    fn default() -> Self {
        ContextSettings::builder().build()
    }
}

impl Context {
    pub fn store(&self) -> std::sync::RwLockWriteGuard<'_, wasmer::Store> {
        self.store.write().unwrap()
    }
}

#[derive(TypedBuilder)]
#[builder(build_method(into = Context))]
pub struct ContextSettings {
    #[cfg(not(target_arch = "wasm32"))]
    #[builder(default, setter(strip_option))]
    trap_handler: Option<Box<TrapHandlerFn<'static>>>,
    #[cfg(not(target_arch = "wasm32"))]
    #[builder(default, setter(transform=|tunables: impl wasmer::Tunables + Send + Sync + 'static| Some(Box::new(tunables) as Box<dyn wasmer::Tunables + Send + Sync + 'static>)))]
    tunables: Option<Box<dyn wasmer::Tunables + Send + Sync + 'static>>,
}

impl From<ContextSettings> for Context {
    #[allow(unused, unused_mut)]
    fn from(builder: ContextSettings) -> Self {
        let mut store = Store::default();
        #[cfg(not(target_arch = "wasm32"))]
        {
            store.set_trap_handler(builder.trap_handler);
            if let Some(tunables) = builder.tunables {
                store.engine_mut().set_tunables(tunables);
            }
        }

        let runtime = Module::new(&store, RUNTIME).unwrap();

        Self {
            store: RwLock::new(store),
            runtime,
        }
    }
}
