use std::prelude::v1::*;

use base::format::debug;
use eth_types::{
    Block, BlockHeader, HexBytes, Receipt, Signer, TransactionInner, Withdrawal, SH160, SH256,
    SU256, SU64, U256,
};
use statedb::StateDB;
use std::sync::Arc;

use crate::{Context, Engine, ExecuteResult, PrecompileSet};

#[derive(Clone, Debug)]
pub struct Ethereum {
    signer: Signer,
}

impl Ethereum {
    pub fn new(chain_id: SU256) -> Self {
        let signer = Signer::new(chain_id);
        Self { signer }
    }
}

#[derive(Debug, Clone)]
pub struct ConsensusBlockInfo {
    pub gas_limit: SU64,
    pub timestamp: u64,
    pub random: SH256,
    pub extra: HexBytes,
    pub coinbase: SH160,
}

impl Engine for Ethereum {
    type BlockHeader = BlockHeader;
    type Transaction = TransactionInner;
    type Receipt = Receipt;
    type Withdrawal = Withdrawal;
    type Block = Block;
    type NewBlockContext = ConsensusBlockInfo;

    fn new_block_header(
        &self,
        prev_header: &Self::BlockHeader,
        ctx: ConsensusBlockInfo,
    ) -> Self::BlockHeader {
        let gas_limit =
            Self::calc_gas_limit(prev_header.gas_limit.as_u64(), ctx.gas_limit.as_u64()).into();
        let base_fee = Self::calc_base_fee(
            prev_header.gas_limit.as_u64(),
            prev_header.gas_used.as_u64(),
            prev_header.base_fee_per_gas.raw().clone(),
        );
        Self::BlockHeader {
            parent_hash: prev_header.hash(),
            number: prev_header.number + SU64::from(1),
            gas_limit,
            timestamp: ctx.timestamp.into(),
            miner: ctx.coinbase,
            mix_hash: ctx.random,
            extra_data: ctx.extra,
            base_fee_per_gas: base_fee,
            difficulty: 0u64.into(),
            ..Default::default()
        }
    }

    fn evm_config(&self) -> evm::Config {
        evm::Config::shanghai()
    }

    fn precompile(&self) -> PrecompileSet {
        PrecompileSet::berlin()
    }

    fn signer(&self) -> Signer {
        self.signer.clone()
    }

    fn tx_context<'a>(&self, ctx: &mut Context<'a, Self::Transaction, Self::BlockHeader>) {
        ctx.block_base_fee = ctx.header.base_fee_per_gas;
        ctx.miner = Some(ctx.header.miner);
    }

    fn build_receipt(
        &self,
        result: &ExecuteResult,
        tx_idx: usize,
        tx: &Self::Transaction,
        header: &Self::BlockHeader,
    ) -> Self::Receipt {
        let mut receipt = Receipt {
            status: (result.success as u64).into(),
            transaction_hash: tx.hash(),
            transaction_index: (tx_idx as u64).into(),
            r#type: Some(tx.ty().into()),
            gas_used: result.used_gas.into(),
            cumulative_gas_used: header.gas_used + SU64::from(result.used_gas),
            logs: result.logs.clone(),
            logs_bloom: HexBytes::new(),

            // not affect the rlp encoding
            contract_address: None,
            root: None,
            block_hash: None,
            block_number: None,
        };
        receipt.logs_bloom = eth_types::create_bloom([&receipt].into_iter()).to_hex();
        receipt
    }

    fn process_withdrawals<D: StateDB>(
        &mut self,
        statedb: &mut D,
        withdrawals: &[Self::Withdrawal],
    ) -> Result<(), statedb::Error> {
        for withdrawal in withdrawals {
            let amount = withdrawal.amount.as_u256() * eth_types::gwei();
            statedb.add_balance(&withdrawal.address, &amount.into())?;
        }
        Ok(())
    }

    fn finalize_block<D: StateDB>(
        &mut self,
        statedb: &mut D,
        header: Self::BlockHeader,
        txs: Vec<Arc<Self::Transaction>>,
        receipts: Vec<Self::Receipt>,
        withdrawals: Option<Vec<Self::Withdrawal>>,
    ) -> Result<Self::Block, String> {
        Ok(Block::new(header, txs, &receipts, withdrawals))
    }
}

impl Ethereum {
    pub fn calc_gas_limit(parent_gas_limit: u64, mut desired_limit: u64) -> u64 {
        const GAS_LIMIT_BOUND_DIVISOR: u64 = 1024;
        const MIN_GAS_LIMIT: u64 = 5000;
        let delta = parent_gas_limit / GAS_LIMIT_BOUND_DIVISOR - 1;
        let mut limit = parent_gas_limit;
        if desired_limit < MIN_GAS_LIMIT {
            desired_limit = MIN_GAS_LIMIT;
        }
        // If we're outside our allowed gas range, we try to hone towards them
        if limit < desired_limit {
            limit = parent_gas_limit + delta;
            if limit > desired_limit {
                limit = desired_limit;
            }
            return limit;
        }
        if limit > desired_limit {
            limit = parent_gas_limit - delta;
            if limit < desired_limit {
                limit = desired_limit;
            }
        }
        return limit;
    }

    pub fn calc_base_fee(gas_limit: u64, gas_used: u64, base_fee: U256) -> SU256 {
        const ELASTICITY_MULTIPLIER: u64 = 2;
        const BASE_FEE_CHANGE_DENOMINATOR: u64 = 8;
        let parent_gas_target = gas_limit / ELASTICITY_MULTIPLIER;
        if gas_used == parent_gas_target {
            return base_fee.into();
        }

        if gas_used > parent_gas_target {
            // If the parent block used more gas than its target, the baseFee should increase.
            // max(1, parentBaseFee * gasUsedDelta / parent_gas_target / BASE_FEE_CHANGE_DENOMINATOR)
            let mut num = U256::from(gas_used) - U256::from(parent_gas_target);
            num *= base_fee;
            num /= U256::from(parent_gas_target);
            num /= U256::from(BASE_FEE_CHANGE_DENOMINATOR);
            let base_fee_delta = num.max(1.into());

            return (base_fee_delta + base_fee).into();
        } else {
            // Otherwise if the parent block used less gas than its target, the baseFee should decrease.
            // max(0, parentBaseFee * gasUsedDelta / parent_gas_target / BASE_FEE_CHANGE_DENOMINATOR)
            let mut num = U256::from(parent_gas_target) - U256::from(gas_used);
            num *= base_fee;
            num /= U256::from(parent_gas_target);
            num /= U256::from(BASE_FEE_CHANGE_DENOMINATOR);
            let base_fee: U256 = base_fee - num;
            return base_fee.max(0.into()).into();
        }
    }
}
