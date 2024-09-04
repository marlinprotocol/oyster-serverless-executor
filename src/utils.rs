use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

use actix_web::web::Bytes;
use ethers::contract::abigen;
use ethers::middleware::SignerMiddleware;
use ethers::providers::{Http, Provider};
use ethers::signers::LocalWallet;
use ethers::types::{Address, H160, U256};
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};

use crate::cgroups::Cgroups;

pub const GAS_LIMIT_BUFFER: u64 = 200000; // Fixed buffer to add to the estimated gas for setting gas limit
pub const TIMEOUT_TXN_RESEND_DEADLINE: u128 = 20; // Deadline (in secs) for resending pending/dropped execution timeout txns
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

#[derive(Debug, Clone)]
pub struct JobResponse {
    pub job_output: Option<JobOutput>,
    pub timeout_response: Option<U256>,
}

#[derive(Debug, Clone)]
pub struct JobOutput {
    pub signature: Bytes,
    pub id: U256,
    pub execution_response: ExecutionResponse,
    pub sign_timestamp: U256,
    pub user_deadline: u128,
}

#[derive(Debug, Clone)]
pub struct ExecutionResponse {
    pub output: Bytes,
    pub error_code: u8,
    pub total_time: u128,
}
