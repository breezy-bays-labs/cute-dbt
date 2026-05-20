//! Trait seams used by the run loop where >1 real-or-test impl exists.
//!
//! v0.1 introduces exactly one port — the manifest source — landing with
//! PR 4b (#TBD): real-file impl in `adapters/manifest.rs`, in-memory test
//! impl in the test suite. The renderer is NOT a port (one output format
//! in v0.1).
