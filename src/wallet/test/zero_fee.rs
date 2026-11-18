use super::*;

fn compose(args: &[&str]) {
    let (ok, out) = cli(vec![s!("-f"), s!("tests/compose.yaml")], args);
    assert!(ok, "{out}");
}

fn esplora_cli() -> Vec<String> {
    ["-f", "tests/compose.yaml", "exec", "-T", "esplora", "cli"]
        .map(String::from)
        .to_vec()
}

fn cli(base: Vec<String>, args: &[&str]) -> (bool, String) {
    let output = Command::new("docker")
        .stdin(Stdio::null())
        .arg("compose")
        .args(&base)
        .args(args)
        .output()
        .unwrap();
    let out = if output.status.success() {
        output.stdout
    } else {
        output.stderr
    };
    (
        output.status.success(),
        String::from_utf8(out).unwrap().trim().to_string(),
    )
}

fn miner_cli(args: &[&str]) -> String {
    let mut base = bitcoin_cli();
    base.push(s!("-rpcwallet=miner"));
    let (ok, out) = cli(base, args);
    assert!(ok, "{out}");
    out
}

fn wait_short_grace_proxy() {
    assert!(wait_for_function(
        || check_proxy_url(PROXY_URL_SHORT_GRACE).is_ok(),
        60,
        500
    ));
}

struct ProxyStopped;

impl ProxyStopped {
    fn new() -> Self {
        compose(&["stop", "proxy-short-grace"]);
        Self
    }
}

impl Drop for ProxyStopped {
    fn drop(&mut self) {
        compose(&["start", "proxy-short-grace"]);
        wait_short_grace_proxy();
    }
}

fn sleep_until(timestamp: i64) {
    let sleep_secs = std::cmp::max(timestamp - now().unix_timestamp(), 0) as u64;
    std::thread::sleep(Duration::from_secs(sleep_secs));
}

/// Mine and wait until every colorable UTXO of the party is confirmed.
fn ready(p: &mut SinglesigParty) {
    mine(false);
    assert!(wait_for_function(
        || p.get_colorable_unspents_with_sync(false)
            .iter()
            .all(|u| get_tx_height(&u.utxo.outpoint.txid).is_some_and(|c| c > 0)),
        60,
        500
    ));
    p.list_unspents_with_sync(false);
}

fn zero_fee_send(
    p: &mut SinglesigParty,
    recipient_map: HashMap<String, Vec<Recipient>>,
    endpoint: &str,
    expiration: u64,
    min_confirmations: u8,
) -> (Psbt, BdkTransaction, OperationResult) {
    let begin = p
        .wallet
        .send_begin(
            p.online,
            recipient_map,
            false,
            0,
            min_confirmations,
            expiration,
            Some(endpoint.to_string()),
            false,
        )
        .unwrap();
    let signed = p.wallet.sign_psbt(begin.psbt.clone(), None).unwrap();
    let tx = Psbt::from_str(&signed)
        .unwrap()
        .extract_tx_unchecked_fee_rate();
    let res = p.wallet.send_end(p.online, signed).unwrap();
    (Psbt::from_str(&begin.psbt).unwrap(), tx, res)
}

fn assert_zero_fee_shape(psbt: &Psbt, tx: &BdkTransaction) {
    assert_eq!(psbt.unsigned_tx.compute_txid(), tx.compute_txid());
    assert_eq!(tx.version.0, 3);
    assert_eq!(psbt.fee().unwrap(), BdkAmount::ZERO);
    assert!(tx.output[0].script_pubkey.is_op_return());
    let anchors: Vec<usize> = tx
        .output
        .iter()
        .enumerate()
        .filter(|(_, o)| o.script_pubkey == ScriptBuf::new_p2a())
        .map(|(i, _)| i)
        .collect();
    assert_eq!(anchors, vec![tx.output.len() - 1]);
    for input in &tx.input {
        let prev = input.previous_output.txid.to_string();
        assert!(get_tx_height(&prev).is_some_and(|c| c > 0));
    }
}

/// Refresh until every party has the transfer in `WaitingConfirmations`, with no refresh failure.
fn wait_recipients(ps: &mut [&mut SinglesigParty], ids: &[&str]) {
    assert!(wait_for_function(
        || {
            for p in ps.iter_mut() {
                let res = p.refresh_result(None, &[]).unwrap();
                assert!(res.values().all(|r| r.failure.is_none()), "{res:?}");
            }
            ps.iter().zip(ids).all(|(p, id)| {
                p.check_test_transfer_status_recipient(id, TransferStatus::WaitingConfirmations)
            })
        },
        120,
        1000
    ));
}

/// Refresh every party until the sender and all recipients have the given status.
fn wait_all(
    sender: &mut SinglesigParty,
    txid: &str,
    ps: &mut [&mut SinglesigParty],
    ids: &[&str],
    status: TransferStatus,
    secs: u8,
) {
    assert!(wait_for_function(
        || {
            let _ = sender.refresh_result(None, &[]);
            for p in ps.iter_mut() {
                let _ = p.refresh_result(None, &[]);
            }
            sender.check_test_transfer_status_sender(txid, status)
                && ps
                    .iter()
                    .zip(ids)
                    .all(|(p, id)| p.check_test_transfer_status_recipient(id, status))
        },
        secs,
        1000
    ));
}

fn received_parent(p: &SinglesigParty, id: &str) -> BdkTransaction {
    let hex = p
        .wallet
        .get_received_signed_tx(id.to_string())
        .unwrap()
        .unwrap();
    deserialize_hex(&hex).unwrap()
}

struct ChildCoin {
    sk: SecretKey,
    pk: CompressedPublicKey,
    outpoint: BdkOutPoint,
    txout: TxOut,
}

/// A confirmed P2WPKH coin that pays the child TX.
fn child_coin() -> ChildCoin {
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(&rand::random::<[u8; 32]>()).unwrap();
    let pk = CompressedPublicKey::from_private_key(
        &secp,
        &bdk_wallet::bitcoin::PrivateKey::new(sk, BdkNetwork::Regtest),
    )
    .unwrap();
    let addr = BdkAddress::p2wpkh(&pk, BdkNetwork::Regtest);
    let txid = miner_cli(&["sendtoaddress", &addr.to_string(), "0.001"]);
    mine_tx(&txid);
    let res: Value = serde_json::from_str(&miner_cli(&["gettransaction", &txid])).unwrap();
    let tx = deserialize_hex::<BdkTransaction>(res["hex"].as_str().unwrap()).unwrap();
    let (vout, txout) = tx
        .output
        .iter()
        .enumerate()
        .find(|(_, o)| o.script_pubkey == addr.script_pubkey())
        .unwrap();
    ChildCoin {
        sk,
        pk,
        outpoint: BdkOutPoint::new(tx.compute_txid(), vout as u32),
        txout: txout.clone(),
    }
}

/// Submit the parent with a child that spends its P2A output and pays the fee.
fn submit_package(parent: &BdkTransaction, coin: &ChildCoin) -> String {
    let anchor = parent.output.len() - 1;
    let mut child = BdkTransaction {
        version: Version(3),
        lock_time: LockTime::ZERO,
        input: vec![
            TxIn {
                previous_output: BdkOutPoint::new(parent.compute_txid(), anchor as u32),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                ..Default::default()
            },
            TxIn {
                previous_output: coin.outpoint,
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                ..Default::default()
            },
        ],
        output: vec![TxOut {
            value: coin.txout.value + parent.output[anchor].value - BdkAmount::from_sat(5_000),
            script_pubkey: coin.txout.script_pubkey.clone(),
        }],
    };
    let sighash = SighashCache::new(&child)
        .p2wpkh_signature_hash(
            1,
            &coin.txout.script_pubkey,
            coin.txout.value,
            EcdsaSighashType::All,
        )
        .unwrap();
    let signature = EcdsaSignature {
        signature: Secp256k1::new().sign_ecdsa(&SecpMessage::from(sighash), &coin.sk),
        sighash_type: EcdsaSighashType::All,
    };
    child.input[1].witness = BdkWitness::p2wpkh(&signature, &coin.pk.0);
    let package = format!(
        "[\"{}\",\"{}\"]",
        serialize_hex(parent),
        serialize_hex(&child)
    );
    let (ok, out) = cli(bitcoin_cli(), &["submitpackage", &package]);
    assert!(ok, "{out}");
    let res: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(res["package_msg"], "success", "{out}");
    child.compute_txid().to_string()
}

struct Released {
    sender: SinglesigParty,
    rcv: SinglesigParty,
    payer: SinglesigParty,
    asset_id: String,
    rcv_id: String,
    payer_id: String,
    txid: String,
    idx: i32,
    parent: BdkTransaction,
    colored_before: u64,
}

/// A zero-fee send to two blind recipients, nobody has refreshed yet.
fn begin_zero_fee() -> (Released, ReceiveData, ReceiveData) {
    let mut sender = get_funded_party!();
    let asset_id = sender.issue_asset_nia(Some(&[AMOUNT])).asset_id;
    let mut rcv = get_funded_party!();
    let mut payer = get_funded_party!();
    ready(&mut sender);
    let colored_before = sender.get_btc_balance_with_sync().colored.settled;
    let rcv_data = rcv.blind_receive();
    let payer_data = payer.blind_receive();
    let recipient_map = HashMap::from([(
        asset_id.clone(),
        vec![
            blind_recipient(&rcv_data, Assignment::Fungible(AMOUNT_SMALL)),
            blind_recipient(&payer_data, Assignment::Fungible(AMOUNT_SMALL)),
        ],
    )]);
    let (_, parent, res) = zero_fee_send(
        &mut sender,
        recipient_map,
        &PROXY_ENDPOINT,
        default_send_expiration(),
        MIN_CONFIRMATIONS,
    );
    let r = Released {
        sender,
        rcv,
        payer,
        asset_id,
        rcv_id: rcv_data.recipient_id.clone(),
        payer_id: payer_data.recipient_id.clone(),
        txid: res.txid,
        idx: res.batch_transfer_idx,
        parent,
        colored_before,
    };
    (r, rcv_data, payer_data)
}

/// A zero-fee send to two blind recipients, released to both.
fn release_to_payer() -> Released {
    let (mut r, rcv_data, payer_data) = begin_zero_fee();
    wait_recipients(
        &mut [&mut r.rcv, &mut r.payer],
        &[&rcv_data.recipient_id, &payer_data.recipient_id],
    );
    r.parent = received_parent(&r.payer, &payer_data.recipient_id);
    assert_eq!(r.parent.compute_txid().to_string(), r.txid);
    r
}

fn assert_balance(p: &SinglesigParty, asset_id: &str, amount: u64) {
    let balance = p.get_asset_balance(asset_id);
    assert_eq!(
        (balance.settled, balance.future, balance.spendable),
        (amount, amount, amount)
    );
}

fn assert_settled(r: &mut Released) {
    wait_all(
        &mut r.sender,
        &r.txid,
        &mut [&mut r.rcv, &mut r.payer],
        &[&r.rcv_id, &r.payer_id],
        TransferStatus::Settled,
        120,
    );
    assert_balance(&r.sender, &r.asset_id, AMOUNT - 2 * AMOUNT_SMALL);
    assert_balance(&r.rcv, &r.asset_id, AMOUNT_SMALL);
    assert_balance(&r.payer, &r.asset_id, AMOUNT_SMALL);
    // the fee is 0, so the input comes back whole as change
    assert_eq!(
        r.sender.get_btc_balance_with_sync().colored.settled,
        r.colored_before
    );
    assert!(r.sender.db_reserved_txos().is_empty());
}

fn log_count(p: &SinglesigParty, line: &str) -> usize {
    fs::read_to_string(p.wallet.get_wallet_dir().join(LOG_FILE))
        .unwrap()
        .matches(line)
        .count()
}

#[test]
#[parallel]
fn zero_fee_parent_shape() {
    initialize();

    for p2wpkh in [false, true] {
        let mut sender = if p2wpkh {
            get_funded_party_p2wpkh()
        } else {
            get_funded_party!()
        };
        let asset_id = sender.issue_asset_nia(Some(&[AMOUNT])).asset_id;
        let mut rcv = get_funded_party!();
        let mut wrcv = get_funded_party!();
        let mut payer = get_funded_party!();
        ready(&mut sender);
        let rcv_data = rcv.blind_receive();
        let wrcv_data = wrcv.witness_receive();
        let payer_data = payer.blind_receive();
        let recipient_map = HashMap::from([(
            asset_id.clone(),
            vec![
                blind_recipient(&rcv_data, Assignment::Fungible(AMOUNT_SMALL)),
                witness_recipient(&wrcv_data, Assignment::Fungible(AMOUNT_SMALL), 500),
                blind_recipient(&payer_data, Assignment::Fungible(AMOUNT_SMALL)),
            ],
        )]);
        let (psbt, tx, res) = zero_fee_send(
            &mut sender,
            recipient_map,
            &PROXY_ENDPOINT,
            default_send_expiration(),
            MIN_CONFIRMATIONS,
        );
        assert_zero_fee_shape(&psbt, &tx);
        let ids = [
            rcv_data.recipient_id.as_str(),
            wrcv_data.recipient_id.as_str(),
            payer_data.recipient_id.as_str(),
        ];
        wait_recipients(&mut [&mut rcv, &mut wrcv, &mut payer], &ids);
        for (p, id) in [(&rcv, ids[0]), (&wrcv, ids[1]), (&payer, ids[2])] {
            let transfers = p.list_transfers(Some(&asset_id));
            let transfer = transfers
                .iter()
                .find(|t| t.recipient_id.as_deref() == Some(id))
                .unwrap();
            assert_eq!(transfer.txid, Some(res.txid.clone()));
        }
        submit_package(&received_parent(&payer, ids[2]), &child_coin());
        mine(false);
        wait_all(
            &mut sender,
            &res.txid,
            &mut [&mut rcv, &mut wrcv, &mut payer],
            &ids,
            TransferStatus::Settled,
            120,
        );
    }
}

#[test]
#[parallel]
fn zero_fee_dust_change_to_anchor() {
    initialize();

    let mut sender = get_funded_party!();
    let asset = sender.issue_asset_nia(Some(&[AMOUNT]));
    let mut wrcv = get_funded_party!();
    ready(&mut sender);
    let wrcv_data = wrcv.witness_receive();
    let recipient_map = HashMap::from([(
        asset.asset_id.clone(),
        vec![witness_recipient(
            &wrcv_data,
            Assignment::Fungible(AMOUNT_SMALL),
            800,
        )],
    )]);
    let begin = sender
        .wallet
        .send_begin(
            sender.online,
            recipient_map,
            false,
            0,
            MIN_CONFIRMATIONS,
            default_send_expiration(),
            Some(PROXY_ENDPOINT.clone()),
            true,
        )
        .unwrap();
    let psbt = Psbt::from_str(&begin.psbt).unwrap();
    let tx = &psbt.unsigned_tx;
    assert_eq!(tx.version.0, 3);
    let asset_outpoint = sender
        .list_unspents(false)
        .into_iter()
        .find(|u| {
            u.rgb_allocations
                .iter()
                .any(|a| a.asset_id.as_ref() == Some(&asset.asset_id))
        })
        .unwrap()
        .utxo
        .outpoint;
    assert_eq!(tx.input.len(), 1);
    assert_eq!(
        tx.input[0].previous_output.to_string(),
        asset_outpoint.to_string()
    );
    assert_eq!(tx.output.len(), 3);
    assert!(tx.output[0].script_pubkey.is_op_return());
    assert_eq!(tx.output[0].value, BdkAmount::ZERO);
    assert_eq!(tx.output[1].value, BdkAmount::from_sat(800));
    assert_eq!(tx.output[2].script_pubkey, ScriptBuf::new_p2a());
    assert_eq!(tx.output[2].value, BdkAmount::from_sat(200));
    assert_eq!(psbt.fee().unwrap(), BdkAmount::ZERO);
}

/// Begin a zero-fee send to two blind recipients and have only the first one refresh.
fn send_unreleased() -> Released {
    let (mut r, rcv_data, _) = begin_zero_fee();
    let res_rcv = r.rcv.refresh_result(None, &[]).unwrap();
    assert!(res_rcv.values().all(|r| r.failure.is_none()));
    assert!(r.rcv.check_test_transfer_status_recipient(
        &rcv_data.recipient_id,
        TransferStatus::WaitingBroadcast
    ));
    r
}

fn assert_unreleased_and_free(r: &Released) {
    assert_eq!(
        r.rcv
            .wallet
            .get_received_signed_tx(r.rcv_id.clone())
            .unwrap(),
        None
    );
    assert_eq!(
        r.payer
            .wallet
            .get_received_signed_tx(r.payer_id.clone())
            .unwrap(),
        None
    );
    assert_eq!(get_tx_height(&r.txid), None);
    assert!(r.sender.db_reserved_txos().is_empty());
    assert_eq!(r.sender.get_asset_balance(&r.asset_id).spendable, AMOUNT);
}

#[test]
#[parallel]
fn zero_fee_cancel_before_release() {
    initialize();

    let mut r = send_unreleased();
    assert!(r.sender.fail_transfers_single(r.idx));
    assert!(
        r.sender
            .check_test_transfer_status_sender(&r.txid, TransferStatus::Failed)
    );
    assert_unreleased_and_free(&r);
    let _ = r.payer.refresh_result(None, &[]);
    assert_unreleased_and_free(&r);
}

#[test]
#[parallel]
fn zero_fee_nack_frees_inputs() {
    initialize();

    let mut r = send_unreleased();
    MOCK_MIN_RETENTION_GRACE.replace(Some(u32::MAX));
    let _ = r.payer.refresh_result(None, &[]);
    MOCK_MIN_RETENTION_GRACE.replace(None);
    assert!(
        r.payer
            .check_test_transfer_status_recipient(&r.payer_id, TransferStatus::Failed)
    );
    let _ = r.rcv.refresh_result(None, &[]);
    assert_eq!(
        r.rcv
            .wallet
            .get_received_signed_tx(r.rcv_id.clone())
            .unwrap(),
        None
    );
    assert!(wait_for_function(
        || {
            let _ = r.sender.refresh_result(None, &[]);
            r.sender
                .check_test_transfer_status_sender(&r.txid, TransferStatus::Failed)
        },
        60,
        1000
    ));
    assert_unreleased_and_free(&r);
}

#[test]
#[parallel]
fn zero_fee_receiver_holds_parent() {
    initialize();

    let mut sender = get_funded_party!();
    let asset_id = sender.issue_asset_nia(Some(&[AMOUNT])).asset_id;
    let mut rcv = get_funded_party!();
    ready(&mut sender);
    let rcv_data = rcv.blind_receive();
    let recipient_map =
        blind_recipient_map(&asset_id, &rcv_data, Assignment::Fungible(AMOUNT_SMALL));
    let (_, _, res) = zero_fee_send(
        &mut sender,
        recipient_map,
        &PROXY_ENDPOINT,
        default_send_expiration(),
        MIN_CONFIRMATIONS,
    );
    let id = rcv_data.recipient_id.as_str();
    wait_recipients(&mut [&mut rcv], &[id]);
    assert_eq!(
        log_count(&rcv, "Not broadcasting the zero-fee signed TX\n"),
        1
    );
    for _ in 0..3 {
        let res = rcv.refresh_result(None, &[]).unwrap();
        assert!(res.values().all(|r| r.failure.is_none()), "{res:?}");
        assert!(rcv.check_test_transfer_status_recipient(id, TransferStatus::WaitingConfirmations));
    }
    assert!(wait_for_function(
        || log_count(&rcv, "Not broadcasting the zero-fee signed TX again") >= 3,
        10,
        200
    ));
    assert_eq!(log_count(&rcv, "Broadcasting the saved signed TX again"), 0);
    assert_eq!(
        log_count(&rcv, "Failed to broadcast the saved signed TX again"),
        0
    );
    assert_eq!(get_tx_height(&res.txid), None);
    assert_eq!(
        received_parent(&rcv, id).compute_txid().to_string(),
        res.txid
    );
    assert_eq!(
        rcv.wallet.get_received_signed_tx(s!("unknown")).unwrap(),
        None
    );
}

#[test]
#[parallel]
fn zero_fee_sender_waits_on_sight() {
    initialize();

    let mut r = release_to_payer();
    for _ in 0..2 {
        let res = r.sender.refresh_result(None, &[]).unwrap();
        assert!(res.values().all(|r| r.failure.is_none()), "{res:?}");
        assert!(
            r.sender
                .check_test_transfer_status_sender(&r.txid, TransferStatus::WaitingCounterparty)
        );
    }
    assert_eq!(get_tx_height(&r.txid), None);
    let res = r.sender.fail_transfers(Some(r.idx), false, false);
    assert!(
        matches!(res, Err(Error::CannotFailBatchTransfer)),
        "{res:?}"
    );
    assert!(!r.sender.db_reserved_txos().is_empty());
    let coin = child_coin();
    let guard = stop_mining();
    submit_package(&r.parent, &coin);
    assert!(wait_for_function(
        || {
            let _ = r.sender.refresh_result(None, &[]);
            r.sender
                .check_test_transfer_status_sender(&r.txid, TransferStatus::WaitingConfirmations)
        },
        60,
        1000
    ));
    assert_eq!(get_tx_height(&r.txid), Some(0));
    drop(guard);
}

#[test]
#[parallel]
fn zero_fee_guards() {
    initialize();

    let refused = |res: Result<SendBeginResult, Error>| {
        assert!(
            matches!(&res, Err(Error::InvalidFeeRate { details: m }) if m == FEE_MSG_LOW),
            "{res:?}"
        );
    };
    let mut sender = get_funded_noutxo_party!();
    let mut rcv = get_funded_party!();
    let mut oob = get_funded_party!();
    let guard = stop_mining();
    sender.create_utxos_default();
    let asset_id = sender.issue_asset_nia(Some(&[AMOUNT])).asset_id;
    let rcv_data = rcv.blind_receive();
    let oob_data = oob.blind_receive();
    let recipient_map =
        blind_recipient_map(&asset_id, &rcv_data, Assignment::Fungible(AMOUNT_SMALL));
    let send_begin = |sender: &mut SinglesigParty,
                      map: &HashMap<String, Vec<Recipient>>,
                      donation: bool,
                      fee_rate: u64,
                      dry_run: bool| {
        sender.wallet.send_begin(
            sender.online,
            map.clone(),
            donation,
            fee_rate,
            MIN_CONFIRMATIONS,
            default_send_expiration(),
            Some(PROXY_ENDPOINT.clone()),
            dry_run,
        )
    };

    // only unconfirmed inputs
    refused(send_begin(&mut sender, &recipient_map, false, 0, false));
    assert!(sender.db_reserved_txos().is_empty());
    drop(guard);
    ready(&mut sender);

    // out-of-band recipient
    let mixed_map = HashMap::from([(
        asset_id.clone(),
        vec![
            blind_recipient(&rcv_data, Assignment::Fungible(AMOUNT_SMALL)),
            Recipient {
                transport_endpoints: vec![],
                ..blind_recipient(&oob_data, Assignment::Fungible(AMOUNT_SMALL))
            },
        ],
    )]);
    refused(send_begin(&mut sender, &mixed_map, false, 0, false));
    assert!(sender.db_reserved_txos().is_empty());

    let begin = send_begin(&mut sender, &recipient_map, false, 0, true).unwrap();
    assert_eq!(
        Psbt::from_str(&begin.psbt).unwrap().unsigned_tx.version.0,
        3
    );
    let begin = send_begin(&mut sender, &recipient_map, false, FEE_RATE, true).unwrap();
    assert_eq!(
        Psbt::from_str(&begin.psbt).unwrap().unsigned_tx.version.0,
        2
    );
}

#[test]
#[parallel]
fn free_utxo_skips_unconfirmed_v3() {
    initialize();

    let mut sender = get_funded_party!();
    let asset_id = sender.issue_asset_nia(Some(&[AMOUNT])).asset_id;
    let mut payer = get_funded_party!();
    let mut rcv = get_funded_party!();
    ready(&mut sender);
    let payer_data = payer.blind_receive();
    let recipient_map =
        blind_recipient_map(&asset_id, &payer_data, Assignment::Fungible(AMOUNT_SMALL));
    let (_, _, res) = zero_fee_send(
        &mut sender,
        recipient_map,
        &PROXY_ENDPOINT,
        default_send_expiration(),
        0,
    );
    let id = payer_data.recipient_id.as_str();
    wait_recipients(&mut [&mut payer], &[id]);
    let coin = child_coin();
    let guard = stop_mining();
    submit_package(&received_parent(&payer, id), &coin);
    assert!(wait_for_function(
        || {
            let _ = sender.refresh_result(None, &[]);
            sender.check_test_transfer_status_sender(&res.txid, TransferStatus::Settled)
        },
        60,
        1000
    ));
    let rcv_data = rcv.blind_receive();
    let recipient_map =
        blind_recipient_map(&asset_id, &rcv_data, Assignment::Fungible(AMOUNT_SMALL));
    let dry_run = |sender: &mut SinglesigParty| {
        sender.wallet.send_begin(
            sender.online,
            recipient_map.clone(),
            false,
            FEE_RATE,
            MIN_CONFIRMATIONS,
            default_send_expiration(),
            Some(PROXY_ENDPOINT.clone()),
            true,
        )
    };
    let res_dry = dry_run(&mut sender);
    assert!(
        matches!(res_dry, Err(Error::InsufficientAssignments { .. })),
        "{res_dry:?}"
    );
    drop(guard);
    mine(false);
    ready(&mut sender);
    let psbt = Psbt::from_str(&dry_run(&mut sender).unwrap().psbt).unwrap();
    assert_eq!(psbt.unsigned_tx.version.0, 2);
    assert!(
        psbt.unsigned_tx
            .input
            .iter()
            .any(|i| i.previous_output.txid.to_string() == res.txid)
    );
}

#[test]
#[serial]
fn zero_fee_package_confirms() {
    initialize();

    let mut r = release_to_payer();
    let child_txid = submit_package(&r.parent, &child_coin());
    assert!(wait_for_function(
        || [&r.txid, &child_txid]
            .iter()
            .all(|t| cli(esplora_cli(), &["getmempoolentry", t]).0),
        60,
        500
    ));
    mine(true);
    assert!(wait_for_function(
        || get_tx_height(&r.txid).is_some_and(|c| c > 0),
        60,
        500
    ));
    assert_eq!(get_tx_height(&r.txid), get_tx_height(&child_txid));
    assert_settled(&mut r);
}

#[test]
#[serial]
fn zero_fee_parent_alone() {
    initialize();

    let mut r = release_to_payer();
    let parent_hex = serialize_hex(&r.parent);
    let (ok, out) = cli(bitcoin_cli(), &["sendrawtransaction", &parent_hex]);
    assert!(!ok);
    assert!(out.contains("min relay fee not met"), "{out}");
    let addr = miner_cli(&["getnewaddress"]);
    miner_cli(&["generateblock", &addr, &format!("[\"{parent_hex}\"]")]);
    wait_indexers_sync();
    assert_settled(&mut r);
}

struct ShortGrace {
    sender: SinglesigParty,
    asset_id: String,
    expiration: i64,
}

fn short_grace_setup() -> ShortGrace {
    wait_short_grace_proxy();
    MOCK_MIN_RETENTION_GRACE.replace(Some(0));
    MOCK_CONFIRMATION_MARGIN.replace(Some(5));
    MOCK_CLOCK_SKEW_MARGIN.replace(Some(5));
    let mut sender = get_funded_party!();
    let asset_id = sender.issue_asset_nia(Some(&[AMOUNT])).asset_id;
    ready(&mut sender);
    ShortGrace {
        sender,
        asset_id,
        expiration: 0,
    }
}

fn short_grace_send(sg: &mut ShortGrace, recipients: Vec<Recipient>) -> OperationResult {
    sg.expiration = now().unix_timestamp() + 30;
    let (_, _, res) = zero_fee_send(
        &mut sg.sender,
        HashMap::from([(sg.asset_id.clone(), recipients)]),
        &PROXY_ENDPOINT_SHORT_GRACE,
        sg.expiration as u64,
        MIN_CONFIRMATIONS,
    );
    res
}

fn assert_fail_denied(sender: &mut SinglesigParty, idx: i32) {
    let res = sender.fail_transfers(Some(idx), false, false);
    assert!(
        matches!(res, Err(Error::CannotFailBatchTransfer)),
        "{res:?}"
    );
    assert!(!sender.fail_transfers_all());
    assert!(!sender.db_reserved_txos().is_empty());
}

fn assert_sender_free(sg: &SinglesigParty, asset_id: &str) {
    assert!(sg.db_reserved_txos().is_empty());
    assert_balance(sg, asset_id, AMOUNT);
}

#[test]
#[serial(short_grace)]
fn zero_fee_deadline_no_child() {
    initialize();

    let mut sg = short_grace_setup();
    let mut rcv = get_funded_party!();
    let mut payer = get_funded_party!();
    let rcv_data = rcv.blind_receive();
    let payer_data = payer.blind_receive();
    let res = short_grace_send(
        &mut sg,
        vec![
            blind_recipient(&rcv_data, Assignment::Fungible(AMOUNT_SMALL)),
            blind_recipient(&payer_data, Assignment::Fungible(AMOUNT_SMALL)),
        ],
    );
    let ids = [
        rcv_data.recipient_id.as_str(),
        payer_data.recipient_id.as_str(),
    ];
    wait_recipients(&mut [&mut rcv, &mut payer], &ids);
    let refresh = sg.sender.refresh_result(None, &[]).unwrap();
    assert!(refresh.values().all(|r| r.failure.is_none()), "{refresh:?}");
    assert!(
        sg.sender
            .check_test_transfer_status_sender(&res.txid, TransferStatus::WaitingCounterparty)
    );

    sleep_until(sg.expiration + 7);
    assert_fail_denied(&mut sg.sender, res.batch_transfer_idx);
    assert!(
        sg.sender
            .check_test_transfer_status_sender(&res.txid, TransferStatus::WaitingCounterparty)
    );
    for (p, id) in [(&rcv, ids[0]), (&payer, ids[1])] {
        assert!(p.check_test_transfer_status_recipient(id, TransferStatus::WaitingConfirmations));
    }

    let secs = (sg.expiration + 10 + 60 + 40 - now().unix_timestamp()) as u8;
    wait_all(
        &mut sg.sender,
        &res.txid,
        &mut [&mut rcv, &mut payer],
        &ids,
        TransferStatus::Failed,
        secs,
    );
    assert_sender_free(&sg.sender, &sg.asset_id);
    assert_eq!(get_tx_height(&res.txid), None);
}

#[test]
#[serial(short_grace)]
fn zero_fee_proxy_lost() {
    initialize();

    let mut sg = short_grace_setup();
    let mut rcv = get_funded_party!();
    let rcv_data = rcv.blind_receive();
    let res = short_grace_send(
        &mut sg,
        vec![blind_recipient(
            &rcv_data,
            Assignment::Fungible(AMOUNT_SMALL),
        )],
    );
    let idx = res.batch_transfer_idx;
    recreate_service("proxy-short-grace", &[]);
    wait_short_grace_proxy();
    let refresh = sg.sender.refresh_result(None, &[]).unwrap();
    assert_matches!(refresh[&idx].failure, Some(Error::OperationNotFound));
    assert_fail_denied(&mut sg.sender, idx);
    sleep_until(sg.expiration + 7);
    assert_fail_denied(&mut sg.sender, idx);

    sleep_until(sg.expiration + 10 + 5 + 2);
    assert!(wait_for_function(
        || {
            let _ = sg.sender.refresh_result(None, &[]);
            sg.sender
                .check_test_transfer_status_sender(&res.txid, TransferStatus::Failed)
        },
        30,
        1000
    ));
    assert_sender_free(&sg.sender, &sg.asset_id);
}

#[test]
#[serial(short_grace)]
fn zero_fee_bulk_fail_proxy_down() {
    initialize();

    let mut sg = short_grace_setup();
    let mut rcv = get_funded_party!();
    let rcv_data = rcv.blind_receive();
    let res = short_grace_send(
        &mut sg,
        vec![blind_recipient(
            &rcv_data,
            Assignment::Fungible(AMOUNT_SMALL),
        )],
    );
    wait_recipients(&mut [&mut rcv], &[&rcv_data.recipient_id]);
    let _down = ProxyStopped::new();

    sleep_until(sg.expiration + 7);
    assert_fail_denied(&mut sg.sender, res.batch_transfer_idx);
    assert!(
        sg.sender
            .check_test_transfer_status_sender(&res.txid, TransferStatus::WaitingCounterparty)
    );

    sleep_until(sg.expiration + 10 + 5 + 2);
    assert!(sg.sender.fail_transfers_all());
    assert!(
        sg.sender
            .check_test_transfer_status_sender(&res.txid, TransferStatus::Failed)
    );
    assert_sender_free(&sg.sender, &sg.asset_id);
}
