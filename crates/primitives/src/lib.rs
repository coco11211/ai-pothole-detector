//! CHAINNAME primitives: block header, block body, and chain parameters.
//!
//! This crate is the bottom of the dependency graph. It knows nothing about
//! the DAG, about execution, or about storage.

#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod header;
pub mod params;

pub use header::{Block, BlockHash, HEADER_VERSION, Header, HeaderError};
pub use params::{ChainParams, ParamsError};
