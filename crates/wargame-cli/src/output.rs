//! Human-readable tables and `--json` rendering shared by every CLI command
//! (DESIGN.md §7).

use comfy_table::{presets::UTF8_FULL_CONDENSED, Table};
use serde::Serialize;
use time::OffsetDateTime;

use wargame_core::app::Check;
use wargame_core::domain::{Challenge, KeepPolicy, Profile, Session};
use wargame_core::ports::SearchPage;

/// Pretty-prints any `Serialize` value as JSON to stdout (the `--json` path
/// every command below shares).
pub fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(rendered) => println!("{rendered}"),
        Err(e) => eprintln!("Error: failed to render JSON: {e}"),
    }
}

fn table() -> Table {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table
}

/// A compact "how long ago" for tables: `just now`, `42s`, `7m`, `3h`, `2d`.
pub fn age(since: OffsetDateTime, now: OffsetDateTime) -> String {
    let secs = (now - since).whole_seconds().max(0);
    match secs {
        0..=4 => "just now".to_string(),
        5..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

pub fn print_search_page(page: &SearchPage, json: bool) {
    if json {
        print_json(page);
        return;
    }
    if page.items.is_empty() {
        println!("No challenges matched.");
        println!("Try: a shorter or different query, or drop --category");
        return;
    }
    let mut table = table();
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
    match page.next_page {
        Some(next) => println!(
            "{} result(s) in total; add --page {next} for the next page.",
            page.total
        ),
        None => println!("{} result(s).", page.total),
    }
    if let Some(first) = page.items.first() {
        println!(
            "Next: wargame open {0}   # or: wargame launch {0}",
            first.r#ref
        );
    }
}

pub fn print_challenge(challenge: &Challenge, json: bool) {
    if json {
        print_json(challenge);
        return;
    }
    let r#ref = &challenge.summary.r#ref;
    println!("{} ({})", challenge.summary.title, r#ref);
    println!("categories: {}", challenge.summary.categories.join(", "));
    if let Some(d) = &challenge.summary.difficulty {
        println!("difficulty: {d}");
    }
    if !challenge.source_url.is_empty() {
        println!("url: {}", challenge.source_url);
    }
    println!();
    println!("{}", challenge.description.trim_end());
    if !challenge.assets.is_empty() {
        println!();
        println!("files:");
        for asset in &challenge.assets {
            println!("  - {}", asset.filename);
        }
    }
    if !challenge.remote_endpoints.is_empty() {
        println!();
        println!("remote endpoints (need --network nat to reach):");
        for endpoint in &challenge.remote_endpoints {
            println!("  - {endpoint}");
        }
    }
    println!();
    println!("Next: wargame launch {ref}");
}

fn keep_marker(keep: KeepPolicy) -> &'static str {
    match keep {
        KeepPolicy::Keep => "yes",
        KeepPolicy::Discard => "",
    }
}

pub fn print_sessions(sessions: &[Session], json: bool) {
    if json {
        print_json(&sessions);
        return;
    }
    if sessions.is_empty() {
        println!("No sessions yet.");
        println!("Try: wargame search <query>   # then: wargame launch <provider:id>");
        return;
    }
    let now = OffsetDateTime::now_utc();
    let mut table = table();
    table.set_header(vec![
        "id", "title", "state", "network", "profile", "keep", "age",
    ]);
    for s in sessions {
        table.add_row(vec![
            s.id.clone(),
            s.display_name.clone(),
            s.state.to_string(),
            s.network.to_string(),
            s.profile.clone(),
            keep_marker(s.keep).to_string(),
            age(s.created_at, now),
        ]);
    }
    println!("{table}");
    let up = sessions.iter().filter(|s| s.state.is_up()).count();
    let failed = sessions
        .iter()
        .filter(|s| s.state == wargame_core::domain::SessionState::Failed)
        .count();
    let mut summary = format!("{} session(s), {up} up", sessions.len());
    if failed > 0 {
        summary.push_str(&format!(", {failed} failed"));
    }
    println!("{summary}. Ids may be shortened to any unique prefix.");
}

pub fn print_session(session: &Session, log_path: Option<&std::path::Path>, json: bool) {
    if json {
        print_json(session);
        return;
    }
    let now = OffsetDateTime::now_utc();
    println!("id:        {}", session.id);
    println!(
        "challenge: {} ({})",
        session.display_name, session.challenge.r#ref
    );
    println!("state:     {}", session.state);
    println!("profile:   {}", session.profile);
    println!("network:   {}", session.network);
    println!(
        "resources: {} cpu / {} MiB",
        session.resources.cpu, session.resources.memory_mib
    );
    let created = session
        .created_at
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    match age(session.created_at, now).as_str() {
        "just now" => println!("created:   {created} (just now)"),
        ago => println!("created:   {created} ({ago} ago)"),
    }
    println!(
        "keep:      {}",
        match session.keep {
            KeepPolicy::Keep => "yes (survives prune; delete needs --force)",
            KeepPolicy::Discard => "no",
        }
    );
    if let Some(vm) = &session.vm_ref {
        println!("vm:        {} ({})", vm.name, vm.backend);
    }
    if let Some(ip) = session.guest_ip {
        println!("guest ip:  {ip}");
    }
    if let Some(log) = log_path {
        println!("log:       {}", log.display());
    }
    if !session.assets.is_empty() {
        println!("files:");
        for a in &session.assets {
            println!("  - {} ({} bytes, sha256 {})", a.filename, a.size, a.sha256);
        }
    }
    if let Some(err) = &session.last_error {
        println!("last error: {err}");
    }
    println!();
    println!("Next: {}", next_step_for(session));
}

/// The single most useful follow-up command for a session in its current
/// state, shown under `session inspect` and the launch summary.
pub fn next_step_for(session: &Session) -> String {
    use wargame_core::domain::SessionState::*;
    let id = &session.id;
    match session.state {
        Ready | Running => format!("wargame ssh {id}   # or: wargame gui {id}"),
        Stopped => format!("wargame resume {id}   # or: wargame delete {id}"),
        Failed => format!("wargame delete {id}   # discard it and launch again"),
        _ => format!("wargame sessions   # wait for {id} to settle"),
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
    let mut table = table();
    table.set_header(vec!["", "check", "detail"]);
    for c in checks {
        table.add_row(vec![
            if c.ok { "✓" } else { "✗" }.to_string(),
            c.name.clone(),
            c.detail.clone(),
        ]);
    }
    println!("{table}");
    let failing: Vec<&Check> = checks.iter().filter(|c| !c.ok).collect();
    if failing.is_empty() {
        println!("All {} checks passed.", checks.len());
        return;
    }
    println!("{} of {} checks failed:", failing.len(), checks.len());
    for c in failing {
        match &c.hint {
            Some(hint) => println!("  {}: {hint}", c.name),
            None => println!("  {}: {}", c.name, c.detail),
        }
    }
}

/// `profiles`, with one image-readiness line per profile (spec.md §3 FR-1:
/// "profiles ... 화면에 OS/arch/backend/mode와 이미지 준비 상태를 표시한다").
/// `status` is parallel to `profiles`; `None` means the status could not be
/// determined (an unsupported combination, say).
pub fn print_profiles(
    profiles: &[Profile],
    status: &[Option<String>],
    default_profile: &str,
    json: bool,
) {
    if json {
        print_json(&profiles);
        return;
    }
    let mut table = table();
    table.set_header(vec![
        "name",
        "description",
        "guest",
        "backend/mode",
        "resources",
        "image",
        "default",
    ]);
    for (i, p) in profiles.iter().enumerate() {
        table.add_row(vec![
            p.name.clone(),
            p.description.clone(),
            format!(
                "{:?}/{:?}",
                p.guest
                    .distribution
                    .clone()
                    .unwrap_or(format!("{:?}", p.guest.os)),
                p.guest.arch
            )
            .replace('"', "")
            .to_lowercase(),
            format!("{}/{}", p.vm.backend, p.vm.mode),
            format!(
                "{} cpu / {} GiB / {} GiB disk",
                p.resources.cpu,
                p.resources.memory_mib / 1024,
                p.resources.disk_gib
            ),
            status
                .get(i)
                .cloned()
                .flatten()
                .unwrap_or_else(|| "unknown".to_string()),
            if p.name == default_profile { "yes" } else { "" }.to_string(),
        ]);
    }
    println!("{table}");
    println!("Next: wargame profile build <name>   # once per profile; then wargame launch <ref> --profile <name>");
    println!("      wargame profile init <template> --name <new>   # a UTM template (Kali, Windows) to install yourself");
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    #[test]
    fn age_buckets() {
        let now = OffsetDateTime::now_utc();
        assert_eq!(age(now, now), "just now");
        assert_eq!(age(now - Duration::seconds(42), now), "42s");
        assert_eq!(age(now - Duration::minutes(7), now), "7m");
        assert_eq!(age(now - Duration::hours(3), now), "3h");
        assert_eq!(age(now - Duration::days(2), now), "2d");
        // A clock that went backwards must not panic or print nonsense.
        assert_eq!(age(now + Duration::minutes(5), now), "just now");
    }
}
