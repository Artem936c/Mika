//! Provider adapters: LLM, embeddings, pictures, MCP servers and Telegram.

pub mod anthropic;
mod common;
pub mod factory;
pub mod live;
pub mod mcp;
pub mod openai;
pub mod speech;
pub mod sse;
pub mod telegram;

pub use factory::{build_or_placeholder, build_provider, probe, redact};
