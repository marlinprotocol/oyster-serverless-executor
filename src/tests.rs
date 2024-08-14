// NOTE: Tests have to be run one by one currently

/* To run an unit test 'test_name', hit the following commands on terminal ->
   1.    sudo ./cgroupv2_setup.sh
   2.    export RUSTFLAGS="--cfg tokio_unstable"
   3.    sudo echo && cargo test 'test name' -- --nocapture &
   4.    sudo echo && cargo test -- --test-threads 1 &           (For running all the tests sequentially)
*/

#[cfg(test)]
pub mod serverless_executor_test {
    use std::collections::HashSet;
    use std::str::FromStr;
    use std::sync::atomic::Ordering;

    use actix_web::body::MessageBody;
    use actix_web::dev::{ServiceFactory, ServiceRequest, ServiceResponse};
    use actix_web::web::{Bytes, Data};
    use actix_web::{http, test, App, Error};
    use ethers::abi::{encode, encode_packed, Token};
    use ethers::providers::Middleware;
    use ethers::types::{Address, BigEndianHash, Log, H160, H256, U256, U64};
    use ethers::utils::{keccak256, public_key_to_address};
    use k256::ecdsa::SigningKey;
    use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
    use rand::rngs::OsRng;
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use tokio::runtime::Handle;
    use tokio::sync::mpsc::channel;
    use tokio::time::{sleep, Duration};
    use tokio_stream::StreamExt as _;

    use crate::cgroups::Cgroups;
    use crate::event_handler::handle_event_logs;
    use crate::node_handler::{
        export_signed_registration_message, get_executor_details, index, inject_immutable_config,
        inject_mutable_config,
    };
    use crate::utils::{AppState, JobResponse};

    // Testnet or Local blockchain (Hardhat) configurations
    const CHAIN_ID: u64 = 421614;
    const HTTP_RPC_URL: &str = "https://sepolia-rollup.arbitrum.io/rpc";
    const WS_URL: &str = "wss://arb-sepolia.g.alchemy.com/v2/U8uYtmU3xK9j7HEZ74riWfj3C4ode7n1";
    const EXECUTORS_CONTRACT_ADDR: &str = "0xE35E287DBC371561E198bFaCBdbEc9cF78bDe930";
    const JOBS_CONTRACT_ADDR: &str = "0xd3b682f6F58323EC77dEaE730733C6A83a1561Fd";
    const CODE_CONTRACT_ADDR: &str = "0x44fe06d2940b8782a0a9a9ffd09c65852c0156b1";

    // Generate test app state
    async fn generate_app_state() -> Data<AppState> {
        let signer = SigningKey::random(&mut OsRng);
        let signer_verifier_address = public_key_to_address(signer.verifying_key());

        Data::new(AppState {
            job_capacity: 20,
            cgroups: Cgroups::new().unwrap().into(),
            workerd_runtime_path: "./runtime/".to_owned(),
            execution_buffer_time: 10,
            common_chain_id: CHAIN_ID,
            http_rpc_url: HTTP_RPC_URL.to_owned(),
            ws_rpc_url: WS_URL.to_owned(),
            executors_contract_addr: EXECUTORS_CONTRACT_ADDR.parse::<Address>().unwrap(),
            jobs_contract_addr: JOBS_CONTRACT_ADDR.parse::<Address>().unwrap(),
            code_contract_addr: CODE_CONTRACT_ADDR.to_owned(),
            num_selected_executors: 1,
            enclave_address: signer_verifier_address,
            enclave_signer: signer,
            immutable_params_injected: false.into(),
            mutable_params_injected: false.into(),
            enclave_registered: false.into(),
            events_listener_active: false.into(),
            enclave_owner: H160::zero().into(),
            http_rpc_client: None.into(),
            job_requests_running: HashSet::new().into(),
            last_block_seen: 0.into(),
        })
    }

    // Return the actix server with the provided app state
    fn new_app(
        app_state: Data<AppState>,
    ) -> App<
        impl ServiceFactory<
            ServiceRequest,
            Response = ServiceResponse<impl MessageBody + std::fmt::Debug>,
            Config = (),
            InitError = (),
            Error = Error,
        >,
    > {
        App::new()
            .app_data(app_state)
            .service(index)
            .service(inject_immutable_config)
            .service(inject_mutable_config)
            .service(get_executor_details)
            .service(export_signed_registration_message)
    }

    #[actix_web::test]
    // Test the various response cases for the 'inject_immutable_config' endpoint
    async fn inject_immutable_config_test() {
        let app_state = generate_app_state().await;
        let app = test::init_service(new_app(app_state.clone())).await;

        // Inject invalid owner address hex string (odd length)
        let req = test::TestRequest::post()
            .uri("/immutable-config")
            .set_json(&json!({
                "owner_address_hex": "32255",
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Invalid owner address hex string: OddLength\n"
        );

        // Inject invalid owner address hex string (invalid hex character)
        let req = test::TestRequest::post()
            .uri("/immutable-config")
            .set_json(&json!({
                "owner_address_hex": "32255G",
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Invalid owner address hex string: InvalidHexCharacter { c: 'G', index: 5 }\n"
        );

        // Inject invalid owner address hex string (less than 20 bytes)
        let req = test::TestRequest::post()
            .uri("/immutable-config")
            .set_json(&json!({
                "owner_address_hex": "322557",
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Owner address must be 20 bytes long!\n"
        );

        // Inject valid immutable config params
        let valid_owner = H160::random();
        let req = test::TestRequest::post()
            .uri("/immutable-config")
            .set_json(&json!({
                "owner_address_hex": hex::encode(valid_owner),
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Immutable params configured!\n"
        );
        assert_eq!(*app_state.enclave_owner.lock().unwrap(), valid_owner);

        // Inject valid immutable config params again to test immutability
        let valid_owner_2 = H160::random();
        let req = test::TestRequest::post()
            .uri("/immutable-config")
            .set_json(&json!({
                "owner_address_hex": hex::encode(valid_owner_2),
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Immutable params already configured!\n"
        );
    }

    #[actix_web::test]
    // Test the various response cases for the 'inject_mutable_config' endpoint
    async fn inject_mutable_config_test() {
        let app_state = generate_app_state().await;
        let app = test::init_service(new_app(app_state.clone())).await;

        // Inject invalid gas private key hex string (less than 32 bytes)
        let req = test::TestRequest::post()
            .uri("/mutable-config")
            .set_json(&json!({
                "gas_key_hex": "322557",
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Gas private key must be 32 bytes long!\n"
        );

        // Inject invalid gas private key hex string (invalid hex character)
        let req = test::TestRequest::post()
            .uri("/mutable-config")
            .set_json(&json!({
                "gas_key_hex": "fffffffffffffffffzffffffffffffffffffffffffffffgfffffffffffffffff",
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Invalid gas private key hex string: InvalidHexCharacter { c: 'z', index: 17 }\n"
        );

        // Inject invalid gas private key hex string (not ecdsa valid key)
        let req = test::TestRequest::post()
            .uri("/mutable-config")
            .set_json(&json!({
                "gas_key_hex": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Invalid gas private key provided: EcdsaError(signature::Error { source: None })\n"
        );

        // Inject valid mutable config params
        let gas_wallet_key = SigningKey::random(&mut OsRng);
        let req = test::TestRequest::post()
            .uri("/mutable-config")
            .set_json(&json!({
                "gas_key_hex": hex::encode(gas_wallet_key.to_bytes()),
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Mutable params configured!\n"
        );
        assert_eq!(
            app_state
                .http_rpc_client
                .lock()
                .unwrap()
                .clone()
                .unwrap()
                .inner()
                .address(),
            public_key_to_address(gas_wallet_key.verifying_key())
        );

        // Inject valid mutable config params again to test mutability
        let gas_wallet_key = SigningKey::random(&mut OsRng);
        let req = test::TestRequest::post()
            .uri("/mutable-config")
            .set_json(&json!({
                "gas_key_hex": hex::encode(gas_wallet_key.to_bytes()),
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Mutable params configured!\n"
        );
        assert_eq!(
            app_state
                .http_rpc_client
                .lock()
                .unwrap()
                .clone()
                .unwrap()
                .inner()
                .address(),
            public_key_to_address(gas_wallet_key.verifying_key())
        );
    }

    #[derive(Serialize, Deserialize, Debug)]
    struct ExecutorDetails {
        enclave_address: H160,
        enclave_public_key: String,
        owner_address: H160,
        gas_address: H160,
    }

    #[actix_web::test]
    // Test the various response cases for the 'get_executor_details' endpoint
    async fn get_executor_details_test() {
        let app_state = generate_app_state().await;
        let app = test::init_service(new_app(app_state.clone())).await;

        // Get the executor details without injecting any config params
        let req = test::TestRequest::get()
            .uri("/executor-details")
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::OK);

        let response: Result<ExecutorDetails, serde_json::Error> =
            serde_json::from_slice(&resp.into_body().try_into_bytes().unwrap());
        assert!(response.is_ok());

        let response = response.unwrap();
        assert_eq!(response.enclave_address, app_state.enclave_address);
        assert_eq!(
            response.enclave_public_key,
            format!(
                "0x{}",
                hex::encode(
                    &(app_state
                        .enclave_signer
                        .verifying_key()
                        .to_encoded_point(false)
                        .as_bytes())[1..]
                )
            )
        );
        assert_eq!(response.owner_address, H160::zero());
        assert_eq!(response.gas_address, H160::zero());

        // Inject valid immutable config params
        let valid_owner = H160::random();
        let req = test::TestRequest::post()
            .uri("/immutable-config")
            .set_json(&json!({
                "owner_address_hex": hex::encode(valid_owner),
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Immutable params configured!\n"
        );
        assert_eq!(*app_state.enclave_owner.lock().unwrap(), valid_owner);

        // Get the executor details without injecting mutable config params
        let req = test::TestRequest::get()
            .uri("/executor-details")
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::OK);

        let response: Result<ExecutorDetails, serde_json::Error> =
            serde_json::from_slice(&resp.into_body().try_into_bytes().unwrap());
        assert!(response.is_ok());

        let response = response.unwrap();
        assert_eq!(response.enclave_address, app_state.enclave_address);
        assert_eq!(
            response.enclave_public_key,
            format!(
                "0x{}",
                hex::encode(
                    &(app_state
                        .enclave_signer
                        .verifying_key()
                        .to_encoded_point(false)
                        .as_bytes())[1..]
                )
            )
        );
        assert_eq!(response.owner_address, valid_owner);
        assert_eq!(response.gas_address, H160::zero());

        // Inject valid mutable config params
        let gas_wallet_key = SigningKey::random(&mut OsRng);
        let req = test::TestRequest::post()
            .uri("/mutable-config")
            .set_json(&json!({
                "gas_key_hex": hex::encode(gas_wallet_key.to_bytes()),
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Mutable params configured!\n"
        );
        assert_eq!(
            app_state
                .http_rpc_client
                .lock()
                .unwrap()
                .clone()
                .unwrap()
                .inner()
                .address(),
            public_key_to_address(gas_wallet_key.verifying_key())
        );

        // Get the executor details
        let req = test::TestRequest::get()
            .uri("/executor-details")
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::OK);

        let response: Result<ExecutorDetails, serde_json::Error> =
            serde_json::from_slice(&resp.into_body().try_into_bytes().unwrap());
        assert!(response.is_ok());

        let response = response.unwrap();
        assert_eq!(response.enclave_address, app_state.enclave_address);
        assert_eq!(
            response.enclave_public_key,
            format!(
                "0x{}",
                hex::encode(
                    &(app_state
                        .enclave_signer
                        .verifying_key()
                        .to_encoded_point(false)
                        .as_bytes())[1..]
                )
            )
        );
        assert_eq!(response.owner_address, valid_owner);
        assert_eq!(
            response.gas_address,
            public_key_to_address(gas_wallet_key.verifying_key())
        );
    }

    #[derive(Serialize, Deserialize, Debug)]
    struct ExportResponse {
        job_capacity: usize,
        sign_timestamp: usize,
        owner: H160,
        signature: String,
    }

    #[actix_web::test]
    // Test the various response cases for the 'export_signed_registration_message' endpoint
    async fn export_signed_registration_message_test() {
        let metrics = Handle::current().metrics();

        let app_state = generate_app_state().await;
        let verifying_key = app_state.enclave_signer.verifying_key().to_owned();

        let app = test::init_service(new_app(app_state.clone())).await;

        // Export the enclave registration details without injecting executor config params
        let req = test::TestRequest::get()
            .uri("/signed-registration-message")
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Immutable params not configured yet!\n"
        );

        // Inject valid immutable config params
        let valid_owner = H160::random();
        let req = test::TestRequest::post()
            .uri("/immutable-config")
            .set_json(&json!({
                "owner_address_hex": hex::encode(valid_owner),
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Immutable params configured!\n"
        );
        assert_eq!(*app_state.enclave_owner.lock().unwrap(), valid_owner);

        // Export the enclave registration details without injecting executor config params
        let req = test::TestRequest::get()
            .uri("/signed-registration-message")
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Mutable params not configured yet!\n"
        );

        // Inject valid mutable config params
        let gas_wallet_key = SigningKey::random(&mut OsRng);
        let req = test::TestRequest::post()
            .uri("/mutable-config")
            .set_json(&json!({
                "gas_key_hex": hex::encode(gas_wallet_key.to_bytes()),
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(
            resp.into_body().try_into_bytes().unwrap(),
            "Mutable params configured!\n"
        );
        assert_eq!(
            app_state
                .http_rpc_client
                .lock()
                .unwrap()
                .clone()
                .unwrap()
                .inner()
                .address(),
            public_key_to_address(gas_wallet_key.verifying_key())
        );

        // Export the enclave registration details
        let req = test::TestRequest::get()
            .uri("/signed-registration-message")
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::OK);

        let response: Result<ExportResponse, serde_json::Error> =
            serde_json::from_slice(&resp.into_body().try_into_bytes().unwrap());
        assert!(response.is_ok());

        let response = response.unwrap();
        assert_eq!(response.job_capacity, 20);
        assert_eq!(response.owner, valid_owner);
        assert_eq!(response.signature.len(), 130);
        assert_eq!(
            recover_key(
                response.owner,
                response.job_capacity,
                response.sign_timestamp,
                response.signature
            ),
            verifying_key
        );
        assert_eq!(*app_state.events_listener_active.lock().unwrap(), true);
        let active_tasks = metrics.active_tasks_count();

        // Export the enclave registration details again
        let req = test::TestRequest::get()
            .uri("/signed-registration-message")
            .to_request();

        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), http::StatusCode::OK);

        let response: Result<ExportResponse, serde_json::Error> =
            serde_json::from_slice(&resp.into_body().try_into_bytes().unwrap());
        assert!(response.is_ok());

        let response = response.unwrap();
        assert_eq!(response.job_capacity, 20);
        assert_eq!(response.owner, valid_owner);
        assert_eq!(response.signature.len(), 130);
        assert_eq!(
            recover_key(
                response.owner,
                response.job_capacity,
                response.sign_timestamp,
                response.signature
            ),
            verifying_key
        );
        assert_eq!(active_tasks, metrics.active_tasks_count());
    }

    #[actix_web::test]
    // Test a valid job request with different inputs and verify the responses
    async fn valid_job_test() {
        let app_state = generate_app_state().await;

        let code_hash = "9468bb6a8e85ed11e292c8cac0c1539df691c8d8ec62e7dbfa9f1bd7f504e46e";
        let user_deadline = 5000;

        let code_input_bytes: Bytes = serde_json::to_vec(&json!({
            "num": 10
        }))
        .unwrap()
        .into();

        // Prepare the logs for JobCreated and JobResponded events accordingly
        let mut job_logs = vec![
            get_job_created_log(
                1.into(),
                0.into(),
                code_hash,
                code_input_bytes,
                user_deadline,
                app_state.enclave_address,
            ),
            get_job_responded_log(1.into(), 0.into()),
        ];

        let code_input_bytes: Bytes = serde_json::to_vec(&json!({
            "num": 20
        }))
        .unwrap()
        .into();

        job_logs.append(&mut vec![
            get_job_created_log(
                1.into(),
                1.into(),
                code_hash,
                code_input_bytes,
                user_deadline,
                app_state.enclave_address,
            ),
            get_job_responded_log(1.into(), 1.into()),
        ]);

        let code_input_bytes: Bytes = serde_json::to_vec(&json!({
            "num": 600
        }))
        .unwrap()
        .into();

        job_logs.append(&mut vec![
            get_job_created_log(
                1.into(),
                2.into(),
                code_hash,
                code_input_bytes,
                user_deadline,
                app_state.enclave_address,
            ),
            get_job_responded_log(1.into(), 2.into()),
        ]);

        job_logs.push(Log {
            ..Default::default()
        });

        let (tx, mut rx) = channel::<JobResponse>(10);

        tokio::spawn(async move {
            // Introduce time interval between events to be polled
            let jobs_stream = std::pin::pin!(tokio_stream::iter(job_logs.into_iter()).then(
                |log| async move {
                    sleep(Duration::from_millis(user_deadline)).await;
                    log
                }
            ));

            // Call the event handler for the contract logs
            handle_event_logs(
                jobs_stream,
                std::pin::pin!(tokio_stream::empty()),
                app_state,
                tx,
            )
            .await;
        });

        let mut responses: Vec<JobResponse> = vec![];

        // Receive and store the responses
        while let Some(job_response) = rx.recv().await {
            responses.push(job_response);
        }

        assert_eq!(responses.len(), 3);

        assert_response(responses[0].clone(), 0.into(), 0, "2,5");
        assert_response(responses[1].clone(), 1.into(), 0, "2,2,5");
        assert_response(responses[2].clone(), 2.into(), 0, "2,2,2,3,5,5");
    }

    #[actix_web::test]
    // Test a valid job request with invalid input and verify the response
    async fn invalid_input_job_test() {
        let app_state = generate_app_state().await;

        let code_hash = "9468bb6a8e85ed11e292c8cac0c1539df691c8d8ec62e7dbfa9f1bd7f504e46e";
        let user_deadline = 5000;

        let code_input_bytes: Bytes = serde_json::to_vec(&json!({})).unwrap().into();

        let job_logs = vec![
            get_job_created_log(
                1.into(),
                0.into(),
                code_hash,
                code_input_bytes,
                user_deadline,
                app_state.enclave_address,
            ),
            get_job_responded_log(1.into(), 0.into()),
            Log {
                ..Default::default()
            },
        ];

        let (tx, mut rx) = channel::<JobResponse>(10);

        tokio::spawn(async move {
            let jobs_stream = std::pin::pin!(tokio_stream::iter(job_logs.into_iter()).then(
                |log| async move {
                    sleep(Duration::from_millis(user_deadline)).await;
                    log
                }
            ));

            // Call the event handler for the contract logs
            handle_event_logs(
                jobs_stream,
                std::pin::pin!(tokio_stream::empty()),
                app_state,
                tx,
            )
            .await;
        });

        let mut responses: Vec<JobResponse> = vec![];

        // Receive and store the responses
        while let Some(job_response) = rx.recv().await {
            responses.push(job_response);
        }

        assert_eq!(responses.len(), 1);

        assert_response(
            responses[0].clone(),
            0.into(),
            0,
            "Please provide a valid integer as input in the format{'num':10}",
        );
    }

    #[actix_web::test]
    // Test '1' error code job requests and verify the responses
    async fn invalid_transaction_job_test() {
        let app_state = generate_app_state().await;

        let user_deadline = 5000;
        let code_input_bytes: Bytes = serde_json::to_vec(&json!({
            "num": 10
        }))
        .unwrap()
        .into();

        // Given transaction hash doesn't belong to the expected smart contract
        let mut job_logs = vec![
            get_job_created_log(
                1.into(),
                0.into(),
                "fed8ab36cc27831836f6dcb7291049158b4d8df31c0ffb05a3d36ba6555e29d7",
                code_input_bytes.clone(),
                user_deadline,
                app_state.enclave_address,
            ),
            get_job_responded_log(1.into(), 0.into()),
        ];

        // Given transaction hash doesn't exist in the expected rpc network
        job_logs.append(&mut vec![
            get_job_created_log(
                1.into(),
                1.into(),
                "37b0b2d9dd58d9130781fc914da456c16ec403010e8d4c27b0ea4657a24c8546",
                code_input_bytes,
                user_deadline,
                app_state.enclave_address,
            ),
            get_job_responded_log(1.into(), 1.into()),
        ]);

        job_logs.push(Log {
            ..Default::default()
        });

        let (tx, mut rx) = channel::<JobResponse>(10);

        tokio::spawn(async move {
            let jobs_stream = std::pin::pin!(tokio_stream::iter(job_logs.into_iter()).then(
                |log| async move {
                    sleep(Duration::from_millis(user_deadline)).await;
                    log
                }
            ));

            // Call the event handler for the contract logs
            handle_event_logs(
                jobs_stream,
                std::pin::pin!(tokio_stream::empty()),
                app_state,
                tx,
            )
            .await;
        });

        let mut responses: Vec<JobResponse> = vec![];

        // Receive and store the responses
        while let Some(job_response) = rx.recv().await {
            responses.push(job_response);
        }

        assert_eq!(responses.len(), 2);

        assert_response(responses[0].clone(), 0.into(), 1, "");
        assert_response(responses[1].clone(), 1.into(), 1, "");
    }

    #[actix_web::test]
    // Test '2' error code job request and verify the response
    async fn invalid_code_calldata_test() {
        let app_state = generate_app_state().await;

        let code_hash = "d23370ce64d1679fb53497b882347e25a026ba0bc54536340243ae7464d5d12d";
        let user_deadline = 5000;

        let code_input_bytes: Bytes = serde_json::to_vec(&json!({})).unwrap().into();

        // Calldata corresponding to the provided transaction hash is invalid
        let job_logs = vec![
            get_job_created_log(
                1.into(),
                0.into(),
                code_hash,
                code_input_bytes,
                user_deadline,
                app_state.enclave_address,
            ),
            get_job_responded_log(1.into(), 0.into()),
            Log {
                ..Default::default()
            },
        ];

        let (tx, mut rx) = channel::<JobResponse>(10);

        tokio::spawn(async move {
            let jobs_stream = std::pin::pin!(tokio_stream::iter(job_logs.into_iter()).then(
                |log| async move {
                    sleep(Duration::from_millis(user_deadline)).await;
                    log
                }
            ));

            // Call the event handler for the contract logs
            handle_event_logs(
                jobs_stream,
                std::pin::pin!(tokio_stream::empty()),
                app_state,
                tx,
            )
            .await;
        });

        let mut responses: Vec<JobResponse> = vec![];

        // Receive and store the responses
        while let Some(job_response) = rx.recv().await {
            responses.push(job_response);
        }

        assert_eq!(responses.len(), 1);

        assert_response(responses[0].clone(), 0.into(), 2, "");
    }

    #[actix_web::test]
    // Test '3' error code job request and verify the response
    async fn invalid_code_job_test() {
        let app_state = generate_app_state().await;

        let code_hash = "96179f60fd7917c04ad9da6dd64690a1a960f39b50029d07919bf2628f5e7fe5";
        let user_deadline = 5000;

        let code_input_bytes: Bytes = serde_json::to_vec(&json!({})).unwrap().into();

        // Code corresponding to the provided transaction hash has a syntax error
        let job_logs = vec![
            get_job_created_log(
                1.into(),
                0.into(),
                code_hash,
                code_input_bytes,
                user_deadline,
                app_state.enclave_address,
            ),
            get_job_responded_log(1.into(), 0.into()),
            Log {
                ..Default::default()
            },
        ];

        let (tx, mut rx) = channel::<JobResponse>(10);

        tokio::spawn(async move {
            let jobs_stream = std::pin::pin!(tokio_stream::iter(job_logs.into_iter()).then(
                |log| async move {
                    sleep(Duration::from_millis(user_deadline)).await;
                    log
                }
            ));

            // Call the event handler for the contract logs
            handle_event_logs(
                jobs_stream,
                std::pin::pin!(tokio_stream::empty()),
                app_state,
                tx,
            )
            .await;
        });

        let mut responses: Vec<JobResponse> = vec![];

        // Receive and store the responses
        while let Some(job_response) = rx.recv().await {
            responses.push(job_response);
        }

        assert_eq!(responses.len(), 1);

        assert_response(responses[0].clone(), 0.into(), 3, "");
    }

    #[actix_web::test]
    // Test '4' error code job request and verify the response
    async fn deadline_timeout_job_test() {
        let app_state = generate_app_state().await;

        let code_hash = "9c641b535e5586200d0f2fd81f05a39436c0d9dd35530e9fb3ca18352c3ba111";
        let user_deadline = 5000;

        let code_input_bytes: Bytes = serde_json::to_vec(&json!({})).unwrap().into();

        // User code didn't return a response in the expected period
        let job_logs = vec![
            get_job_created_log(
                1.into(),
                0.into(),
                code_hash,
                code_input_bytes,
                user_deadline,
                app_state.enclave_address,
            ),
            get_job_responded_log(1.into(), 0.into()),
            Log {
                ..Default::default()
            },
        ];

        let (tx, mut rx) = channel::<JobResponse>(10);

        tokio::spawn(async move {
            let jobs_stream = std::pin::pin!(tokio_stream::iter(job_logs.into_iter()).then(
                |log| async move {
                    sleep(Duration::from_millis(user_deadline)).await;
                    log
                }
            ));

            // Call the event handler for the contract logs
            handle_event_logs(
                jobs_stream,
                std::pin::pin!(tokio_stream::empty()),
                app_state,
                tx,
            )
            .await;
        });

        let mut responses: Vec<JobResponse> = vec![];

        // Receive and store the responses
        while let Some(job_response) = rx.recv().await {
            responses.push(job_response);
        }

        assert_eq!(responses.len(), 1);

        assert_response(responses[0].clone(), 0.into(), 4, "");
    }

    #[actix_web::test]
    // Test the execution timeout case where enough job responses are not received and slashing transaction should be sent for the job request
    async fn timeout_job_execution_test() {
        let app_state = generate_app_state().await;

        let code_hash = "9c641b535e5586200d0f2fd81f05a39436c0d9dd35530e9fb3ca18352c3ba111";
        let user_deadline = 5000;
        let execution_buffer_time = app_state.execution_buffer_time;

        let code_input_bytes: Bytes = serde_json::to_vec(&json!({})).unwrap().into();

        // Add log entry to relay a job but job response event is not sent and the executor doesn't execute the job request
        let job_logs = vec![
            get_job_created_log(
                1.into(),
                0.into(),
                code_hash,
                code_input_bytes,
                user_deadline,
                H160::random(),
            ),
            Log {
                ..Default::default()
            },
        ];

        let (tx, mut rx) = channel::<JobResponse>(10);

        tokio::spawn(async move {
            let jobs_stream = std::pin::pin!(tokio_stream::iter(job_logs.into_iter()).then(
                |log| async move {
                    sleep(Duration::from_millis(
                        user_deadline + execution_buffer_time * 1000 + 1000,
                    ))
                    .await;
                    log
                }
            ));

            // Call the event handler for the contract logs
            handle_event_logs(
                jobs_stream,
                std::pin::pin!(tokio_stream::empty()),
                app_state,
                tx,
            )
            .await;
        });

        let mut responses: Vec<JobResponse> = vec![];

        // Receive and store the responses
        while let Some(job_response) = rx.recv().await {
            responses.push(job_response);
        }

        assert_eq!(responses.len(), 1);
        let job_response = responses[0].clone();
        assert!(job_response.job_output.is_none());
        assert!(job_response.timeout_response.is_some());
        assert_eq!(job_response.timeout_response.unwrap(), 0.into());
    }

    #[actix_web::test]
    // Test ExecutorDeregistered event handling
    async fn executor_deregistered_test() {
        let app_state = generate_app_state().await;
        app_state.enclave_registered.store(true, Ordering::SeqCst);

        let (tx, mut rx) = channel::<JobResponse>(10);

        // Add log for deregistering the current executor
        let executor_logs = vec![Log {
            address: H160::from_str(EXECUTORS_CONTRACT_ADDR).unwrap(),
            topics: vec![
                keccak256("ExecutorDeregistered(address)").into(),
                H256::from(app_state.enclave_address),
            ],
            removed: Some(false),
            ..Default::default()
        }];

        let app_state_clone = app_state.clone();
        tokio::spawn(async move {
            let executors_stream = std::pin::pin!(
                tokio_stream::iter(executor_logs.into_iter()).chain(tokio_stream::pending())
            );

            handle_event_logs(
                std::pin::pin!(tokio_stream::pending()),
                executors_stream,
                app_state,
                tx,
            )
            .await;
        });

        while rx.recv().await.is_some() {
            assert!(false, "Response received even after deregistration!");
        }

        assert!(
            !app_state_clone.enclave_registered.load(Ordering::SeqCst),
            "Enclave not set to deregistered in the app_state!"
        );
    }

    fn get_job_created_log(
        block_number: U64,
        job_id: U256,
        code_hash: &str,
        code_inputs: Bytes,
        user_deadline: u64,
        enclave: H160,
    ) -> Log {
        Log {
            block_number: Some(block_number),
            address: H160::from_str(JOBS_CONTRACT_ADDR).unwrap(),
            topics: vec![
                keccak256("JobCreated(uint256,address,bytes32,bytes,uint256,address[])").into(),
                H256::from_uint(&job_id),
                H256::from(H160::random()),
            ],
            data: encode(&[
                Token::FixedBytes(hex::decode(code_hash).unwrap()),
                Token::Bytes(code_inputs.into()),
                Token::Uint(user_deadline.into()),
                Token::Array(vec![Token::Address(enclave)]),
            ])
            .into(),
            removed: Some(false),
            ..Default::default()
        }
    }

    fn get_job_responded_log(block_number: U64, job_id: U256) -> Log {
        Log {
            block_number: Some(block_number),
            address: H160::from_str(JOBS_CONTRACT_ADDR).unwrap(),
            topics: vec![
                keccak256("JobResponded(uint256,bytes,uint256,uint8,uint8)").into(),
                H256::from_uint(&job_id),
            ],
            data: encode(&[
                Token::Bytes([].into()),
                Token::Uint(U256::one()),
                Token::Uint((0 as u8).into()),
                Token::Uint((1 as u8).into()),
            ])
            .into(),
            removed: Some(false),
            ..Default::default()
        }
    }

    fn recover_key(
        owner: H160,
        job_capacity: usize,
        sign_timestamp: usize,
        sign: String,
    ) -> VerifyingKey {
        // Regenerate the digest for verification
        let domain_separator = keccak256(encode(&[
            Token::FixedBytes(keccak256("EIP712Domain(string name,string version)").to_vec()),
            Token::FixedBytes(keccak256("marlin.oyster.Executors").to_vec()),
            Token::FixedBytes(keccak256("1").to_vec()),
        ]));
        let register_typehash =
            keccak256("Register(address owner,uint256 jobCapacity,uint256 signTimestamp)");
        let hash_struct = keccak256(encode(&[
            Token::FixedBytes(register_typehash.to_vec()),
            Token::Address(owner),
            Token::Uint(job_capacity.into()),
            Token::Uint(sign_timestamp.into()),
        ]));
        let digest = encode_packed(&[
            Token::String("\x19\x01".to_string()),
            Token::FixedBytes(domain_separator.to_vec()),
            Token::FixedBytes(hash_struct.to_vec()),
        ])
        .unwrap();
        let digest = keccak256(digest);

        let signature =
            Signature::from_slice(hex::decode(&sign[0..128]).unwrap().as_slice()).unwrap();
        let v = RecoveryId::try_from((hex::decode(&sign[128..]).unwrap()[0]) - 27).unwrap();
        let recovered_key = VerifyingKey::recover_from_prehash(&digest, &signature, v).unwrap();

        return recovered_key;
    }

    fn assert_response(job_response: JobResponse, id: U256, error: u8, output: &str) {
        assert!(job_response.job_output.is_some());
        assert!(job_response.timeout_response.is_none());
        let job_output = job_response.job_output.unwrap();

        assert_eq!(job_output.id, id);
        assert_eq!(job_output.execution_response.error_code, error);
        assert_eq!(job_output.execution_response.output, output);
    }
}
