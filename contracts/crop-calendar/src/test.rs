#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, vec, Address, Env};

/// 00:00 UTC of a date.
fn at(y: i64, m: u32, d: u32) -> u64 {
    days_from_civil(y, m, d) as u64 * DAY
}

fn setup() -> (Env, CropCalendarContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let id = env.register(CropCalendarContract, (admin,));
    let c = CropCalendarContractClient::new(&env, &id);
    (env, c)
}

#[test]
fn the_dates_are_civil_dates() {
    assert_eq!(at(1970, 1, 1), 0);
    assert_eq!(at(2027, 6, 1), 1_811_808_000);
    assert_eq!(year_month((at(2028, 2, 29) / DAY) as i64), (2028, 2));
    assert_eq!(year_month((at(2026, 12, 31) / DAY) as i64), (2026, 12));
}

/// Maize: Jun–Jul, Oct–Nov, Jan–Feb. A June sowing opens on 25 May (a week before 1 June) and
/// closes at the end of 31 July (executive decision 25 Oct).
#[test]
fn a_share_opens_a_week_before_its_sowing_window_and_closes_on_its_last_day() {
    let (env, c) = setup();
    let maize = vec![
        &env,
        Window { first: 6, last: 7 },
        Window {
            first: 10,
            last: 11,
        },
        Window { first: 1, last: 2 },
    ];
    c.set_windows(&6, &maize);
    assert_eq!(
        c.window(&6000, &(at(2027, 6, 20) + 3600)),
        Some((at(2027, 5, 25), at(2027, 8, 1) - 1))
    );
    assert_eq!(
        c.window(&6001, &at(2026, 11, 3)),
        Some((at(2026, 9, 24), at(2026, 12, 1) - 1))
    );
    // Not one of its sowing months.
    assert_eq!(c.window(&6000, &at(2027, 4, 1)), None);
    // A family without a calendar.
    assert_eq!(c.window(&9000, &at(2027, 6, 1)), None);
}

/// A window over the new year (sesame in navarai: Dec–Feb), from either side of it.
#[test]
fn a_window_runs_over_the_new_year() {
    let (env, c) = setup();
    c.set_windows(
        &2,
        &vec![
            &env,
            Window { first: 12, last: 2 },
            Window { first: 3, last: 3 },
        ],
    );
    let w = Some((at(2026, 11, 24), at(2027, 3, 1) - 1));
    assert_eq!(c.window(&2000, &at(2026, 12, 10)), w);
    assert_eq!(c.window(&2000, &at(2027, 2, 10)), w);
    assert_eq!(
        c.window(&2000, &at(2027, 3, 5)),
        Some((at(2027, 2, 22), at(2027, 4, 1) - 1))
    );
}

#[test]
fn windows_are_months_and_never_overlap() {
    let (env, c) = setup();
    assert_eq!(
        c.try_set_windows(&1, &vec![&env, Window { first: 0, last: 2 }])
            .unwrap_err(),
        Ok(Error::BadWindow)
    );
    assert_eq!(
        c.try_set_windows(
            &1,
            &vec![
                &env,
                Window { first: 12, last: 2 },
                Window { first: 2, last: 3 }
            ]
        )
        .unwrap_err(),
        Ok(Error::BadWindow)
    );
    c.set_windows(&1, &vec![&env, Window { first: 6, last: 7 }]);
    c.set_windows(&1, &vec![&env]);
    assert_eq!(c.windows(&1).len(), 0);
}

/// A stand-in roles contract: `leader` leads `union`, nobody else leads anything.
mod mock_roles {
    use soroban_sdk::{contract, contractimpl, contracttype, Address, Env};
    #[contracttype]
    pub enum K {
        Leader,
        Union,
    }
    #[contract]
    pub struct MockRoles;
    #[contractimpl]
    impl MockRoles {
        pub fn __constructor(env: Env, union: Address, leader: Address) {
            env.storage().instance().set(&K::Union, &union);
            env.storage().instance().set(&K::Leader, &leader);
        }
        pub fn is_leader(env: Env, union: Address, account: Address) -> bool {
            env.storage().instance().get::<_, Address>(&K::Union) == Some(union)
                && env.storage().instance().get::<_, Address>(&K::Leader) == Some(account)
        }
    }
}

/// A leader of the union sets its local price per crop family (26 Oct); another union's
/// price is its own, and a non-leader is refused.
#[test]
fn a_leader_sets_the_unions_local_price() {
    let (env, c) = setup();
    let union = Address::generate(&env);
    let leader = Address::generate(&env);
    let roles = env.register(mock_roles::MockRoles, (union.clone(), leader.clone()));
    // No roles contract yet: nobody can set a price.
    assert_eq!(
        c.try_set_price(&leader, &union, &9, &6_780_u32),
        Err(Ok(Error::NotLeader))
    );
    c.set_roles(&roles);
    c.set_price(&leader, &union, &9, &6_780_u32);
    let p = c.price(&union, &9).unwrap();
    assert_eq!(p.paise_per_kg, 6_780);
    // Another union, another crop: nothing set.
    assert_eq!(c.price(&Address::generate(&env), &9), None);
    assert_eq!(c.price(&union, &6), None);
    // Not a leader of the union.
    assert_eq!(
        c.try_set_price(&Address::generate(&env), &union, &9, &1_u32),
        Err(Ok(Error::NotLeader))
    );
    // 0 removes it.
    c.set_price(&leader, &union, &9, &0_u32);
    assert_eq!(c.price(&union, &9), None);
}
