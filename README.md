# WebXtism

WebXtism is a Rust library designed to provide a flexible and efficient runtime environment for WebAssembly (Wasm) plugins, particularly in a web context. It leverages the Wasmer runtime for executing Wasm modules and offers an extensible plugin system. The plugins are compatible with [extism](https://extism.org/) and this project is designed to use their PDK in any language they support.

This crate is highly experimental and not ready for any kind of production use!

## Features

- **Wasm Execution**: Run Wasm modules efficiently using Wasmer.
- **Plugin System**: Easily load and manage extism plugins from various sources such as files, data, or URLs.
- **Logging**: Integrated logging support for debugging and information tracking using the [tracing crate](https://github.com/tokio-rs/tracing).

## Web Support

Native (Windows/macOS/Linux/iOS/Android) and Web support using the same codebase is a major goal of this implementation. However, there is a caveat:

Web has some restrictions when it comes to threading. Rust nightly for wasm32-unknown-unknown has had support for native threads for a long time (when certain compile flags are enabled and cross origin isolation is activated), but JsValues cannot be shared (they don't implement Send or Sync). Transfer is possible using the Web API, but that has to be done manually and explicitly.

So, wasmer's `Instance` does not implement Send or Sync on the wasm32 target. This results in this project only supporting plugin function calls on the thread the plugin was created on. It was implemented in the way that calls on the wrong thread simply fail with an error, rather than being caught by the compiler (`Plugin` still implements Send and Sync). This limitation is caused by wasmer requiring all wasm function imports to implement Send and Sync even on Web, thus making it impossible to check this at compile time.

## Installation

Add the following to your `Cargo.toml`:

```toml
[dependencies]
webxtism = "0.1.0"
```

TODO: right now, the extism kernel isn't built this way. This has to be fixed before this crate can be used as an external dependency!

## Usage

```rust
use webxtism::{Context, Plugin, PluginIdentifier, WasmInput};

// Create a new context
let context = Context::<String>::default();

// Load and instantiate a plugin
let plugin = Plugin::new(&context, "my_plugin", WasmInput::file("path/to/wasm_file.wasm"), &[]).await.unwrap();

// Interact with the plugin
// ...
```

## Contributing

Contributions are welcome! Please submit a pull request or open an issue for any bugs or feature requests.

## License

This project is licensed under the BSD 3-Clause "Revised" license. See the [LICENSE](LICENSE) file for details.
