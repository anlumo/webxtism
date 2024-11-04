use std::{env, io::Write, path::PathBuf};

fn main() {
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());

    let response = reqwest::blocking::get(
        "https://github.com/extism/extism/raw/refs/heads/main/runtime/src/extism-runtime.wasm",
    )
    .unwrap()
    .error_for_status()
    .unwrap();
    let mut file = std::fs::File::create(out_path.join("extism-runtime.wasm")).unwrap();
    file.write_all(&response.bytes().unwrap()).unwrap();
}
