use std::borrow::Cow;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

use extism_manifest::Manifest;

pub enum WasmInput<'a> {
    Data(Cow<'a, [u8]>),
    Manifest(Cow<'a, Manifest>),
}

impl WasmInput<'_> {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn file(path: impl AsRef<Path>) -> Self {
        use extism_manifest::{Wasm, WasmMetadata};

        Self::Manifest(Cow::Owned(Manifest {
            wasm: vec![Wasm::File {
                path: path.as_ref().to_path_buf(),
                meta: WasmMetadata::default(),
            }],
            ..Default::default()
        }))
    }
}

impl<'a> From<&'a Manifest> for WasmInput<'a> {
    fn from(manifest: &'a Manifest) -> Self {
        WasmInput::Manifest(Cow::Borrowed(manifest))
    }
}

impl From<Manifest> for WasmInput<'_> {
    fn from(manifest: Manifest) -> Self {
        WasmInput::Manifest(Cow::Owned(manifest))
    }
}

impl<'a> From<&'a [u8]> for WasmInput<'a> {
    fn from(data: &'a [u8]) -> Self {
        WasmInput::Data(Cow::Borrowed(data))
    }
}

impl<'a> From<&'a Vec<u8>> for WasmInput<'a> {
    fn from(data: &'a Vec<u8>) -> Self {
        WasmInput::Data(Cow::Borrowed(data.as_slice()))
    }
}

impl From<Vec<u8>> for WasmInput<'_> {
    fn from(data: Vec<u8>) -> Self {
        WasmInput::Data(Cow::Owned(data))
    }
}

impl<'a> From<&'a str> for WasmInput<'a> {
    fn from(data: &'a str) -> Self {
        WasmInput::Data(Cow::Borrowed(data.as_bytes()))
    }
}
