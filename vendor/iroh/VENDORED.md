# Vendored iroh 1.4.1

Unmodified copy of [iroh 1.4.1](https://crates.io/crates/iroh/1.4.1) (MIT OR Apache-2.0)
with one change in `Cargo.toml`:

```diff
-crate-type = ["lib", "cdylib"]
+crate-type = ["lib"]
```

Cargo builds every crate type of a dependency, and on the `x86_64-pc-windows-gnu`
toolchain the extra `cdylib` fails to link (`export ordinal too large`, the 65535-export
limit of Windows DLLs). Proofkin only needs the Rust library. Remove this folder and the
`[patch.crates-io]` entry in the top-level `Cargo.toml` when building only with MSVC,
Linux or macOS toolchains, or once upstream drops the `cdylib` crate type.
