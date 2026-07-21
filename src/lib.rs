//! Tetra: a modular host agent and recipe renderer for the Ultramarine Server
//! web control plane.
//!
//! The crate is organized into two top-level modules:
//!
//! - [`agent`]: the dispatcher, its feature-gated modules, and the transports
//!   (HTTP, vsock, WSS) that expose the same command-envelope protocol to the
//!   dashboard or control plane.
//! - [`catalog`]: the recipe engine that renders YAML recipes into Quadlet and
//!   companion files using Tera templates.
//!
//! The binary in `src/main.rs` wires these into CLI subcommands.

pub mod agent;
pub mod catalog;
