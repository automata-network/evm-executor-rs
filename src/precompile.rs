use std::prelude::v1::*;

use std::collections::BTreeMap;

use crypto::{keccak_hash, secp256k1_ecdsa_recover, sha256_sum};
use eth_types::{HexBytes, H160, SU256, U256};
use std::borrow::Cow;

use evm::{
    executor::stack::{
        IsPrecompileResult, PrecompileFailure, PrecompileHandle, PrecompileOutput,
        PrecompileSet as EvmPrecompileSet,
    },
    ExitFatal, ExitSucceed,
};
use num_bigint::BigUint;
use num_traits::identities::{One, Zero};
use std::ops::Deref;

lazy_static::lazy_static! {
    static ref SECP256K1N: SU256 = "115792089237316195423570985008687907852837564279074904382605163141518161494337".into();
}

pub type PrecompileResult = Result<PrecompileOutput, PrecompileFailure>;

#[derive(Debug, Default)]
pub struct PrecompileSet {
    fns: BTreeMap<H160, Box<dyn PrecompiledContract + Send + Sync>>,
}

impl PrecompileSet {
    pub fn berlin() -> Self {
        let mut def = Self::default();
        for i in 1..=9 {
            def.add(i, PrecompileUnimplemented { addr: i });
        }

        def.add(1, PrecompileEcrecover {});
        def.add(2, PrecompileSha256Hash {});
        def.add(3, PrecompileRipemd160Hash {});
        def.add(4, PrecompileDataCopy {});
        def.add(
            5,
            PrecompileBigModExp {
                eip2565: true,
                length_limit: None,
            },
        );
        def.add(6, PrecompileAddIstanbul {});
        def.add(7, PrecompileMulIstanbul {});
        def.add(
            8,
            PrecompilePairIstanbul {
                max_input_num: None,
            },
        );
        def.add(9, PrecompileBlake2F {});

        def
    }

    pub fn scroll() -> Self {
        let mut def = Self::default();
        for i in 1..=9 {
            def.add(i, PrecompileUnimplemented { addr: i });
        }

        def.add(1, PrecompileEcrecover {});
        def.add(2, PrecompileRevert {});
        def.add(3, PrecompileRevert {});
        def.add(4, PrecompileDataCopy {});
        def.add(
            5,
            PrecompileBigModExp {
                eip2565: true,
                length_limit: Some(32),
            },
        );
        def.add(6, PrecompileAddIstanbul {});
        def.add(7, PrecompileMulIstanbul {});
        def.add(
            8,
            PrecompilePairIstanbul {
                max_input_num: Some(4),
            },
        );
        def.add(9, PrecompileRevert {});

        def
    }

    pub fn get_addresses(&self) -> Vec<H160> {
        self.fns.keys().map(|k| k.clone()).collect()
    }

    fn add<P>(&mut self, idx: u8, p: P)
    where
        P: PrecompiledContract + Send + Sync + 'static,
    {
        let mut addr = H160::default();

        addr.0[addr.0.len() - 1] = idx;
        self.fns.insert(addr.clone(), Box::new(p));
    }
}

impl EvmPrecompileSet for PrecompileSet {
    fn execute(&self, handle: &mut impl PrecompileHandle) -> Option<PrecompileResult> {
        let p = self.fns.get(&handle.code_address())?;
        Some(run_precompiled_contract(p.as_ref(), handle))
    }

    fn is_precompile(&self, address: H160, _remaining_gas: u64) -> IsPrecompileResult {
        IsPrecompileResult::Answer {
            is_precompile: self.fns.contains_key(&address),
            extra_cost: 0,
        }
    }
}

fn run_precompiled_contract<P>(p: &P, handle: &mut impl PrecompileHandle) -> PrecompileResult
where
    P: PrecompiledContract + ?Sized,
{
    let gas_cost = p.required_gas(handle.input());
    handle.record_cost(gas_cost)?;
    p.run(handle.input())
}

pub trait PrecompiledContract: core::fmt::Debug {
    fn calculate_gas(&self, input: &[u8], per_word_gas: usize, base_gas: usize) -> u64 {
        ((input.len() + 31) / 32 * per_word_gas + base_gas) as u64
    }
    fn required_gas(&self, input: &[u8]) -> u64;
    fn run(&self, input: &[u8]) -> PrecompileResult;
}

#[derive(Debug)]
pub struct PrecompileUnimplemented {
    addr: u8,
}

impl PrecompiledContract for PrecompileUnimplemented {
    fn required_gas(&self, _: &[u8]) -> u64 {
        0
    }
    fn run(&self, _: &[u8]) -> PrecompileResult {
        glog::error!("unimplemented addr: {}", self.addr);
        PrecompileResult::Err(PrecompileFailure::Fatal {
            exit_status: ExitFatal::NotSupported,
        })
    }
}

#[derive(Debug)]
pub struct PrecompileRevert {}

impl PrecompiledContract for PrecompileRevert {
    fn required_gas(&self, _: &[u8]) -> u64 {
        1_000_000_000
    }
    fn run(&self, _: &[u8]) -> PrecompileResult {
        PrecompileResult::Err(PrecompileFailure::Fatal {
            exit_status: ExitFatal::Other("DISABLED".into()),
        })
    }
}

/// Input length for the add operation.
const ADD_INPUT_LEN: usize = 128;

/// Input length for the multiplication operation.
const MUL_INPUT_LEN: usize = 128;

/// Pair element length.
const PAIR_ELEMENT_LEN: usize = 192;

/// Reads the `x` and `y` points from an input at a given position.
fn read_point(input: &[u8], pos: usize) -> bn::G1 {
    use bn::{AffineG1, Fq, Group, G1};

    let mut px_buf = [0u8; 32];
    px_buf.copy_from_slice(&input[pos..(pos + 32)]);
    let px = Fq::from_slice(&px_buf).unwrap(); // .unwrap(); //.map_err(|_| Error::Bn128FieldPointNotAMember)?;

    let mut py_buf = [0u8; 32];
    py_buf.copy_from_slice(&input[(pos + 32)..(pos + 64)]);
    let py = Fq::from_slice(&py_buf).unwrap(); //.unwrap(); //.map_err(|_| Error::Bn128FieldPointNotAMember)?;

    if px == Fq::zero() && py == bn::Fq::zero() {
        G1::zero()
    } else {
        AffineG1::new(px, py).map(Into::into).unwrap() //.map_err(|_| Error::Bn128AffineGFailedToCreate)
    }
}

#[derive(Debug)]
pub struct PrecompileAddIstanbul {}

impl PrecompiledContract for PrecompileAddIstanbul {
    fn required_gas(&self, _: &[u8]) -> u64 {
        150
    }
    fn run(&self, input: &[u8]) -> PrecompileResult {
        use bn::AffineG1;

        let mut input = input.to_vec();
        input.resize(ADD_INPUT_LEN, 0);

        let p1 = read_point(&input, 0);
        let p2 = read_point(&input, 64);

        let mut output = [0u8; 64];
        if let Some(sum) = AffineG1::from_jacobian(p1 + p2) {
            sum.x()
                .into_u256()
                .to_big_endian(&mut output[..32])
                .unwrap();
            sum.y()
                .into_u256()
                .to_big_endian(&mut output[32..])
                .unwrap();
        }

        Ok(PrecompileOutput {
            exit_status: ExitSucceed::Returned,
            output: output.into(),
        })
    }
}

#[derive(Debug)]
pub struct PrecompileMulIstanbul {}

impl PrecompiledContract for PrecompileMulIstanbul {
    fn required_gas(&self, _: &[u8]) -> u64 {
        6000
    }
    fn run(&self, input: &[u8]) -> PrecompileResult {
        use bn::AffineG1;

        let mut input = input.to_vec();
        input.resize(MUL_INPUT_LEN, 0);

        let p = read_point(&input, 0);

        let mut fr_buf = [0u8; 32];
        fr_buf.copy_from_slice(&input[64..96]);
        // Fr::from_slice can only fail on incorect length, and this is not a case.
        let fr = bn::Fr::from_slice(&fr_buf[..]).unwrap();

        let mut out = [0u8; 64];
        if let Some(mul) = AffineG1::from_jacobian(p * fr) {
            mul.x().to_big_endian(&mut out[..32]).unwrap();
            mul.y().to_big_endian(&mut out[32..]).unwrap();
        }

        Ok(PrecompileOutput {
            exit_status: ExitSucceed::Returned,
            output: out.to_vec(),
        })
    }
}

#[derive(Debug)]
pub struct PrecompilePairIstanbul {
    max_input_num: Option<usize>,
}

fn exit_error(val: Cow<'static, str>) -> PrecompileFailure {
    PrecompileFailure::Error {
        exit_status: evm::ExitError::Other(val),
    }
}

impl PrecompiledContract for PrecompilePairIstanbul {
    fn required_gas(&self, input: &[u8]) -> u64 {
        45000 + (input.len() / 192) as u64 * 34000
    }
    fn run(&self, input: &[u8]) -> PrecompileResult {
        use bn::{AffineG1, AffineG2, Fq, Fq2, Group, Gt, G1, G2};

        if let Some(max_input_num) = self.max_input_num {
            if input.len() > max_input_num * PAIR_ELEMENT_LEN {
                return Err(exit_error(
                    "bad elliptic curve pairing size, the input num exceed limitation".into(),
                ));
            }
        }

        if input.len() % PAIR_ELEMENT_LEN != 0 {
            return Err(exit_error("bad elliptic curve pairing size".into()));
        }

        let output = if input.is_empty() {
            U256::from(1u64)
        } else {
            let elements = input.len() / PAIR_ELEMENT_LEN;
            let mut vals = Vec::with_capacity(elements);

            const PEL: usize = PAIR_ELEMENT_LEN;

            for idx in 0..elements {
                let mut buf = [0u8; 32];

                buf.copy_from_slice(&input[(idx * PEL)..(idx * PEL + 32)]);
                let ax = Fq::from_slice(&buf)
                    .map_err(|_| exit_error("Invalid a argument x coordinate".into()))?;
                buf.copy_from_slice(&input[(idx * PEL + 32)..(idx * PEL + 64)]);
                let ay = Fq::from_slice(&buf)
                    .map_err(|_| exit_error("Invalid a argument y coordinate".into()))?;
                buf.copy_from_slice(&input[(idx * PEL + 64)..(idx * PEL + 96)]);
                let bay = Fq::from_slice(&buf).map_err(|_| {
                    exit_error("Invalid b argument imaginary coeff y coordinate".into())
                })?;
                buf.copy_from_slice(&input[(idx * PEL + 96)..(idx * PEL + 128)]);
                let bax = Fq::from_slice(&buf).map_err(|_| {
                    exit_error("Invalid b argument imaginary coeff x coordinate".into())
                })?;
                buf.copy_from_slice(&input[(idx * PEL + 128)..(idx * PEL + 160)]);
                let bby = Fq::from_slice(&buf)
                    .map_err(|_| exit_error("Invalid b argument real coeff y coordinate".into()))?;
                buf.copy_from_slice(&input[(idx * PEL + 160)..(idx * PEL + 192)]);
                let bbx = Fq::from_slice(&buf)
                    .map_err(|_| exit_error("Invalid b argument real coeff x coordinate".into()))?;

                let a = {
                    if ax.is_zero() && ay.is_zero() {
                        G1::zero()
                    } else {
                        let g1 = AffineG1::new(ax, ay)
                            .map_err(|_| exit_error("Invalid a argument - not on curve".into()))?;
                        G1::from(g1)
                    }
                };
                let b = {
                    let ba = Fq2::new(bax, bay);
                    let bb = Fq2::new(bbx, bby);

                    if ba.is_zero() && bb.is_zero() {
                        G2::zero()
                    } else {
                        let g2 = AffineG2::new(ba, bb)
                            .map_err(|_| exit_error("Invalid a argument - not on curve".into()))?;
                        G2::from(g2)
                    }
                };
                vals.push((a, b))
            }

            let mul = vals
                .into_iter()
                .fold(Gt::one(), |s, (a, b)| s * bn::pairing(a, b));

            if mul == Gt::one() {
                U256::from(1u64)
            } else {
                U256::zero()
            }
        };

        let mut b = [0_u8; 32];
        output.to_big_endian(&mut b);
        Ok(PrecompileOutput {
            exit_status: ExitSucceed::Returned,
            output: b.into(),
        })
    }
}

#[derive(Debug)]
pub struct PrecompileEcrecover {}

impl PrecompiledContract for PrecompileEcrecover {
    fn required_gas(&self, _: &[u8]) -> u64 {
        3000
    }
    fn run(&self, input: &[u8]) -> PrecompileResult {
        fn ecrecover(i: &[u8]) -> Vec<u8> {
            let mut input = [0u8; 128];
            input[..i.len().min(128)].copy_from_slice(&i[..i.len().min(128)]);

            let mut msg = [0u8; 32];
            let mut sig = [0u8; 65];

            msg[0..32].copy_from_slice(&input[0..32]);
            sig[0..32].copy_from_slice(&input[64..96]);
            sig[32..64].copy_from_slice(&input[96..128]);
            sig[64] = input[63];

            // Make sure that input[32:63] are all zeros
            if input[32..63].iter().any(|i| i != &0u8) {
                return Vec::new();
            }
            // Check signatures
            let r = SU256::from_big_endian(&sig[0..32]);
            let s = SU256::from_big_endian(&sig[32..64]);
            let v: u8 = sig[64];
            if r.is_zero() || s.is_zero() {
                return Vec::new();
            }
            if &r >= SECP256K1N.deref() || &s >= SECP256K1N.deref() || (v != 27 && v != 28) {
                return Vec::new();
            }

            let pubkey = match secp256k1_ecdsa_recover(&sig, &msg) {
                Some(pubkey) => pubkey,
                None => return Vec::new(),
            };
            let mut address = keccak_hash(&pubkey);
            address[0..12].copy_from_slice(&[0u8; 12]);
            address.to_vec()
        }

        Ok(PrecompileOutput {
            exit_status: ExitSucceed::Returned,
            output: ecrecover(input),
        })
    }
}

#[derive(Debug)]
pub struct PrecompileSha256Hash {}

impl PrecompiledContract for PrecompileSha256Hash {
    fn required_gas(&self, input: &[u8]) -> u64 {
        self.calculate_gas(input, 12, 60)
    }

    fn run(&self, input: &[u8]) -> PrecompileResult {
        let val = sha256_sum(input);
        Ok(PrecompileOutput {
            exit_status: ExitSucceed::Returned,
            output: val.to_vec(),
        })
    }
}

#[derive(Debug)]
pub struct PrecompileDataCopy {}

impl PrecompiledContract for PrecompileDataCopy {
    // testcase: https://goerli.etherscan.io/tx/0x5e928106ec0115b89df07315d7b980c8a072a00c977c2834ac8b41bfb3241324#internal
    fn required_gas(&self, input: &[u8]) -> u64 {
        self.calculate_gas(input, 3, 15)
    }

    fn run(&self, input: &[u8]) -> PrecompileResult {
        Ok(PrecompileOutput {
            exit_status: ExitSucceed::Returned,
            output: input.to_vec(),
        })
    }
}

#[derive(Debug)]
pub struct PrecompileRipemd160Hash {}

impl PrecompiledContract for PrecompileRipemd160Hash {
    fn required_gas(&self, input: &[u8]) -> u64 {
        self.calculate_gas(input, 120, 600)
    }

    fn run(&self, input: &[u8]) -> PrecompileResult {
        glog::debug!("input: {:?}", HexBytes::from(input.to_vec()));
        use ripemd160::{Digest, Ripemd160};
        let output = Ripemd160::digest(input).to_vec();
        let mut val = [0_u8; 32];
        val[12..].copy_from_slice(&output[..]);
        Ok(PrecompileOutput {
            exit_status: ExitSucceed::Returned,
            output: val.into(),
        })
    }
}

#[derive(Debug)]
pub struct PrecompileBlake2F {}

impl PrecompiledContract for PrecompileBlake2F {
    fn required_gas(&self, input: &[u8]) -> u64 {
        if input.len() != 213 {
            return 0;
        }
        let mut val = [0_u8; 4];
        val.copy_from_slice(&input[..4]);
        return u32::from_be_bytes(val) as u64;
    }

    fn run(&self, input: &[u8]) -> PrecompileResult {
        if input.len() != 213 {
            return Err(exit_error(
                "Invalid input for blake2f precompile: incorrect length".into(),
            ));
        }

        let f = match input[212] {
            1 => true,
            0 => false,
            _ => {
                return Err(exit_error(
                    "Invalid input for blake2f precompile: incorrect final flag".into(),
                ))
            }
        };

        // rounds 4 bytes
        let rounds = u32::from_be_bytes(input[..4].try_into().unwrap()) as usize;

        let mut h = [0u64; 8];
        let mut m = [0u64; 16];

        for (i, pos) in (4..68).step_by(8).enumerate() {
            h[i] = u64::from_le_bytes(input[pos..pos + 8].try_into().unwrap());
        }
        for (i, pos) in (68..196).step_by(8).enumerate() {
            m[i] = u64::from_le_bytes(input[pos..pos + 8].try_into().unwrap());
        }
        let t = [
            u64::from_le_bytes(input[196..196 + 8].try_into().unwrap()),
            u64::from_le_bytes(input[204..204 + 8].try_into().unwrap()),
        ];

        eip_152::compress(&mut h, m, t, f, rounds);

        let mut out = [0u8; 64];
        for (i, h) in (0..64).step_by(8).zip(h.iter()) {
            out[i..i + 8].copy_from_slice(&h.to_le_bytes());
        }

        Ok(PrecompileOutput {
            exit_status: ExitSucceed::Returned,
            output: out.into(),
        })
    }
}

mod eip_152 {
    const SIGMA: [[usize; 16]; 10] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
        [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
        [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
        [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
        [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
        [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
        [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
        [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
        [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
    ];

    /// IV is the initialization vector for BLAKE2b. See https://tools.ietf.org/html/rfc7693#section-2.6
    /// for details.
    const IV: [u64; 8] = [
        0x6a09e667f3bcc908,
        0xbb67ae8584caa73b,
        0x3c6ef372fe94f82b,
        0xa54ff53a5f1d36f1,
        0x510e527fade682d1,
        0x9b05688c2b3e6c1f,
        0x1f83d9abfb41bd6b,
        0x5be0cd19137e2179,
    ];

    #[inline(always)]
    /// The G mixing function. See https://tools.ietf.org/html/rfc7693#section-3.1
    fn g(v: &mut [u64], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
        v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
        v[d] = (v[d] ^ v[a]).rotate_right(32);
        v[c] = v[c].wrapping_add(v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(24);
        v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
        v[d] = (v[d] ^ v[a]).rotate_right(16);
        v[c] = v[c].wrapping_add(v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(63);
    }

    /// The Blake2 compression function F. See https://tools.ietf.org/html/rfc7693#section-3.2
    /// Takes as an argument the state vector `h`, message block vector `m`, offset counter `t`, final
    /// block indicator flag `f`, and number of rounds `rounds`. The state vector provided as the first
    /// parameter is modified by the function.
    pub fn compress(h: &mut [u64; 8], m: [u64; 16], t: [u64; 2], f: bool, rounds: usize) {
        let mut v = [0u64; 16];
        v[..h.len()].copy_from_slice(h); // First half from state.
        v[h.len()..].copy_from_slice(&IV); // Second half from IV.

        v[12] ^= t[0];
        v[13] ^= t[1];

        if f {
            v[14] = !v[14] // Invert all bits if the last-block-flag is set.
        }
        for i in 0..rounds {
            // Message word selection permutation for this round.
            let s = &SIGMA[i % 10];
            g(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
            g(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
            g(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
            g(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);

            g(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
            g(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
            g(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
            g(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
        }

        for i in 0..8 {
            h[i] ^= v[i] ^ v[i + 8];
        }
    }
}

#[derive(Debug)]
pub struct PrecompileBigModExp {
    // testcase 0x6baf80b76832ff53cd551d3d607c04596ec45dd098dc7c0ac292f6a1264c1337
    eip2565: bool,
    length_limit: Option<usize>,
}

impl PrecompiledContract for PrecompileBigModExp {
    fn required_gas(&self, input: &[u8]) -> u64 {
        // Padding data to be at least 32 * 3 bytes.
        let mut data: Vec<u8> = input.into();
        while data.len() < 32 * 3 {
            data.push(0);
        }

        let base_len = U256::from(&data[0..32]).as_usize();
        let exp_len = U256::from(&data[32..64]).as_usize();
        let mod_len = U256::from(&data[64..96]).as_usize();

        let input = input.get(96..).unwrap_or(&[]);

        let exp_head = if input.len() <= base_len {
            U256::from(0u64)
        } else {
            if exp_len > 32 {
                U256::from(&input[base_len..base_len + 32])
            } else {
                U256::from(&input[base_len..base_len + exp_len])
            }
        };

        let msb = match exp_head.bits() {
            0 => 0,
            other => other - 1,
        };
        // adjExpLen := new(big.Int)
        let mut adj_exp_len = 0;
        if exp_len > 32 {
            adj_exp_len = exp_len - 32;
            adj_exp_len *= 8;
        }
        adj_exp_len += msb;
        // Calculate the gas cost of the operation
        let mut gas = U256::from(mod_len.max(base_len));

        if self.eip2565 {
            // EIP-2565 has three changes
            // 1. Different multComplexity (inlined here)
            // in EIP-2565 (https://eips.ethereum.org/EIPS/eip-2565):
            //
            // def mult_complexity(x):
            //    ceiling(x/8)^2
            //
            //where is x is max(length_of_MODULUS, length_of_BASE)
            gas += U256::from(7u64);
            gas /= U256::from(8u64);
            gas *= gas;

            gas *= U256::from(adj_exp_len.max(1));

            // 2. Different divisor (`GQUADDIVISOR`) (3)
            gas /= U256::from(3u64);
            if gas.bits() > 64 {
                return u64::MAX;
            }

            // 3. Minimum price of 200 gas
            if gas < U256::from(200u64) {
                return 200;
            }
            return gas.as_u64();
        }
        unimplemented!()
    }

    fn run(&self, input: &[u8]) -> PrecompileResult {
        // Padding data to be at least 32 * 3 bytes.
        let mut data: Vec<u8> = input.into();
        while data.len() < 32 * 3 {
            data.push(0);
        }

        let base_length = U256::from(&data[0..32]);
        let exponent_length = U256::from(&data[32..64]);
        let modulus_length = U256::from(&data[64..96]);

        // if base_length > U256::from(usize::max_value())
        //     || exponent_length > U256::from(usize::max_value())
        //     || modulus_length > U256::from(usize::max_value())
        // {
        //     panic!(
        //         "MemoryIndexNotSupported, {}, {}, {}",
        //         base_length, exponent_length, modulus_length
        //     )
        // }

        let base_length: usize = base_length.as_usize();
        let exponent_length: usize = exponent_length.as_usize();
        let modulus_length: usize = modulus_length.as_usize();

        if let Some(length_limit) = self.length_limit {
            if base_length > length_limit
                || exponent_length > length_limit
                || modulus_length > length_limit
            {
                return Err(exit_error("input length exceed limitation".into()));
            }
        }

        if base_length == 0 && modulus_length == 0 {
            return Ok(PrecompileOutput {
                exit_status: ExitSucceed::Returned,
                output: Vec::new(),
            });
        }

        let mut base_arr = Vec::new();
        let mut exponent_arr = Vec::new();
        let mut modulus_arr = Vec::new();

        for i in 0..base_length {
            if 96 + i >= data.len() {
                base_arr.push(0u8);
            } else {
                base_arr.push(data[96 + i]);
            }
        }
        for i in 0..exponent_length {
            if 96 + base_length + i >= data.len() {
                exponent_arr.push(0u8);
            } else {
                exponent_arr.push(data[96 + base_length + i]);
            }
        }
        for i in 0..modulus_length {
            if 96 + base_length + exponent_length + i >= data.len() {
                modulus_arr.push(0u8);
            } else {
                modulus_arr.push(data[96 + base_length + exponent_length + i]);
            }
        }

        let base = BigUint::from_bytes_be(&base_arr);
        let exponent = BigUint::from_bytes_be(&exponent_arr);
        let modulus = BigUint::from_bytes_be(&modulus_arr);

        let mut result = if modulus.is_zero() || modulus.is_one() {
            BigUint::zero().to_bytes_be()
        } else {
            base.modpow(&exponent, &modulus).to_bytes_be()
        };
        assert!(result.len() <= modulus_length);
        while result.len() < modulus_length {
            result.insert(0, 0u8);
        }

        Ok(PrecompileOutput {
            exit_status: ExitSucceed::Returned,
            output: result,
        })
    }
}

#[cfg(test)]
mod test {
    use std::{io::Read};

    use serde::ser;

    use super::*;

    fn test_precompile(precompile: &dyn PrecompiledContract, input: &[u8], expected: &[u8], expected_gas: u64) {
        let result: HexBytes = precompile
                .run(&HexBytes::from_hex(input).unwrap())
                .unwrap()
                .output
                .into();
        let gas = precompile.required_gas(&HexBytes::from_hex(input).unwrap());

        assert_eq!(result, HexBytes::from_hex(expected).unwrap());
        assert_eq!(gas, expected_gas);
    }

    fn load_and_test_precompile(precompile: &dyn PrecompiledContract, test_data_path: &str, precompile_name: &str) {
        // Read testdata from {test_data_path}
        let mut file = std::fs::File::open(test_data_path).unwrap();        
        let mut buf = Vec::new();
        file.read_to_end(&mut buf);

        let test_data = buf.as_slice();
        let test_data_str = std::str::from_utf8(test_data).expect("Invalid UTF-8");
        let test_data_json = serde_json::from_str::<serde_json::Value>(test_data_str).unwrap();
        // Iterate over the test cases
        for (i, test_case) in test_data_json.as_array().unwrap().iter().enumerate() {
            let input = test_case["Input"].as_str().unwrap().as_bytes();
            let output = test_case["Expected"].as_str().unwrap();
            let expected_gas = test_case["Gas"].as_u64().unwrap();
            test_precompile(precompile, input, output.as_bytes(), expected_gas);
            glog::info!("[{}] test case {} passed", precompile_name, i);
        }
    }

    // Precompile idx: 1
    #[test]
    fn test_ecrecover() {
        glog::init_test();
        let contract = PrecompileEcrecover {};
        load_and_test_precompile(&contract, "src/testdata/ecrecover.json", "ecrecover");
    }

    // Precompile idx: 2
    #[test]
    fn test_sha256() {
        glog::init_test();
        let input = HexBytes::from_hex(b"38d18acb67d25c8bb9942764b62f18e17054f66a817bd4295423adf9ed98873e000000000000000000000000000000000000000000000000000000000000001b38d18acb67d25c8bb9942764b62f18e17054f66a817bd4295423adf9ed98873e789d1dd423d25f0772d2748d60f7e4b81bb14d086eba8e8e8efb6dcff8a4ae02").unwrap();
        let expect = HexBytes::from_hex(b"811c7003375852fabd0d362e40e68607a12bdabae61a7d068fe5fdd1dbbf2a5d").unwrap();
        let contract = PrecompileSha256Hash {};
        let result: HexBytes = contract.run(&input).unwrap().output.into();
        assert_eq!(expect, result);
        assert_eq!(108, contract.required_gas(&input));
    }

    // Precompile idx: 3
    #[test]
    fn test_ripemd() {
        glog::init_test();
        let input = HexBytes::from_hex(b"38d18acb67d25c8bb9942764b62f18e17054f66a817bd4295423adf9ed98873e000000000000000000000000000000000000000000000000000000000000001b38d18acb67d25c8bb9942764b62f18e17054f66a817bd4295423adf9ed98873e789d1dd423d25f0772d2748d60f7e4b81bb14d086eba8e8e8efb6dcff8a4ae02").unwrap();
        let expect = HexBytes::from_hex(b"0000000000000000000000009215b8d9882ff46f0dfde6684d78e831467f65e6").unwrap();
        let contract = PrecompileRipemd160Hash {};
        let result: HexBytes = contract.run(&input).unwrap().output.into();
        assert_eq!(expect, result);
        assert_eq!(1080, contract.required_gas(&input));
    }

    // Precompile idx: 5
    #[test]
    fn test_bigmodexp_eip2565() {
        glog::init_test();
        let contract = PrecompileBigModExp {
            eip2565: true,
            length_limit: None,
        };
        load_and_test_precompile(&contract, "src/testdata/modexp_eip2565.json", "modexp_eip2565");
    }

    // Precompile idx: 6
    #[test]
    fn test_add_istanbul() {
        glog::init_test();
        let contract = PrecompileAddIstanbul {};
        load_and_test_precompile(&contract, "src/testdata/bn256add.json", "AddIstanbul");
    }

    // Precompile idx: 7
    #[test]
    fn test_mul_istanbul() {
        glog::init_test();
        let contract = PrecompileMulIstanbul {};
        load_and_test_precompile(&contract, "src/testdata/bn256mul.json", "MulIstanbul");
    }

    // Precompile idx: 8
    #[test]
    fn test_pairing_istanbul() {
        glog::init_test();
        let contract = PrecompilePairIstanbul {
            max_input_num: None,
        };
        load_and_test_precompile(&contract, "src/testdata/bn256pairing.json", "PairIstanbul");
    }

    // Precompile idx: 9
    #[test]
    fn test_blake2f() {
        glog::init_test();
        let contract = PrecompileBlake2F {};
        load_and_test_precompile(&contract, "src/testdata/blake2f.json", "Blake2F");
    }

    #[test]
    fn test_blake2f_fail() {
        glog::init_test();
        let contract = PrecompileBlake2F {};

        let input = HexBytes::from_hex(b"").unwrap();
        let result = contract.run(&input);
        assert_eq!(result, Err(PrecompileFailure::Error{exit_status: evm::ExitError::Other("Invalid input for blake2f precompile: incorrect length".into())}));

        let input = HexBytes::from_hex(b"00000c48c9bdf267e6096a3ba7ca8485ae67bb2bf894fe72f36e3cf1361d5f3af54fa5d182e6ad7f520e511f6c3e2b8c68059b6bbd41fbabd9831f79217e1319cde05b61626300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000300000000000000000000000000000001").unwrap();
        let result = contract.run(&input);
        assert_eq!(result, Err(PrecompileFailure::Error{exit_status: evm::ExitError::Other("Invalid input for blake2f precompile: incorrect length".into())}));

        let input = HexBytes::from_hex(b"000000000c48c9bdf267e6096a3ba7ca8485ae67bb2bf894fe72f36e3cf1361d5f3af54fa5d182e6ad7f520e511f6c3e2b8c68059b6bbd41fbabd9831f79217e1319cde05b61626300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000300000000000000000000000000000001").unwrap();
        let result = contract.run(&input);
        assert_eq!(result, Err(PrecompileFailure::Error{exit_status: evm::ExitError::Other("Invalid input for blake2f precompile: incorrect length".into())}));

        let input = HexBytes::from_hex(b"0000000c48c9bdf267e6096a3ba7ca8485ae67bb2bf894fe72f36e3cf1361d5f3af54fa5d182e6ad7f520e511f6c3e2b8c68059b6bbd41fbabd9831f79217e1319cde05b61626300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000300000000000000000000000000000002").unwrap();
        let result = contract.run(&input);
        assert_eq!(result, Err(PrecompileFailure::Error{exit_status: evm::ExitError::Other("Invalid input for blake2f precompile: incorrect final flag".into())}));
    }

    #[test]
    fn test_ecrecover_old() {
        glog::init_test();
        let input = HexBytes::from_hex(b"0x9161131deff2aea942dd43fbce9eb5b409b21670953e583fa10499dc52db57e3000000000000000000000000000000000000000000000000000000000000001bae2054dc5b25097032a64cdda29eb1da01a75ac4297249623bed59a44e91ae4b418e411747af2cd5e7e4a2ba2ed86b1d67ab8dccba4fc2adeab18ad66d8551d7").unwrap();
        let run = PrecompileEcrecover {}.run(&input).unwrap();
        let result: HexBytes = run.output.into();
        let expect = HexBytes::from_hex(
            b"0x000000000000000000000000a040a4e812306d66746508bcfbe84b3e73de67fa",
        )
        .unwrap();
        assert_eq!(expect, result);
    }

    #[test]
    fn test_ripemd_old() {
        glog::init_test();
        let input  = HexBytes::from_hex(b"0x099538be21d9ee24d052fb9bdc46307416b983d076f3bf04ccbe120ed514ca7589c83b3859bb92919a9d1006fbe59aeac6154321ab0ba37d3490a8c90000").unwrap();
        let result: HexBytes = PrecompileRipemd160Hash {}
            .run(&input)
            .unwrap()
            .output
            .into();
        let expect = HexBytes::from_hex(
            b"0x0000000000000000000000006b0f28fb610ce4d01c1d210a6aeb3967bf7bf0f7",
        )
        .unwrap();
        assert_eq!(expect, result);
    }

    #[test]
    fn test_bigexpmod() {
        glog::init_test();
        let input = HexBytes::from_hex(b"0x00000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000002005ec467b88826aba4537602d514425f3b0bdf467bbf302458337c45f6021e539000000000000000000000000000000000000000000000000000000000000000f0800000000000011000000000000000000000000000000000000000000000001").unwrap();
        let expect = HexBytes::from_hex(
            b"0x05c3ed0c6f6ac6dd647c9ba3e4721c1eb14011ea3d174c52d7981c5b8145aa75",
        )
        .unwrap();
        let contract = PrecompileBigModExp {
            eip2565: true,
            length_limit: None,
        };
        let output: HexBytes = contract.run(&input).unwrap().output.into();
        assert_eq!(expect, output);
        assert_eq!(contract.required_gas(&input), 200); // 16
    }
}
