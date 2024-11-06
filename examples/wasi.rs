use std::sync::Arc;

use extism_convert::{FromBytes, Json};
use tracing::level_filters::LevelFilter;
use wasmer_wasix::WasiEnv;
use webxtism::{Context, InMemoryVars, Plugin};

#[allow(unused)]
#[derive(serde::Deserialize, FromBytes, Debug)]
#[encoding(Json)]
struct TestOutput {
    pub count: i32,
}

#[tokio::main]
async fn main() {
    let subscriber = tracing_subscriber::fmt()
        .compact()
        .with_max_level(LevelFilter::DEBUG)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let count_vowels_plugin =
        reqwest::get("https://github.com/extism/rust-pdk/raw/refs/heads/main/count_vowels.wasm")
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .bytes()
            .await
            .unwrap();

    let context = Arc::new(Context::default());
    let mut wasi_env = WasiEnv::builder("count_vowels")
        .finalize(&mut context.store())
        .unwrap();

    let plugin = Plugin::new(
        &context,
        "count_vowels",
        count_vowels_plugin.as_ref(),
        [],
        InMemoryVars::default(),
        Some(&mut wasi_env),
    )
    .await
    .unwrap();

    let result: Json<TestOutput> = plugin
        .call_in_out(context.store(), "count_vowels", "hello woorld".to_owned())
        .unwrap();

    println!("{:?}", result.0);
}
