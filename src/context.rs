use std::sync::RwLock;

use typed_builder::TypedBuilder;
use wasmer::{Module, NativeEngineExt, Store, TrapHandlerFn};

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
    #[builder(default, setter(strip_option))]
    trap_handler: Option<Box<TrapHandlerFn<'static>>>,
    #[builder(default, setter(transform=|tunables: impl wasmer::Tunables + Send + Sync + 'static| Some(Box::new(tunables) as Box<dyn wasmer::Tunables + Send + Sync + 'static>)))]
    tunables: Option<Box<dyn wasmer::Tunables + Send + Sync + 'static>>,
}

impl From<ContextSettings> for Context {
    fn from(builder: ContextSettings) -> Self {
        let mut store = Store::default();
        store.set_trap_handler(builder.trap_handler);
        if let Some(tunables) = builder.tunables {
            store.engine_mut().set_tunables(tunables);
        }

        let runtime = Module::new(&store, RUNTIME).unwrap();

        Self {
            store: RwLock::new(store),
            runtime,
        }
    }
}
