# pigeonpost-groupcrypto web bundle (vendored, built from this repository)

Group end-to-end encryption for the inbox (OpenMLS / RFC 9420), compiled from
`crates/pigeonpost-groupcrypto/wasm` with wasm-bindgen 0.2.128 (`--target web`). Protocol and
threat model: `docs/group-e2ee.md`. Licence: MIT (this repository); bundled Rust dependencies are
MIT / Apache-2.0 / BSD / MPL-2.0 (see the doc, §1).

| File | Role |
|---|---|
| `loader.js` | Hand-written. Fetches the wasm same-origin, checks its SHA-256 against `integrity.js`, then initialises. Use `await (await import('./vendor/groupcrypto/loader.js')).loadGroupCrypto()`. |
| `pigeonpost_groupcrypto.js` / `.d.ts` | Generated wasm-bindgen glue (no `eval`, no `new Function`). |
| `pigeonpost_groupcrypto_bg.wasm` | The module (1.73 MB, 0.54 MB gzip). |
| `integrity.js` | Generated: pinned SHA-256 of the wasm. |
| `SHA256SUMS` | Hashes of the shipped files. |
| `package.json` | Marks the directory as ES modules (for Node tests only). |

CSP: works under the inbox policy `script-src 'self' 'wasm-unsafe-eval'` — same-origin modules,
WebAssembly compilation only. Serve `.wasm` as `application/wasm`.

wasm SHA-256: `23d910dd09aca1ffdc5cf62b8219a166a51d418d6aa433c0461b7044daf6aa53`

## Reproduce

The build is byte-reproducible (pinned Rust 1.91.1 via `rust-toolchain.toml`, `Cargo.lock`,
`--locked`, host paths remapped). CI rebuilds it on Linux and compares every generated file.

```bash
cargo install wasm-bindgen-cli --version 0.2.128 --locked
crates/pigeonpost-groupcrypto/scripts/build-wasm.sh          # rewrite this directory
crates/pigeonpost-groupcrypto/scripts/build-wasm.sh --check  # compare with a clean rebuild
(cd site-inbox/vendor/groupcrypto && shasum -a 256 -c SHA256SUMS)
```

Never edit the generated files by hand; change the crate and rebuild.
