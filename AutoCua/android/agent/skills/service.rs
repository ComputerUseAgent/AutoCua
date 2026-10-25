//! <skills> for the Android driver: the .md matched to the app in front.
//! The Rust copy of ios/agent/skills/service.py.
//!
//! The app in front costs nothing to know: every scan reads the phone's UI
//! tree, whose root window carries the app's package (the scanner exposes it
//! as `application_name`, "" on the home screen). skills.json maps that
//! package, or the app's name as the launcher shows it, to a markdown file:
//! a shipped default in AutoCua/default_skills/android/ or the user's own in
//! AutoCua_data/skills/android/ (this folder holds only the code).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use serde_json::{Map, Value};

use crate::agent::android_use::service::read_text;

pub struct DomainKnowledgeService {
    /// AutoCua_data/skills/android: the user's own skills (added from the UI
    /// or by hand).
    dir: PathBuf,
    /// The defaults shipped in AutoCua/default_skills/android, {filename: text}.
    defaults: BTreeMap<String, String>,
    /// skills.json of the defaults merged with the user's skills.json.
    mappings: Map<String, Value>,
}

impl DomainKnowledgeService {
    pub fn new(py: Python<'_>) -> Self {
        // Two folders, read together: the defaults shipped in
        // AutoCua/default_skills/android/ and the user's own skills in
        // AutoCua_data/skills/android/. Where AutoCua_data is, only
        // AutoCua/__init__.py knows (data_root), so it is asked; when that
        // fails, this folder stands in for the user's and there are no
        // defaults, as on iOS.
        let (dir, defaults) = match Self::folders(py) {
            Ok(found) => found,
            Err(_) => {
                let here = crate::agent_dir(py)
                    .map(|d| d.join("skills"))
                    .unwrap_or_else(|_| PathBuf::from("."));
                (here, BTreeMap::new())
            }
        };
        let mappings = Self::load(&dir, &defaults);
        DomainKnowledgeService { dir, defaults, mappings }
    }

    /// (the user's skills folder, created empty on first use, the shipped
    /// defaults): skills_dir("android") and default_skills("android").
    fn folders(py: Python<'_>) -> PyResult<(PathBuf, BTreeMap<String, String>)> {
        let dir = crate::data_root(py)?.join("skills").join("android");
        // Best effort, as skills_dir's mkdir is: a read-only or full disk
        // leaves the user's folder missing, not the shipped defaults.
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("mkdir {}: {e}", dir.display());
        }
        let shipped = crate::android_dir(py)?
            .parent()
            .map(|p| p.join("default_skills").join("android"))
            .unwrap_or_default();
        Ok((dir, Self::skills_from_dir(&shipped)))
    }

    /// {filename: text} of a folder's skills.json and *.md files, in name order.
    fn skills_from_dir(src: &Path) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        if !src.is_dir() {
            return out;
        }
        let mut paths: Vec<PathBuf> = std::fs::read_dir(src)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        paths.sort();
        for p in paths {
            let name = file_name(&p);
            let is_md = p.extension().is_some_and(|x| x.to_string_lossy().to_lowercase() == "md");
            if p.is_file() && (is_md || name == "skills.json") {
                match read_text(&p) {
                    Ok(text) => {
                        out.insert(name, text);
                    }
                    Err(_) => eprintln!("unreadable default skill {}", p.display()),
                }
            }
        }
        out
    }

    /// skills.json of the defaults merged with the user's skills.json: the
    /// default entry wins for a key present in both, the user file only adds.
    fn load(dir: &Path, defaults: &BTreeMap<String, String>) -> Map<String, Value> {
        let mut merged: Map<String, Value> = Map::new();
        let mut sources: Vec<(&str, String)> = Vec::new();
        if let Some(text) = defaults.get("skills.json").filter(|t| !t.is_empty()) {
            sources.push(("default", text.clone()));
        }
        let user = dir.join("skills.json");
        if user.is_file() {
            match read_text(&user) {
                Ok(text) => sources.push(("user", text)),
                Err(e) => eprintln!("Error reading user skills.json: {e}"),
            }
        }
        for (origin, text) in &sources {
            let data = match serde_json::from_str::<Value>(text) {
                Ok(data) => data,
                Err(e) => {
                    eprintln!("Error loading {origin} skills.json: {e}");
                    continue;
                }
            };
            let Value::Object(data) = data else { continue };
            for (bucket, mapping) in data {
                let Value::Object(mapping) = mapping else { continue };
                let target = merged
                    .entry(bucket)
                    .or_insert_with(|| Value::Object(Map::new()));
                if let Value::Object(target) = target {
                    for (key, md) in mapping {
                        target.entry(key).or_insert(md); // first (default) wins
                    }
                }
            }
        }
        if sources.is_empty() {
            eprintln!("skills.json not found in {} or the defaults", dir.display());
        }
        merged
    }

    fn norm(text: &str) -> String {
        text.trim().to_lowercase()
    }

    /// The skill filename for the app in front, or "" (home screen, no
    /// entry). Exact match on the package first; then the app's name as the
    /// launcher shows it, resolved from the installed-app list the launcher
    /// already caches (no device request), so a skill keyed by the app's name
    /// still finds the package the scanner read.
    pub fn match_skill(&self, application_name: &str) -> String {
        let name = Self::norm(application_name);
        if name.is_empty() {
            return String::new();
        }
        let apps: Map<String, Value> = match self.mappings.get("apps") {
            Some(Value::Object(apps)) => apps.clone(),
            _ => Map::new(),
        };
        let by_key: HashMap<String, &Value> = apps.iter().map(|(k, v)| (Self::norm(k), v)).collect();
        if let Some(md) = by_key.get(&name) {
            return md_name(md);
        }
        for (display_name, package) in crate::controller::tool::open_app::service::installed_apps() {
            if Self::norm(&package) == name {
                if let Some(md) = by_key.get(&Self::norm(&display_name)) {
                    let md = md_name(md);
                    if !md.is_empty() {
                        return md;
                    }
                }
            }
        }
        String::new()
    }

    /// The shipped default of that name if there is one, else the user's
    /// copy, else "" (a missing skill is ignored). One name loads once.
    fn read(&self, filename: &str) -> String {
        if let Some(text) = self.defaults.get(filename) {
            return text.trim().to_string();
        }
        let p = self.dir.join(filename);
        if p.is_file() {
            return match read_text(&p) {
                Ok(text) => text.trim().to_string(),
                Err(e) => {
                    eprintln!("Error reading skill {filename}: {e}");
                    String::new()
                }
            };
        }
        eprintln!("Skill file not found: {}", p.display());
        String::new()
    }

    /// <domain_knowledge="<skill>">...</domain_knowledge> for the app in
    /// front, or "" when nothing matches.
    pub fn get_knowledge(&self, application_name: &str) -> String {
        let md = self.match_skill(application_name);
        if md.is_empty() {
            return String::new();
        }
        let text = self.read(&md);
        if text.is_empty() {
            return String::new();
        }
        let context = stem(&md);
        format!("<domain_knowledge=\"{context}\">\n{text}\n</domain_knowledge>")
    }
}

/// A skills.json value as a file name: its text, or "" for anything that is
/// not a string (such an entry names no file).
fn md_name(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// os.path.splitext(name)[0]: the name without its last extension.
fn stem(name: &str) -> &str {
    let start = name.rfind('/').map(|i| i + 1).unwrap_or(0);
    let base = &name[start..];
    match base.rfind('.') {
        Some(i) if i > 0 && base[..i].chars().any(|c| c != '.') => &name[..start + i],
        _ => name,
    }
}
