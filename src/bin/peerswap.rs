use anyhow::{Context, Result};
use bitcoincore_rpc::{Auth, Client as BitcoinClient, RpcApi};
use std::process::Command;

fn elements_cli(args: &[&str]) -> Result<serde_json::Value> {
    let output = Command::new("docker")
        .args(["exec", "elementsd", "elements-cli", "-chain=liquidregtest", "-rpcuser=user", "-rpcpassword=pass", "-rpcport=7041"])
        .args(args)
        .output()?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(serde_json::from_slice(&output.stdout).unwrap_or(serde_json::Value::Null))
}

fn get_channel_balance(container: &str, scid: &str) -> Result<u64> {
    let funds = cli(container, &["listfunds"])?;
    Ok(funds["channels"]
        .as_array()
        .and_then(|chs| chs.iter().find(|c| c["short_channel_id"].as_str() == Some(scid)))
        .and_then(|c| c["our_amount_msat"].as_u64())
        .unwrap_or(0) / 1000)
}

fn cli(container: &str, args: &[&str]) -> Result<serde_json::Value> {
    let output = Command::new("docker")
        .args(["exec", container, "lightning-cli", "--network=regtest"])
        .args(args)
        .output()?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn newaddr(container: &str) -> Result<bitcoincore_rpc::bitcoin::Address> {
    let resp = cli(container, &["newaddr"])?;
    Ok(resp["bech32"].as_str().context("No bech32")?
        .parse::<bitcoincore_rpc::bitcoin::Address<_>>()?.assume_checked())
}

fn invoice(container: &str, amount_msat: u64) -> Result<String> {
    let label = format!("inv-{}", rand::random::<u64>());
    let resp = cli(container, &["invoice", &amount_msat.to_string(), &label, "test"])?;
    Ok(resp["bolt11"].as_str().context("No bolt11")?.to_string())
}

fn pay(container: &str, bolt11: &str) -> Result<()> {
    cli(container, &["pay", bolt11])?;
    Ok(())
}

fn set_premium_rate(container: &str, asset: &str, ppm_swap_out: u64, ppm_swap_in: u64) -> Result<()> {
    cli(container, &["peerswap-updateglobalpremiumrate", asset, "swap_out", &ppm_swap_out.to_string()])?;
    cli(container, &["peerswap-updateglobalpremiumrate", asset, "swap_in", &ppm_swap_in.to_string()])?;
    Ok(())
}

fn liquid_newaddr() -> Result<String> {
    let output = Command::new("docker")
        .args(["exec", "elementsd", "elements-cli", "-chain=liquidregtest", "-rpcuser=user", "-rpcpassword=pass", "-rpcport=7041", "getnewaddress"])
        .output()?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn liquid_generate(blocks: u64) -> Result<()> {
    let addr = liquid_newaddr()?;
    elements_cli(&["generatetoaddress", &blocks.to_string(), &addr])?;
    Ok(())
}

fn liquid_send(to_addr: &str, amount: f64) -> Result<String> {
    let txid = elements_cli_raw(&["-rpcwallet=peerswap", "sendtoaddress", to_addr, &amount.to_string()])?;
    Ok(txid)
}

fn elements_cli_raw(args: &[&str]) -> Result<String> {
    let output = Command::new("docker")
        .args(["exec", "elementsd", "elements-cli", "-chain=liquidregtest", "-rpcuser=user", "-rpcpassword=pass", "-rpcport=7041"])
        .args(args)
        .output()?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn liquid_claim_genesis() -> Result<()> {
    // Check if we already have funds
    let balance = elements_cli(&["-rpcwallet=peerswap", "getbalance"])?;
    if let Some(bal) = balance["bitcoin"].as_f64() {
        if bal > 1.0 {
            println!("Wallet already has {} L-BTC, skipping genesis claim", bal);
            return Ok(());
        }
    }

    // Find anyone-can-spend output (OP_TRUE = 0x51) using scantxoutset
    let scan = elements_cli(&["scantxoutset", "start", r#"["raw(51)"]"#])?;
    let utxo = scan["unspents"].as_array()
        .and_then(|arr| arr.first())
        .context("No anyone-can-spend UTXO found (already claimed?)")?;

    let txid = utxo["txid"].as_str().context("No txid")?;
    let vout = utxo["vout"].as_u64().context("No vout")?;
    let amount = utxo["amount"].as_f64().context("No amount")?;
    let asset = utxo["asset"].as_str().context("No asset")?;

    let addr = liquid_newaddr()?;
    let fee = 0.0001;
    let send_amount = amount - fee;

    // Create raw transaction with explicit fee output
    let inputs = format!(r#"[{{"txid":"{}","vout":{}}}]"#, txid, vout);
    let outputs = format!(r#"[{{"{}":{}}},{{"fee":{}}}]"#, addr, send_amount, fee);
    let raw_hex = elements_cli_raw(&["createrawtransaction", &inputs, &outputs])?;

    // Blind the transaction (Liquid uses confidential transactions)
    let zero_blinder = r#"["0000000000000000000000000000000000000000000000000000000000000000"]"#;
    let amounts = format!("[{}]", amount);
    let assets = format!(r#"["{}"]"#, asset);
    let blinded_hex = elements_cli_raw(&[
        "rawblindrawtransaction", &raw_hex, zero_blinder, &amounts, &assets, zero_blinder
    ])?;

    // Sign (anyone-can-spend)
    let prevtx = format!(r#"[{{"txid":"{}","vout":{},"scriptPubKey":"51","amount":{}}}]"#, txid, vout, amount);
    let signed = elements_cli(&["-rpcwallet=peerswap", "signrawtransactionwithwallet", &blinded_hex, &prevtx])?;
    let signed_hex = signed["hex"].as_str().context("No signed hex")?;

    // Broadcast and confirm
    elements_cli_raw(&["sendrawtransaction", signed_hex])?;
    liquid_generate(1)?;

    println!("Claimed {} L-BTC from genesis", send_amount);
    Ok(())
}

fn peerswap_lbtc_addr(container: &str) -> Result<String> {
    let resp = cli(container, &["peerswap-lbtc-getaddress"])?;
    resp["address"].as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("No address in response: {:?}", resp))
}

struct SwapResult {
    onchain_fee: i64,
    premium: i64,
    opening_tx_hex: String,
}

fn swap_out(
    btc: &BitcoinClient,
    container: &str,
    scid: &str,
    amount_sat: u64,
    max_premium_ppm: u64,
    mine_to: &bitcoincore_rpc::bitcoin::Address,
) -> Result<SwapResult> {
    let swap = cli(container, &[
        "peerswap-swap-out", scid, &amount_sat.to_string(), "btc", &max_premium_ppm.to_string(),
    ])?;
    let swap_id = swap["id"].as_str().context("No swap id")?;
    for _ in 0..30 {
        btc.generate_to_address(1, mine_to)?;
        std::thread::sleep(std::time::Duration::from_secs(2));
        let status = cli(container, &["peerswap-getswap", swap_id])?;
        let state = status["current"].as_str().unwrap_or("");
        if state == "State_ClaimedPreimage" || state == "State_ClaimedCoop" {
            return Ok(SwapResult {
                onchain_fee: status["data"]["opening_tx_fee"].as_i64().unwrap_or(0),
                premium: status["data"]["swap_out_agreement"]["premium"].as_i64().unwrap_or(0),
                opening_tx_hex: status["data"]["opening_tx_hex"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    anyhow::bail!("Timeout waiting for swap")
}

fn swap_in(
    btc: &BitcoinClient,
    container: &str,
    scid: &str,
    amount_sat: u64,
    max_premium_ppm: u64,
    mine_to: &bitcoincore_rpc::bitcoin::Address,
) -> Result<SwapResult> {
    let swap = cli(container, &[
        "peerswap-swap-in", scid, &amount_sat.to_string(), "btc", &max_premium_ppm.to_string()
    ])?;
    let swap_id = swap["id"].as_str().context("No swap id")?.to_string();
    for _ in 0..30 {
        btc.generate_to_address(1, mine_to)?;
        std::thread::sleep(std::time::Duration::from_secs(2));
        let status = cli(container, &["peerswap-getswap", &swap_id])?;
        let state = status["current"].as_str().unwrap_or("");
        if state == "State_ClaimedPreimage" || state == "State_ClaimedCoop" {
            return Ok(SwapResult {
                onchain_fee: status["data"]["opening_tx_fee"].as_i64().unwrap_or(0),
                premium: status["data"]["swap_in_agreement"]["premium"].as_i64().unwrap_or(0),
                opening_tx_hex: status["data"]["opening_tx_hex"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    anyhow::bail!("Timeout waiting for swap")
}

fn decode_tx_output(tx_hex: &str, vout: usize) -> Result<u64> {
    let tx: bitcoincore_rpc::bitcoin::Transaction = bitcoincore_rpc::bitcoin::consensus::deserialize(
        &hex::decode(tx_hex)?
    )?;
    Ok(tx.output.get(vout).context("No output")?.value.to_sat())
}

fn swap_out_lbtc(
    container: &str,
    scid: &str,
    amount_sat: u64,
    max_premium_ppm: u64,
) -> Result<SwapResult> {
    let swap = cli(container, &[
        "peerswap-swap-out", scid, &amount_sat.to_string(), "lbtc", &max_premium_ppm.to_string(),
    ])?;
    let swap_id = swap["id"].as_str().context("No swap id")?;
    for _ in 0..30 {
        liquid_generate(1)?;
        std::thread::sleep(std::time::Duration::from_secs(2));
        let status = cli(container, &["peerswap-getswap", swap_id])?;
        let state = status["current"].as_str().unwrap_or("");
        if state == "State_ClaimedPreimage" || state == "State_ClaimedCoop" {
            return Ok(SwapResult {
                onchain_fee: status["data"]["opening_tx_fee"].as_i64().unwrap_or(0),
                premium: status["data"]["swap_out_agreement"]["premium"].as_i64().unwrap_or(0),
                opening_tx_hex: status["data"]["opening_tx_hex"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    anyhow::bail!("Timeout waiting for swap")
}

fn swap_in_lbtc(
    container: &str,
    scid: &str,
    amount_sat: u64,
    max_premium_ppm: u64,
) -> Result<SwapResult> {
    let swap = cli(container, &[
        "peerswap-swap-in", scid, &amount_sat.to_string(), "lbtc", &max_premium_ppm.to_string()
    ])?;
    let swap_id = swap["id"].as_str().context("No swap id")?.to_string();
    for _ in 0..30 {
        liquid_generate(1)?;
        std::thread::sleep(std::time::Duration::from_secs(2));
        let status = cli(container, &["peerswap-getswap", &swap_id])?;
        let state = status["current"].as_str().unwrap_or("");
        if state == "State_ClaimedPreimage" || state == "State_ClaimedCoop" {
            return Ok(SwapResult {
                onchain_fee: status["data"]["opening_tx_fee"].as_i64().unwrap_or(0),
                premium: status["data"]["swap_in_agreement"]["premium"].as_i64().unwrap_or(0),
                opening_tx_hex: status["data"]["opening_tx_hex"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    anyhow::bail!("Timeout waiting for swap")
}

fn open_channel(
    btc: &BitcoinClient,
    from: &str,
    to_id: &str,
    amount_sat: &str,
    mine_to: &bitcoincore_rpc::bitcoin::Address,
) -> Result<String> {
    let funding_txid = cli(from, &["fundchannel", to_id, amount_sat])?["txid"]
        .as_str()
        .context("No txid")?
        .to_string();
    btc.generate_to_address(6, mine_to)?;
    for _ in 0..60 {
        if let Some(ch) = cli(from, &["listfunds"])?["channels"]
            .as_array()
            .and_then(|chs| chs.iter().find(|c| c["funding_txid"].as_str() == Some(&funding_txid)))
        {
            if ch["state"].as_str() == Some("CHANNELD_NORMAL") {
                return Ok(ch["short_channel_id"].as_str().context("No scid")?.into());
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    anyhow::bail!("Timeout waiting for channel")
}

#[tokio::main]
async fn main() -> Result<()> {
    let btc = BitcoinClient::new(
        "http://127.0.0.1:18443",
        Auth::UserPass("user".into(), "pass".into()),
    )?;
    println!("Block height: {}", btc.get_block_count()?);

    let bob_id = cli("bob", &["getinfo"])?["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Commenting out premium rates to use defaults (swap_out=2000, swap_in=0)
    // set_premium_rate("alice", "btc", 6100, 7100)?;
    // set_premium_rate("bob", "btc", 4100, 5100)?;

    // Fund Alice
    let alice_addr = newaddr("alice")?;
    btc.generate_to_address(101, &alice_addr)?;
    std::thread::sleep(std::time::Duration::from_secs(5));

    // Connect and open channel
    cli("alice", &["connect", &format!("{}@bob:9735", bob_id)])?;
    let scid = open_channel(&btc, "alice", &bob_id, "500000", &alice_addr)?;
    println!("Channel active: {}", scid);

    // Fund Bob (needs on-chain for swap)
    btc.generate_to_address(101, &newaddr("bob")?)?;
    std::thread::sleep(std::time::Duration::from_secs(5));

    // Pay Bob to give him channel balance
    pay("alice", &invoice("bob", 200_000_000)?)?;
    println!("Paid 200k sats to Bob");

    // Swap-out: Alice gets on-chain BTC, Bob gets lightning
    let alice_before = get_channel_balance("alice", &scid)?;
    let bob_before = get_channel_balance("bob", &scid)?;
    println!("Before: Alice={} Bob={}", alice_before, bob_before);

    let result = swap_out(&btc, "alice", &scid, 100_000, 10_000, &alice_addr)?;
    println!("Swap completed! onchain_fee={} premium={}", result.onchain_fee, result.premium);
    let onchain_sent = decode_tx_output(&result.opening_tx_hex, 0)?;
    println!("On-chain sent: {} (amount={} + premium={})", onchain_sent, 100_000, result.premium);

    let alice_after = get_channel_balance("alice", &scid)?;
    let bob_after = get_channel_balance("bob", &scid)?;
    println!("After:  Alice={} Bob={}", alice_after, bob_after);
    println!("Delta:  Alice={:+} Bob={:+}",
        alice_after as i64 - alice_before as i64,
        bob_after as i64 - bob_before as i64);

    // Swap-in: Alice gets lightning, Bob gets on-chain BTC
    let alice_before = get_channel_balance("alice", &scid)?;
    let bob_before = get_channel_balance("bob", &scid)?;
    println!("Before: Alice={} Bob={}", alice_before, bob_before);

    let result = swap_in(&btc, "alice", &scid, 100_000, 10_000, &alice_addr)?;
    println!("Swap completed! onchain_fee={} premium={}", result.onchain_fee, result.premium);

    // Verify on-chain amount = swap amount + premium
    let onchain_sent = decode_tx_output(&result.opening_tx_hex, 0)?;
    println!("On-chain sent: {} (amount={} + premium={})", onchain_sent, 100_000, result.premium);
    assert_eq!(onchain_sent, 100_000 + result.premium as u64, "On-chain amount mismatch");

    let alice_after = get_channel_balance("alice", &scid)?;
    let bob_after = get_channel_balance("bob", &scid)?;
    println!("After:  Alice={} Bob={}", alice_after, bob_after);
    println!("Delta:  Alice={:+} Bob={:+}",
             alice_after as i64 - alice_before as i64,
             bob_after as i64 - bob_before as i64);

    // === Liquid Swaps ===
    println!("\n=== Liquid Swaps ===");

    // Initialize Liquid
    let _ = elements_cli(&["createwallet", "peerswap"]); // ignore if exists
    let _ = elements_cli(&["loadwallet", "peerswap"]);   // load if not loaded
    liquid_claim_genesis()?;

    // Fund Alice and Bob with L-BTC
    let alice_lbtc_addr = peerswap_lbtc_addr("alice")?;
    let bob_lbtc_addr = peerswap_lbtc_addr("bob")?;
    liquid_send(&alice_lbtc_addr, 1.0)?;
    liquid_send(&bob_lbtc_addr, 1.0)?;
    liquid_generate(1)?;
    std::thread::sleep(std::time::Duration::from_secs(5));
    println!("Funded Alice and Bob with L-BTC");

    // L-BTC Swap-out: Alice gets L-BTC, Bob gets lightning
    let alice_before = get_channel_balance("alice", &scid)?;
    let bob_before = get_channel_balance("bob", &scid)?;
    println!("Before: Alice={} Bob={}", alice_before, bob_before);

    let result = swap_out_lbtc("alice", &scid, 100_000, 10_000)?;
    println!("L-BTC Swap-out completed! premium={}", result.premium);
    std::thread::sleep(std::time::Duration::from_secs(2));

    let alice_after = get_channel_balance("alice", &scid)?;
    let bob_after = get_channel_balance("bob", &scid)?;
    println!("After:  Alice={} Bob={}", alice_after, bob_after);
    println!("Delta:  Alice={:+} Bob={:+}",
        alice_after as i64 - alice_before as i64,
        bob_after as i64 - bob_before as i64);

    // L-BTC Swap-in: Alice gets lightning, Bob gets L-BTC
    let alice_before = get_channel_balance("alice", &scid)?;
    let bob_before = get_channel_balance("bob", &scid)?;
    println!("Before: Alice={} Bob={}", alice_before, bob_before);

    let result = swap_in_lbtc("alice", &scid, 100_000, 10_000)?;
    println!("L-BTC Swap-in completed! premium={}", result.premium);
    std::thread::sleep(std::time::Duration::from_secs(2));

    let alice_after = get_channel_balance("alice", &scid)?;
    let bob_after = get_channel_balance("bob", &scid)?;
    println!("After:  Alice={} Bob={}", alice_after, bob_after);
    println!("Delta:  Alice={:+} Bob={:+}",
             alice_after as i64 - alice_before as i64,
             bob_after as i64 - bob_before as i64);

    Ok(())
}
