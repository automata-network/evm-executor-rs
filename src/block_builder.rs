use std::prelude::v1::*;

use base::format::debug;
use eth_types::{
    BlockHeaderTrait, FetchState, FetchStateResult, ReceiptTrait, Signer, TransactionAccessTuple,
    TxTrait, SH160, SH256,
};
use statedb::StateDB;
use std::borrow::Cow;
use std::sync::Arc;
use std::time::Instant;

use crate::{BlockHashGetter, ExecuteError, ExecuteResult, PrecompileSet, TxContext, TxExecutor};

pub trait Engine {
    type Transaction: TxTrait;
    type BlockHeader: BlockHeaderTrait;
    type Receipt: ReceiptTrait;
    type Withdrawal;
    type Block;
    type NewBlockContext;
    fn signer(&self) -> Signer;
    fn evm_config(&self) -> evm::Config;
    fn precompile(&self) -> PrecompileSet;
    fn new_block_header(
        &self,
        prev_header: &Self::BlockHeader,
        ctx: Self::NewBlockContext,
    ) -> Self::BlockHeader;
    fn build_receipt(
        &self,
        cumulative_gas_used: u64,
        result: &ExecuteResult,
        tx_idx: usize,
        tx: &Self::Transaction,
        header: &Self::BlockHeader,
    ) -> Self::Receipt;
    fn author(&self, header: &Self::BlockHeader) -> Result<Option<SH160>, String>;
    fn tx_context<'a, H: BlockHashGetter>(
        &self,
        ctx: &mut TxContext<'a, Self::Transaction, Self::BlockHeader, H>,
    );
    fn process_withdrawals<D: StateDB>(
        &mut self,
        statedb: &mut D,
        withdrawals: &[Self::Withdrawal],
    ) -> Result<(), statedb::Error>;
    fn finalize_block<D: StateDB>(
        &mut self,
        statedb: &mut D,
        header: Self::BlockHeader,
        txs: Vec<Arc<Self::Transaction>>,
        receipts: Vec<Self::Receipt>,
        withdrawals: Option<Vec<Self::Withdrawal>>,
    ) -> Result<Self::Block, String>;
}

pub struct BlockBuilder<E: Engine, D: StateDB, P: BlockHashGetter> {
    engine: E,
    header: E::BlockHeader,
    statedb: D,
    signer: Signer,
    miner: Option<SH160>,

    evm_cfg: evm::Config,
    precompile: PrecompileSet,

    cumulative_gas_used: u64,
    prefetcher: P,

    txs: Vec<Arc<E::Transaction>>,
    receipts: Vec<E::Receipt>,
    withdrawals: Option<Vec<E::Withdrawal>>,
}

impl<E, D, P> BlockBuilder<E, D, P>
where
    E: Engine,
    D: StateDB,
    P: BlockHashGetter,
{
    pub fn new(
        engine: E,
        statedb: D,
        prefetcher: P,
        header: E::BlockHeader,
    ) -> Result<BlockBuilder<E, D, P>, String> {
        let miner = engine.author(&header)?;
        Ok(BlockBuilder {
            signer: engine.signer(),
            evm_cfg: engine.evm_config(),
            miner,
            statedb,
            precompile: engine.precompile(),
            engine,
            header,
            cumulative_gas_used: 0,
            prefetcher,

            txs: Vec::new(),
            receipts: Vec::new(),
            withdrawals: None,
        })
    }

    pub fn txs(&self) -> &[Arc<E::Transaction>] {
        &self.txs
    }

    pub fn receipts(&self) -> &[E::Receipt] {
        &self.receipts
    }

    pub fn truncate_and_revert(&mut self, tx_len: usize, state_root: SH256) {
        let refund_gases: Vec<_> = self.receipts[tx_len..]
            .iter()
            .map(|receipt| receipt.gas_used().as_u64())
            .collect();
        for gas in refund_gases {
            self.refund_gas(gas);
        }
        self.txs.truncate(tx_len);
        self.receipts.truncate(tx_len);
        self.statedb.revert(state_root);
    }

    pub fn flush_state(&mut self) -> Result<SH256, statedb::Error> {
        self.statedb.flush()
    }

    pub fn commit(&mut self, tx: Arc<E::Transaction>) -> Result<&E::Receipt, CommitError> {
        let receipt = match self.execute_tx(&tx) {
            Ok(execute_result) => {
                let receipt = self.engine.build_receipt(
                    self.cumulative_gas_used,
                    &execute_result,
                    self.txs.len(),
                    &tx,
                    &self.header,
                );
                self.cost_gas(execute_result.used_gas);
                self.receipts.push(receipt);
                self.txs.push(tx.clone());
                self.receipts.last().unwrap()
            }
            Err(err) => return Err(err),
        };
        Ok(receipt)
    }

    fn refund_gas(&mut self, gas: u64) {
        self.cumulative_gas_used -= gas;
    }

    fn cost_gas(&mut self, gas: u64) {
        self.cumulative_gas_used += gas;
    }

    pub fn finalize_header(&mut self) -> Result<&E::BlockHeader, String> {
        let state_root = self.flush_state().map_err(debug)?;
        self.header.set_state_root(state_root);
        self.header.set_gas_used(self.cumulative_gas_used.into());
        Ok(&self.header)
    }

    pub fn finalize(mut self) -> Result<E::Block, String> {
        self.finalize_header()?;
        let blk = self.engine.finalize_block(
            &mut self.statedb,
            self.header,
            self.txs,
            self.receipts,
            self.withdrawals,
        )?;
        Ok(blk)
    }

    fn execute_tx(&mut self, tx: &E::Transaction) -> Result<ExecuteResult, CommitError> {
        let caller = tx.sender(&self.signer);
        let mut ctx = TxContext {
            chain_id: self.signer.chain_id,
            caller,
            cfg: &self.evm_cfg,
            precompile: &self.precompile,
            tx,
            header: &self.header,
            block_hash_getter: &self.prefetcher,
            no_gas_fee: false,
            extra_fee: None,
            gas_overcommit: false,
            miner: self.miner,
            block_base_fee: 0.into(),
            difficulty: 0.into(),
        };
        self.engine.tx_context(&mut ctx);

        let gas_limit = tx.gas_limit();
        if !ctx.no_gas_fee {
            let block_gas_limit = self.header.gas_limit();
            let gas_pool = block_gas_limit
                .as_u64()
                .saturating_sub(self.cumulative_gas_used);
            if gas_pool < gas_limit {
                return Err(CommitError::NotEnoughGasLimit {
                    gas_pool,
                    gas_limit,
                });
            }
        }

        let state_db = &mut self.statedb;
        let result = TxExecutor::new(ctx, state_db)
            .execute()
            .map_err(|err| CommitError::Execute(err))?;
        Ok(result)
    }

    pub fn withdrawal(&mut self, withdrawals: Vec<E::Withdrawal>) -> Result<(), statedb::Error> {
        self.engine
            .process_withdrawals(&mut self.statedb, &withdrawals)?;
        self.withdrawals = Some(withdrawals);
        Ok(())
    }
}

impl<E, D, P> BlockBuilder<E, D, P>
where
    E: Engine,
    P: BlockHashGetter + StatePrefetcher,
    D: StateDB,
{
    pub fn prefetch<'a, I>(&mut self, list: I) -> Result<usize, statedb::Error>
    where
        I: Iterator<Item = &'a TransactionAccessTuple>,
    {
        let mut out = Vec::new();
        let _start = Instant::now();
        for item in list {
            let mut fetch = FetchState {
                access_list: None,
                code: None,
            };
            let missing_state = self
                .statedb
                .check_missing_state(&item.address, &item.storage_keys)?;
            if missing_state.account {
                fetch.code = Some(item.address);
                fetch.access_list = Some(Cow::Borrowed(item));
            } else {
                if missing_state.code {
                    fetch.code = Some(item.address);
                }
                let mut item = Cow::Borrowed(item);
                item.to_mut().storage_keys = missing_state.storages;
                fetch.access_list = Some(item);
            }
            if fetch.get_addr().is_some() {
                match out.iter_mut().find(|item| fetch.is_match(item)) {
                    Some(item) => item.merge(fetch),
                    None => out.push(fetch),
                }
            }
        }
        if out.len() > 0 {
            let result = self.prefetcher.prefetch(&out)?;
            self.statedb.apply_states(result)?;
        }
        Ok(out.len())
    }
}

pub trait StatePrefetcher {
    fn prefetch(&self, req: &[FetchState]) -> Result<Vec<FetchStateResult>, statedb::Error>;
}

#[derive(Debug)]
pub enum CommitError {
    NotEnoughGasLimit { gas_pool: u64, gas_limit: u64 },
    Execute(ExecuteError),
}
