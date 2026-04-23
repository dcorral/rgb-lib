use super::*;

// End-to-end RGB on P2WPKH: fund + create_utxos + issue NIA + blind_receive + send.
// Exercises the OpretFirst commitment on a non-taproot output.
#[cfg(feature = "electrum")]
#[test]
#[parallel]
fn p2wpkh_send_receive_nia() {
    initialize();

    let amount: u64 = 66;

    let (mut wallet, online) = get_funded_wallet_p2wpkh();
    let (mut rcv_wallet, _rcv_online) = get_funded_wallet_p2wpkh();

    let asset = test_issue_asset_nia(&mut wallet, online, None);
    let receive_data = test_blind_receive(&mut rcv_wallet);
    let recipient_map = HashMap::from([(
        asset.asset_id.clone(),
        vec![Recipient {
            assignment: Assignment::Fungible(amount),
            recipient_id: receive_data.recipient_id.clone(),
            witness_data: None,
            transport_endpoints: TRANSPORT_ENDPOINTS.clone(),
        }],
    )]);
    let txid = test_send(&mut wallet, online, &recipient_map);
    assert!(!txid.is_empty());

    let balance = test_get_asset_balance(&wallet, &asset.asset_id);
    assert_eq!(balance.future, AMOUNT - amount);
}

// Cross-type RGB transfer (P2TR → P2WPKH).
#[cfg(feature = "electrum")]
#[test]
#[parallel]
fn cross_type_p2tr_to_p2wpkh() {
    initialize();

    let amount: u64 = 66;

    let (mut wallet, online) = get_funded_wallet!();
    let (mut rcv_wallet, _rcv_online) = get_funded_wallet_p2wpkh();

    let asset = test_issue_asset_nia(&mut wallet, online, None);
    let receive_data = test_blind_receive(&mut rcv_wallet);
    let recipient_map = HashMap::from([(
        asset.asset_id.clone(),
        vec![Recipient {
            assignment: Assignment::Fungible(amount),
            recipient_id: receive_data.recipient_id.clone(),
            witness_data: None,
            transport_endpoints: TRANSPORT_ENDPOINTS.clone(),
        }],
    )]);
    let txid = test_send(&mut wallet, online, &recipient_map);
    assert!(!txid.is_empty());

    let balance = test_get_asset_balance(&wallet, &asset.asset_id);
    assert_eq!(balance.future, AMOUNT - amount);
}
