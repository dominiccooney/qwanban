//! Integration tests composing the real crate implementations across boundaries.
//! All tests run in-process (no VM startup/teardown); the in-guest stub `serve()`
//! runs in this process and writes real files for debugging.
