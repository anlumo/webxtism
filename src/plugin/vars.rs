use dashmap::DashMap;

pub trait PluginVars: Send + Sync + 'static {
    fn get_var(&self, key: &str) -> Option<Vec<u8>>;
    fn set_var(&self, key: String, value: Vec<u8>);
}

/// Example implementation of the PluginVars trait.
/// It is empty by default and stores all variables in memory.
#[derive(Default, Debug)]
pub struct InMemoryVars {
    vars: DashMap<String, Vec<u8>>,
}

impl PluginVars for InMemoryVars {
    fn get_var(&self, key: &str) -> Option<Vec<u8>> {
        self.vars.get(key).map(|v| v.to_vec())
    }

    fn set_var(&self, key: String, value: Vec<u8>) {
        self.vars.insert(key, value);
    }
}
