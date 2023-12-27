use std::prelude::v1::*;

use eth_types::{BlockHeaderTrait, HexBytes, Log, TxTrait, H256, SH160, SH256, SU256};
use evm::backend::Apply;
use std::collections::BTreeMap;

use crate::PrecompileSet;

#[derive(Debug)]
pub enum ExecuteError {
    NotSupported,
    InsufficientFunds,
    InsufficientBaseFee {
        tx_hash: SH256,
        block_base_fee_gwei: String,
        base_fee_gwei: String,
        block_number: u64,
    },
    ExecutePaymentTxFail(String),
    NonceTooLow {
        expect: u64,
        got: u64,
    },
    NonceTooHigh {
        expect: u64,
        got: u64,
    },
    StateError(statedb::Error),
}

#[derive(Debug, Clone)]
pub struct TxContext<'a, T: TxTrait, B: BlockHeaderTrait> {
    pub chain_id: SU256,
    pub caller: SH160,
    pub cfg: &'a evm::Config,
    pub precompile: &'a PrecompileSet,
    pub tx: &'a T,
    pub header: &'a B,
    pub no_gas_fee: bool,
    pub extra_fee: Option<SU256>,
    pub gas_overcommit: bool,

    // will no send the tx fee if it's None
    pub miner: Option<SH160>,

    pub block_base_fee: SU256,
    pub difficulty: SU256,
}

#[derive(Debug, Default)]
pub struct ExecuteResult {
    pub success: bool,
    pub used_gas: u64, // Total used gas but include the refunded gas
    pub err: HexBytes, // Any error encountered during the execution(listed in core/vm/errors.go)
    pub logs: Vec<Log>,
    pub states: StateChangeLog,
}

type StateChangeLog = Vec<Apply<BTreeMap<H256, H256>>>;
