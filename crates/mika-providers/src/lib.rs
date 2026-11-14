//! Provider adapters: LLM, embeddings, pictures, MCP servers and Telegram.

pub mod anthropic;
pub mod catalog;
mod common;
pub mod factory;
pub mod live;
pub mod mcp;
pub mod metering;
pub mod openai;
pub mod speech;
pub mod sse;
pub mod telegram;
mod text;
mod trace;

pub use factory::{build_or_placeholder, build_provider, probe, redact};
