//! A card's refresh family rotates through one counter: replay is refused and nothing is
//! recorded per refresh.
use billing::{
    card::Card,
    engine::{BillingEngine, RefreshRotation},
};

fn engine() -> BillingEngine {
    let engine = BillingEngine::new();
    engine.upsert_card(Card::new("card", "group", 1_000));
    engine
}

#[test]
fn only_the_current_version_rotates_and_nothing_is_recorded_per_refresh() {
    let e = engine();
    assert_eq!(
        e.rotate_refresh("card", 1, "a", 9_999, 100, 0).unwrap(),
        RefreshRotation::Rotated(2)
    );
    assert!(
        e.rotate_refresh("card", 1, "b", 9_999, 101, 0).is_err(),
        "replay outside any window"
    );
    assert_eq!(
        e.rotate_refresh("card", 1, "c", 9_999, 101, 120).unwrap(),
        RefreshRotation::Reissued(2),
        "one version behind, just after the rotation, is a retry"
    );
    assert!(
        e.rotate_refresh("card", 1, "d", 9_999, 300, 120).is_err(),
        "the window has passed"
    );
    assert_eq!(
        e.rotate_refresh("card", 2, "e", 9_999, 300, 120).unwrap(),
        RefreshRotation::Rotated(3)
    );
    assert!(
        e.rotate_refresh("card", 7, "f", 9_999, 300, 120).is_err(),
        "a version never issued"
    );
    assert!(e.export_snapshot().consumed_refresh_tokens.is_empty());
}

#[test]
fn a_token_used_before_this_release_stays_used() {
    let e = engine();
    // Recorded the way the previous release recorded every refresh.
    e.consume_refresh_token("legacy", 9_999, 100).unwrap();
    assert!(e
        .rotate_refresh("card", 1, "legacy", 9_999, 101, 120)
        .is_err());
    assert_eq!(
        e.rotate_refresh("card", 1, "fresh", 9_999, 101, 120)
            .unwrap(),
        RefreshRotation::Rotated(2)
    );
    // Expired records drain on the next commit instead of accumulating.
    e.rotate_refresh("card", 2, "later", 20_000, 10_000, 120)
        .unwrap();
    assert!(e.export_snapshot().consumed_refresh_tokens.is_empty());
}

#[test]
fn a_sign_in_starts_a_new_family() {
    let e = engine();
    e.activate_card_with_device(
        "card",
        100,
        86_400,
        Some("dev_0123456789abcdef0123456789abcdef"),
    )
    .unwrap();
    assert_eq!(e.get_card("card").unwrap().refresh_version, 2);
    assert!(
        e.rotate_refresh("card", 1, "old", 9_999, 101, 120).is_err(),
        "no retry window reaches across a sign-in"
    );
}
