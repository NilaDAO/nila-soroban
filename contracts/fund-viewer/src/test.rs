#![cfg(test)]

extern crate std;

use super::*;
use soroban_sdk::{testutils::Address as _, Address, Env, String};

#[test]
fn union_registry() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let id = env.register(FundViewerContract, (admin.clone(),));
    let client = FundViewerContractClient::new(&env, &id);
    let union = Address::generate(&env);
    client.create_union(&union, &String::from_str(&env, "Mother Theresa"));
    let rec = client.get_union(&union).unwrap();
    assert!(rec.active);

    client.activate_union(&union, &false);
    assert!(!client.get_union(&union).unwrap().active);
}

#[test]
fn upgrade_is_timelocked() {
    use soroban_sdk::BytesN;
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let id = env.register(FundViewerContract, (admin,));
    let client = FundViewerContractClient::new(&env, &id);
    assert_eq!(client.upgrade_delay(), 48 * 3600);
    client.upgrade(&BytesN::from_array(&env, &[1; 32]));
    assert_eq!(
        client.try_execute_upgrade(),
        Err(Ok(Error::UpgradeNotReady))
    );
    client.cancel_upgrade();
    assert_eq!(client.pending_upgrade(), None);
    env.set_auths(&[]);
    assert!(client
        .try_upgrade(&BytesN::from_array(&env, &[1; 32]))
        .is_err());
}

/// sha256 over the XDR of one sample of every stored key and value. If this changes, data a
/// deployed fund_viewer holds no longer decodes after an upgrade: write a migration, or leave the
/// stored shape alone (docs/STORAGE_LAYOUT.md).
#[test]
fn storage_layout_is_frozen() {
    use soroban_sdk::{xdr::ToXdr, BytesN, IntoVal, Val};
    let env = Env::default();
    let a = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    #[allow(unused_variables)]
    let h = BytesN::from_array(&env, &[7; 32]);
    let mut v: soroban_sdk::Vec<Val> = soroban_sdk::Vec::new(&env);
    v.push_back(DataKey::Admin.into_val(&env));
    v.push_back(DataKey::Union(a.clone()).into_val(&env));
    v.push_back(
        UnionRecord {
            id: a.clone(),
            display_name: soroban_sdk::String::from_str(&env, "u"),
            active: true,
            created_ts: 1,
        }
        .into_val(&env),
    );
    for k in [
        nila_common::upgrade::UpgradeKey::UpgDelay,
        nila_common::upgrade::UpgradeKey::UpgPending,
        nila_common::upgrade::UpgradeKey::UpgPendingDelay,
    ] {
        v.push_back(k.into_val(&env));
    }
    let got: BytesN<32> = env.crypto().sha256(&v.to_xdr(&env)).into();
    let hex: std::string::String = got
        .to_array()
        .iter()
        .map(|b| std::format!("{b:02x}"))
        .collect();
    assert_eq!(
        hex, "5aee22acdc81c2871c28f340662055c74ccaf53d763b0b3aab086d2a6505d896",
        "fund_viewer storage layout changed"
    );
}
