//! Provider adapters: LLM over HTTP.

pub mod anthropic;
mod common;
pub mod factory;
pub mod openai;
pub mod sse;

pub use factory::{build_or_placeholder, build_provider, probe, redact};
