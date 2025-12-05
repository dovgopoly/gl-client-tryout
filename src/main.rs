use anyhow::{Context, Result};
use bip39::{Language, Mnemonic};
use bitcoincore_rpc::{Auth, Client as BitcoinClient, RpcApi};
use gl_client::bitcoin::Network;
use gl_client::credentials::{Device, Nobody};
use gl_client::node::ClnClient;
use gl_client::pb::cln::{
    AmountOrAll, ConnectRequest, FundchannelRequest, GetinfoRequest, ListfundsRequest,
    ListpeersRequest, NewaddrRequest,
};
use gl_client::scheduler::Scheduler;
use gl_client::signer::Signer;
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::str::FromStr;
use tokio::sync::mpsc;

const NETWORK: Network = Network::Regtest;

#[derive(Debug, serde::Deserialize)]
struct TestServerMetadata {
    scheduler_grpc_uri: String,
    bitcoind_rpc_uri: String,
    ca_crt_path: String,
    nobody_crt_path: String,
    nobody_key_path: String,
}

const GL_TESTSERVER_METADATA_PATH: &str = "/repo/.gltestserver/metadata.json";
const CREDS_FILE_NAME: &str = "creds";
const SEED_FILE_NAME: &str = "seed";

fn load_testserver_config() -> Result<TestServerMetadata> {
    let content = fs::read_to_string(GL_TESTSERVER_METADATA_PATH)?;
    let mut config: TestServerMetadata = serde_json::from_str(&content)?;
    // Replace localhost with 127.0.0.1 in bitcoind URI to avoid IPv6 resolution issues in Docker
    // But keep localhost for scheduler/grpc URIs because the TLS certificate is valid for localhost
    config.bitcoind_rpc_uri = config.bitcoind_rpc_uri.replace("localhost", "127.0.0.1");
    Ok(config)
}

fn create_bitcoin_client(rpc_uri: &str) -> Result<BitcoinClient> {
    let url = url::Url::parse(rpc_uri)?;
    let host = format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap(),
        url.port().unwrap()
    );
    println!("Connecting to bitcoind at: {}", host);
    let auth = Auth::UserPass(
        url.username().to_string(),
        url.password().unwrap_or("").to_string(),
    );
    BitcoinClient::new(&host, auth).map_err(Into::into)
}

#[allow(dead_code)]
struct GlNode {
    node: ClnClient,
    _shutdown_tx: mpsc::Sender<()>,
}

impl GlNode {
    async fn new(
        creds_dir: &str,
        config: &TestServerMetadata,
        nobody_creds: &Nobody,
        scheduler_uri: String,
    ) -> Result<Self> {
        let seed_path = format!("{}/{}", creds_dir, SEED_FILE_NAME);
        let creds_path = format!("{}/{}", creds_dir, CREDS_FILE_NAME);

        fs::create_dir_all(creds_dir)?;
        let seed = Self::load_or_create_seed(&seed_path)?;

        let ca = fs::read(&config.ca_crt_path)?;

        let device = if Path::new(&creds_path).exists() {
            Device::from_path(&creds_path).with_ca(ca)
        } else {
            let signer = Signer::new(seed.to_vec(), NETWORK, nobody_creds.clone())?;
            let scheduler = Scheduler::with(
                NETWORK,
                nobody_creds.clone(),
                config.scheduler_grpc_uri.clone(),
            )
            .await?;
            let reg = scheduler.register(&signer, None).await?;
            let device = Device::from_bytes(reg.creds).with_ca(ca.clone());
            File::create(&creds_path)?.write_all(&device.to_bytes())?;
            device
        };

        let signer = Signer::new(seed.to_vec(), NETWORK, device.clone())?;
        let (_shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);

        let signer_scheduler_uri = scheduler_uri.clone();
        tokio::spawn(async move {
            if let Err(e) = signer
                .run_forever_with_uri(shutdown_rx, signer_scheduler_uri)
                .await
            {
                eprintln!("Signer error: {:?}", e);
            }
        });

        let scheduler =
            Scheduler::with(NETWORK, device.clone(), config.scheduler_grpc_uri.clone()).await?;
        let node: ClnClient = scheduler.node().await?;

        Ok(Self { node, _shutdown_tx })
    }

    fn load_or_create_seed(path: &str) -> Result<[u8; 32]> {
        if Path::new(path).exists() {
            let mut file = File::open(path)?;
            let mut seed = [0u8; 32];
            file.read_exact(&mut seed)?;
            return Ok(seed);
        }

        let mut rng = rand::thread_rng();
        let mnemonic = Mnemonic::generate_in_with(&mut rng, Language::English, 24)?;
        let seed: [u8; 32] = mnemonic.to_seed("")[..32].try_into()?;

        File::create(path)?.write_all(&seed)?;

        Ok(seed)
    }

    async fn get_info(&mut self) -> Result<gl_client::pb::cln::GetinfoResponse> {
        Ok(self
            .node
            .getinfo(GetinfoRequest::default())
            .await?
            .into_inner())
    }

    async fn new_address(&mut self) -> Result<String> {
        let resp = self
            .node
            .new_addr(NewaddrRequest::default())
            .await?
            .into_inner();
        resp.bech32.context("no bech32 address returned")
    }

    async fn list_funds(&mut self) -> Result<gl_client::pb::cln::ListfundsResponse> {
        Ok(self
            .node
            .list_funds(ListfundsRequest::default())
            .await?
            .into_inner())
    }

    async fn connect_peer(&mut self, node_id: &str, host: &str, port: u32) -> Result<()> {
        self.node
            .connect_peer(ConnectRequest {
                id: format!("{}@{}:{}", node_id, host, port),
                host: None,
                port: None,
            })
            .await?;
        Ok(())
    }

    async fn fund_channel(&mut self, node_id: &[u8], amount_sat: u64) -> Result<()> {
        self.node
            .fund_channel(FundchannelRequest {
                id: node_id.to_vec(),
                amount: Some(AmountOrAll {
                    value: Some(gl_client::pb::cln::amount_or_all::Value::Amount(
                        gl_client::pb::cln::Amount {
                            msat: amount_sat * 1000,
                        },
                    )),
                }),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    async fn list_peers(&mut self) -> Result<gl_client::pb::cln::ListpeersResponse> {
        Ok(self
            .node
            .list_peers(ListpeersRequest::default())
            .await?
            .into_inner())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = load_testserver_config()?;
    println!("Loaded testserver config: {:#?}", config);

    let btc = create_bitcoin_client(&config.bitcoind_rpc_uri)?;
    println!(
        "Connected to bitcoind, block height: {}",
        btc.get_block_count()?
    );

    let nobody_creds = Nobody {
        cert: fs::read(&config.nobody_crt_path)?,
        key: fs::read(&config.nobody_key_path)?,
        ca: fs::read(&config.ca_crt_path)?,
    };

    println!("\n--- Creating Node Alice ---");
    let mut alice = GlNode::new(
        "creds/alice",
        &config,
        &nobody_creds,
        config.scheduler_grpc_uri.clone(),
    )
    .await?;
    let alice_info = alice.get_info().await?;
    println!("Alice node_id: {}", hex::encode(&alice_info.id));
    println!("Alice binding: {:?}", alice_info.binding);

    println!("\n--- Creating Node Bob ---");
    let mut bob = GlNode::new(
        "creds/bob",
        &config,
        &nobody_creds,
        config.scheduler_grpc_uri.clone(),
    )
    .await?;
    let bob_info = bob.get_info().await?;
    println!("Bob node_id: {}", hex::encode(&bob_info.id));
    println!("Bob binding: {:?}", bob_info.binding);
    println!("Bob address: {:?}", bob_info.address);

    println!("\n--- Funding Alice ---");
    let alice_addr = alice.new_address().await?;
    println!("Alice address: {}", alice_addr);

    // Mine blocks to fund Alice
    let alice_btc_addr = bitcoincore_rpc::bitcoin::Address::from_str(&alice_addr)?.assume_checked();
    btc.generate_to_address(101, &alice_btc_addr)?;
    println!("Mined 101 blocks to Alice's address");

    tokio::time::sleep(std::time::Duration::from_secs(30)).await;

    let alice_funds = alice.list_funds().await?;
    let total_sats: u64 = alice_funds
        .outputs
        .iter()
        .map(|o| o.amount_msat.as_ref().map(|a| a.msat / 1000).unwrap_or(0))
        .sum();
    println!("Alice confirmed balance: {} sats", total_sats);

    println!("\n--- Connecting Alice to Bob ---");
    let bob_binding = bob_info.binding.first().context("Bob has no binding")?;
    let bob_p2p_port = bob_binding.port.context("Bob binding has no port")?;
    alice
        .connect_peer(
            &hex::encode(&bob_info.id),
            bob_binding.address.as_deref().unwrap_or("127.0.0.1"),
            bob_p2p_port,
        )
        .await?;
    println!("Alice connected to Bob");

    println!("\n--- Opening Channel: Alice -> Bob (100,000 sats) ---");
    alice.fund_channel(&bob_info.id, 100_000).await?;
    println!("Channel funding initiated");

    btc.generate_to_address(6, &alice_btc_addr)?;
    println!("Mined 6 blocks to confirm channel");

    println!("\n--- Verifying Channel ---");

    let alice_peers = alice.list_peers().await?;
    println!("Alice has {} peer(s)", alice_peers.peers.len());
    for peer in &alice_peers.peers {
        println!(
            "  Peer: {} (connected: {})",
            hex::encode(&peer.id),
            peer.connected
        );
    }

    let alice_funds = alice.list_funds().await?;
    println!("\nAlice channels:");
    for ch in &alice_funds.channels {
        println!(
            "  Channel with {}: our_amount={:?} msat, state={:?}",
            hex::encode(&ch.peer_id),
            ch.our_amount_msat,
            ch.state()
        );
    }

    let bob_peers = bob.list_peers().await?;
    println!("\nBob has {} peer(s)", bob_peers.peers.len());
    for peer in &bob_peers.peers {
        println!(
            "  Peer: {} (connected: {})",
            hex::encode(&peer.id),
            peer.connected
        );
    }

    let bob_funds = bob.list_funds().await?;
    println!("\nBob channels:");
    for ch in &bob_funds.channels {
        println!(
            "  Channel with {}: our_amount={:?} msat, state={:?}",
            hex::encode(&ch.peer_id),
            ch.our_amount_msat,
            ch.state()
        );
    }

    println!("\n=== Test Complete ===");
    Ok(())
}
