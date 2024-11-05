use std::sync::RwLock;

use wasmer::{Module, Store};

const RUNTIME: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/extism-runtime.wasm"));

pub struct Context {
    pub(crate) store: RwLock<Store>,
    pub(crate) runtime: Module,
}

unsafe impl Send for Context {}
unsafe impl Sync for Context {}

impl Default for Context {
    fn default() -> Self {
        // TODO: allow for configuration on non-web targets
        let store = Store::default();
        let runtime = Module::new(&store, RUNTIME).unwrap();

        Self {
            store: RwLock::new(store),
            runtime,
        }
    }
}

impl Context {
    pub fn store(&self) -> std::sync::RwLockWriteGuard<'_, wasmer::Store> {
        self.store.write().unwrap()
    }
}
