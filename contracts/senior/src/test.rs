extern crate std;

use super::*;
use soroban_sdk::{testutils::Address as _, Env};

fn setup() -> (Env, SeniorContractClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let a = || Address::generate(&env);
    let id = env.register(SeniorContract, (admin.clone(), a(), a(), a(), a()));
    (env.clone(), SeniorContractClient::new(&env, &id), admin)
}

#[test]
fn deposits_wait_for_the_cap_and_residency_is_set_once() {
    let (env, s, _) = setup();
    let inv = Address::generate(&env);
    let union = Address::generate(&env);
    let kyc = BytesN::from_array(&env, &[1; 32]);
    // Not accepted.
    assert_eq!(
        s.try_deposit(&inv, &union, &10),
        Err(Ok(Error::NotAccepted))
    );
    s.set_investor(&inv, &true, &kyc, &Tag::Domestic);
    // Unaudited: no deposits until the trust sets a cap.
    assert_eq!(s.cap(), 0);
    assert_eq!(s.try_deposit(&inv, &union, &10), Err(Ok(Error::AboveCap)));
    assert_eq!(
        s.try_set_investor(&inv, &true, &kyc, &Tag::Offshore),
        Err(Ok(Error::TagFixed))
    );
    s.set_investor(&inv, &false, &kyc, &Tag::Domestic);
    assert!(!s.investor(&inv).unwrap().accepted);
    // Paused stops deposits, exits and payouts.
    s.set_paused(&true);
    assert_eq!(s.try_deposit(&inv, &union, &10), Err(Ok(Error::Paused)));
    assert_eq!(s.try_redeem(&inv, &union, &0), Err(Ok(Error::Paused)));
    assert_eq!(
        s.try_pay_income(&union, &inv, &inv, &inv, &1, &1),
        Err(Ok(Error::Paused))
    );
    assert_eq!(s.try_mint_guaranteed(&union, &1), Err(Ok(Error::Paused)));
    // The vault is set once.
    s.set_paused(&false);
    assert_eq!(s.try_mint_guaranteed(&union, &1), Err(Ok(Error::NoVault)));
    s.set_vault(&inv);
    assert_eq!(s.try_set_vault(&union), Err(Ok(Error::VaultAlreadySet)));
    assert_eq!(s.vault(), Some(inv));
}

#[test]
fn only_the_trust_sets_cap_investors_vault_and_upgrades() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let a = || Address::generate(&env);
    let id = env.register(SeniorContract, (admin.clone(), a(), a(), a(), a()));
    let s = SeniorContractClient::new(&env, &id);
    let kyc = BytesN::from_array(&env, &[1; 32]);
    let x = Address::generate(&env);
    assert!(s.try_set_cap(&1).is_err());
    assert!(s.try_set_investor(&x, &true, &kyc, &Tag::Offshore).is_err());
    assert!(s.try_set_vault(&x).is_err());
    assert!(s.try_set_paused(&true).is_err());
    assert!(s.try_upgrade(&kyc).is_err());
    assert_eq!(s.upgrade_delay(), UPGRADE_DELAY);
}

#[test]
fn storage_layout_is_frozen() {
    use soroban_sdk::{xdr::ToXdr, IntoVal, Val};
    let env = Env::default();
    let a = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    let h = BytesN::from_array(&env, &[7; 32]);
    let mut v: soroban_sdk::Vec<Val> = soroban_sdk::Vec::new(&env);
    for k in [
        DataKey::Admin,
        DataKey::Usdc,
        DataKey::FundCore,
        DataKey::FxPool,
        DataKey::Roles,
        DataKey::Paused,
        DataKey::Cap,
        DataKey::Total,
        DataKey::Vault,
        DataKey::Investor(a.clone()),
        DataKey::Principal(a.clone(), a.clone()),
        DataKey::UnionUsdc(a.clone(), Tag::Offshore),
        DataKey::UnionUsdc(a.clone(), Tag::Domestic),
    ] {
        v.push_back(k.into_val(&env));
    }
    v.push_back(
        Investor {
            accepted: true,
            kyc_ref: h.clone(),
            tag: Tag::Domestic,
        }
        .into_val(&env),
    );
    v.push_back(Principal { usdc: 1, nin: 2 }.into_val(&env));
    let got: BytesN<32> = env.crypto().sha256(&v.to_xdr(&env)).into();
    let hex: std::string::String = got
        .to_array()
        .iter()
        .map(|b| std::format!("{b:02x}"))
        .collect();
    assert_eq!(
        hex, "cde7d2cb4bdba0377376ccb27347b145637bd9eb7944dc622903b13e5843cd3f",
        "senior storage layout changed"
    );
}
