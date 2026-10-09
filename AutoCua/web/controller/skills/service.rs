//! The `skills` tool, the <skills> block and the <domain_knowledge> block,
//! both inside <persistent_memory>. Two different things:
//!
//! - skills (default_skills/web/skills): listed by id with their size, loaded
//!   into the input and removed again only when the agent asks (a run_script
//!   scrape loads the scraping skill when it opens scrape mode);
//! - domain knowledge (default_skills/web/domain_skill, tied to sites by
//!   skills.json: {"docs.google.com/spreadsheets": "google_sheets.md"}): no
//!   ids, no tool; it comes with the current tab's site and goes with it,
//!   on its own (`follow_url`).
//!
//! The state is the run's, not the history's: it survives scrape mode's
//! history retirement.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::agent::main_driver::view::py_str_of;
use crate::controller::service::{err, py_int_value, ActErr, ActResult};

/// The skill a scrape read needs, loaded as the mode opens (`load_named`).
pub const SCRAPING_SKILL: &str = "scraping.md";

struct Skill {
    name: String,
    path: PathBuf,
    /// About chars / 4, shown on the skill's line so the cost is known before a load.
    tokens: usize,
    loaded: bool,
}

struct Domain {
    name: String,
    path: PathBuf,
    /// The sites it belongs to: url prefixes without a scheme.
    sites: Vec<String>,
    /// The current tab is on one of its sites.
    on_site: bool,
}

static SKILLS: Mutex<Vec<Skill>> = Mutex::new(Vec::new());
static DOMAIN: Mutex<Vec<Domain>> = Mutex::new(Vec::new());

/// A run starts with the skill files of `dir` (in name order), none loaded,
/// and the domain knowledge `domain_dir`/skills.json names, none on.
pub fn new_run(dir: &Path, domain_dir: &Path) {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "md"))
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    let skills = paths
        .into_iter()
        .map(|path| {
            let name = file_name(&path);
            let chars = std::fs::read_to_string(&path).map(|s| s.chars().count()).unwrap_or(0);
            Skill { name, path, tokens: chars / 4, loaded: false }
        })
        .collect();
    if let Ok(mut s) = SKILLS.lock() {
        *s = skills;
    }
    // skills.json: site -> file. One file can serve several sites.
    let manifest: BTreeMap<String, String> = std::fs::read_to_string(domain_dir.join("skills.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut by_file: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (site, file) in manifest {
        by_file.entry(file).or_default().push(bare(&site).to_string());
    }
    let domain = by_file
        .into_iter()
        .map(|(file, sites)| Domain { name: file.clone(), path: domain_dir.join(&file), sites, on_site: false })
        .filter(|d| d.path.is_file())
        .collect();
    if let Ok(mut d) = DOMAIN.lock() {
        *d = domain;
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A url without its scheme.
fn bare(url: &str) -> &str {
    url.trim().trim_start_matches("https://").trim_start_matches("http://")
}

/// The current tab's url: the domain knowledge of its site comes on, the
/// rest goes off. Called every step before the blocks are rendered.
pub fn follow_url(url: &str) {
    let url = bare(url);
    if let Ok(mut domain) = DOMAIN.lock() {
        for d in domain.iter_mut() {
            d.on_site = d.sites.iter().any(|site| on_site(url, site));
        }
    }
}

/// `url` (without its scheme) is on `site` when it starts with it, or with a
/// subdomain of it ("www.skyscanner.com/..." is on "skyscanner.com").
fn on_site(url: &str, site: &str) -> bool {
    if url.starts_with(site) {
        return true;
    }
    let host_end = url.find('/').unwrap_or(url.len());
    let (host, rest) = url.split_at(host_end);
    match site.split_once('/') {
        None => host.ends_with(&format!(".{site}")),
        Some((shost, spath)) => host.ends_with(&format!(".{shost}")) && rest.trim_start_matches('/').starts_with(spath),
    }
}

/// The `skills` tool: `kind` is "load" or "remove", `raw_id` the skill's [id].
pub fn run(kind: &Value, raw_id: &Value) -> ActResult<Value> {
    let kind = if kind.is_null() { String::new() } else { py_str_of(kind).trim().to_lowercase() };
    if kind != "load" && kind != "remove" {
        return err("skills' `type` is \"load\" (into <skills>) or \"remove\" (out of it)");
    }
    let id = match py_int_value(raw_id) {
        Ok(i) => i,
        Err(_) => return err(format!("'{}' is not a skill id", py_str_of(raw_id))),
    };
    let mut skills = SKILLS.lock().map_err(|_| ActErr::Msg("skills state poisoned".into()))?;
    if skills.is_empty() {
        return err("there are no skills to load");
    }
    let n = skills.len();
    let Some(skill) = (id >= 1).then(|| skills.get_mut((id - 1) as usize)).flatten() else {
        return err(format!("there is no skill [{id}] - <skills> lists [1] to [{n}]"));
    };
    let message = match (kind.as_str(), skill.loaded) {
        ("load", true) => format!("skill [{id}] {} is already loaded", skill.name),
        ("load", false) => {
            skill.loaded = true;
            format!(
                "skill [{id}] {} loaded ({}): its content is in <skills> from the next step on, until you remove it",
                skill.name,
                size(skill.tokens)
            )
        }
        ("remove", false) => format!("skill [{id}] {} is not loaded", skill.name),
        _ => {
            skill.loaded = false;
            format!("skill [{id}] {} removed from <skills>", skill.name)
        }
    };
    Ok(json!({"status": "success", "tool": "skills", "type": kind, "id": id, "message": message}))
}

/// Load the skill called `name`, if there is one and it is not loaded yet.
pub fn load_named(name: &str) {
    if let Ok(mut skills) = SKILLS.lock() {
        if let Some(s) = skills.iter_mut().find(|s| s.name == name) {
            s.loaded = true;
        }
    }
}

/// <skills>: one line per skill, [id] name (size), and under a loaded one its
/// content as the file holds it. Empty when there are no skill files.
pub fn block() -> String {
    let Ok(skills) = SKILLS.lock() else {
        return String::new();
    };
    if skills.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = Vec::new();
    for (i, s) in skills.iter().enumerate() {
        let line = format!("[{}] {} ({})", i + 1, s.name, size(s.tokens));
        if s.loaded {
            let content = std::fs::read_to_string(&s.path).unwrap_or_default();
            lines.push(format!("{line} - loaded\n{}\n", content.trim()));
        } else {
            lines.push(line);
        }
    }
    format!("<skills>\n{}\n</skills>", lines.join("\n"))
}

/// <domain_knowledge>: the files of the current tab's site, each named with
/// its site and given in full. Empty when the tab is on no known site.
pub fn domain_block() -> String {
    let Ok(domain) = DOMAIN.lock() else {
        return String::new();
    };
    let shown: Vec<String> = domain
        .iter()
        .filter(|d| d.on_site)
        .map(|d| {
            let content = std::fs::read_to_string(&d.path).unwrap_or_default();
            format!("{} - {}\n{}", d.name, d.sites.join(", "), content.trim())
        })
        .collect();
    if shown.is_empty() {
        String::new()
    } else {
        format!("<domain_knowledge>\n{}\n</domain_knowledge>", shown.join("\n\n"))
    }
}

fn size(tokens: usize) -> String {
    format!("{:.1}k tokens", tokens as f64 / 1000.0)
}
