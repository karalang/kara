# Deferred examples

These programs need something scheduled after 0.8, so they do not check today,
and `tests/example_corpus.rs` and the conformance sweeps leave this directory
out. Each moves back to `examples/` when what it waits for lands.

| Program | Waits for |
|---|---|
| `autograd_training.kara`, `semantic_search.kara` | `Tensor`, with the `kara-data` package ([deferred.md § M4c data](../../docs/deferred.md#m4c-data)) |
| `interop/` | the exported C ABI, `--crate-type staticlib` / `cdylib` ([deferred.md § Exported C ABI](../../docs/deferred.md#exported-c-abi)) |
