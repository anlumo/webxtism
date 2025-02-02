use std::sync::Arc;

use extism_convert::{FromBytes, Json, ToBytes};
use tracing::level_filters::LevelFilter;
use wasmer::StoreMut;
use webxtism::{HostExportBuilder, InMemoryVars, Plugin};

#[allow(unused)]
#[derive(serde::Deserialize, serde::Serialize, ToBytes, FromBytes, Debug)]
#[encoding(Json)]
struct TestOutput {
    pub count: i32,
}

fn hello_world(
    _store: &mut StoreMut<'_>,
    _env: &(),
    count: Json<TestOutput>,
) -> Result<Json<TestOutput>, ()> {
    tracing::info!("Host function called with {:?}", count.0);
    Ok(count)
}

#[tokio::main]
async fn main() {
    let subscriber = tracing_subscriber::fmt()
        .compact()
        .with_max_level(LevelFilter::DEBUG)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let host_function_plugin = reqwest::get(
        "https://github.com/extism/rust-pdk/raw/refs/heads/main/test/host_function.wasm",
    )
    .await
    .unwrap()
    .error_for_status()
    .unwrap()
    .bytes()
    .await
    .unwrap();

    let context = Arc::default();

    let plugin = Plugin::new(
        &context,
        "host_function",
        host_function_plugin.as_ref(),
        [HostExportBuilder::new("hello_world")
            .namespace("extism:env/user")
            .function_in_out(hello_world, ())],
        InMemoryVars::default(),
        #[cfg(feature = "wasix")]
        None,
    )
    .await
    .unwrap();

    let result: Json<TestOutput> = plugin
        .call_in_out(context.store(), "count_vowels", "hello woorld".to_owned())
        .unwrap();

    println!("{:?}", result.0);
}
