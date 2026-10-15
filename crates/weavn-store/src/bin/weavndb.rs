//! weavndb — admin CLI for the Weavn operational store.
//!
//! Needs DATABASE_URL (a Postgres / Supabase connection string).
//!   weavndb ping
//!   weavndb migrate
//!   weavndb config set <ws> <key> <value> | get <ws> <key> | list <ws>
//!   weavndb emoji add <ws> <shortcode> <image_ref> <created_by> [allowed_csv]
//!   weavndb emoji list <ws>
//!   weavndb emoji can  <ws> <shortcode> <account>

use weavn_store as store;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let url = std::env::var("DATABASE_URL")
        .map_err(|_| "set DATABASE_URL (the Supabase/Postgres connection string)".to_string())?;
    let pool = store::connect(&url).await.map_err(|e| e.to_string())?;
    let e = |x: sqlx::Error| x.to_string();

    match a.as_slice() {
        ["ping"] => {
            sqlx::query("select 1").execute(&pool).await.map_err(e)?;
            println!("db reachable");
        }
        ["migrate"] => {
            store::migrate(&pool).await.map_err(|x| x.to_string())?;
            println!("migrations applied");
        }
        ["config", "set", ws, key, value] => {
            store::config_set(&pool, ws, key, value).await.map_err(e)?;
            println!("set {key}");
        }
        ["config", "get", ws, key] => {
            let v = store::config_get(&pool, ws, key).await.map_err(e)?;
            println!("{}", v.unwrap_or_else(|| "(unset)".into()));
        }
        ["config", "list", ws] => {
            for (k, v) in store::config_list(&pool, ws).await.map_err(e)? {
                println!("{k} = {v}");
            }
        }
        ["emoji", "add", ws, shortcode, image, by, rest @ ..] => {
            let allowed: Vec<String> = rest
                .first()
                .map(|csv| csv.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            store::emoji_register(&pool, ws, shortcode, image, by, &allowed).await.map_err(e)?;
            let who = if allowed.is_empty() { "anyone".to_string() } else { allowed.join(", ") };
            println!("registered {shortcode} (usable by: {who})");
        }
        ["emoji", "list", ws] => {
            for (sc, allowed) in store::emoji_list(&pool, ws).await.map_err(e)? {
                println!("{sc} — {}", allowed.unwrap_or_else(|| "anyone".into()));
            }
        }
        ["emoji", "can", ws, shortcode, account] => {
            let ok = store::emoji_can_use(&pool, ws, shortcode, account).await.map_err(e)?;
            println!("{}", if ok { "yes" } else { "no" });
        }

        // --- licensing & seat metering (docs/LICENSING.md) ------------------
        ["license", "grant", ws, seats, rest @ ..] => {
            let n: i64 = seats.parse().map_err(|_| "seats must be an integer".to_string())?;
            let opt = |i: usize| rest.get(i).copied().filter(|s| !s.is_empty() && *s != "-");
            store::license_set(&pool, ws, n, opt(0), opt(1), opt(2), opt(3)).await.map_err(e)?;
            println!("license set for {ws}: {n} seats");
        }
        ["license", "show", ws] => {
            let r = store::usage_report(&pool, ws).await.map_err(e)?;
            println!("workspace : {ws}");
            println!("entitled  : {} ({}{})", r.entitled, r.source,
                r.plan.map(|p| format!(", plan={p}")).unwrap_or_default());
            if r.licensed {
                println!("window    : {} .. {}",
                    r.starts_at.unwrap_or_else(|| "-".into()),
                    r.ends_at.unwrap_or_else(|| "open-ended".into()));
                if let Some(s) = r.status { println!("status    : {s}"); }
            }
            println!("active    : {} (distinct ever: {})", r.active_seats, r.total_seats);
            println!("overage   : {}{}", r.over,
                if r.over > 0 { "   <-- right-size the license" } else { "" });
        }
        ["license", "revoke", ws] => {
            let ok = store::license_clear(&pool, ws).await.map_err(e)?;
            println!("{}", if ok { "license removed" } else { "no license for that workspace" });
        }
        ["license", "list"] => {
            let rows = store::fleet(&pool).await.map_err(e)?;
            if rows.is_empty() {
                println!("(no licensed or metered namespaces yet)");
            }
            for (ws, r) in rows {
                println!(
                    "{:<44} active {:>4} / entitled {:>4} ({:<7}) {}",
                    ws, r.active_seats, r.entitled, r.source,
                    if r.over > 0 { format!("OVER by {}", r.over) } else { "ok".into() },
                );
            }
        }
        ["seat", "history", ws] => {
            for ev in store::seat_history(&pool, ws).await.map_err(e)? {
                println!("{}  {:<12} {}  {}", ev.at, ev.event, ev.user_id, ev.note.unwrap_or_default());
            }
        }
        ["seat", "list", ws] => {
            for s in store::seats(&pool, ws).await.map_err(e)? {
                println!("{} {}  {}  {}  last_seen={}",
                    if s.active { "*" } else { " " }, s.user_id, s.display,
                    s.email.unwrap_or_default(), s.last_seen);
            }
        }
        ["seat", "release", ws, user_id] => {
            let ok = store::seat_release(&pool, ws, user_id).await.map_err(e)?;
            println!("{}", if ok { "seat released" } else { "no active seat for that user" });
        }

        _ => {
            eprintln!("{}", USAGE);
            return Err("unknown command".into());
        }
    }
    Ok(())
}

const USAGE: &str = "\
weavndb — operational store admin (needs DATABASE_URL)
  weavndb ping
  weavndb migrate
  weavndb config set <ws> <key> <value> | get <ws> <key> | list <ws>
  weavndb emoji add <ws> <shortcode> <image_ref> <created_by> [allowed_csv]
  weavndb emoji list <ws> | can <ws> <shortcode> <account>
  weavndb license grant <ws> <seats> [starts YYYY-MM-DD|-] [ends|-] [plan] [note]
  weavndb license show <ws> | revoke <ws> | list
  weavndb seat list <ws> | release <ws> <user_id> | history <ws>";
