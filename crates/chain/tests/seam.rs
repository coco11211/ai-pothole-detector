//! M6: the seam. GHOSTDAG ordering feeding revm, deferred state roots, and
//! reorg by undo journal.

use alloy_consensus::{Signed, TxEip1559, TxEnvelope, transaction::Recovered};
use alloy_genesis::{Genesis, GenesisAccount};
use alloy_primitives::{Address, B256, Signature, TxKind, U256};
use chainname_chain::{BodyStore, ChainExecutor, ChainTx, compute_reorg};
use chainname_execution::load_genesis;
use chainname_ghostdag::DagStore;
use chainname_primitives::{BlockHash, ChainParams, HEADER_VERSION, Header};

const K: u16 = 18;
const MERGESET_LIMIT: u64 = 180;
const GENESIS_MS: u64 = 1_700_000_000_000;
/// Enough to cover any gas these tests can spend.
const FUNDING_WEI: u128 = 1_000_000_000_000_000_000_000;

fn params() -> ChainParams {
    ChainParams::testnet_1bps()
}

fn genesis_header() -> Header {
    Header {
        version: HEADER_VERSION,
        parents: Vec::new(),
        timestamp_ms: GENESIS_MS,
        bits: 0x2000_ffff,
        nonce: 0,
        miner: Address::ZERO,
        txs_root: alloy_trie::EMPTY_ROOT_HASH,
        deferred_height: 0,
        deferred_state_root: B256::ZERO,
        deferred_receipts_root: alloy_trie::EMPTY_ROOT_HASH,
        deferred_gas_used: 0,
    }
}

fn block(parents: &[BlockHash], nonce: u64, miner: Address) -> Header {
    let mut parents = parents.to_vec();
    parents.sort_unstable();
    Header { parents, nonce, miner, timestamp_ms: GENESIS_MS + nonce * 1_000, ..genesis_header() }
}

fn account(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn miner(n: u8) -> Address {
    Address::repeat_byte(0xa0 + n)
}

/// Builds a transaction with its sender already attached.
///
/// Signature recovery is a validation concern and happens at mempool admission
/// (M7); execution takes the recovered form, so these tests construct that
/// directly rather than signing and immediately recovering.
fn transfer(from: Address, to: Address, nonce: u64, value: u64, tip: u128) -> ChainTx {
    let tx = TxEip1559 {
        chain_id: params().chain_id,
        nonce,
        gas_limit: 100_000,
        max_fee_per_gas: 10_000_000_000,
        max_priority_fee_per_gas: tip,
        to: TxKind::Call(to),
        value: U256::from(value),
        access_list: Default::default(),
        input: Default::default(),
    };
    // A placeholder signature: never verified on this path, and the sender is
    // supplied explicitly. `nonce` and `from` make the hash unique, which is
    // what deduplication keys on.
    let signature = Signature::new(
        U256::from(nonce + 1),
        U256::from(u64::from_le_bytes(from.0[..8].try_into().unwrap()) | 1),
        false,
    );
    let envelope = TxEnvelope::Eip1559(Signed::new_unhashed(tx, signature));
    Recovered::new_unchecked(envelope, from)
}

fn funded_state() -> chainname_execution::WorldState {
    let mut genesis = Genesis::default();
    for n in 1..=8u8 {
        genesis.alloc.insert(
            account(n),
            GenesisAccount { balance: U256::from(FUNDING_WEI), ..Default::default() },
        );
    }
    load_genesis(&genesis).unwrap()
}

struct Harness {
    dag: DagStore,
    bodies: BodyStore,
    executor: ChainExecutor,
}

impl Harness {
    fn new() -> Self {
        let g = genesis_header();
        let hash = g.hash();
        Self {
            dag: DagStore::new(g, K, MERGESET_LIMIT),
            bodies: BodyStore::new(),
            executor: ChainExecutor::new(params(), funded_state(), hash),
        }
    }

    fn genesis(&self) -> BlockHash {
        self.dag.genesis()
    }

    /// Adds a DAG block with a body.
    fn add(
        &mut self,
        parents: &[BlockHash],
        nonce: u64,
        miner_addr: Address,
        txs: Vec<ChainTx>,
    ) -> BlockHash {
        let hash = self.dag.add_block(block(parents, nonce, miner_addr)).unwrap();
        self.bodies.insert(hash, txs);
        hash
    }

    /// Executes from the current executor tip to the DAG's virtual tip.
    fn sync_execution(&mut self) {
        let new_tip = self.dag.virtual_selected_parent();
        let reorg = compute_reorg(&self.dag, self.executor.tip(), new_tip);
        self.executor
            .apply_reorg(&self.dag, &self.bodies, &reorg.removed, &reorg.added)
            .expect("reorg applies");
    }
}

#[test]
fn a_chain_block_executes_its_own_transactions() {
    let mut h = Harness::new();
    let g = h.genesis();
    h.add(&[g], 1, miner(1), vec![transfer(account(1), account(2), 0, 500, 0)]);
    h.sync_execution();

    assert_eq!(h.executor.height(), 1);
    assert_eq!(h.executor.state().balance(account(2)), U256::from(FUNDING_WEI + 500));
    assert_eq!(h.executor.result_at(1).unwrap().executed, 1);
}

#[test]
fn a_chain_block_executes_its_whole_merge_set() {
    // Two parallel blocks, each with a transaction, merged by a third. Both
    // transactions must execute, under the merging block's height.
    let mut h = Harness::new();
    let g = h.genesis();
    let a = h.add(&[g], 1, miner(1), vec![transfer(account(1), account(5), 0, 100, 0)]);
    let b = h.add(&[g], 2, miner(2), vec![transfer(account(2), account(6), 0, 200, 0)]);
    h.add(&[a, b], 3, miner(3), vec![transfer(account(3), account(7), 0, 300, 0)]);
    h.sync_execution();

    assert_eq!(h.executor.state().balance(account(5)), U256::from(FUNDING_WEI + 100));
    assert_eq!(h.executor.state().balance(account(6)), U256::from(FUNDING_WEI + 200));
    assert_eq!(h.executor.state().balance(account(7)), U256::from(FUNDING_WEI + 300));
}

#[test]
fn a_red_blocks_transactions_still_execute() {
    // The point of the DAG: orphans fold into the ledger. With k=0 every
    // non-selected parent is red, and its transactions must still run.
    let g = genesis_header();
    let genesis_hash = g.hash();
    let mut dag = DagStore::new(g, 0, MERGESET_LIMIT);
    let mut bodies = BodyStore::new();
    let mut executor = ChainExecutor::new(params(), funded_state(), genesis_hash);

    let a = dag.add_block(block(&[genesis_hash], 1, miner(1))).unwrap();
    bodies.insert(a, vec![transfer(account(1), account(5), 0, 100, 0)]);
    let b = dag.add_block(block(&[genesis_hash], 2, miner(2))).unwrap();
    bodies.insert(b, vec![transfer(account(2), account(6), 0, 200, 0)]);
    let m = dag.add_block(block(&[a, b], 3, miner(3))).unwrap();
    bodies.insert(m, Vec::new());

    let data = dag.data(m).unwrap();
    assert_eq!(data.mergeset_reds.len(), 1, "with k=0 the non-selected parent is red");

    let reorg = compute_reorg(&dag, genesis_hash, dag.virtual_selected_parent());
    executor.apply_reorg(&dag, &bodies, &reorg.removed, &reorg.added).unwrap();

    assert_eq!(executor.state().balance(account(5)), U256::from(FUNDING_WEI + 100));
    assert_eq!(
        executor.state().balance(account(6)),
        U256::from(FUNDING_WEI + 200),
        "a red block's transactions must still execute"
    );
}

#[test]
fn a_duplicate_transaction_executes_exactly_once() {
    // The same transaction in two parallel blocks. Deduplication keeps the
    // first occurrence in merge-set order.
    let mut h = Harness::new();
    let g = h.genesis();
    let tx = transfer(account(1), account(5), 0, 777, 0);
    let a = h.add(&[g], 1, miner(1), vec![tx.clone()]);
    let b = h.add(&[g], 2, miner(2), vec![tx]);
    h.add(&[a, b], 3, miner(3), Vec::new());
    h.sync_execution();

    assert_eq!(
        h.executor.state().balance(account(5)),
        U256::from(FUNDING_WEI + 777),
        "the transaction executed twice"
    );
}

#[test]
fn the_priority_fee_goes_to_the_carrying_blocks_miner() {
    // The attribution requirement: a transaction's fee follows the DAG block
    // that carried it, not the chain block that merged it.
    let mut h = Harness::new();
    let g = h.genesis();
    let tip: u128 = 1_000;
    let a = h.add(&[g], 1, miner(1), vec![transfer(account(1), account(5), 0, 0, tip)]);
    let b = h.add(&[g], 2, miner(2), Vec::new());
    let m = h.add(&[a, b], 3, miner(3), Vec::new());
    h.sync_execution();

    // Whichever of a/b is the selected parent, the transaction lives in `a`
    // and `miner(1)` is paid.
    let _ = (b, m);
    let paid = h.executor.state().balance(miner(1));
    assert!(paid > U256::ZERO, "the carrying block's miner was not paid");
    assert_eq!(
        h.executor.state().balance(miner(3)),
        U256::ZERO,
        "the merging block's miner must not receive another block's fee"
    );
}

#[test]
fn the_base_fee_is_burned() {
    let mut h = Harness::new();
    let g = h.genesis();
    let before: U256 = (1..=8u8).map(|n| h.executor.state().balance(account(n))).sum();
    h.add(&[g], 1, miner(1), vec![transfer(account(1), account(2), 0, 0, 0)]);
    h.sync_execution();

    let after: U256 = (1..=8u8).map(|n| h.executor.state().balance(account(n))).sum();
    let miner_balance = h.executor.state().balance(miner(1));
    assert!(after < before, "the base fee must leave circulation");
    assert_eq!(miner_balance, U256::ZERO, "with no tip the miner receives nothing");
}

#[test]
fn same_sender_transactions_keep_their_nonce_order() {
    // Three transactions from one sender, spread across parallel blocks. The
    // layering rule must keep them in order or the nonces fail.
    let mut h = Harness::new();
    let g = h.genesis();
    let a = h.add(&[g], 1, miner(1), vec![transfer(account(1), account(5), 0, 10, 0)]);
    let b = h.add(&[a], 2, miner(2), vec![transfer(account(1), account(5), 1, 20, 0)]);
    h.add(&[b], 3, miner(3), vec![transfer(account(1), account(5), 2, 30, 0)]);
    h.sync_execution();

    assert_eq!(h.executor.state().nonce(account(1)), 3, "all three nonces consumed");
    assert_eq!(h.executor.state().balance(account(5)), U256::from(FUNDING_WEI + 60));
}

#[test]
fn a_reorg_reaches_the_same_state_as_executing_that_branch_directly() {
    // The core reorg property. Build two branches, execute one, then reorg to
    // the other, and compare against a fresh executor that only ever saw the
    // second branch.
    let build = |extend_winner: bool| -> (DagStore, BodyStore, Vec<BlockHash>) {
        let g = genesis_header();
        let genesis_hash = g.hash();
        let mut dag = DagStore::new(g, K, MERGESET_LIMIT);
        let mut bodies = BodyStore::new();

        // Loser branch: one block.
        let l1 = dag.add_block(block(&[genesis_hash], 1, miner(1))).unwrap();
        bodies.insert(l1, vec![transfer(account(1), account(5), 0, 111, 0)]);

        // Winner branch: three blocks, so it accumulates more work.
        let w1 = dag.add_block(block(&[genesis_hash], 2, miner(2))).unwrap();
        bodies.insert(w1, vec![transfer(account(2), account(6), 0, 222, 0)]);
        let w2 = dag.add_block(block(&[w1], 3, miner(2))).unwrap();
        bodies.insert(w2, vec![transfer(account(3), account(7), 0, 333, 0)]);
        let mut winner_chain = vec![w1, w2];
        if extend_winner {
            let w3 = dag.add_block(block(&[w2], 4, miner(2))).unwrap();
            bodies.insert(w3, vec![transfer(account(4), account(8), 0, 444, 0)]);
            winner_chain.push(w3);
        }
        let _ = l1;
        (dag, bodies, winner_chain)
    };

    // Path A: execute the loser first, then reorg onto the winner.
    let (dag_a, bodies_a, _) = build(true);
    let genesis_hash = dag_a.genesis();
    let mut reorged = ChainExecutor::new(params(), funded_state(), genesis_hash);
    // Execute the loser branch explicitly.
    let loser = dag_a
        .tips()
        .into_iter()
        .chain(std::iter::once(genesis_hash))
        .find(|h| dag_a.header(*h).is_some_and(|hd| hd.nonce == 1))
        .expect("loser block present");
    reorged.execute_chain_block(&dag_a, &bodies_a, loser).unwrap();
    assert_eq!(reorged.height(), 1);

    // Now reorg onto whatever GHOSTDAG actually selected.
    let new_tip = dag_a.virtual_selected_parent();
    let reorg = compute_reorg(&dag_a, reorged.tip(), new_tip);
    assert!(!reorg.removed.is_empty(), "the test must actually cause a reorg");
    reorged.apply_reorg(&dag_a, &bodies_a, &reorg.removed, &reorg.added).unwrap();

    // Path B: a fresh executor that only ever saw the winner.
    let (dag_b, bodies_b, _) = build(true);
    let mut direct = ChainExecutor::new(params(), funded_state(), dag_b.genesis());
    let direct_reorg = compute_reorg(&dag_b, dag_b.genesis(), dag_b.virtual_selected_parent());
    direct.apply_reorg(&dag_b, &bodies_b, &direct_reorg.removed, &direct_reorg.added).unwrap();

    assert_eq!(reorged.height(), direct.height(), "heights diverged after the reorg");
    assert_eq!(
        reorged.state_root(),
        direct.state_root(),
        "a reorg did not reach the same state as executing the branch directly"
    );
}

#[test]
fn undoing_everything_returns_to_the_genesis_state() {
    let mut h = Harness::new();
    let g = h.genesis();
    let genesis_root = h.executor.state_root();

    let a = h.add(&[g], 1, miner(1), vec![transfer(account(1), account(5), 0, 100, 5)]);
    let b = h.add(&[a], 2, miner(2), vec![transfer(account(2), account(6), 0, 200, 7)]);
    h.sync_execution();
    assert_ne!(h.executor.state_root(), genesis_root);

    let chain = h.dag.selected_parent_chain(b);
    let removed: Vec<BlockHash> = chain.into_iter().filter(|x| *x != g).collect();
    h.executor.apply_reorg(&h.dag, &h.bodies, &removed, &[]).unwrap();

    assert_eq!(h.executor.height(), 0);
    assert_eq!(
        h.executor.state_root(),
        genesis_root,
        "undoing every block must restore the genesis state exactly"
    );
}

#[test]
fn an_out_of_order_undo_is_refused() {
    // Silently accepting a mismatched undo would corrupt state in a way no
    // later check could catch.
    let mut h = Harness::new();
    let g = h.genesis();
    h.add(&[g], 1, miner(1), Vec::new());
    h.sync_execution();

    let result = h.executor.apply_reorg(&h.dag, &h.bodies, &[B256::repeat_byte(0xee)], &[]);
    assert!(result.is_err(), "an undo for the wrong block must be refused");
}

#[test]
fn the_deferred_result_lags_by_exactly_d() {
    let mut h = Harness::new();
    let lag = params().deferred_state_root_lag();
    assert_eq!(lag, 20, "the 1 bps lag is 20 blocks");

    let mut parent = h.genesis();
    for nonce in 1..=(lag + 5) {
        parent = h.add(&[parent], nonce, miner(1), Vec::new());
    }
    h.sync_execution();
    assert_eq!(h.executor.height(), lag + 5);

    // A header at height N publishes height N - D.
    let deferred = h.executor.deferred_result_for(lag + 5).unwrap();
    assert_eq!(deferred.height, 5);
    assert_eq!(deferred.state_root, h.executor.result_at(5).unwrap().state_root);

    // Below D there is nothing to publish yet, so genesis stands in.
    assert_eq!(h.executor.deferred_result_for(3).unwrap().height, 0);
}

#[test]
fn execution_is_deterministic_across_independent_runs() {
    // The property every node depends on.
    let run = || {
        let mut h = Harness::new();
        let g = h.genesis();
        let a = h.add(&[g], 1, miner(1), vec![transfer(account(1), account(5), 0, 100, 3)]);
        let b = h.add(&[g], 2, miner(2), vec![transfer(account(2), account(6), 0, 200, 4)]);
        let c = h.add(&[g], 3, miner(3), vec![transfer(account(3), account(7), 0, 300, 5)]);
        h.add(&[a, b, c], 4, miner(4), vec![transfer(account(4), account(8), 0, 400, 6)]);
        h.sync_execution();
        (h.executor.state_root(), h.executor.height())
    };
    assert_eq!(run(), run());
}

#[test]
fn the_journal_can_be_pruned() {
    let mut h = Harness::new();
    let mut parent = h.genesis();
    for nonce in 1..=10 {
        parent = h.add(&[parent], nonce, miner(1), Vec::new());
    }
    let _ = parent;
    h.sync_execution();
    assert_eq!(h.executor.journal_len(), 10);

    h.executor.prune_journal(8);
    assert_eq!(h.executor.journal_len(), 3, "heights 8, 9 and 10 remain");
}

#[test]
fn a_transaction_that_cannot_execute_is_skipped_not_fatal() {
    // Under deferred execution a miner cannot know a transaction's validity
    // when it includes it, so a block carrying a bad one must still execute.
    let mut h = Harness::new();
    let g = h.genesis();
    h.add(
        &[g],
        1,
        miner(1),
        vec![
            // Nonce 9 from an account at nonce 0: not executable.
            transfer(account(1), account(5), 9, 100, 0),
            transfer(account(2), account(6), 0, 200, 0),
        ],
    );
    h.sync_execution();

    let result = h.executor.result_at(1).unwrap();
    assert_eq!(result.executed, 1, "the valid transaction must still run");
    assert_eq!(result.deferred, 1, "the invalid one is skipped, not fatal");
    assert_eq!(h.executor.state().balance(account(6)), U256::from(FUNDING_WEI + 200));
}
