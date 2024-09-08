use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use actix_web::web::Bytes;
use ethers::contract::{abigen, FunctionCall};
use ethers::middleware::SignerMiddleware;
use ethers::providers::{Http, Provider};
use ethers::signers::LocalWallet;
use ethers::types::{Address, H160, H256, U256};
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};

use crate::cgroups::Cgroups;

pub const GAS_LIMIT_BUFFER: u64 = 200000; // Fixed buffer to add to the estimated gas for setting gas limit
pub const TIMEOUT_TXN_RESEND_DEADLINE: u64 = 20; // Deadline (in secs) for resending pending/dropped execution timeout txns
pub const RESEND_TXN_INTERVAL: u64 = 5; // Interval (in secs) in which to resend pending/dropped txns
pub const RESEND_GAS_PRICE_INCREMENT_PERCENT: u64 = 10; // Gas price increment percent while resending pending/dropped txns

// Generate type-safe ABI bindings for the Jobs contract at compile time
abigen!(
    Jobs,
    "Jobs.json",
    derives(serde::Serialize, serde::Deserialize)
);

pub type HttpSignerProvider = SignerMiddleware<Provider<Http>, LocalWallet>;

pub struct ConfigManager {
    pub path: String,
}

// Config struct containing the executor configuration parameters
#[derive(Debug, Deserialize)]
pub struct Config {
    pub workerd_runtime_path: String,
    pub common_chain_id: u64,
    pub http_rpc_url: String,
    pub web_socket_url: String,
    pub executors_contract_addr: H160,
    pub jobs_contract_addr: H160,
    pub code_contract_addr: String,
    pub enclave_signer_file: String,
    pub execution_buffer_time: u64,
    pub num_selected_executors: u8,
}

// App data struct containing the necessary fields to run the executor
#[derive(Debug)]
pub struct AppState {
    pub cgroups: Mutex<Cgroups>,
    pub job_capacity: usize,
    pub workerd_runtime_path: String,
    pub execution_buffer_time: u64,
    pub common_chain_id: u64,
    pub http_rpc_url: String,
    pub ws_rpc_url: String,
    pub executors_contract_addr: Address,
    pub jobs_contract_addr: Address,
    pub code_contract_addr: String,
    pub num_selected_executors: u8,
    pub enclave_address: H160,
    pub enclave_signer: SigningKey,
    pub immutable_params_injected: Mutex<bool>,
    pub mutable_params_injected: Mutex<bool>,
    pub enclave_registered: AtomicBool,
    pub events_listener_active: Mutex<bool>,
    pub enclave_owner: Mutex<H160>,
    pub http_rpc_client: Mutex<Option<Arc<HttpSignerProvider>>>,
    pub job_requests_running: Mutex<HashSet<U256>>,
    pub last_block_seen: AtomicU64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ImmutableConfig {
    pub owner_address_hex: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MutableConfig {
    pub gas_key_hex: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobsTxnType {
    OUTPUT,
    TIMEOUT,
}

impl JobsTxnType {
    pub fn as_str(&self) -> &str {
        match self {
            JobsTxnType::OUTPUT => "output",
            JobsTxnType::TIMEOUT => "timeout",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct JobOutput {
    pub output: Bytes,
    pub error_code: u8,
    pub total_time: u128,
    pub sign_timestamp: U256,
    pub signature: Bytes,
}

#[derive(Debug, Clone)]
pub struct JobsTxnMetadata {
    pub txn_type: JobsTxnType,
    pub job_id: U256,
    pub job_output: Option<JobOutput>,
    pub retry_deadline: Instant,
}

#[derive(Debug, Clone)]
pub struct PendingTxnData {
    pub txn_hash: H256,
    pub txn_data: FunctionCall<Arc<HttpSignerProvider>, HttpSignerProvider, ()>,
    pub nonce: U256,
    pub gas_limit: U256,
    pub gas_price: U256,
    pub retry_deadline: Instant,
}
