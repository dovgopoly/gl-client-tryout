use anyhow::Result;
use bip39::{Language, Mnemonic};
use gl_client::bitcoin::Network;
use gl_client::credentials::{Device, Nobody};
use gl_client::node::ClnClient;
use gl_client::pb::cln::GetinfoRequest;
use gl_client::scheduler::Scheduler;
use gl_client::signer::Signer;
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use bitcoincore_rpc::{Auth, Client as BitcoinClient, RpcApi};

const NETWORK: Network = Network::Regtest;
const SEED_PATH: &str = "creds/seed";
const CREDS_PATH: &str = "creds/creds";
const NOBODY_CERT_PATH: &str = "creds/client.crt";
const NOBODY_KEY_PATH: &str = "creds/client-key.pem";

fn load_seed_bytes() -> Result<[u8; 32]> {
    let seed_path = Path::new(SEED_PATH);

    if seed_path.exists() {
        let mut seed_file = File::open(seed_path)?;
        let mut seed_bytes = [0u8; 32];
        seed_file.read_exact(&mut seed_bytes)?;

        return Ok(seed_bytes);
    };

    let mut rng = rand::thread_rng();
    let mnemonic = Mnemonic::generate_in_with(&mut rng, Language::English, 24)?;

    const EMPTY_PASSPHRASE: &str = "";
    let seed_bytes: [u8; 32] = mnemonic.to_seed(EMPTY_PASSPHRASE)[..32].try_into()?;

    File::create_new(seed_path)?.write_all(&seed_bytes)?;

    Ok(seed_bytes)
}

async fn load_creds_bytes(seed_bytes: [u8; 32], config: &TestServerMetadata) -> Result<()> {
    let creds_path = Path::new(CREDS_PATH);

    if creds_path.exists() {
        return Ok(());
    }

    let developer_cert = fs::read(&config.nobody_crt_path)?;
    let developer_key = fs::read(&config.nobody_key_path)?;
    let ca_cert = fs::read(&config.ca_crt_path)?;

    let developer_creds = Nobody {
        cert: developer_cert,
        key: developer_key,
        ca: ca_cert,
    };

    let signer = Signer::new(seed_bytes.to_vec(), NETWORK, developer_creds.clone())?;
    let scheduler = Scheduler::with(NETWORK, developer_creds, config.scheduler_grpc_uri.clone()).await?;
    let registration_response = scheduler.register(&signer, None).await?;
    let device_creds = Device::from_bytes(registration_response.creds);

    File::create_new(creds_path)?.write_all(&device_creds.to_bytes())?;

    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct TestServerMetadata {
    scheduler_grpc_uri: String,
    bitcoind_rpc_uri: String,
    cert_path: String,
    ca_crt_path: String,
    nobody_crt_path: String,
    nobody_key_path: String,
}

const GL_TESTSERVER_METADATA_PATH: &str = "greenlight/.gltestserver/metadata.json";

fn load_testserver_config() -> Result<TestServerMetadata> {
    let content = fs::read_to_string(GL_TESTSERVER_METADATA_PATH)?;
    let config: TestServerMetadata = serde_json::from_str(&content)?;
    Ok(config)
}

fn create_bitcoin_client(rpc_uri: &str) -> Result<BitcoinClient> {
    // Parse URI like "http://rpcuser:rpcpass@localhost:18443"
    let url = url::Url::parse(rpc_uri)?;
    let host = format!("{}://{}:{}", url.scheme(), url.host_str().unwrap(), url.port().unwrap());

    println!("Connecting to Bitcoin Core RPC at {}", host);
    println!("RPC user: {}", url.username());
    println!("RPC password: {}", url.password().unwrap_or(""));

    let auth = Auth::UserPass(
        url.username().to_string(),
        url.password().unwrap_or("").to_string()
    );
    BitcoinClient::new(&host, auth).map_err(Into::into)
}

fn load_device_with_ca(config: &TestServerMetadata) -> Result<Device> {
    let device = Device::from_path(CREDS_PATH);
    let ca = fs::read(&config.ca_crt_path)?;
    Ok(device.with_ca(ca))
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = load_testserver_config()?;
    println!("GL testserver config: {:#?}", config);

    let btc = create_bitcoin_client(&config.bitcoind_rpc_uri)?;
    println!("Blockchain info: {:#?}", btc.get_blockchain_info()?);

    let seed_bytes = load_seed_bytes()?;
    load_creds_bytes(seed_bytes, &config).await?;

    // Connect to scheduler
    let device = load_device_with_ca(&config)?;
    let scheduler = Scheduler::with(NETWORK, device.clone(), config.scheduler_grpc_uri.clone()).await?;

    // Start the signer BEFORE scheduling the node - it needs to be ready
    println!("Starting signer...");
    let signer = Signer::new(seed_bytes.to_vec(), NETWORK, device)?;
    let (_shutdown_tx, shutdown_rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        if let Err(e) = signer.run_forever(shutdown_rx).await {
            eprintln!("Signer error: {:?}", e);
        }
    });

    // Give signer a moment to start
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Get node client - signer must be running for this to work
    println!("Scheduling node...");
    let mut node: ClnClient = scheduler.node().await?;

    println!("Node scheduled, getting info...");

    // Test the node
    let node_info = node
        .getinfo(GetinfoRequest::default())
        .await?
        .into_inner();
    println!("Node info: {:#?}", node_info);

    Ok(())
}
