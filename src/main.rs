use anyhow::Result;
use bip39::{Language, Mnemonic};
use gl_client::bitcoin::Network;
use gl_client::credentials::{Device, Nobody};
use gl_client::node::ClnClient;
use gl_client::pb::cln;
use gl_client::pb::cln::newaddr_request::NewaddrAddresstype;
use gl_client::pb::cln::{Amount, AmountOrAny, DisconnectRequest, GetinfoRequest, ListchannelsRequest, ListfundsRequest, NewaddrRequest, amount_or_any, FundchannelRequest, AmountOrAll, amount_or_all};
use gl_client::scheduler::Scheduler;
use gl_client::signer::Signer;
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

const NETWORK: Network = Network::Testnet;
const SEED_PATH: &str = "seed";
const CREDS_PATH: &str = "creds";
const DEVELOPER_CERT_PATH: &str = "gl-certs/client.crt";
const DEVELOPER_KEY_PATH: &str = "gl-certs/client-key.pem";

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

    let developer_cert = fs::read(DEVELOPER_CERT_PATH)?;
    let developer_key = fs::read(DEVELOPER_KEY_PATH)?;
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

    let amount = AmountOrAny {
        value: Some(amount_or_any::Value::Amount(Amount { msat: 10000 })),
    };

    const REMOTE_NODE_ID: &str =
        "038863cf8ab91046230f561cd5b386cbff8309fa02e3f0c3ed161a3aeb64a643b9";
    let remote_node_id_bytes = hex::decode(REMOTE_NODE_ID)?;
    const REMOTE_NODE_HOST: &str = "203.132.94.196";
    const REMOTE_NODE_PORT: u32 = 9735;

    // let _ = node
    //     .connect_peer(cln::ConnectRequest {
    //         id: REMOTE_NODE_ID.to_string(),
    //         host: Some(REMOTE_NODE_HOST.to_string()),
    //         port: Some(REMOTE_NODE_PORT),
    //     })
    //     .await?;

    let peers = node
        .list_peers(cln::ListpeersRequest {
            ..Default::default()
        })
        .await?
        .into_inner()
        .peers;

    println!("peers count: {}", peers.len());
    for peer in peers.iter() {
        println!("peer: {:#?}", peer);
    }

    let node_info = node
        .getinfo(GetinfoRequest {
            ..Default::default()
        })
        .await?
        .into_inner();
    println!("node info: {:#?}", node_info);

    let channels = node
        .list_channels(ListchannelsRequest {
            source: Some(node_info.id.clone()),
            ..Default::default()
        })
        .await?
        .into_inner()
        .channels;
    println!("channels count: {}", channels.len());
    for channel in channels.iter() {
        println!("channel: {:#?}", channel);
    }

    let addr = node
        .new_addr(NewaddrRequest {
            addresstype: Some(NewaddrAddresstype::P2tr as i32),
        })
        .await?
        .into_inner();

    println!("new_addr: {:#?}", addr);

    let funds = node
        .list_funds(ListfundsRequest {
            ..Default::default()
        })
        .await?
        .into_inner();

    println!("funds: {:#?}", funds);

    // let resp = node.fund_channel(FundchannelRequest {
    //     id: remote_node_id_bytes,
    //     amount: Some(AmountOrAll {
    //         value: Some(amount_or_all::Value::Amount(Amount { msat: 1_000_000 })),
    //     }),
    //     ..Default::default()
    // }).await?.into_inner();

    // println!("fund_channel: {:#?}", resp);

    // let invoice = node.invoice(cln::InvoiceRequest {
    //     amount_msat: Some(amount),
    //     description: "description".to_string(),
    //     label: "label5".to_string(),
    //     ..Default::default()
    // })
    //     .await?;

    // .await?;
    // node.disconnect(cln::DisconnectRequest {
    //     id: "03ecef675be448b615e6176424070673ef8284e0fd19d8be062a6cb5b130a0a0d1".t(),
    //     force: Some(true),
    // })
    // .await?;
    //
    // println!(
    //     "channels: {:?}",
    //     node.list_peers(cln::ListpeersRequest {
    //         ..Default::default()
    //     })
    //     .await?
    // );

    Ok(())
}
