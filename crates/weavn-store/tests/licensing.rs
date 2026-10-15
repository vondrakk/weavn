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
