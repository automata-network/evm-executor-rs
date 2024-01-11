use std::prelude::v1::*;

use crypto::{Secp256k1PrivateKey, Secp256k1RecoverableSignature};
use eth_types::{HexBytes, SH160, SH256, SU256};
use serde::{Deserialize, Serialize};
use solidity::EncodeArg;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Poe {
    pub batch_hash: SH256,
    pub state_hash: SH256,
    pub prev_state_root: SH256,
    pub new_state_root: SH256,
    pub withdrawal_root: SH256,
    pub signature: HexBytes, // 65bytes
}

impl Poe {
    pub fn single_block(
        state_hash: SH256,
        prev_state_root: SH256,
        new_state_root: SH256,
        withdrawal_root: SH256,
    ) -> Self {
        Self {
            state_hash,
            prev_state_root,
            new_state_root,
            withdrawal_root,
            signature: vec![0_u8; 65].into(),
            batch_hash: SH256::default(),
        }
    }

    pub fn batch(batch_hash: SH256, block_poes: &[Self]) -> Result<Self, String> {
        if block_poes.len() < 1 {
            return Err("length of block poe is zero".into());
        }

        let mut prev_state_root = None;
        let mut new_state_root = None;
        let mut withdrawal_root = None;
        for (idx, poe) in block_poes.iter().enumerate() {
            if prev_state_root.is_none() {
                prev_state_root = Some(poe.prev_state_root);
            }

            if let Some(state_root) = &new_state_root {
                if state_root != &poe.prev_state_root {
                    return Err(format!(
                        "unexpected state_root in poe[{}]: want: {:?}, got: {:?}",
                        idx, state_root, poe.prev_state_root
                    ));
                }
            }
            new_state_root = Some(poe.new_state_root);
            withdrawal_root = Some(poe.withdrawal_root);
        }

        let state_hash = crypto::keccak_encode(|hash| {
            for poe in block_poes {
                hash(&poe.state_hash.0);
            }
        })
        .into();
        let batch_poe = Self {
            batch_hash,
            state_hash,
            prev_state_root: prev_state_root.expect("prev_state_root should not be none"),
            new_state_root: new_state_root.expect("new_state_root should not be none"),
            withdrawal_root: withdrawal_root.expect("withdrawal_root should not be none"),
            signature: vec![0_u8; 65].into(),
        };

        Ok(batch_poe)
    }

    pub fn sign(&mut self, chain_id: &SU256, prvkey: &Secp256k1PrivateKey) {
        let data = self.sign_msg(chain_id);
        let sig = prvkey.sign(&data);
        self.signature = sig.to_array().to_vec().into();
    }
}

impl Default for Poe {
    fn default() -> Self {
        Self {
            batch_hash: SH256::default(),
            state_hash: SH256::default(),
            prev_state_root: SH256::default(),
            new_state_root: SH256::default(),
            withdrawal_root: SH256::default(),
            signature: vec![0_u8; 65].into(),
        }
    }
}

impl Poe {
    pub fn sign_msg(&self, chain_id: &SU256) -> Vec<u8> {
        let mut encoder = solidity::Encoder::new("");
        encoder.add(chain_id);
        encoder.add(&self.batch_hash);
        encoder.add(&self.state_hash);
        encoder.add(&self.prev_state_root);
        encoder.add(&self.new_state_root);
        encoder.add(&self.withdrawal_root);
        encoder.add(self.signature.as_bytes());
        encoder.encode()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut encoder = solidity::Encoder::new("");
        encoder.add(&self.batch_hash);
        encoder.add(&self.state_hash);
        encoder.add(&self.prev_state_root);
        encoder.add(&self.new_state_root);
        encoder.add(&self.withdrawal_root);
        encoder.add(self.signature.as_bytes());
        encoder.encode()
    }

    pub fn recover(&self, chain_id: &SU256) -> SH160 {
        let mut tmp = self.clone();
        tmp.signature = vec![0_u8; 65].into();
        let data = tmp.sign_msg(chain_id);
        let mut sig = [0_u8; 65];
        sig.copy_from_slice(&self.signature);
        let sig = Secp256k1RecoverableSignature::new(sig);
        crypto::secp256k1_recover_pubkey(&sig, &data)
            .eth_accountid()
            .into()
    }
}
