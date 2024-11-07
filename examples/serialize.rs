use std::sync::Arc;

use extism_convert::{FromBytes, Json};
use tracing::level_filters::LevelFilter;
use webxtism::{InMemoryVars, Plugin};

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

    let context = Arc::default();

    let data = {
        let plugin = Plugin::new(
            &context,
            "count_vowels",
            count_vowels_plugin.as_ref(),
            [],
            InMemoryVars::default(),
            #[cfg(feature = "wasix")]
            None,
        )
        .await
        .unwrap();

        serde_cbor::to_vec(plugin.as_ref()).unwrap()
    };

    println!("Serialized plugin size: {:?} bytes.", data.len());

    // SAFETY: We know the plugin is valid and the bytes are in the correct format, because
    // we just generated them with the same engine.
    let plugin = unsafe {
        Plugin::deserialize(
            &context,
            "count_vowels",
            &mut serde_cbor::Deserializer::from_slice(&data),
            [],
            InMemoryVars::default(),
            #[cfg(feature = "wasix")]
            None,
        )
        .await
        .unwrap()
    };

    let result: Json<TestOutput> = plugin
        .call_in_out(context.store(), "count_vowels", "hello woorld".to_owned())
        .unwrap();

    println!("{:?}", result.0);
}
