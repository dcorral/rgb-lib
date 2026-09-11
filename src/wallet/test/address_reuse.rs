use super::*;

pub(crate) fn get_test_reuse_wallet_data() -> WalletData {
    let mut wallet_data = get_test_wallet_data(&get_test_data_dir_string());
    wallet_data.reuse_addresses = true;
    wallet_data
}

pub(crate) fn get_test_reuse_wallet() -> Wallet {
    create_test_data_dir();
    let keys = generate_keys(BitcoinNetwork::Regtest, WitnessVersion::Taproot);
    Wallet::new(
        get_test_reuse_wallet_data(),
        SinglesigKeys::from_keys(&keys, None),
    )
    .unwrap()
}

#[test]
#[parallel]
fn flag_survives_load() {
    create_test_data_dir();
    let keys = generate_keys(BitcoinNetwork::Regtest, WitnessVersion::Taproot);
    let wallet_data = get_test_reuse_wallet_data();
    let wallet = Wallet::new(wallet_data.clone(), SinglesigKeys::from_keys(&keys, None)).unwrap();
    assert!(wallet.get_wallet_data().reuse_addresses);
    drop(wallet);

    let loaded = Wallet::load(
        &wallet_data.data_dir,
        &keys.master_fingerprint,
        Some(keys.mnemonic.clone()),
    )
    .unwrap();
    assert!(loaded.get_wallet_data().reuse_addresses);
}

#[test]
#[parallel]
fn same_address_until_rotated() {
    let mut wallet = get_test_reuse_wallet();

    // vanilla: the first call reveals index 0, later calls return it again
    let vanilla_1 = wallet.get_address().unwrap();
    let vanilla_2 = wallet.get_address().unwrap();
    assert_eq!(vanilla_1, vanilla_2);

    // colored: same behavior through the internal address getter
    let colored_1 = wallet.get_new_address().unwrap();
    let colored_2 = wallet.get_new_address().unwrap();
    assert_eq!(colored_1, colored_2);
    assert_ne!(vanilla_1, colored_1.to_string());

    // rotating one keychain moves only that keychain's pin
    let vanilla_3 = wallet.rotate_vanilla_address().unwrap();
    assert_ne!(vanilla_3, vanilla_1);
    assert_eq!(wallet.get_address().unwrap(), vanilla_3);
    assert_eq!(wallet.get_new_address().unwrap(), colored_1);

    let colored_3 = wallet.rotate_colored_address().unwrap();
    assert_ne!(colored_3, colored_1.to_string());
    assert_eq!(wallet.get_new_address().unwrap().to_string(), colored_3);
    assert_eq!(wallet.get_address().unwrap(), vanilla_3);
}

#[test]
#[parallel]
fn rotate_requires_flag() {
    let mut wallet = get_test_wallet(false, None);
    assert_matches!(
        wallet.rotate_vanilla_address(),
        Err(Error::AddressReuseDisabled)
    );
    assert_matches!(
        wallet.rotate_colored_address(),
        Err(Error::AddressReuseDisabled)
    );
}

#[test]
#[parallel]
fn pin_survives_reload() {
    create_test_data_dir();
    let keys = generate_keys(BitcoinNetwork::Regtest, WitnessVersion::Taproot);
    let wallet_keys = SinglesigKeys::from_keys(&keys, None);
    let wallet_data = get_test_reuse_wallet_data();

    let mut wallet = Wallet::new(wallet_data.clone(), wallet_keys.clone()).unwrap();
    wallet.get_address().unwrap();
    let rotated = wallet.rotate_vanilla_address().unwrap();
    drop(wallet);

    let mut wallet = Wallet::new(wallet_data, wallet_keys).unwrap();
    assert_eq!(wallet.get_address().unwrap(), rotated);
}

#[cfg(feature = "electrum")]
#[test]
#[parallel]
fn btc_change_lands_on_pin() {
    initialize();

    let mut wallet = get_test_reuse_wallet();
    let online = wallet.go_online(test_go_online_options(None)).unwrap();
    let vanilla_pin = wallet.get_address().unwrap();
    let colored_pin = wallet.get_new_address().unwrap();
    let vanilla_script = wallet.get_script_pubkey(&vanilla_pin).unwrap();

    fund_wallet(vanilla_pin.clone());
    wallet
        .create_utxos(online, false, None, None, FEE_RATE, false)
        .unwrap();
    mine(false);

    let mut rcv_wallet = get_test_wallet(false, None);
    let rcv_addr = rcv_wallet.get_address().unwrap();
    wallet
        .send_btc(online, rcv_addr, 1000, FEE_RATE, false)
        .unwrap();
    mine(false);

    // pins did not move
    assert_eq!(wallet.get_address().unwrap(), vanilla_pin);
    assert_eq!(wallet.get_new_address().unwrap(), colored_pin);

    // every vanilla unspent (change from both operations) sits on the pinned script
    let vanilla_unspents = wallet
        .list_unspents_vanilla(online, MIN_CONFIRMATIONS, false)
        .unwrap();
    assert!(!vanilla_unspents.is_empty());
    assert!(
        vanilla_unspents
            .iter()
            .all(|u| u.txout.script_pubkey == vanilla_script)
    );

    // every colored unspent sits on the pinned colored script and every vanilla one (which
    // `list_unspents` appends as non-colorable) on the pinned vanilla script
    let colored_script = colored_pin.script_pubkey();
    let unspents = wallet.list_unspents(Some(online), false, false).unwrap();
    assert!(unspents.iter().any(|u| u.utxo.colorable));
    for unspent in unspents {
        let txo = wallet
            .bdk_wallet()
            .get_utxo(BdkOutPoint::from_str(&unspent.utxo.outpoint.to_string()).unwrap())
            .unwrap();
        let expected = if unspent.utxo.colorable {
            &colored_script
        } else {
            &vanilla_script
        };
        assert_eq!(&txo.txout.script_pubkey, expected);
    }
}

#[cfg(feature = "electrum")]
fn get_funded_reuse_party() -> SinglesigParty {
    initialize();
    let mut wallet = get_test_reuse_wallet();
    let online = wallet.go_online(test_go_online_options(None)).unwrap();
    fund_wallet(wallet.get_address().unwrap());
    wallet
        .create_utxos(online, false, None, None, FEE_RATE, false)
        .unwrap();
    mine(false);
    party!(wallet, online)
}

/// Issue a witness invoice on `receiver`, have `sender` pay `AMOUNT / 3` of the asset with 1000
/// sats of witness funding and settle it; returns the invoice's recipient ID. The settled-balance
/// assertions in the tests below depend on these amounts.
#[cfg(feature = "electrum")]
fn pay_witness_invoice(
    sender: &mut SinglesigParty,
    receiver: &mut SinglesigParty,
    asset_id: &str,
) -> String {
    let receive_data = receiver.witness_receive();
    // the sender parses the invoice, so its transport endpoints carry the nonce
    let endpoints = Invoice::new(receive_data.invoice.clone())
        .unwrap()
        .invoice_data()
        .transport_endpoints;
    let recipient_map = HashMap::from([(
        asset_id.to_string(),
        vec![Recipient {
            assignment: Assignment::Fungible(AMOUNT / 3),
            recipient_id: receive_data.recipient_id.clone(),
            witness_data: Some(WitnessData {
                amount_sat: 1000,
                blinding: None,
            }),
            transport_endpoints: endpoints,
        }],
    )]);
    let txid = sender.send_retry(&recipient_map);
    assert!(!txid.is_empty());

    receiver.wait_for_refresh(None);
    sender.wait_for_refresh(Some(asset_id));
    mine(false);
    receiver.wait_for_refresh(None);
    sender.wait_for_refresh(Some(asset_id));
    receive_data.recipient_id
}

#[cfg(feature = "electrum")]
#[test]
#[parallel]
fn witness_invoices_share_recipient_id_but_not_nonce() {
    let mut party = get_funded_reuse_party();

    let receive_1 = party.witness_receive();
    let receive_2 = party.witness_receive();
    assert_eq!(receive_1.recipient_id, receive_2.recipient_id);

    let nonces: Vec<Vec<u8>> = [&receive_1, &receive_2]
        .iter()
        .map(|r| {
            let endpoints = Invoice::new(r.invoice.clone())
                .unwrap()
                .invoice_data()
                .transport_endpoints;
            let (_, nonce) = extract_recipient_nonce(&endpoints[0]).unwrap();
            nonce.expect("reuse invoices carry a nonce")
        })
        .collect();
    assert_eq!(nonces[0].len(), 16);
    assert_ne!(nonces[0], nonces[1]);

    // the nonces are persisted on the transfers, the stored endpoints are bare
    let txn = party.wallet.database().begin_transaction().unwrap();
    let transfers: Vec<DbTransfer> = txn
        .iter_transfers()
        .unwrap()
        .into_iter()
        .filter(|t| t.recipient_id.as_deref() == Some(&receive_1.recipient_id))
        .collect();
    txn.commit().unwrap();
    assert_eq!(transfers.len(), 2);
    let mut stored: Vec<Vec<u8>> = transfers
        .iter()
        .map(|t| match &t.recipient_type {
            Some(RecipientTypeFull::Witness {
                recipient_nonce, ..
            }) => recipient_nonce.clone(),
            other => panic!("unexpected recipient type: {other:?}"),
        })
        .collect();
    stored.sort();
    let mut expected = nonces.clone();
    expected.sort();
    assert_eq!(stored, expected);
    for transfer in &transfers {
        let endpoints = party.db_transfer_transport_endpoints_data(transfer.idx);
        assert!(!endpoints.is_empty());
        assert!(
            endpoints
                .iter()
                .all(|(_, te)| !te.endpoint.contains("rid_nonce"))
        );
    }
}

#[cfg(feature = "electrum")]
#[test]
#[parallel]
fn out_of_band_witness_refused_under_reuse() {
    let mut party = get_funded_reuse_party();
    let result = party.wallet.witness_receive(
        None,
        Assignment::Any,
        default_rcv_expiration(),
        vec![],
        MIN_CONFIRMATIONS,
    );
    assert_matches!(result, Err(Error::InvalidTransportEndpoints { .. }));

    // blind out-of-band is unaffected: the blinded seal is unique per invoice
    party
        .wallet
        .blind_receive(
            None,
            Assignment::Any,
            default_rcv_expiration(),
            vec![],
            MIN_CONFIRMATIONS,
        )
        .unwrap();
}

#[cfg(feature = "electrum")]
#[test]
#[parallel]
fn endpoint_with_reserved_parameter_is_rejected() {
    // offline, but the service bootstrap wipes the test data dir: wait for it like every other test
    initialize();

    let receive = |wallet: &mut Wallet, endpoint: String| {
        wallet.witness_receive(
            None,
            Assignment::Any,
            default_rcv_expiration(),
            vec![endpoint],
            MIN_CONFIRMATIONS,
        )
    };

    // the nonce parameter is reserved on every wallet: the sender would derive a proxy key the
    // receiver never polls
    let reserved = format!("{}?rid_nonce={}", *PROXY_ENDPOINT, "11".repeat(16));
    for mut wallet in [get_test_reuse_wallet(), get_test_wallet(false, None)] {
        assert_matches!(
            receive(&mut wallet, reserved.clone()),
            Err(Error::InvalidTransportEndpoints { .. })
        );
    }

    // any other query string is refused only under reuse, where the nonce takes the query string
    let query = format!("{}?token=x", *PROXY_ENDPOINT);
    assert_matches!(
        receive(&mut get_test_reuse_wallet(), query.clone()),
        Err(Error::InvalidTransportEndpoints { .. })
    );
    receive(&mut get_test_wallet(false, None), query).unwrap();
}

#[cfg(feature = "electrum")]
#[test]
#[parallel]
fn sender_rejects_malformed_invoice_nonce() {
    initialize();

    let mut sender = get_funded_party!();
    let asset = sender.issue_asset_nia(None);
    let mut receiver = get_funded_reuse_party();

    let receive_data = receiver.witness_receive();
    let mut endpoint = Invoice::new(receive_data.invoice.clone())
        .unwrap()
        .invoice_data()
        .transport_endpoints
        .remove(0);
    // drop the last nonce byte: 30 hex characters instead of 32
    endpoint.truncate(endpoint.len() - 2);
    let recipient_map = HashMap::from([(
        asset.asset_id.clone(),
        vec![Recipient {
            assignment: Assignment::Fungible(AMOUNT / 3),
            recipient_id: receive_data.recipient_id.clone(),
            witness_data: Some(WitnessData {
                amount_sat: 1000,
                blinding: None,
            }),
            transport_endpoints: vec![endpoint],
        }],
    )]);
    assert_matches!(
        sender.send_result(&recipient_map),
        Err(Error::InvalidTransportEndpoints { .. })
    );
}

#[cfg(feature = "electrum")]
#[test]
#[parallel]
fn two_witness_transfers_settle_and_pins_hold() {
    initialize();

    let mut sender = get_funded_party!();
    let asset = sender.issue_asset_nia(Some(&[AMOUNT, AMOUNT * 2]));
    // each witness send burns two of the sender's UTXOs (BTC funding and RGB change)
    sender.create_utxos(false, Some(5), None, FEE_RATE, None);
    mine(false);

    let mut receiver = get_funded_reuse_party();
    let vanilla_pin = receiver.wallet.get_address().unwrap();
    let colored_pin = receiver.wallet.get_new_address().unwrap();

    let mut recipient_ids = vec![];
    for _ in 0..2 {
        recipient_ids.push(pay_witness_invoice(
            &mut sender,
            &mut receiver,
            &asset.asset_id,
        ));
    }
    assert_eq!(recipient_ids[0], recipient_ids[1]);

    // both receives settled
    let settled = receiver
        .list_transfers(Some(&asset.asset_id))
        .into_iter()
        .filter(|t| t.status == TransferStatus::Settled && t.kind == TransferKind::ReceiveWitness)
        .count();
    assert_eq!(settled, 2);
    assert_eq!(
        receiver.get_asset_balance(&asset.asset_id).settled,
        AMOUNT / 3 * 2
    );

    // nothing above moved either pin
    assert_eq!(receiver.wallet.get_address().unwrap(), vanilla_pin);
    assert_eq!(receiver.wallet.get_new_address().unwrap(), colored_pin);

    // rotation moves the pins, the next invoice lands on the new colored one
    let new_colored = receiver.wallet.rotate_colored_address().unwrap();
    let new_vanilla = receiver.wallet.rotate_vanilla_address().unwrap();
    assert_ne!(new_colored, colored_pin.to_string());
    assert_ne!(new_vanilla, vanilla_pin);
    let third = pay_witness_invoice(&mut sender, &mut receiver, &asset.asset_id);
    assert!(!recipient_ids.contains(&third));
    assert_eq!(
        receiver.get_asset_balance(&asset.asset_id).settled,
        AMOUNT / 3 * 3
    );

    // the receiver spends some of it back (RGB send with change)
    let receive_back = sender.blind_receive();
    let recipient_map = HashMap::from([(
        asset.asset_id.clone(),
        vec![Recipient {
            assignment: Assignment::Fungible(1),
            recipient_id: receive_back.recipient_id.clone(),
            witness_data: None,
            transport_endpoints: TRANSPORT_ENDPOINTS.clone(),
        }],
    )]);
    receiver.send_retry(&recipient_map);
    sender.wait_for_refresh(None);
    receiver.wait_for_refresh(Some(&asset.asset_id));
    mine(false);
    sender.wait_for_refresh(None);
    receiver.wait_for_refresh(Some(&asset.asset_id));

    // invariant: nothing since the rotation moved either pin
    assert_eq!(receiver.wallet.get_address().unwrap(), new_vanilla);
    assert_eq!(
        receiver.wallet.get_new_address().unwrap().to_string(),
        new_colored
    );
}

#[cfg(feature = "electrum")]
#[test]
#[parallel]
fn own_utxos_on_pinned_script_are_not_witness_flagged() {
    initialize();

    let mut sender = get_funded_party!();
    let asset = sender.issue_asset_nia(None);

    let mut receiver = get_funded_reuse_party();
    let colored_script = receiver.wallet.get_new_address().unwrap().script_pubkey();

    // an invoice is pending on the pinned script...
    let receive_data = receiver.witness_receive();

    // ...while the wallet creates more UTXOs on that same script
    fund_wallet(receiver.wallet.get_address().unwrap());
    mine(false);
    receiver.create_utxos(false, Some(2), None, FEE_RATE, None);
    mine(false);
    receiver.list_unspents_with_sync(false);
    assert!(
        receiver.db_txos().iter().all(|t| !t.pending_witness),
        "own UTXOs must not be flagged as pending witness receives"
    );

    // a sender-funded output on the pinned script is flagged (paid from outside the wallet)
    let colored_address = receiver.wallet.get_new_address().unwrap().to_string();
    send_to_address(colored_address);
    mine(false);
    receiver.list_unspents_with_sync(false);
    let flagged: Vec<DbTxo> = receiver
        .db_txos()
        .into_iter()
        .filter(|t| t.pending_witness)
        .collect();
    assert_eq!(flagged.len(), 1);
    let bdk_txo = receiver
        .wallet
        .bdk_wallet()
        .get_utxo(BdkOutPoint::new(
            Txid::from_str(&flagged[0].txid).unwrap(),
            flagged[0].vout,
        ))
        .unwrap();
    assert_eq!(bdk_txo.txout.script_pubkey, colored_script);
    let foreign_txid = flagged[0].txid.clone();

    // the actual witness payment: refresh records the TXO as pending witness at ACK time; the
    // later sync only marks the outpoint as existing and leaves the flag alone (sync-time
    // flagging is what the foreign payment above observes)
    let endpoints = Invoice::new(receive_data.invoice.clone())
        .unwrap()
        .invoice_data()
        .transport_endpoints;
    let recipient_map = HashMap::from([(
        asset.asset_id.clone(),
        vec![Recipient {
            assignment: Assignment::Fungible(AMOUNT / 2),
            recipient_id: receive_data.recipient_id.clone(),
            witness_data: Some(WitnessData {
                amount_sat: 1000,
                blinding: None,
            }),
            transport_endpoints: endpoints,
        }],
    )]);
    let txid = sender.send_retry(&recipient_map);
    // the receiver ACKs (recording the pending witness TXO), the sender broadcasts
    receiver.wait_for_refresh(None);
    sender.wait_for_refresh(Some(&asset.asset_id));
    receiver.list_unspents_with_sync(false);
    let flagged: Vec<DbTxo> = receiver
        .db_txos()
        .into_iter()
        .filter(|t| t.pending_witness && t.txid != foreign_txid)
        .collect();
    assert_eq!(flagged.len(), 1);
    assert_eq!(flagged[0].txid, txid);
    assert!(flagged[0].exists);

    // the pending witness script row survives settlement (the settle path syncs it) and is
    // released by the next sync
    mine(false);
    receiver.wait_for_refresh(None);
    sender.wait_for_refresh(Some(&asset.asset_id));
    assert!(
        receiver
            .db_txos()
            .iter()
            .all(|t| !t.pending_witness || t.txid == foreign_txid)
    );
    assert_eq!(receiver.db_pending_witness_scripts().len(), 1);
    receiver.list_unspents_with_sync(false);
    assert!(receiver.db_pending_witness_scripts().is_empty());
}

#[test]
#[parallel]
fn enabling_reuse_pins_last_revealed_address() {
    create_test_data_dir();
    let keys = generate_keys(BitcoinNetwork::Regtest, WitnessVersion::Taproot);
    let wallet_keys = SinglesigKeys::from_keys(&keys, None);
    let mut wallet_data = get_test_wallet_data(&get_test_data_dir_string());
    wallet_data.reuse_addresses = false;

    let mut wallet = Wallet::new(wallet_data.clone(), wallet_keys.clone()).unwrap();
    wallet.get_address().unwrap();
    let last_revealed = wallet.get_address().unwrap();
    drop(wallet);

    wallet_data.reuse_addresses = true;
    let mut wallet = Wallet::new(wallet_data, wallet_keys).unwrap();
    assert_eq!(wallet.get_address().unwrap(), last_revealed);
    assert_eq!(wallet.get_address().unwrap(), last_revealed);
}

#[cfg(feature = "electrum")]
#[test]
#[parallel]
fn replayed_consignment_is_refused() {
    initialize();

    let mut sender = get_funded_party!();
    let asset = sender.issue_asset_nia(None);
    let mut receiver = get_funded_reuse_party();

    // invoice A: paid and settled
    let receive_a = receiver.witness_receive();
    let endpoints = Invoice::new(receive_a.invoice.clone())
        .unwrap()
        .invoice_data()
        .transport_endpoints;
    let recipient_map = HashMap::from([(
        asset.asset_id.clone(),
        vec![Recipient {
            assignment: Assignment::Fungible(AMOUNT / 2),
            recipient_id: receive_a.recipient_id.clone(),
            witness_data: Some(WitnessData {
                amount_sat: 1000,
                blinding: None,
            }),
            transport_endpoints: endpoints,
        }],
    )]);
    let txid = sender.send_retry(&recipient_map);
    receiver.wait_for_refresh(None);
    sender.wait_for_refresh(Some(&asset.asset_id));
    mine(false);
    receiver.wait_for_refresh(None);
    sender.wait_for_refresh(Some(&asset.asset_id));
    let settled_before = receiver.get_asset_balance(&asset.asset_id).settled;
    assert_eq!(settled_before, AMOUNT / 2);
    let transfer_a = receiver
        .db_transfers()
        .into_iter()
        .find(|t| t.recipient_id.as_deref() == Some(receive_a.recipient_id.as_str()))
        .unwrap();
    let vout = match transfer_a.recipient_type {
        Some(RecipientTypeFull::Witness { vout, .. }) => vout,
        _ => unreachable!(),
    };
    let consignment_a = receiver
        .wallet
        .get_receive_consignment_path(&transfer_a.proxy_recipient_id());

    // invoice B on the same script: replay A's consignment into B's proxy mailbox
    let receive_b = receiver.witness_receive();
    assert_eq!(receive_a.recipient_id, receive_b.recipient_id);
    let endpoints = Invoice::new(receive_b.invoice)
        .unwrap()
        .invoice_data()
        .transport_endpoints;
    let (_, nonce) = extract_recipient_nonce(&endpoints[0]).unwrap();
    let replay_key = derive_proxy_recipient_id(&receive_b.recipient_id, &nonce.unwrap());
    let response = ProxyClient::new(PROXY_URL)
        .unwrap()
        .post_consignment(&replay_key, &consignment_a, &txid, vout)
        .unwrap();
    assert!(response.error.is_none(), "{response:?}");
    receiver.refresh_result(None, &[]).unwrap();

    // nothing was credited twice and B failed
    assert_eq!(
        receiver.get_asset_balance(&asset.asset_id).settled,
        settled_before
    );
    let settled = receiver
        .list_transfers(Some(&asset.asset_id))
        .into_iter()
        .filter(|t| t.kind == TransferKind::ReceiveWitness && t.status == TransferStatus::Settled)
        .count();
    assert_eq!(settled, 1);
    let transfer_b = receiver
        .db_transfers()
        .into_iter()
        .find(|t| t.idx != transfer_a.idx)
        .unwrap();
    let (transfer_b_data, _) = receiver.get_test_transfer_data(&transfer_b);
    assert_eq!(transfer_b_data.status, TransferStatus::Failed);
}

#[cfg(feature = "electrum")]
#[test]
#[parallel]
fn disabling_reuse_keeps_pending_invoices_protected() {
    initialize();

    let mut receiver = get_funded_reuse_party();
    let receive_1 = receiver.witness_receive();
    let receive_2 = receiver.witness_receive();
    assert_eq!(receive_1.recipient_id, receive_2.recipient_id);
    let colored_address = receiver.wallet.get_new_address().unwrap().to_string();

    // reopen with reuse disabled while both invoices are pending
    let keys = receiver.wallet.get_keys();
    let mut wallet_data = receiver.wallet.get_wallet_data();
    wallet_data.reuse_addresses = false;
    drop(receiver);
    let mut wallet = Wallet::new(wallet_data, keys).unwrap();
    let online = wallet.go_online(test_go_online_options(None)).unwrap();
    let mut receiver = party!(wallet, online);

    // two foreign payments to the pinned script, synced one at a time
    for _ in 0..2 {
        send_to_address(colored_address.clone());
        mine(false);
        receiver.list_unspents_with_sync(false);
    }
    let flagged = receiver
        .db_txos()
        .iter()
        .filter(|t| t.pending_witness)
        .count();
    assert_eq!(flagged, 2);
    assert_eq!(receiver.db_pending_witness_scripts().len(), 1);
}

#[cfg(feature = "electrum")]
#[test]
#[parallel]
fn failed_invoices_release_the_watched_script() {
    // non-reuse wallet: before the transfer-linked watch lifecycle the row of a failed invoice
    // leaked forever
    initialize();

    let mut party = get_funded_party!();
    party.witness_receive();
    assert_eq!(party.db_pending_witness_scripts().len(), 1);

    let batch_transfer_idx = party.db_batch_transfers()[0].idx;
    assert!(party.fail_transfers_single(batch_transfer_idx));
    party.list_unspents_with_sync(false);
    assert!(party.db_pending_witness_scripts().is_empty());
}

#[cfg(feature = "electrum")]
#[test]
#[parallel]
fn inflation_with_several_outputs_works_under_reuse() {
    let mut party = get_funded_reuse_party();
    let asset = party.issue_asset_ifa(Some(&[AMOUNT]), Some(&[500]), None);
    let colored_pin = party.wallet.get_new_address().unwrap();

    // every inflation output lands on the pinned colored script
    let inflation_amounts = [100, 200];
    party.inflate(&asset.asset_id, &inflation_amounts);
    mine(false);
    assert!(party.refresh_asset(&asset.asset_id));

    let balance = party.get_asset_balance(&asset.asset_id);
    assert_eq!(balance.settled, AMOUNT + 300);
    assert_eq!(party.wallet.get_new_address().unwrap(), colored_pin);
}

#[cfg(feature = "electrum")]
#[test]
#[parallel]
fn multisig_rotation_advances_past_legacy_local_pin() {
    initialize();

    // mocked hub: current internal index 1 (the legacy gap, it only feeds the initial catch-up and
    // never moves since rotation reads the local index), bumps hand out 2 then 3
    let mut server = mockito::Server::new();
    server
        .mock("GET", "/getcurrentaddressindices")
        .with_status(200)
        .with_body(r#"{"internal":1,"external":null}"#)
        .create();
    let next_index = AtomicU64::new(2);
    let bump = server
        .mock("POST", "/bumpaddressindices")
        .with_status(200)
        .with_body_from_request(move |_| {
            let first = next_index.fetch_add(1, Ordering::SeqCst);
            format!(r#"{{"first":{first}}}"#).into_bytes()
        })
        .expect(2)
        .create();

    // OnlineData is built by go_online, which for a multisig wallet needs a real hub; borrow it
    // from a singlesig fixture (same indexer and proxy) and swap in the mocked hub
    let mut network_wallet = get_test_reuse_wallet();
    let online = network_wallet
        .go_online(test_go_online_options(None))
        .unwrap();
    let mut network = network_wallet.online_data_mut().take().unwrap();
    network.hub_client = Some(MultisigHubClient::new(&server.url(), "token").unwrap());
    network.user_role = Some(UserRole::Cosigner);
    let keys_1 = generate_keys(BitcoinNetwork::Regtest, WitnessVersion::Taproot);
    let keys_2 = generate_keys(BitcoinNetwork::Regtest, WitnessVersion::Taproot);
    let cosigners = vec![
        Cosigner::from_keys(&keys_1, None),
        Cosigner::from_keys(&keys_2, None),
    ];
    let mut wallet = MultisigWallet::new(
        get_test_reuse_wallet_data(),
        MultisigKeys::new(cosigners, 2, 2),
    )
    .unwrap();
    *wallet.online_data_mut() = Some(network);

    // legacy state (old off-by-one local reveal): local last revealed 2, hub at 1
    for _ in 0..3 {
        wallet
            .bdk_wallet_mut()
            .reveal_next_address(KeychainKind::Internal);
    }
    let old = wallet.get_address(online).unwrap();

    // the first bump returns the local pin (2), rotation must bump again and land on 3
    let rotated = wallet.rotate_vanilla_address(online).unwrap();
    assert_ne!(rotated, old);
    bump.assert();
    assert_eq!(
        wallet.bdk_wallet().derivation_index(KeychainKind::Internal),
        Some(3)
    );
    assert_eq!(wallet.get_address(online).unwrap(), rotated);
}
