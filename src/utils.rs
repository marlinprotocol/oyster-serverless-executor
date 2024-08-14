use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use actix_web::web::Bytes;
use anyhow::{anyhow, Context, Result};
use ethers::contract::{abigen, FunctionCall};
use ethers::middleware::{NonceManagerMiddleware, SignerMiddleware};
use ethers::providers::{Http, Provider};
use ethers::signers::LocalWallet;
use ethers::types::{Address, TransactionReceipt, H160, U256};
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};

use crate::cgroups::Cgroups;

// Fixed buffer to add to the estimated gas for setting gas limit
const GAS_LIMIT_BUFFER: u64 = 200000;

// Generate type-safe ABI bindings for the Jobs contract at compile time
abigen!(
    Jobs,
    "Jobs.json",
    derives(serde::Serialize, serde::Deserialize)
);

pub type HttpSignerProvider = NonceManagerMiddleware<SignerMiddleware<Provider<Http>, LocalWallet>>;

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
}

#[derive(Debug, Clone)]
pub struct ExecutionResponse {
    pub output: Bytes,
    pub error_code: u8,
    pub total_time: U256,
}

// Send a signed transaction to the rpc network and report its confirmation or rejection
pub async fn send_txn(
    txn: FunctionCall<Arc<HttpSignerProvider>, HttpSignerProvider, ()>,
) -> Result<TransactionReceipt> {
    let estimated_gas = txn
        .estimate_gas()
        .await
        .context("Failed to estimate gas from the rpc")?;
    let txn = txn.gas(estimated_gas + GAS_LIMIT_BUFFER);

    let pending_txn = txn
        .send()
        .await
        .context("Failed to send the transaction to the network")?;

    let txn_hash = pending_txn.tx_hash();
    let Some(txn_receipt) = pending_txn
        .confirmations(1)
        .interval(Duration::from_millis(1000))
        .await
        .context("Failed to confirm the transaction")?
    else {
        return Err(anyhow!(
            "Transaction with hash {:?} has been dropped from mempool!",
            txn_hash
        ));
    };

    Ok(txn_receipt)
}
