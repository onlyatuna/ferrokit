# ferrokit-model

Standalone concurrency and formal-verification models for `ferrokit` (`loom`, `shuttle`, and `kani` proofs).

## Why this crate is outside the root workspace

`ferrokit-model` declares its own `[workspace]` in `Cargo.toml` and keeps its own `Cargo.lock` intentionally:

- Isolates model-checking dependencies (`loom`, `shuttle`) and their transitive dependency trees from the root `Cargo.lock` and workspace builds.
- Allows running verification suites without unifying features or lints (`[lints]`) with the runtime crates.

## Running checks

Run Cargo commands from `crates/ferrokit-model` (or pass `--manifest-path crates/ferrokit-model/Cargo.toml`):

```sh
cargo test --locked
cargo test --locked --features loom --test loom_tests
cargo test --locked --features shuttle --test shuttle_tests
```
