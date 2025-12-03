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

const NETWORK: Network = Network::Testnet;
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

async fn load_creds_bytes(seed_bytes: [u8; 32]) -> Result<()> {
    let creds_path = Path::new(CREDS_PATH);

    if creds_path.exists() {
        return Ok(());
    }

    let developer_cert = fs::read(NOBODY_CERT_PATH)?;
    let developer_key = fs::read(NOBODY_KEY_PATH)?;
    let developer_creds = Nobody {
        cert: developer_cert,
        key: developer_key,
        ..Nobody::default()
    };

    let signer = Signer::new(seed_bytes.to_vec(), NETWORK, developer_creds.clone())?;
    let scheduler = Scheduler::new(NETWORK, developer_creds).await?;
    let registration_response = scheduler.register(&signer, None).await?;
    let device_creds = Device::from_bytes(registration_response.creds);

    File::create_new(creds_path)?.write_all(&device_creds.to_bytes())?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let seed_bytes = load_seed_bytes()?;
    load_creds_bytes(seed_bytes).await?;

    let device = Device::from_path(CREDS_PATH);

    let scheduler = Scheduler::new(NETWORK, device.clone()).await?;
    let mut node: ClnClient = scheduler.node().await?;

    let signer = Signer::new(seed_bytes.to_vec(), NETWORK, device)?;

    let (_tx, rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        signer.run_forever(rx).await.unwrap();
    });

    let node_info = node
        .getinfo(GetinfoRequest {
            ..Default::default()
        })
        .await?
        .into_inner();
    println!("node info: {:#?}", node_info);

    Ok(())
}
