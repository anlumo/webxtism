# WebXtism

WebXtism is a Rust library designed to provide a flexible and efficient runtime environment for WebAssembly (Wasm) plugins, particularly in a web context. It leverages the Wasmer runtime for executing Wasm modules and offers an extensible plugin system. The plugins are compatible with [extism](https://extism.org/) and this project is designed to use their PDK in any language they support.

This crate is highly experimental and not ready for any kind of production use!

## Features

- **Wasm Execution**: Run Wasm modules efficiently using Wasmer.
- **Plugin System**: Easily load and manage extism plugins from various sources such as files, data, or URLs.
- **Logging**: Integrated logging support for debugging and information tracking using the [tracing crate](https://github.com/tokio-rs/tracing).

Web support is still pending!

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
