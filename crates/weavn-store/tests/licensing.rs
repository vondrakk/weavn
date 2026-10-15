//! Licensing/seat-metering integration test (docs/LICENSING.md).
//! Runs only when DATABASE_URL is set; uses a unique workspace and cleans up.
//! Verifies the "meter, never deny" contract: usage can exceed entitlement, the
//! overage is reported, and seats are reclaimable (release / reactivate).

use weavn_store as store;

async fn cleanup(pool: &store::Pool, ws: &str) {
    for t in ["seat", "seat_event", "license"] {
        let _ = sqlx::query(&format!("delete from {t} where workspace = $1"))
            .bind(ws)
            .execute(pool)
            .await;
    }
    let _ = sqlx::query("delete from config where workspace_id = $1")
        .bind(ws)
        .execute(pool)
        .await;
}

#[tokio::test]
async fn meter_never_deny_and_reclaim() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL unset — skipping licensing integration test");
        return;
    };
    let pool = store::connect(&url).await.expect("connect");
    store::migrate(&pool).await.expect("migrate");
    let ws = format!("test://licensing-{}", std::process::id());
    cleanup(&pool, &ws).await;

    // Free tier: entitlement is 10, two sign-ins are well within it.
    let a = store::seat_claim(&pool, &ws, "acc-1", "Ada", Some("ada@x.io")).await.unwrap();
    assert!(a.newly_claimed && !a.licensed && a.entitled == store::FREE_SEATS);
    let a2 = store::seat_claim(&pool, &ws, "acc-1", "Ada", Some("ada@x.io")).await.unwrap();
    assert!(!a2.newly_claimed, "same user re-signin does not re-claim");
    store::seat_claim(&pool, &ws, "acc-2", "Bo", None).await.unwrap();
    assert_eq!(store::seat_active_count(&pool, &ws).await.unwrap(), 2);

    // Grant a 1-seat license: now 2 active > 1 entitled, but claims are NEVER denied.
    store::license_set(&pool, &ws, 1, Some("2026-10-14"), Some("2030-01-01"), Some("team"), None)
        .await.unwrap();
    let c = store::seat_claim(&pool, &ws, "acc-3", "Cy", None).await.unwrap();
    assert!(c.licensed && c.entitled == 1, "entitlement is the licensed seat count");
    assert_eq!(c.active_seats, 3);
    assert_eq!(c.over, 2, "overage = active - entitled, reported not enforced");

    let rep = store::usage_report(&pool, &ws).await.unwrap();
    assert_eq!((rep.active_seats, rep.entitled, rep.over), (3, 1, 2));

    // Reclaim: release one (quit) frees a seat; a re-sign-in reactivates it.
    assert!(store::seat_release(&pool, &ws, "acc-3").await.unwrap());
    assert_eq!(store::seat_active_count(&pool, &ws).await.unwrap(), 2);
    let re = store::seat_claim(&pool, &ws, "acc-3", "Cy", None).await.unwrap();
    assert!(re.newly_claimed, "reactivation counts as a new claim");
    assert_eq!(store::seat_active_count(&pool, &ws).await.unwrap(), 3);

    // Expired license falls back to the free entitlement.
    store::license_set(&pool, &ws, 50, Some("2000-01-01"), Some("2000-12-31"), None, None)
        .await.unwrap();
    assert!(!store::license_is_current(&pool, &ws).await.unwrap());
    assert_eq!(store::entitlement(&pool, &ws).await.unwrap().entitled, store::FREE_SEATS);

    cleanup(&pool, &ws).await;
}

/// The hosted free-tier HARD cap: a new user beyond the free seats is refused on an
/// unlicensed namespace, existing users still re-admit, and a commercial license
/// lifts the block (metered, never denied).
#[tokio::test]
async fn enforced_free_cap_blocks_then_license_lifts() {
    use store::Admission;
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL unset — skipping enforced-cap test");
        return;
    };
    let pool = store::connect(&url).await.expect("connect");
    store::migrate(&pool).await.expect("migrate");
    let ws = format!("test://cap-{}", std::process::id());
    cleanup(&pool, &ws).await;

    // Shrink the free cap to 2 for a fast test.
    store::config_set(&pool, &ws, "license.free_seats", "2").await.unwrap();

    // Two new users fit within the free cap.
    for acc in ["u1", "u2"] {
        match store::seat_claim_enforced(&pool, &ws, acc, acc, None).await.unwrap() {
            Admission::Admitted(c) => assert!(c.newly_claimed && !c.licensed),
            Admission::Full { .. } => panic!("{acc} should be admitted within the free cap"),
        }
    }
    assert_eq!(store::seat_active_count(&pool, &ws).await.unwrap(), 2);

    // The 3rd NEW user is refused, and no seat is created.
    match store::seat_claim_enforced(&pool, &ws, "u3", "u3", None).await.unwrap() {
        Admission::Full { active_seats, entitled } => assert_eq!((active_seats, entitled), (2, 2)),
        Admission::Admitted(_) => panic!("u3 should be blocked by the free cap"),
    }
    assert_eq!(store::seat_active_count(&pool, &ws).await.unwrap(), 2, "blocked user leaves no seat");

    // An existing user re-signing in is always admitted, even at the cap.
    match store::seat_claim_enforced(&pool, &ws, "u1", "u1", None).await.unwrap() {
        Admission::Admitted(c) => assert!(!c.newly_claimed),
        Admission::Full { .. } => panic!("existing user must never be blocked"),
    }

    // A commercial license lifts the cap: the new user is admitted and metered.
    store::license_set(&pool, &ws, 2, Some("2026-01-01"), Some("2030-01-01"), Some("commercial"), None)
        .await.unwrap();
    match store::seat_claim_enforced(&pool, &ws, "u3", "u3", None).await.unwrap() {
        Admission::Admitted(c) => {
            assert!(c.licensed && c.active_seats == 3 && c.over == 1, "licensed: admitted + overage reported");
        }
        Admission::Full { .. } => panic!("a licensed namespace is never blocked"),
    }

    cleanup(&pool, &ws).await;
}
