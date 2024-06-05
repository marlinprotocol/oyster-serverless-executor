use actix_web::web::Data;
use ethers::abi::{decode, ParamType};
use ethers::providers::{Middleware, StreamExt};
use ethers::types::{BigEndianHash, Filter, Log, H256, U64};
use ethers::utils::keccak256;
use log::info;
use scopeguard::defer;
use tokio::select;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio_stream::Stream;

use crate::job_handler::handle_job;
use crate::timeout_handler::handle_timeout;
use crate::utils::{send_txn, AppState, JobResponse, Jobs};

// Start listening to Job requests emitted by the Jobs contract if enclave is registered else listen for Executor registered events first
pub async fn events_listener(app_state: Data<AppState>, starting_block: U64) {
    defer! {
        *app_state.events_listener_active.lock().unwrap() = false;
    }

    if *app_state.enclave_registered.lock().unwrap() == false {
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
        let register_stream = app_state
            .web_socket_client
            .subscribe_logs(&register_executor_filter)
            .await;
        let Ok(mut register_stream) = register_stream else {
            unsafe {
                eprintln!(
                    "Failed to subscribe to Executors ({:?}) contract 'ExecutorRegistered' event logs: {:?}",
                    app_state.executors_contract_addr,
                    register_stream.unwrap_err_unchecked(),
                );
            }
            return;
        };

        while let Some(event) = register_stream.next().await {
            if event.removed.unwrap_or(true) {
                continue;
            }

            *app_state.enclave_registered.lock().unwrap() = true;
            *app_state.last_block_seen.lock().unwrap() =
                event.block_number.unwrap_or(starting_block);
            break;
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
        .from_block(*app_state.last_block_seen.lock().unwrap());
    // Subscribe to the jobs filter through the rpc web socket client
    let jobs_stream = app_state
        .web_socket_client
        .subscribe_logs(&jobs_event_filter)
        .await;
    let Ok(jobs_stream) = jobs_stream else {
        unsafe {
            eprintln!(
                "Failed to subscribe to Jobs ({:?}) contract 'JobCreated' and 'JobResponded' event logs: {:?}",
                app_state.jobs_contract_addr,
                jobs_stream.unwrap_err_unchecked(),
            );
        }
        return;
    };
    let jobs_stream = std::pin::pin!(jobs_stream);

    // Create filter to listen to 'ExecutorDeregistered' event emitted by the Executors contract
    let executors_event_filter = Filter::new()
        .address(app_state.executors_contract_addr)
        .topic0(H256::from(keccak256("ExecutorDeregistered(address)")))
        .topic1(H256::from(app_state.enclave_address))
        .from_block(*app_state.last_block_seen.lock().unwrap());

    // Subscribe to the executors filter through the rpc web socket client
    let executors_stream = app_state
        .web_socket_client
        .subscribe_logs(&executors_event_filter)
        .await;
    let Ok(executors_stream) = executors_stream else {
        unsafe {
            eprintln!(
                "Failed to subscribe to Executors ({:?}) contract 'ExecutorDeregistered' event logs: {:?}",
                app_state.executors_contract_addr,
                executors_stream.unwrap_err_unchecked()
            );
        }
        return;
    };
    let executors_stream = std::pin::pin!(executors_stream);

    // Create tokio mpsc channel to receive contract events and send transactions to them
    let (tx, rx) = channel::<JobResponse>(100);
    let app_state_clone = app_state.clone();

    tokio::spawn(async move {
        send_execution_output(app_state_clone, rx).await;
    });

    handle_event_logs(jobs_stream, executors_stream, app_state.clone(), tx).await;
}

// Receive job execution responses and send the resulting transactions to the common chain
async fn send_execution_output(app_state: Data<AppState>, mut rx: Receiver<JobResponse>) {
    while let Some(job_response) = rx.recv().await {
        let Some(job_output) = job_response.job_output else {
            let Some(job_id) = job_response.timeout_response else {
                continue;
            };

            // Prepare the execution timeout transaction to be send to the jobs contract
            let txn = Jobs::new(
                app_state.jobs_contract_addr,
                app_state.http_rpc_client.lock().unwrap().clone().unwrap(),
            )
            .slash_on_execution_timeout(job_id);

            let txn_result = send_txn(txn).await;
            let Ok(_) = txn_result else {
                eprintln!(
                    "Failed to submit the execution timeout transaction: {:?}",
                    txn_result.unwrap_err()
                );
                continue;
            };

            continue;
        };

        // Prepare the execution output transaction to be send to the jobs contract
        let txn = Jobs::new(
            app_state.jobs_contract_addr,
            app_state.http_rpc_client.lock().unwrap().clone().unwrap(),
        )
        .submit_output(
            job_output.signature.into(),
            job_output.id,
            job_output.execution_response.output.into(),
            job_output.execution_response.total_time.into(),
            job_output.execution_response.error_code.into(),
            job_output.sign_timestamp,
        );

        let txn_result = send_txn(txn).await;
        let Ok(_) = txn_result else {
            eprintln!(
                "Failed to submit the execution output transaction: {:?}",
                txn_result.unwrap_err()
            );
            continue;
        };
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
                *app_state.enclave_registered.lock().unwrap() = false;

                println!("Stopped listening to job events!");
                return;
            }
            Some(event) = jobs_stream.next() => {
                if event.removed.unwrap_or(true) {
                    continue;
                }

                if event.block_number.is_some() {
                    *app_state.last_block_seen.lock().unwrap() = event.block_number.unwrap();
                }

                // Capture the Job created event emitted by the jobs contract
                if event.topics[0]
                    == keccak256("JobCreated(uint256,address,bytes32,bytes,uint256,address[])")
                    .into()
                {
                    info!("Job Created event received");

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
                            "Failed to decode 'JobCreated' event data {}: {:?}",
                            event.data,
                            event_tokens.unwrap_err()
                        );
                        continue;
                    };

                    // Extract the 'indexed' parameter of the event
                    let job_id = event.topics[1].into_uint();

                    let Some(code_hash) = event_tokens[0].clone().into_fixed_bytes() else {
                        eprintln!(
                            "Failed to decode codeHash token from the 'JobCreated' event data: {:?}",
                            event_tokens[0]
                        );
                        continue;
                    };
                    let Some(code_inputs) = event_tokens[1].clone().into_bytes() else {
                        eprintln!(
                            "Failed to decode codeInputs token from the 'JobCreated' event data: {:?}",
                            event_tokens[1]
                        );
                        continue;
                    };
                    let Some(user_deadline) = event_tokens[2].clone().into_uint() else {
                        eprintln!(
                            "Failed to decode deadline token from the 'JobCreated' event data: {:?}",
                            event_tokens[2]
                        );
                        continue;
                    };
                    let Some(selected_nodes) = event_tokens[3].clone().into_array() else {
                        eprintln!(
                            "Failed to decode selectedExecutors token from the 'JobCreated' event data: {:?}",
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
                    info!("Job Responded event received");

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
                            "Failed to decode 'JobResponded' event data {}: {:?}",
                            event.data,
                            event_tokens.unwrap_err()
                        );
                        continue;
                    };

                    let Some(output_count) = event_tokens[3].clone().into_uint() else {
                        eprintln!(
                            "Failed to decode outputCount token from the 'JobResponded' event data: {:?}",
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
