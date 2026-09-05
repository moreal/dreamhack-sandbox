//! Human-readable tables and `--json` rendering shared by every CLI command
//! (DESIGN.md §7).

use comfy_table::{presets::UTF8_FULL, Table};
use serde::Serialize;

use wargame_core::app::Check;
use wargame_core::domain::{Challenge, Profile, Session};
use wargame_core::ports::SearchPage;

/// Pretty-prints any `Serialize` value as JSON to stdout (the `--json` path
/// every command below shares).
pub fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(rendered) => println!("{rendered}"),
        Err(e) => eprintln!("Error: failed to render JSON: {e}"),
    }
}

pub fn print_search_page(page: &SearchPage, json: bool) {
    if json {
        print_json(page);
        return;
    }
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(vec!["ref", "title", "categories", "difficulty"]);
    for item in &page.items {
        table.add_row(vec![
            item.r#ref.to_string(),
            item.title.clone(),
            item.categories.join(", "),
            item.difficulty.clone().unwrap_or_default(),
        ]);
    }
    println!("{table}");
    println!(
        "{} result(s); next page: {}",
        page.total,
        page.next_page
            .map(|p| p.to_string())
            .unwrap_or_else(|| "none".to_string())
    );
}

pub fn print_challenge(challenge: &Challenge, json: bool) {
    if json {
        print_json(challenge);
        return;
    }
    println!("{} ({})", challenge.summary.title, challenge.summary.r#ref);
    println!("categories: {}", challenge.summary.categories.join(", "));
    if let Some(d) = &challenge.summary.difficulty {
        println!("difficulty: {d}");
    }
    println!();
    println!("{}", challenge.description);
    if !challenge.assets.is_empty() {
        println!();
        println!("assets:");
        for asset in &challenge.assets {
            println!("  - {}", asset.filename);
        }
    }
}

pub fn print_sessions(sessions: &[Session], json: bool) {
    if json {
        print_json(&sessions);
        return;
    }
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(vec!["id", "title", "state", "network", "profile"]);
    for s in sessions {
        table.add_row(vec![
            s.id.clone(),
            s.display_name.clone(),
            format!("{:?}", s.state).to_lowercase(),
            format!("{:?}", s.network).to_lowercase(),
            s.profile.clone(),
        ]);
    }
    println!("{table}");
}

pub fn print_session(session: &Session, json: bool) {
    if json {
        print_json(session);
        return;
    }
    println!("id: {}", session.id);
    println!("title: {}", session.display_name);
    println!("state: {:?}", session.state);
    println!("profile: {}", session.profile);
    println!("network: {:?}", session.network);
    if let Some(ip) = session.guest_ip {
        println!("guest ip: {ip}");
    }
    if !session.assets.is_empty() {
        println!("assets:");
        for a in &session.assets {
            println!("  - {} ({} bytes, sha256 {})", a.filename, a.size, a.sha256);
        }
    }
    if let Some(err) = &session.last_error {
        println!("last error: {err}");
    }
}

/// [`Check`] mirror that adds `Serialize` without requiring
/// `wargame-core::app::doctor` (Task 5's file) to derive it itself.
#[derive(Serialize)]
struct CheckJson<'a> {
    name: &'a str,
    ok: bool,
    detail: &'a str,
    hint: Option<&'a str>,
}

pub fn print_checks(checks: &[Check], json: bool) {
    if json {
        let rendered: Vec<CheckJson> = checks
            .iter()
            .map(|c| CheckJson {
                name: &c.name,
                ok: c.ok,
                detail: &c.detail,
                hint: c.hint.as_deref(),
            })
            .collect();
        print_json(&rendered);
        return;
    }
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(vec!["check", "ok", "detail"]);
    for c in checks {
        table.add_row(vec![
            c.name.clone(),
            if c.ok { "yes" } else { "no" }.to_string(),
            c.detail.clone(),
        ]);
    }
    println!("{table}");
    for c in checks.iter().filter(|c| !c.ok) {
        if let Some(hint) = &c.hint {
            println!("Try: {hint}");
        }
    }
}

pub fn print_profiles(profiles: &[Profile], json: bool) {
    if json {
        print_json(&profiles);
        return;
    }
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(vec!["name", "description", "guest", "resources"]);
    for p in profiles {
        table.add_row(vec![
            p.name.clone(),
            p.description.clone(),
            format!("{:?}/{:?}", p.guest.os, p.guest.arch),
            format!(
                "{} cpu / {} MiB / {} GiB disk",
                p.resources.cpu, p.resources.memory_mib, p.resources.disk_gib
            ),
        ]);
    }
    println!("{table}");
}
