//! Integrate-stage live evidence: feeds real, freshly-curled JSON from
//! api.kamino.finance through the crate's own public `kamino::parse_*`
//! functions to confirm the payload shapes this plugin depends on have not
//! drifted. Never run by default (`#[ignore]`) — no network access in the
//! normal `cargo test` gate. Run explicitly:
//!
//! ```sh
//! LIVE_OBLIGATIONS=/path/to/live_obligations.json \
//! LIVE_PRICES=/path/to/live_prices.json \
//! LIVE_RESERVES_METRICS=/path/to/live_reserves_metrics.json \
//! cargo test --locked --test live_evidence -- --ignored
//! ```
//!
//! Adds no new public API surface — reuses the same three parse functions
//! `tests/kamino.rs` already exercises against committed fixtures.

use liquidation_guard::guard::{run, HttpRequest, HttpResponse, Transport};
use liquidation_guard::kamino::{parse_obligations, parse_prices, parse_reserves_metrics};
use std::env;
use std::fs;

const RESERVE_ACCOUNTS_JSON: &str = include_str!("fixtures/reserve_accounts.json");
const WALLET: &str = "AcNSmd5CxwLs21TYUmhWt7CW2v159TdYRkvQxb1iBYRj";
/// The evidence wallet's cbBTC deposit reserve — absent from
/// `reserve_accounts.json` (that fixture was captured for the `rescue`
/// slice's own golden test against a different reserve set). Mirrors
/// `tests/integration.rs::account_data_body`'s synthetic-entry trick.
const CBBTC_RESERVE: &str = "37Jk2zkz23vkAYBT66HM2gaqJuNg2nYLsCreQAVt5MWK";

fn read_env_file(var: &str) -> String {
    let path = env::var(var).unwrap_or_else(|_| panic!("{var} must be set"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn read_env(var: &str) -> String {
    env::var(var).unwrap_or_else(|_| panic!("{var} must be set"))
}

/// Unlike `tests/integration.rs::account_data_body` (which fills the
/// missing cbBTC reserve slot with a copy of an unrelated reserve's blob,
/// fine for pure-encoder assertions but wrong for a live on-chain
/// simulation), this reads the real cbBTC reserve account bytes from
/// `LIVE_CBBTC_RESERVE_DATA` when set — sourced from the pre-capture
/// `docs/runs/liquidation-guard/fixtures/reserve_accounts.json`, which (
/// unlike the trimmed in-tree copy) still has this market's full reserve
/// set. Falls back to the filler trick if unset, so the test still runs
/// without that file.
fn account_data_body() -> String {
    let mut v: serde_json::Value = serde_json::from_str(RESERVE_ACCOUNTS_JSON).unwrap();
    let cbbtc_data = env::var("LIVE_CBBTC_RESERVE_DATA")
        .unwrap_or_else(|_| v[0]["reserves"][0]["data"].as_str().unwrap().to_string());
    v[0]["reserves"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({ "pubkey": CBBTC_RESERVE, "data": cbbtc_data }));
    v.to_string()
}

struct LiveRoute {
    key: &'static str,
    body: String,
}

/// Canned-response transport identical in shape to
/// `tests/integration.rs::MockTransport`, but every GET route body is real
/// data curled live from api.kamino.finance, and the blockhash is the real
/// value curled live from the public mainnet RPC — only the reserve
/// account-data blob (unchanging wire format) and the wallet-balance read
/// (not needed for this evidence) reuse committed fixtures. `date_header`
/// is the real `Date` response header captured alongside the live
/// `/oracles/prices` body, so the pipeline's own staleness check is
/// evaluated honestly against real capture-time data, not a fabricated
/// clock.
struct LiveTransport {
    routes: Vec<LiveRoute>,
    date_header: String,
}

impl Transport for LiveTransport {
    fn fetch(&mut self, req: &HttpRequest) -> Result<HttpResponse, String> {
        let haystack = format!("{} {}", req.url, req.body.as_deref().unwrap_or(""));
        let route = self
            .routes
            .iter()
            .find(|r| haystack.contains(r.key))
            .ok_or_else(|| format!("LiveTransport: no route matches request: {haystack}"))?;
        Ok(HttpResponse {
            status: 200,
            body: route.body.clone(),
            date_header: Some(self.date_header.clone()),
        })
    }
}

/// The shared live-data transport every tx-building evidence test uses:
/// obligations/prices/reserves-metrics bodies and the blockhash response
/// are real, freshly-curled captures supplied via env vars.
fn live_transport() -> LiveTransport {
    LiveTransport {
        routes: vec![
            LiveRoute {
                key: "/obligations",
                body: read_env_file("LIVE_OBLIGATIONS"),
            },
            LiveRoute {
                key: "/oracles/prices",
                body: read_env_file("LIVE_PRICES"),
            },
            LiveRoute {
                key: "/reserves/metrics",
                body: read_env_file("LIVE_RESERVES_METRICS"),
            },
            LiveRoute {
                key: "/reserves/account-data",
                body: account_data_body(),
            },
            LiveRoute {
                key: "getLatestBlockhash",
                body: read_env_file("LIVE_BLOCKHASH_RESPONSE"),
            },
        ],
        date_header: read_env("LIVE_PRICES_DATE"),
    }
}

/// Pulls the `tx (base64): <...>` line out of a successful report.
fn extract_tx_b64(text: &str) -> String {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("tx (base64):"))
        .map(|s| s.trim().to_string())
        .expect("tx (base64): <...> line present")
}

#[test]
#[ignore]
fn live_obligations_parse() {
    let body = read_env_file("LIVE_OBLIGATIONS");
    let obligations = parse_obligations(&body).expect("live obligations payload shape parses");
    assert!(!obligations.is_empty(), "evidence wallet has an obligation");
}

#[test]
#[ignore]
fn live_prices_parse() {
    let body = read_env_file("LIVE_PRICES");
    let prices = parse_prices(&body).expect("live prices payload shape parses");
    assert!(!prices.is_empty(), "live prices response is non-empty");
}

#[test]
#[ignore]
fn live_reserves_metrics_parse() {
    let body = read_env_file("LIVE_RESERVES_METRICS");
    let metrics =
        parse_reserves_metrics(&body).expect("live reserves-metrics payload shape parses");
    assert!(
        !metrics.is_empty(),
        "live reserves-metrics response is non-empty"
    );
}

/// Builds a real unsigned rescue transaction end to end
/// (`kamino_guard {"action":"rescue",...}` via `guard::run`) from live
/// obligations/prices/reserves-metrics curled from api.kamino.finance and a
/// live blockhash curled from the public mainnet RPC. Writes the resulting
/// base64 tx to `LIVE_TX_OUT` so it can be handed to
/// `simulateTransaction` outside the plugin (curl, not plugin code — the
/// plugin itself never gains a `simulateTransaction`/`sendTransaction`
/// call). No new public API surface: this is the same `guard::run` entry
/// point every other integration test drives, with a mock `Transport`
/// whose responses are live data instead of fixtures.
#[test]
#[ignore]
fn live_rescue_tx_builds() {
    let out_path = read_env("LIVE_TX_OUT");
    let mut t = live_transport();

    let args = serde_json::json!({
        "action": "rescue",
        "wallet": WALLET,
        "__config": { "max_repay_ui": "100000" },
    })
    .to_string();

    let out = run(&args, &mut t);
    assert!(out.success, "live rescue build failed: {}", out.text);

    let tx_b64 = extract_tx_b64(&out.text);
    fs::write(&out_path, &tx_b64).unwrap_or_else(|e| panic!("write {out_path}: {e}"));
    eprintln!(
        "wrote live rescue tx ({} bytes b64) to {out_path}",
        tx_b64.len()
    );
    eprintln!("full rescue output:\n{}", out.text);
}

/// v1.1 live evidence: same end-to-end build as `live_rescue_tx_builds`
/// but with `priority_fee_microlamports` configured, so the resulting tx
/// carries the two compute-budget instructions ahead of the klend suffix.
/// Writes the base64 tx to `LIVE_FEE_TX_OUT` for out-of-plugin simulation.
#[test]
#[ignore]
fn live_rescue_fee_tx_builds() {
    let out_path = read_env("LIVE_FEE_TX_OUT");
    let mut t = live_transport();

    let args = serde_json::json!({
        "action": "rescue",
        "wallet": WALLET,
        "__config": {
            "max_repay_ui": "100000",
            "priority_fee_microlamports": "1000",
        },
    })
    .to_string();

    let out = run(&args, &mut t);
    assert!(out.success, "live fee-on rescue build failed: {}", out.text);
    assert!(
        out.text.contains("priority fee: 1000 microlamports/CU"),
        "report must name the configured priority fee: {}",
        out.text
    );

    let tx_b64 = extract_tx_b64(&out.text);
    fs::write(&out_path, &tx_b64).unwrap_or_else(|e| panic!("write {out_path}: {e}"));
    eprintln!(
        "wrote live fee-on rescue tx ({} bytes b64) to {out_path}",
        tx_b64.len()
    );
}

/// v1.1 live evidence: end-to-end `deposit` build from live Kamino data —
/// same pipeline as the rescue evidence, deposit remedy instead. Writes
/// the base64 tx to `LIVE_DEPOSIT_TX_OUT` for out-of-plugin simulation.
#[test]
#[ignore]
fn live_deposit_tx_builds() {
    let out_path = read_env("LIVE_DEPOSIT_TX_OUT");
    let mut t = live_transport();

    let args = serde_json::json!({
        "action": "deposit",
        "wallet": WALLET,
        "__config": {
            "max_repay_ui": "100000",
            "max_deposit_ui": "100000",
        },
    })
    .to_string();

    let out = run(&args, &mut t);
    assert!(out.success, "live deposit build failed: {}", out.text);

    let tx_b64 = extract_tx_b64(&out.text);
    fs::write(&out_path, &tx_b64).unwrap_or_else(|e| panic!("write {out_path}: {e}"));
    eprintln!(
        "wrote live deposit tx ({} bytes b64) to {out_path}",
        tx_b64.len()
    );
}

/// v1.1 live evidence: the durable-nonce read path against a REAL mainnet
/// nonce account (raw `getAccountInfo` response captured live, supplied
/// via `LIVE_NONCE_ACCOUNT_RESPONSE`). The account's stored authority is
/// a stranger, not the evidence wallet — so the whole `guard::run`
/// pipeline must refuse fail-closed at the nonce parse, proving the
/// 80-byte layout parse and the authority gate work against real
/// on-chain bytes, not just synthesized fixtures.
#[test]
#[ignore]
fn live_nonce_foreign_authority_refused() {
    let nonce_account = read_env("LIVE_NONCE_ACCOUNT");
    let nonce_response = read_env_file("LIVE_NONCE_ACCOUNT_RESPONSE");
    let mut t = live_transport();
    t.routes.push(LiveRoute {
        key: "getAccountInfo",
        body: nonce_response,
    });

    let args = serde_json::json!({
        "action": "rescue",
        "wallet": WALLET,
        "__config": {
            "max_repay_ui": "100000",
            "nonce_account": nonce_account,
        },
    })
    .to_string();

    let out = run(&args, &mut t);
    assert!(
        !out.success,
        "foreign-authority nonce account must refuse, got: {}",
        out.text
    );
    assert!(
        out.text.contains("authority"),
        "refusal must name the authority mismatch: {}",
        out.text
    );
    eprintln!("live nonce fail-closed refusal:\n{}", out.text);
}
