use std::sync::RwLock;

use typed_builder::TypedBuilder;
use wasmer::{Module, Store};

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
    engine: Option<wasmer::Engine>,
}

impl From<ContextSettings> for Context {
    fn from(builder: ContextSettings) -> Self {
        let store = if let Some(engine) = builder.engine {
            Store::new(engine)
        } else {
            Store::default()
        };

        let runtime = Module::new(&store, RUNTIME).unwrap();

        Self {
            store: RwLock::new(store),
            runtime,
        }
    }
}
