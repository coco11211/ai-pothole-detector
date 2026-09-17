//! The `chainname_*` namespace: everything DAG-shaped.
//!
//! None of this belongs in `eth_*`. A client that knows about blockDAGs asks
//! here; one that does not is never shown a field it cannot interpret.

use alloy_primitives::B256;
use jsonrpsee::{core::RpcResult, proc_macros::rpc};
use serde::{Deserialize, Serialize};

use crate::backend::Backend;

/// GHOSTDAG data for one block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DagBlockInfo {
    /// The block's hash.
    pub hash: B256,
    /// Its selected parent, chosen by blue work.
    pub selected_parent: B256,
    /// All declared parents.
    pub parents: Vec<B256>,
    /// Number of blue blocks in its past, inclusive.
    pub blue_score: u64,
    /// Accumulated proof of work over the blue set, as a decimal string.
    ///
    /// A string because blue work is a 256-bit quantity and JSON numbers are
    /// doubles; silently losing precision on the fork-choice metric would be
    /// the worst possible place to do it.
    pub blue_work: String,
    /// Merge-set blocks coloured blue.
    pub mergeset_blues: Vec<B256>,
    /// Merge-set blocks coloured red. Their transactions still execute.
    pub mergeset_reds: Vec<B256>,
    /// True if this block is on the selected parent chain.
    pub is_chain_block: bool,
}

/// Where the DAG currently stands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DagTipInfo {
    /// Every current tip, highest blue work first.
    pub tips: Vec<B256>,
    /// The tip GHOSTDAG selected.
    pub virtual_selected_parent: B256,
    /// Blue score of that tip.
    pub blue_score: u64,
    /// Blocks in the DAG.
    pub block_count: usize,
    /// Selected-chain height executed so far.
    pub executed_height: u64,
}

/// Settlement confidence for a transaction.
///
/// There is no finality on this chain (OPEN-PROBLEMS.md P-007), so this reports
/// depth and lets the caller choose a threshold rather than pretending to
/// answer "is it final".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Confidence {
    /// Whether the transaction has executed at all.
    pub executed: bool,
    /// Selected-chain height it executed at, if it has.
    pub height: Option<u64>,
    /// Blue score of the block that executed it.
    pub blue_score: Option<u64>,
    /// How many blue blocks have accumulated since. This is the number to
    /// threshold on.
    pub blue_score_depth: Option<u64>,
}

/// The DAG namespace.
#[rpc(server, namespace = "chainname")]
pub trait DagApi {
    /// The current tip set and selected tip.
    #[method(name = "getDagTips")]
    fn get_dag_tips(&self) -> RpcResult<DagTipInfo>;

    /// GHOSTDAG data for a block.
    #[method(name = "getBlockDagInfo")]
    fn get_block_dag_info(&self, hash: B256) -> RpcResult<Option<DagBlockInfo>>;

    /// The merge set of a chain block, in execution order.
    #[method(name = "getMergeSet")]
    fn get_merge_set(&self, hash: B256) -> RpcResult<Option<Vec<B256>>>;

    /// Settlement confidence for a transaction.
    #[method(name = "getConfidence")]
    fn get_confidence(&self, tx_hash: B256) -> RpcResult<Confidence>;

    /// The deferred state root a header at `height` would publish.
    #[method(name = "getDeferredStateRoot")]
    fn get_deferred_state_root(&self, height: u64) -> RpcResult<Option<(u64, B256)>>;
}

/// Implements [`DagApiServer`] over a [`Backend`].
#[derive(Debug, Clone)]
pub struct DagApiImpl {
    backend: Backend,
}

impl DagApiImpl {
    /// Wraps a backend.
    pub const fn new(backend: Backend) -> Self {
        Self { backend }
    }
}

impl DagApiServer for DagApiImpl {
    fn get_dag_tips(&self) -> RpcResult<DagTipInfo> {
        Ok(self.backend.read(|state| {
            let selected = state.dag.virtual_selected_parent();
            DagTipInfo {
                tips: state.dag.tips(),
                virtual_selected_parent: selected,
                blue_score: state.dag.data(selected).map_or(0, |d| d.blue_score),
                block_count: state.dag.len(),
                executed_height: state.executor.height(),
            }
        }))
    }

    fn get_block_dag_info(&self, hash: B256) -> RpcResult<Option<DagBlockInfo>> {
        Ok(self.backend.read(|state| {
            let data = state.dag.data(hash)?;
            let header = state.dag.header(hash)?;
            let chain = state.dag.selected_parent_chain(state.dag.virtual_selected_parent());
            Some(DagBlockInfo {
                hash,
                selected_parent: data.selected_parent,
                parents: header.parents.clone(),
                blue_score: data.blue_score,
                blue_work: data.blue_work.to_string(),
                mergeset_blues: data.mergeset_blues.clone(),
                mergeset_reds: data.mergeset_reds.clone(),
                is_chain_block: chain.contains(&hash),
            })
        }))
    }

    fn get_merge_set(&self, hash: B256) -> RpcResult<Option<Vec<B256>>> {
        Ok(self.backend.read(|state| state.dag.data(hash).map(|d| d.mergeset_ordered.clone())))
    }

    fn get_confidence(&self, tx_hash: B256) -> RpcResult<Confidence> {
        Ok(self.backend.read(|state| {
            let Some(location) = state.tx_index.get(&tx_hash) else {
                return Confidence {
                    executed: false,
                    height: None,
                    blue_score: None,
                    blue_score_depth: None,
                };
            };
            let block_score = state.dag.data(location.chain_block).map_or(0, |d| d.blue_score);
            let tip = state.dag.virtual_selected_parent();
            let tip_score = state.dag.data(tip).map_or(0, |d| d.blue_score);
            Confidence {
                executed: true,
                height: Some(location.height),
                blue_score: Some(block_score),
                blue_score_depth: Some(tip_score.saturating_sub(block_score)),
            }
        }))
    }

    fn get_deferred_state_root(&self, height: u64) -> RpcResult<Option<(u64, B256)>> {
        Ok(self.backend.read(|state| {
            state.executor.deferred_result_for(height).map(|r| (r.height, r.state_root))
        }))
    }
}
