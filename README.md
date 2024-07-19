# Oyster-serverless-executor

Oyster Serverless Executor is a cutting-edge, high-performance serverless computing platform designed to securely execute JavaScript (JS) and WebAssembly (WASM) code in a highly controlled environment. It is an integral part of the Oyster-Serverless web3 ecosystem used to run dApps via interaction with smart contracts. Executor node is meant to run inside on-chain verified (Oyster-verification protocol) enclave ensuring that any message signed by it will be treated as truth and smart contracts can execute based on that signed message. The owners provide computation services and manage the lifecycle of multiple executor enclaves, like registration, deregistration, stakes etc. Built using the Rust, Actix Web framework and ethers library - Oyster serverless executor leverages the power and security of AWS Nitro Enclaves, Cloudflare workerd runtime, cgroups to provide unparalleled isolation and protection for the executed code and RPCs to interact with the smart contracts.

## Getting started

<b>Install the following packages : </b>

* build-essential 
* libc++1
* cgroup-tools

`Note : Oyster serverless executor only works on Ubuntu 22.04 and newer versions due to limitations in the workerd dependency.`

<b>cgroups v2 setup</b>
```
sudo ./cgroupv2_setup.sh
```

<b>Signer file setup</b>

A signer secret is required to run the serverless executor applicaton. It'll also be the identity of the executor enclave on chain i.e, the enclave address will be derived from the corresponding public key. The signer must be a `secp256k1` binary secret.
The <a href="https://github.com/marlinprotocol/keygen">Keygen repo</a> can be used to generate this.

<b> RPC and smart contracts configuration</b>

To run the serverless executor, details related to RPC like the HTTP and WebSocket URLs will be needed through which the executor will communicate with the common chain. Also the addresses of the relevant smart contracts deployed there like **Executors**, **Jobs** and **UserCode** will be needed.

## Running serverless executor application

<b>Run the serverless executor application :</b>

```
./target/x86_64-unknown-linux-musl/release/oyster-serverless-executor --help
Usage: oyster-serverless-executor [OPTIONS]

Options:
      --port <PORT>
          [default: 6001]
          Server port
      --workerd-runtime-path <WORKERD_RUNTIME_PATH>
          [default: ./runtime/]
          Runtime path where code and config files will be created and executed (workerd binary should be present here)
      --common-chain-id <COMMON_CHAIN_ID>
          Common chain id
      --http-rpc-url <HTTP_RPC_URL>
          Http url of the RPC endpoint
      --web-socket-url <WEB_SOCKET_URL>
          Websocket url of the RPC endpoint
      --executors-contract-addr <EXECUTORS_CONTRACT_ADDR>
          Executors smart contract address on common chain
      --jobs-contract-addr <JOBS_CONTRACT_ADDR>
          Jobs smart contract address on common chain
      --code-contract-addr <CODE_CONTRACT_ADDR>
          User code calldata smart contract address on common chain
      --enclave-signer-file <ENCLAVE_SIGNER_FILE>
          [default: ./id.sec]
          path to enclave secp256k1 private key file
      --execution-buffer-time <EXECUTION_BUFFER_TIME>
          Execution buffer time as configured on common chain (in seconds)
      --num-selected-executors <NUM_SELECTED_EXECUTORS>
          Number of executors selected at a time to execute a job as configured on common chain

  -h, --help
          Print help
  -V, --version
          Print version
```
```
cargo build --release && sudo ./target/x86_64-unknown-linux-musl/release/oyster-serverless-executor --signer ./path/to/signer
```

Default build target is `x86_64-unknown-linux-musl`. Can be changed in the `.cargo/config.toml` file or in the build command itself.

<b> Inject immutable configuration parameters into the application: </b>

Currently there is only one such parameter and it is the address of the executor enclave owner.
```
curl -X POST -H "Content-Type: application/json" -d '{"owner_address_hex": "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"}' <executor_node_ip:executor_node_port>/immutable-config -vs
* Host <executor_node_ip:executor_node_port> was resolved.
*   Trying <executor_node_ip:executor_node_port>...
* Connected to <executor_node_ip> (<executor_node_ip>) port <executor_node_port>
> POST /immutable-config HTTP/1.1
> Host: <executor_node_ip:executor_node_port>
> User-Agent: curl/8.5.0
> Accept: */*
> Content-Type: application/json
> Content-Length: 65
> 
< HTTP/1.1 200 OK
< content-length: 29
< date: Fri, 19 Jul 2024 08:11:09 GMT
< 
Immutable params configured!
* Connection #0 to host <executor_node_ip> left intact
```

<b> Inject mutable configuration parameters into the application: </b>

Currently there is only one such parameter and it is the gas private key used by the owner to send transactions through the node.
```
curl -X POST -H "Content-Type: application/json" -d '{"gas_key_hex": "0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6"}' <executor_node_ip:executor_node_port>/mutable-config -vs
* Host <executor_node_ip:executor_node_port> was resolved.
*   Trying <executor_node_ip:executor_node_port>...
* Connected to <executor_node_ip> (<executor_node_ip>) port <executor_node_port>
> POST /mutable-config HTTP/1.1
> Host: <executor_node_ip:executor_node_port>
> User-Agent: curl/8.5.0
> Accept: */*
> Content-Type: application/json
> Content-Length: 85
> 
< HTTP/1.1 200 OK
< content-length: 27
< date: Fri, 19 Jul 2024 08:14:38 GMT
< 
Mutable params configured!
* Connection #0 to host <executor_node_ip> left intact
```
**The owner can use the below endpoint to get details about the state of the executor node**:-
```
curl <executor_node_ip:executor_node_port>/executor-details -vs
* Host <executor_node_ip:executor_node_port> was resolved.
*   Trying <executor_node_ip:executor_node_port>...
* Connected to <executor_node_ip> (<executor_node_ip>) port <executor_node_port>
> GET /executor-details HTTP/1.1
> Host: <executor_node_ip:executor_node_port>
> User-Agent: curl/8.5.0
> Accept: */*
> 
< HTTP/1.1 200 OK
< content-length: 184
< content-type: application/json
< date: Fri, 19 Jul 2024 08:21:01 GMT
< 
* Connection #0 to host <executor_node_ip> left intact
{"enclave_address":"0x2e5e17c117efeb0727765988b230c4d95f8e8c9c","gas_address":"0x90f79bf6eb2c4f870365e785982e1f101e93b906","owner_address":"0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"}
```

<b> Exporting registration details from the executor node: </b>

The owner can hit the below endpoint to get the registration details required to register the executor enclave on the common chain **Executors** contract. The endpoint will also start the listening of such event notifications on the common chain inside the enclave node.
```
curl <executor_node_ip:executor_node_port>/signed-registration-message -vs
* Host <executor_node_ip:executor_node_port> was resolved.
*   Trying <executor_node_ip:executor_node_port>...
* Connected to <executor_node_ip> (<executor_node_ip>) port <executor_node_port>
> GET /signed-registration-message HTTP/1.1
> Host: <executor_node_ip:executor_node_port>
> User-Agent: curl/8.5.0
> Accept: */*
> 
< HTTP/1.1 200 OK
< content-length: 245
< content-type: application/json
< date: Fri, 19 Jul 2024 08:27:40 GMT
< 
* Connection #0 to host <executor_node_ip> left intact
{"job_capacity":20,"owner":"0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266","sign_timestamp":1721377660,"signature":"9f7992a1f646963cf62c9ca560c106d1e2aea676f7b7741b3cda13b7f0ca5cd9038ac7892b226fcfafcaf0359950133b41c9e1bc0f5ebc70bcda265ba90e70531b"}
```

**Note:** After the owner will register the executor enclave on the common chain, the node will listen to that event and start the listening of job requests created by the **Jobs** contract on the common chain and execute them acordingly.

## Running the tests

To run the tests, make the following change in file `cgroup.rs`:
```
  -     let child = Command::new("cgexec")
  +     let child = Command::new("sudo")          
  +         .arg("cgexec")
            .arg("-g")
            .arg("memory,cpu:".to_string() + cgroup)
            .args(args)
            .stderr(Stdio::piped())
            .spawn()?;
```
Before running the tests, enable the below flag: 
```
sudo ./cgroupv2_setup.sh
export RUSTFLAGS="--cfg tokio_unstable"
```
The tests need root privileges internally. They should work as long as the shell has sudo cached, a simple `sudo echo` will ensure that.
```
sudo echo && cargo test -- --test-threads 1
```
To run a particular test *test_name* :
```
sudo echo && cargo test 'test name' -- --nocapture &
```