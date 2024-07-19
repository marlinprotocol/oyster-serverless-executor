FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update -y
RUN apt-get install apt-utils -y 
RUN apt-get install net-tools iptables cgroup-tools iproute2 wget -y 
RUN apt-get autoclean && apt-get autoremove
RUN rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Supervisord to manage programs
RUN wget -O supervisord http://public.artifacts.marlin.pro/projects/enclaves/supervisord_master_linux_amd64
RUN chmod +x supervisord

# Transparent proxy component inside the enclave to enable outgoing connections
RUN wget -O ip-to-vsock-transparent http://public.artifacts.marlin.pro/projects/enclaves/ip-to-vsock-transparent_v1.0.0_linux_amd64
RUN chmod +x ip-to-vsock-transparent

# Key generator to generate ecdsa keys
RUN wget -O keygen-secp256k1 http://public.artifacts.marlin.pro/projects/enclaves/keygen-secp256k1_v1.0.0_linux_amd64
RUN chmod +x keygen-secp256k1

# Attestation server inside the enclave that generates attestations
RUN wget -O attestation-server http://public.artifacts.marlin.pro/projects/enclaves/attestation-server_v2.0.0_linux_amd64
RUN chmod +x attestation-server

# Proxy to expose attestation server outside the enclave
RUN wget -O vsock-to-ip http://public.artifacts.marlin.pro/projects/enclaves/vsock-to-ip_v1.0.0_linux_amd64
RUN chmod +x vsock-to-ip

# DNS proxy to provide DNS services inside the enclave
RUN wget -O dnsproxy http://public.artifacts.marlin.pro/projects/enclaves/dnsproxy_v0.46.5_linux_amd64
RUN chmod +x dnsproxy

# cgroups setup script
RUN wget -O cgroupv2_setup.sh https://raw.githubusercontent.com/marlinprotocol/oyster-serverless-executor/master/cgroupv2_setup.sh
RUN chmod +x cgroupv2_setup.sh

# workerd runtime binary
RUN wget -O workerd http://public.artifacts.marlin.pro/projects/enclaves/workerd_v1.20240312.0_linux_amd64
RUN chmod +x workerd

# setup.sh script that will act as entrypoint
COPY setup.sh ./
RUN chmod +x setup.sh

# supervisord config
COPY supervisord.conf /etc/supervisord.conf

# oyster serverless executor inside the enclave that executes web3 jobs
COPY target/x86_64-unknown-linux-musl/release/oyster-serverless-executor ./
RUN chmod +x oyster-serverless-executor

# Entry point
ENTRYPOINT [ "/app/setup.sh" ]