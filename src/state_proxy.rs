use std::prelude::v1::*;

use super::Context;
use core::cell::RefCell;
use crypto::keccak_hash;
use eth_types::{H160, H256, SH256, U256};
use statedb::StateDB;

pub struct StateProxy<'a, D: StateDB> {
    state_db: RefCell<&'a mut D>,
    ctx: Context<'a>,
}

impl<'a, D: StateDB> StateProxy<'a, D> {
    pub fn new(state: &'a mut D, ctx: Context<'a>) -> Self {
        Self {
            state_db: RefCell::new(state),
            ctx,
        }
    }
}

impl<'a, D: StateDB> evm::backend::Backend for StateProxy<'a, D> {
    fn block_base_fee_per_gas(&self) -> U256 {
        glog::debug!(target: "executor", "get base fee");
        self.ctx.block_base_fee.into()
    }

    fn basic(&self, address: H160) -> evm::backend::Basic {
        let (balance, nonce) = self
            .state_db
            .borrow_mut()
            .get_account_basic(&address.into())
            .unwrap();

        glog::debug!(target: "executor", "get basic: {:?} => {},{}", address, balance, nonce);
        evm::backend::Basic {
            balance: balance.into(),
            nonce: nonce.into(),
        }
    }

    fn block_coinbase(&self) -> H160 {
        let miner = match self.ctx.miner {
            Some(miner) => miner,
            None => self.ctx.header.miner,
        };
        glog::debug!(target: "executor", "get coinbase: {:?}", miner);
        miner.into()
    }

    fn block_difficulty(&self) -> U256 {
        glog::debug!(target: "executor", "get difficulty: {:?}", self.ctx.difficulty);
        self.ctx.difficulty.into()
    }

    fn block_gas_limit(&self) -> U256 {
        glog::debug!(target: "executor", "get gas_limit: {:?}", self.ctx.header.gas_limit);
        self.ctx.header.gas_limit.as_u64().into()
    }

    fn block_hash(&self, number: U256) -> H256 {
        glog::info!("get block hash: {:?}", number);
        let chain_id = self.chain_id().as_u64();
        let number = number.as_u64();
        let blk_num = self.ctx.header.number.as_u64();
        let val = if number >= blk_num || number < blk_num.saturating_sub(256) {
            Default::default()
        } else {
            let mut buf = chain_id.to_be_bytes().to_vec();
            buf.extend(number.to_be_bytes());
            keccak_hash(&buf).into()
        };

        glog::debug!(target: "executor", "get block hash: {:?} => {:?}", number, val);
        val
    }

    fn block_number(&self) -> U256 {
        glog::debug!(target: "executor", "get block number: {:?}", self.ctx.header.number);
        self.ctx.header.number.as_u64().into()
    }

    fn block_timestamp(&self) -> U256 {
        glog::debug!(target: "executor", "get timestamp: {}", self.ctx.header.timestamp);
        self.ctx.header.timestamp.as_u64().into()
    }

    fn chain_id(&self) -> U256 {
        glog::debug!(target: "executor", "get chain_id: {}", self.ctx.chain_id);
        self.ctx.chain_id.clone().into()
    }

    fn code(&self, address: H160) -> Vec<u8> {
        let code = self
            .state_db
            .borrow_mut()
            .get_code(&address.into())
            .unwrap();

        glog::debug!(target: "executor", "get code: {:?}, hash:{:?}, size: {}", address, SH256::from(keccak_hash(&code)), code.len());
        code.as_ref().clone().into()
    }

    fn exists(&self, address: H160) -> bool {
        let exists = self.state_db.borrow_mut().exist(&address.into()).unwrap();
        glog::debug!(target: "executor", "get exists: {:?} => {:?}", address, exists);
        exists
    }

    fn gas_price(&self) -> U256 {
        glog::debug!(target: "executor", "get gas price");
        self.ctx
            .tx
            .tx
            .gas_price(Some(self.ctx.header.base_fee_per_gas))
            .into()
    }

    fn origin(&self) -> H160 {
        glog::debug!(target: "executor", "get origin");
        self.ctx.caller.clone().into()
    }

    fn original_storage(&self, address: H160, index: H256) -> Option<H256> {
        let val = self
            .state_db
            .borrow_mut()
            .get_state(&address.into(), &index.into())
            .unwrap()
            .into();
        if val == H256::default() {
            return None;
        }

        glog::debug!(target: "executor", "get storage: {:?}.{:?} = {:?}", address, index, val);
        return Some(val);
    }

    fn storage(&self, address: H160, index: H256) -> H256 {
        let val = self
            .state_db
            .borrow_mut()
            .get_state(&address.into(), &index.into())
            .unwrap()
            .into();
        glog::debug!(target: "executor", "get storage: {:?}.{:?} = {:?}", address, index, val);
        val
    }
}
