use std::{prelude::v1::*, time::Instant};

use base::format::debug;
use eth_types::{
    BlockHeaderTrait, FetchState, FetchStateResult, ReceiptTrait, Signer, TransactionAccessTuple,
    TxTrait, SH256, SU64,
};
use statedb::StateDB;
use std::borrow::Cow;
use std::sync::Arc;

use crate::{Context, ExecuteError, ExecuteResult, PrecompileSet, TxExecutor};

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
        result: &ExecuteResult,
        tx_idx: usize,
        tx: &Self::Transaction,
        header: &Self::BlockHeader,
    ) -> Self::Receipt;
    fn tx_context<'a>(&self, ctx: &mut Context<'a, Self::Transaction, Self::BlockHeader>);
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

pub struct BlockBuilder<E: Engine, D: StateDB, P> {
    engine: E,
    header: E::BlockHeader,
    statedb: D,
    signer: Signer,

    evm_cfg: evm::Config,
    precompile: PrecompileSet,

    gas_pool: u64,
    prefetcher: P,

    txs: Vec<Arc<E::Transaction>>,
    receipts: Vec<E::Receipt>,
    withdrawals: Option<Vec<E::Withdrawal>>,
}

impl<E, D, P> BlockBuilder<E, D, P>
where
    E: Engine,
    D: StateDB,
{
    pub fn new(
        engine: E,
        statedb: D,
        prefetcher: P,
        header: E::BlockHeader,
    ) -> BlockBuilder<E, D, P> {
        let gas_pool = header.gas_limit().as_u64();
        BlockBuilder {
            signer: engine.signer(),
            evm_cfg: engine.evm_config(),
            statedb,
            precompile: engine.precompile(),
            engine,
            header,
            gas_pool,
            prefetcher,

            txs: Vec::new(),
            receipts: Vec::new(),
            withdrawals: None,
        }
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
                let receipt =
                    self.engine
                        .build_receipt(&execute_result, self.txs.len(), &tx, &self.header);
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
        self.gas_pool += gas;
        self.header
            .set_gas_used(self.header.gas_limit() - SU64::from(self.gas_pool));
    }

    fn cost_gas(&mut self, gas: u64) {
        self.gas_pool -= gas;
        self.header
            .set_gas_used(self.header.gas_limit() - SU64::from(self.gas_pool));
    }

    pub fn finalize(mut self) -> Result<E::Block, String> {
        let state_root = self.flush_state().map_err(debug)?;
        self.header.set_state_root(state_root);
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
        let gas_limit = tx.gas_limit();
        if self.gas_pool < gas_limit {
            return Err(CommitError::NotEnoughGasLimit {
                gas_pool: self.gas_pool,
                gas_limit,
            });
        }

        let caller = tx.sender(&self.signer);
        let mut ctx = Context {
            chain_id: self.signer.chain_id,
            caller,
            cfg: &self.evm_cfg,
            precompile: &self.precompile,
            tx,
            header: &self.header,
            no_gas_fee: false,
            extra_fee: None,
            gas_overcommit: false,
            miner: None,
            block_base_fee: 0.into(),
            difficulty: 0.into(),
        };
        self.engine.tx_context(&mut ctx);
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
    P: StatePrefetcher,
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
