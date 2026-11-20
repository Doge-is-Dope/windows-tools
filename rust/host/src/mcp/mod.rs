//! MCP is an authenticated adapter around the toolset, not another executor.
//!
//! - `catalog`: the model-facing tools.
//! - `handler`: MCP `tools/list` and `tools/call`, including transport cancellation.
//! - `http`: routes, Origin/Host checks, the bearer challenge and connector control.
//! - `Adapter` (here): who may call.
mod catalog;
mod handler;
mod http;
pub use http::router;

use sha2::{Digest, Sha256};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use windows_tools_core::{Toolset, relay::Relay};

pub struct Adapter {
    pub tools: Arc<Toolset>,
    pub origin: String,
    /// Another server's tools beside the toolset's own, when installed.
    pub browser: Option<Arc<Relay>>,
    /// SHA-256 of the one token that may call.
    token: [u8; 32],
}

impl Adapter {
    pub fn new(
        tools: Arc<Toolset>,
        origin: String,
        browser: Option<Relay>,
        token: &str,
    ) -> Arc<Self> {
        Arc::new(Self {
            tools,
            origin,
            browser: browser.map(Arc::new),
            token: Sha256::digest(token.as_bytes()).into(),
        })
    }

    pub fn authenticated(&self, token: &str) -> bool {
        let supplied: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        bool::from(supplied.ct_eq(&self.token))
    }
}

#[cfg(test)]
mod tests;
