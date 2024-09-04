use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use actix_web::web::Data;
use ethers::abi::{decode, ParamType};
use ethers::contract::FunctionCall;
use ethers::providers::{Middleware, Provider, StreamExt, Ws};
use ethers::types::{BigEndianHash, Filter, Log, TransactionRequest, H256, U256, U64};
use ethers::utils::keccak256;
use scopeguard::defer;
use tokio::select;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::time::sleep;
use tokio_stream::Stream;

use crate::job_handler::handle_job;
use crate::timeout_handler::handle_timeout;
use crate::utils::{
    AppState, HttpSignerProvider, JobResponse, Jobs, GAS_LIMIT_BUFFER,
    RESEND_GAS_PRICE_INCREMENT_PERCENT, RESEND_TXN_INTERVAL, TIMEOUT_TXN_RESEND_DEADLINE,
};

// Start listening to Job requests emitted by the Jobs contract if enclave is registered else listen for Executor registered events first
pub async fn events_listener(app_state: Data<AppState>, starting_block: U64) {
    defer! {
        *app_state.events_listener_active.lock().unwrap() = false;
    }
    loop {
        // web socket connection
        let web_socket_client =
            match Provider::<Ws>::connect_with_reconnects(&app_state.ws_rpc_url, 0).await {
                Ok(client) => client,
                Err(err) => {
                    eprintln!(
                        "Failed to connect to the common chain websocket provider: {:?}",
                        err
                    );
                    continue;
                }
            };

        if !app_state.enclave_registered.load(Ordering::SeqCst) {
            // Create filter to listen to the 'ExecutorRegistered' event emitted by the Executors contract
            let register_executor_filter = Filter::new()
                .address(app_state.executors_contract_addr)
                .topic0(H256::from(keccak256(
                    "ExecutorRegistered(address,address,uint256)",
                )))
                .topic1(H256::from(app_state.enclave_address))
                .topic2(H256::from(*app_state.enclave_owner.lock().unwrap()))
                .from_block(starting_block);

            // Subscribe to the executors filter through the rpc web socket client
            let mut register_stream = match web_socket_client
                .subscribe_logs(&register_executor_filter)
                .await
            {
                Ok(stream) => stream,
                Err(err) => {
                    eprintln!(
                        "Failed to subscribe to Executors ({:?}) contract 'ExecutorRegistered' event logs: {:?}",
                        app_state.executors_contract_addr,
                        err,
                    );
                    continue;
                }
            };

            while let Some(event) = register_stream.next().await {
                if event.removed.unwrap_or(true) {
                    continue;
                }

                app_state.enclave_registered.store(true, Ordering::SeqCst);
                app_state.last_block_seen.store(
                    event.block_number.unwrap_or(starting_block).as_u64(),
                    Ordering::SeqCst,
                );
                break;
            }

            if !app_state.enclave_registered.load(Ordering::SeqCst) {
                continue;
            }
        }

        println!("Enclave registered successfully on the common chain!");
        // Create filter to listen to relevant events emitted by the Jobs contract
        let jobs_event_filter = Filter::new()
            .address(app_state.jobs_contract_addr)
            .topic0(vec![
                keccak256("JobCreated(uint256,address,bytes32,bytes,uint256,address[])"),
                keccak256("JobResponded(uint256,bytes,uint256,uint8,uint8)"),
            ])
            .from_block(app_state.last_block_seen.load(Ordering::SeqCst));
        // Subscribe to the jobs filter through the rpc web socket client
        let jobs_stream = match web_socket_client.subscribe_logs(&jobs_event_filter).await {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!(
                    "Failed to subscribe to Jobs ({:?}) contract 'JobCreated' and 'JobResponded' event logs: {:?}",
                    app_state.jobs_contract_addr,
                    err,
                );
                continue;
            }
        };
        let jobs_stream = std::pin::pin!(jobs_stream);

        // Create filter to listen to 'ExecutorDeregistered' event emitted by the Executors contract
        let executors_event_filter = Filter::new()
            .address(app_state.executors_contract_addr)
            .topic0(H256::from(keccak256("ExecutorDeregistered(address)")))
            .topic1(H256::from(app_state.enclave_address))
            .from_block(app_state.last_block_seen.load(Ordering::SeqCst));
        // Subscribe to the executors filter through the rpc web socket client
        let executors_stream = match web_socket_client
            .subscribe_logs(&executors_event_filter)
            .await
        {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!(
                    "Failed to subscribe to Executors ({:?}) contract 'ExecutorDeregistered' event logs: {:?}",
                    app_state.executors_contract_addr,
                    err
                );
                continue;
            }
        };
        let executors_stream = std::pin::pin!(executors_stream);

        // Initialize nonce for sending job execution transactions via the injected gas account
        let http_rpc_client = app_state.http_rpc_client.lock().unwrap().clone().unwrap();
        let nonce_to_send = http_rpc_client
            .get_transaction_count(http_rpc_client.address(), None)
            .await;
        let Ok(nonce_to_send) = nonce_to_send else {
            eprintln!(
                "Failed to fetch current nonce for the gas address ({:?}): {:?}",
                http_rpc_client.address(),
                nonce_to_send.unwrap_err()
            );
            continue;
        };

        // Create tokio mpsc channel to receive contract events and send transactions to them
        let (tx, rx) = channel::<JobResponse>(100);
        let app_state_clone = app_state.clone();

        tokio::spawn(async move {
            send_execution_output(nonce_to_send, app_state_clone, rx).await;
        });

        handle_event_logs(jobs_stream, executors_stream, app_state.clone(), tx).await;
        if !app_state.enclave_registered.load(Ordering::SeqCst) {
            return;
        }
    }
}

// Receive job execution responses and send the resulting transactions to the common chain
async fn send_execution_output(
    mut nonce_to_send: U256,
    app_state: Data<AppState>,
    mut rx: Receiver<JobResponse>,
) {
    while let Some(job_response) = rx.recv().await {
        let mut txn: Option<FunctionCall<Arc<HttpSignerProvider>, HttpSignerProvider, ()>> = None;
        let mut resend_deadline = TIMEOUT_TXN_RESEND_DEADLINE;
        let mut txn_type = "timeout";
        let mut job_id: U256 = 0.into();

        if job_response.job_output.is_some() {
            let job_output = job_response.job_output.unwrap();

            // Assign details of job execution output transaction
            txn = Some(
                Jobs::new(
                    app_state.jobs_contract_addr,
                    app_state.http_rpc_client.lock().unwrap().clone().unwrap(),
                )
                .submit_output(
                    job_output.signature.clone().into(),
                    job_output.id,
                    job_output.execution_response.output.clone().into(),
                    job_output.execution_response.total_time.into(),
                    job_output.execution_response.error_code.into(),
                    job_output.sign_timestamp,
                ),
            );
            resend_deadline = (app_state.execution_buffer_time as u128) + job_output.user_deadline
                - job_output.execution_response.total_time;
            txn_type = "output";
            job_id = job_output.id;
        } else if job_response.timeout_response.is_some() {
            // Assign details of job execution timeout transaction
            job_id = job_response.timeout_response.unwrap();
            txn = Some(
                Jobs::new(
                    app_state.jobs_contract_addr,
                    app_state.http_rpc_client.lock().unwrap().clone().unwrap(),
                )
                .slash_on_execution_timeout(job_id),
            );
        }

        if txn.is_none() {
            continue;
        }

        let mut update_nonce = false;
        let job_txn = txn.unwrap();

        // Retry loop for sending a transaction to the common chain
        for _retry in 0..3 {
            let txn = job_txn.clone();

            // Estimate gas required for the transaction to execute and retry otherwise
            let estimated_gas = txn.estimate_gas().await;
            let Ok(estimated_gas) = estimated_gas else {
                eprintln!("Failed to estimate gas from the rpc for sending execution {} transaction for job id {}: {:?}", txn_type, job_id, estimated_gas.unwrap_err());
                sleep(Duration::from_millis(10)).await;
                continue;
            };

            // Get current gas price for the common chain network and retry otherwise
            let http_rpc_client = app_state.http_rpc_client.lock().unwrap().clone().unwrap();
            let gas_price = http_rpc_client.get_gas_price().await;
            let Ok(gas_price) = gas_price else {
                eprintln!(
                    "Failed to get gas price from the rpc for the network: {:?}",
                    gas_price.unwrap_err()
                );
                sleep(Duration::from_millis(10)).await;
                continue;
            };

            // If required retrieve the current nonce from the network and retry otherwise
            if update_nonce == true {
                let new_nonce_to_send = http_rpc_client
                    .get_transaction_count(http_rpc_client.address(), None)
                    .await;
                if new_nonce_to_send.is_err() {
                    eprintln!(
                        "Failed to fetch current nonce for the gas address ({:?}): {:?}",
                        http_rpc_client.address(),
                        new_nonce_to_send.unwrap_err()
                    );
                    continue;
                };
                nonce_to_send = new_nonce_to_send.unwrap();
                update_nonce = false;
            }

            // Update metadata to be used for sending the transaction and send it to the common chain
            let txn = txn
                .gas(estimated_gas + GAS_LIMIT_BUFFER)
                .nonce(nonce_to_send)
                .gas_price(gas_price);
            let pending_txn = txn.send().await;
            let Ok(pending_txn) = pending_txn else {
                let error_string = format!("{:?}", pending_txn.unwrap_err());
                eprintln!(
                    "Failed to send the execution {} transaction for job id {}: {}",
                    txn_type, job_id, error_string
                );

                // Retry if 'nonce too low' error encountered
                if error_string.contains("code: -32000") && error_string.contains("nonce") {
                    update_nonce = true;
                    continue;
                }

                // Retry after a delay if connection failed
                if error_string.contains("code: -32000") && error_string.contains("connection") {
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }

                break;
            };

            let pending_tx_hash = pending_txn.tx_hash();
            println!(
                "Execution {} transaction successfully sent for job id {} with nonce {} and hash {:?}",
                txn_type, job_id, nonce_to_send, pending_tx_hash
            );

            // Monitor the transaction sent for block confirmation
            let app_state_clone = app_state.clone();
            tokio::spawn(async move {
                resend_pending_transaction(
                    app_state_clone,
                    job_txn,
                    pending_tx_hash,
                    nonce_to_send,
                    estimated_gas + GAS_LIMIT_BUFFER,
                    gas_price,
                    resend_deadline,
                )
                .await;
            });

            // Increment nonce for the next transaction to send
            nonce_to_send += 1.into();
            break;
        }
    }

    println!("Transaction sender channel stopped!");
    return;
}

// Listen to the "Jobs" & "Executors" contract event logs and process them accordingly
pub async fn handle_event_logs(
    mut jobs_stream: impl Stream<Item = Log> + Unpin,
    mut executors_stream: impl Stream<Item = Log> + Unpin,
    app_state: Data<AppState>,
    tx: Sender<JobResponse>,
) {
    println!("Started listening to job events!");

    loop {
        select! {
            Some(event) = executors_stream.next() => {
                if event.removed.unwrap_or(true) {
                    continue;
                }

                // Capture the Executor deregistered event emitted by the executors contract
                println!("Enclave deregistered from the common chain!");
                app_state.enclave_registered.store(false, Ordering::SeqCst);

                println!("Stopped listening to job events!");
                return;
            }
            Some(event) = jobs_stream.next() => {
                if event.removed.unwrap_or(true) {
                    continue;
                }

                let Some(current_block) = event.block_number else {
                    continue;
                };

                if current_block.as_u64() < app_state.last_block_seen.load(Ordering::SeqCst) {
                    continue;
                }
                app_state.last_block_seen.store(current_block.as_u64(), Ordering::SeqCst);

                // Capture the Job created event emitted by the jobs contract
                if event.topics[0]
                    == keccak256("JobCreated(uint256,address,bytes32,bytes,uint256,address[])")
                    .into()
                {
                    // Extract the 'indexed' parameter of the event
                    let job_id = event.topics[1].into_uint();

                    // Decode the event parameters using the ABI information
                    let event_tokens = decode(
                        &vec![
                        ParamType::FixedBytes(32),
                        ParamType::Bytes,
                        ParamType::Uint(256),
                        ParamType::Array(Box::new(ParamType::Address)),
                        ],
                        &event.data.to_vec(),
                    );
                    let Ok(event_tokens) = event_tokens else {
                        eprintln!(
                            "Failed to decode 'JobCreated' event data for job id {}: {:?}",
                            job_id,
                            event_tokens.unwrap_err()
                        );
                        continue;
                    };

                    let Some(code_hash) = event_tokens[0].clone().into_fixed_bytes() else {
                        eprintln!(
                            "Failed to decode codeHash token from the 'JobCreated' event data for job id {}: {:?}",
                            job_id,
                            event_tokens[0]
                        );
                        continue;
                    };
                    let Some(code_inputs) = event_tokens[1].clone().into_bytes() else {
                        eprintln!(
                            "Failed to decode codeInputs token from the 'JobCreated' event data for job id {}: {:?}",
                            job_id,
                            event_tokens[1]
                        );
                        continue;
                    };
                    let Some(user_deadline) = event_tokens[2].clone().into_uint() else {
                        eprintln!(
                            "Failed to decode deadline token from the 'JobCreated' event data for job id {}: {:?}",
                            job_id,
                            event_tokens[2]
                        );
                        continue;
                    };
                    let Some(selected_nodes) = event_tokens[3].clone().into_array() else {
                        eprintln!(
                            "Failed to decode selectedExecutors token from the 'JobCreated' event data for job id {}: {:?}",
                            job_id,
                            event_tokens[3]
                        );
                        continue;
                    };

                    // Mark the current job as under execution
                    app_state
                        .job_requests_running
                        .lock()
                        .unwrap()
                        .insert(job_id);

                    // Check if the executor has been selected for the job execution
                    let is_node_selected = selected_nodes
                        .into_iter()
                        .map(|token| token.into_address())
                        .filter(|addr| addr.is_some())
                        .any(|addr| addr.unwrap() == app_state.enclave_address);

                    let app_state_clone = app_state.clone();
                    let tx_clone = tx.clone();

                    tokio::spawn(async move {
                        handle_timeout(job_id, user_deadline.as_u64(), app_state_clone, tx_clone).await;
                    });

                    if is_node_selected {
                        let code_hash =
                            String::from("0x".to_owned() + &data_encoding::HEXLOWER.encode(&code_hash));
                        let app_state_clone = app_state.clone();
                        let tx_clone = tx.clone();

                        tokio::spawn(async move {
                            handle_job(
                                job_id,
                                code_hash,
                                code_inputs.into(),
                                user_deadline.as_u64(),
                                app_state_clone,
                                tx_clone,
                            )
                            .await;
                        });
                    }
                }
                // Capture the Job responded event emitted by the Jobs contract
                else if event.topics[0]
                    == keccak256("JobResponded(uint256,bytes,uint256,uint8,uint8)").into()
                {
                    let job_id = event.topics[1].into_uint();

                    // Decode the event parameters using the ABI information
                    let event_tokens = decode(
                        &vec![
                        ParamType::Bytes,
                        ParamType::Uint(256),
                        ParamType::Uint(8),
                        ParamType::Uint(8),
                        ],
                        &event.data.to_vec(),
                    );
                    let Ok(event_tokens) = event_tokens else {
                        eprintln!(
                            "Failed to decode 'JobResponded' event data for job id {}: {:?}",
                            job_id,
                            event_tokens.unwrap_err()
                        );
                        continue;
                    };

                    let Some(output_count) = event_tokens[3].clone().into_uint() else {
                        eprintln!(
                            "Failed to decode outputCount token from the 'JobResponded' event data for job id {}: {:?}",
                            job_id,
                            event_tokens[3]
                        );
                        continue;
                    };

                    if output_count == app_state.num_selected_executors.into() {
                        // Mark the job as completed
                        app_state
                            .job_requests_running
                            .lock()
                            .unwrap()
                            .remove(&job_id);
                    }
                }
            }
            else => break,
        }
    }

    println!("Both the Jobs and Executors subscription streams have ended!");
}

// Function to regularly check a transaction for block confirmation and resend it if not included
async fn resend_pending_transaction(
    app_state: Data<AppState>,
    txn: FunctionCall<Arc<HttpSignerProvider>, HttpSignerProvider, ()>,
    mut pending_txn_hash: H256,
    nonce: U256,
    mut gas_limit: U256,
    mut gas_price: U256,
    resend_deadline: u128,
) {
    // Calculating resend retries number based on the overall deadline and interval in which to resend pending/dropped txns
    let mut resend_retries = resend_deadline / (RESEND_TXN_INTERVAL as u128);
    while resend_retries > 0 {
        sleep(Duration::from_secs(RESEND_TXN_INTERVAL)).await;

        // Get the transaction receipt for the pending transaction to check if it's still pending or been dropped
        let http_rpc_client = app_state.http_rpc_client.lock().unwrap().clone().unwrap();
        let Ok(txn_receipt) = http_rpc_client
            .get_transaction_receipt(pending_txn_hash)
            .await
        else {
            resend_retries -= 1;
            continue;
        };

        if txn_receipt.is_some() {
            break;
        }

        // Update gas limit and price for sending the replacement transaction with the original nonce
        gas_limit += GAS_LIMIT_BUFFER.into();
        gas_price += (U256::from(RESEND_GAS_PRICE_INCREMENT_PERCENT) * gas_price) / 100;

        // Update replacement transaction metadata and send it to the common chain, stop resending if failed to do so once
        let replacement_txn = txn.clone();
        let replacement_txn = replacement_txn
            .nonce(nonce)
            .gas(gas_limit)
            .gas_price(gas_price);
        let Ok(pending_txn) = replacement_txn.send().await else {
            resend_retries = 0;
            break;
        };

        pending_txn_hash = pending_txn.tx_hash();
        resend_retries -= 1;
    }

    // If the original transaction fails to get included in the common chain, send a dummy transaction that is confirmed to be included and will replace the original nonce transaction
    if resend_retries == 0 {
        // Increasing gas price so that the current transaction can replace the original in the mempool
        gas_price += (U256::from(RESEND_GAS_PRICE_INCREMENT_PERCENT) * gas_price) / 100;
        let http_rpc_client = app_state.http_rpc_client.lock().unwrap().clone().unwrap();

        // Send 0 ETH to self from the gas account as a confirmed replacement transaction
        let dummy_replacement_txn = TransactionRequest::pay(http_rpc_client.address(), 0u64)
            .nonce(nonce)
            .gas_price(gas_price);
        let _ = http_rpc_client
            .send_transaction(dummy_replacement_txn, None)
            .await;
    }
}
