use std::collections::HashSet;

use actix_web::web::Data;
use actix_web::{App, HttpServer};
use anyhow::{anyhow, Context, Result};
use clap::Parser;
use ethers::types::{Address, H160};
use ethers::utils::public_key_to_address;
use k256::ecdsa::SigningKey;
use tokio::fs;

use serverless::cgroups::Cgroups;
use serverless::node_handler::{
    export_signed_registration_message, get_executor_details, index, inject_immutable_config,
    inject_mutable_config,
};
use serverless::utils::AppState;

// EXECUTOR CONFIGURATION PARAMETERS
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    //  TODO: ADD DEFAULT CONFIGURATIONS
    #[clap(long, value_parser, default_value = "6001")]
    port: u16,

    #[clap(long, value_parser, default_value = "./runtime/")]
    workerd_runtime_path: String,

    #[clap(long, value_parser, default_value = "")]
    common_chain_id: u64,

    #[clap(long, value_parser, default_value = "")]
    http_rpc_url: String,

    #[clap(long, value_parser, default_value = "")]
    web_socket_url: String,

    #[clap(long, value_parser, default_value = "")]
    executors_contract_addr: String,

    #[clap(long, value_parser, default_value = "")]
    jobs_contract_addr: String,

    #[clap(long, value_parser, default_value = "")]
    code_contract_addr: String,

    #[clap(long, value_parser, default_value = "/app/id.sec")]
    enclave_signer_file: String,

    #[clap(long, value_parser, default_value = "60")]
    execution_buffer_time: u64, // time in seconds

    #[clap(long, value_parser, default_value = "3")]
    num_selected_executors: u8,
}

#[tokio::main]
// Program to run the executor
async fn main() -> Result<()> {
    let cli = Args::parse();

    // Initialize the 'cgroups' available inside the enclave to execute user code
    let cgroups = Cgroups::new().context("Failed to retrieve cgroups")?;
    if cgroups.free.is_empty() {
        return Err(anyhow!("No cgroups found, make sure you have generated cgroups on your system using the instructions in the readme"));
    }

    // Read the 'secp256k1' private and public key of the enclave instance generated by keygen
    let enclave_signer_key = SigningKey::from_slice(
        fs::read(cli.enclave_signer_file)
            .await
            .context("Failed to read the enclave signer key")?
            .as_slice(),
    )
    .context("Invalid enclave signer key")?;

    let enclave_address = public_key_to_address(&enclave_signer_key.verifying_key());

    // Initialize App data that will be shared across multiple threads and tasks
    let app_data = Data::new(AppState {
        job_capacity: cgroups.free.len(),
        cgroups: cgroups.into(),
        workerd_runtime_path: cli.workerd_runtime_path,
        execution_buffer_time: cli.execution_buffer_time,
        common_chain_id: cli.common_chain_id,
        http_rpc_url: cli.http_rpc_url,
        ws_rpc_url: cli.web_socket_url,
        executors_contract_addr: cli
            .executors_contract_addr
            .parse::<Address>()
            .context("Invalid Executors contract address")?,
        jobs_contract_addr: cli
            .jobs_contract_addr
            .parse::<Address>()
            .context("Invalid Jobs contract address")?,
        code_contract_addr: cli.code_contract_addr,
        num_selected_executors: cli.num_selected_executors,
        enclave_address: enclave_address,
        enclave_signer: enclave_signer_key,
        immutable_params_injected: false.into(),
        mutable_params_injected: false.into(),
        enclave_registered: false.into(),
        events_listener_active: false.into(),
        enclave_owner: H160::zero().into(),
        http_rpc_client: None.into(),
        job_requests_running: HashSet::new().into(),
        last_block_seen: 0.into(),
    });

    // Start actix server to expose the executor outside the enclave
    let server = HttpServer::new(move || {
        App::new()
            .app_data(app_data.clone())
            .service(index)
            .service(inject_immutable_config)
            .service(inject_mutable_config)
            .service(get_executor_details)
            .service(export_signed_registration_message)
    })
    .bind(("0.0.0.0", cli.port))
    .context(format!("could not bind to port {}", cli.port))?
    .run();

    println!("Node server started on port {}", cli.port);

    server.await?;

    Ok(())
}
