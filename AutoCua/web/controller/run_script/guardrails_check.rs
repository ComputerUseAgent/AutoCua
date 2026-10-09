//! The guardrails of `run_script`, all in this one file: checked BEFORE a
//! script runs (which tab, which page, what the script text does) and AFTER
//! it ran, on the reply, before the response is packed (the error text, the
//! result).
//!
//! Scope: block theft, leave scraping alone.
//!
//! - The script runs only in the tab the agent is driving (in a parallel run,
//!   each agent's own tab), and only on a web page.
//! - A script that READS cookies, stored credentials, the clipboard or a
//!   password/payment field, SENDS data to another site, writes to the
//!   person's disk or clipboard, or HIDES what it does (eval, built-up
//!   property names) is refused before it runs, with the construct quoted.
//! - Whatever comes back, result or thrown error, is screened in Rust for
//!   sensitive classes only (session cookies, tokens and keys, cards, bank
//!   accounts, one-time codes, national ids): withheld when the result IS the
//!   secret, redacted in place otherwise. Everything else passes untouched:
//!   page text, tables, links, app state, emails, phones, prices.
//!
//! Rules: every check fails CLOSED (an error is a refusal, never a pass); the
//! detector never runs inside the page (TO_DATA can be patched by the page);
//! the audit line carries hashes, sizes and counts, never the data.
//!
//! The pure logic (screen_code, screen_error, screen_result) has no crate
//! dependencies beyond regex, serde_json, url and base64, so a scratchpad
//! crate can include this file with the `guard_standalone` feature and test
//! it; the glue that touches the scanner and Python sits behind the opposite
//! cfg at the bottom.

#![allow(unexpected_cfgs)]

use std::collections::BTreeMap;
use std::sync::OnceLock;

use base64::Engine;
use regex::Regex;
use serde_json::{json, Value};
use url::Url;

/// Longest script source accepted, in characters. A read is short; a long
/// blob is a payload.
pub const MAX_SOURCE_CHARS: usize = 16 * 1024;
/// Characters dropped before a page-side cut: the page cut at __MAX__ before
/// Rust saw the tail, so a token on the boundary is half a token no pattern
/// matches.
pub const CUT_MARGIN_CHARS: usize = 2048;
/// Longest error text handed back to the model.
pub const MAX_ERROR_CHARS: usize = 300;

// ------------------------------------------------------------ page

/// Where the tool would work: the tab's origin and its site (registrable
/// domain, approximated), from the live URL. Web pages only. `tool` names the
/// caller, because both doors into a page are held to this rule and a refusal
/// has to say which one it came from.
pub fn check_page(tool: &str, url: &str) -> Result<(String, String), String> {
    let url = url.trim();
    if url.is_empty() {
        return Err(format!("{tool} is not available right now: the tab's address could not be read"));
    }
    if url == "about:blank" {
        return Ok(("about:blank".into(), String::new()));
    }
    let parsed = Url::parse(url)
        .map_err(|_| format!("{tool} is not available right now: the tab's address could not be read"))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "{tool} works on web pages only - this tab is a {scheme}: page (local files and browser pages \
             are never read this way)"
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("{tool} is not available right now: the tab's address has no host"))?;
    let origin = match parsed.port() {
        Some(p) => format!("{scheme}://{host}:{p}"),
        None => format!("{scheme}://{host}"),
    };
    Ok((origin, registrable(host)))
}

/// The registrable domain of a host, approximated without a public-suffix
/// list: the last two labels, or three under a known two-level suffix.
/// Addresses stay as they are.
pub fn registrable(host: &str) -> String {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = if host.starts_with('[') {
        host.clone()
    } else {
        host.split(':').next().unwrap_or("").to_string()
    };
    if host.starts_with('[') || host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() <= 2 {
        return host;
    }
    const TWO_LEVEL: &[&str] = &[
        "co.uk", "org.uk", "ac.uk", "gov.uk", "me.uk", "net.uk", "com.au", "net.au", "org.au", "edu.au",
        "gov.au", "co.nz", "org.nz", "co.jp", "ne.jp", "or.jp", "ac.jp", "go.jp", "co.in", "net.in",
        "org.in", "ac.in", "gov.in", "co.kr", "or.kr", "com.br", "net.br", "org.br", "gov.br", "com.mx",
        "com.ar", "com.tr", "com.sg", "com.my", "com.hk", "com.tw", "co.za", "co.id", "co.il", "com.cn",
        "net.cn", "org.cn", "com.ua", "com.pl", "com.vn", "com.ph", "com.pk", "com.bd", "com.ng", "com.eg",
        "com.sa", "com.pe", "com.co", "com.ve", "com.ec", "com.uy", "com.py", "com.bo", "com.gt", "com.do",
        "co.th", "co.ke", "co.tz", "co.ug", "co.zw",
    ];
    let n = labels.len();
    let last2 = format!("{}.{}", labels[n - 2], labels[n - 1]);
    if TWO_LEVEL.contains(&last2.as_str()) {
        format!("{}.{}", labels[n - 3], last2)
    } else {
        last2
    }
}

// ------------------------------------------------------------ script text

/// The script text as the screen reads it: zero-width and bidi characters
/// gone, fullwidth letters folded to ASCII, \uXXXX, \u{...} and \xNN escapes
/// decoded. Screening only; the browser runs the original.
pub fn normalize_for_screen(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}' => {
                i += 1;
                continue;
            }
            '\u{FF01}'..='\u{FF5E}' => {
                out.push(char::from_u32(c as u32 - 0xFEE0).unwrap_or(c));
                i += 1;
                continue;
            }
            '\\' if i + 1 < chars.len() => {
                let next = chars[i + 1];
                if next == 'u' {
                    if i + 2 < chars.len() && chars[i + 2] == '{' {
                        if let Some(close) = chars[i + 3..].iter().position(|&x| x == '}') {
                            let hex: String = chars[i + 3..i + 3 + close].iter().collect();
                            if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                                out.push(ch);
                                i += 3 + close + 1;
                                continue;
                            }
                        }
                    } else if i + 5 < chars.len() {
                        let hex: String = chars[i + 2..i + 6].iter().collect();
                        if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                            out.push(ch);
                            i += 6;
                            continue;
                        }
                    }
                } else if next == 'x' && i + 3 < chars.len() {
                    let hex: String = chars[i + 2..i + 4].iter().collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                        i += 4;
                        continue;
                    }
                }
                out.push(c);
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

struct Rule {
    re: Regex,
    what: &'static str,
    hint: &'static str,
}

fn rules(specs: &[(&'static str, &'static str, &'static str)]) -> Vec<Rule> {
    specs
        .iter()
        .map(|(pat, what, hint)| Rule { re: Regex::new(pat).expect("guardrail pattern"), what, hint })
        .collect()
}

const HINT_READ: &str =
    "cookies, logins, the clipboard and payment details never leave the browser; read the page text instead \
     (innerText, the fields the task is about)";
const HINT_HIDE: &str = "write the property and function names out in plain JavaScript";
const HINT_DISK: &str = "nothing is downloaded or copied to the clipboard by script; return the data instead";

/// Reads that are theft: no page-reading script needs them.
fn theft_reads() -> &'static Vec<Rule> {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        rules(&[
            (r"(?i)document\s*\.\s*cookie", "reads cookies", HINT_READ),
            (r"(?i)\bcookieStore\b", "reads cookies", HINT_READ),
            (r"(?i)navigator\s*\.\s*credentials", "reads stored credentials", HINT_READ),
            (r"\b(PasswordCredential|FederatedCredential)\b", "reads stored credentials", HINT_READ),
            (r"(?i)navigator\s*\.\s*clipboard\s*\.\s*read", "reads the clipboard", HINT_READ),
            (r#"(?i)execCommand\s*\(\s*["']paste"#, "reads the clipboard", HINT_READ),
            (r"(?i)crypto\s*\.\s*subtle\s*\.\s*(exportKey|unwrapKey)", "exports keys", HINT_READ),
            (r#"(?i)type\s*=\s*["']?\s*password"#, "targets a password field", HINT_READ),
            (
                r#"(?i)autocomplete\s*[*^$|~]?=\s*["']?\s*(current-password|new-password|one-time-code|cc-number|cc-exp|cc-exp-month|cc-exp-year|cc-csc)"#,
                "targets a password or payment field",
                HINT_READ,
            ),
            (
                r#"(?i)\[(name|id|placeholder|aria-label)\s*[*^$|~]?=\s*["']?[^"'\]]*(password|passwd|pwd|otp|cvv|cvc|security[-_ ]?code|card[-_ ]?number|ccnum)"#,
                "targets a password or payment field",
                HINT_READ,
            ),
            (
                r#"(?i)(getElementById|getElementsByName|querySelector|querySelectorAll)\s*\(\s*["'][^"']*(password|passwd|pwd|otp|cvv|cvc|cardnumber|card-number|card_number|ccnum)"#,
                "targets a password or payment field",
                HINT_READ,
            ),
        ])
    })
}

/// Writes to the person's machine, refused whatever the target.
fn disk_writes() -> &'static Vec<Rule> {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        rules(&[
            (r"\b(showSaveFilePicker|showOpenFilePicker|showDirectoryPicker)\b", "opens a file picker", HINT_DISK),
            (r"\.download\s*=", "downloads a file", HINT_DISK),
            (r#"(?i)setAttribute\s*\(\s*["']download"#, "downloads a file", HINT_DISK),
            (r"(?i)navigator\s*\.\s*clipboard\s*\.\s*write", "writes the clipboard", HINT_DISK),
            (r#"(?i)execCommand\s*\(\s*["'](copy|cut)"#, "writes the clipboard", HINT_DISK),
            (r"(?i)navigator\s*\.\s*share\s*\(", "hands data to the share sheet", HINT_DISK),
        ])
    })
}

/// Hiding: constructs whose only use in a read is to slip past the lists above.
fn hiding() -> &'static Vec<Rule> {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        rules(&[
            (r"\beval\s*\(", "runs code from a string (eval)", HINT_HIDE),
            (r"\bnew\s+Function\s*\(", "runs code from a string (new Function)", HINT_HIDE),
            (r#"\bFunction\s*\(\s*["'`]"#, "runs code from a string (Function)", HINT_HIDE),
            (r#"constructor\s*\(\s*["'`]"#, "runs code from a string (constructor)", HINT_HIDE),
            (r"\bimport\s*\(", "loads code from a URL (import)", HINT_HIDE),
            (
                r#"\[\s*(["'`][^"'`]*["'`]\s*\+\s*)+["'`]"#,
                "builds a property name from pieces",
                HINT_HIDE,
            ),
            (
                r"\[\s*(atob|unescape|decodeURIComponent|String\s*\.\s*fromCharCode|String\s*\.\s*fromCodePoint)\s*\(",
                "builds a property name from an encoded string",
                HINT_HIDE,
            ),
            (
                r#"\[\s*["'](cookie|cookieStore|credentials|clipboard|fetch|XMLHttpRequest|sendBeacon|WebSocket|EventSource|eval|Function|open|assign|replace|submit|download|share)["']\s*\]"#,
                "reaches a guarded name through brackets",
                HINT_HIDE,
            ),
            (r"\bReflect\s*\.\s*(get|apply|construct|set)\b", "reaches names through Reflect", HINT_HIDE),
            (r"\bwith\s*\(", "uses a with block", HINT_HIDE),
        ])
    })
}

/// Sends whose target is a literal URL: the host is compared with the tab's
/// site. Group `h` is the host (an empty match means no literal host).
fn send_patterns() -> &'static Vec<Regex> {
    static RULES: OnceLock<Vec<Regex>> = OnceLock::new();
    RULES.get_or_init(|| {
        const URL: &str = r#"["'`](?:https?:)?//(?P<h>[^/"'`\s?#]+)"#;
        [
            format!(r"\bfetch\s*\(\s*{URL}"),
            format!(r#"\.open\s*\(\s*["'][A-Za-z]+["']\s*,\s*{URL}"#),
            format!(r"\bsendBeacon\s*\(\s*{URL}"),
            format!(r"\bnew\s+(WebSocket|EventSource)\s*\(\s*{URL}"),
            format!(r#"\bnew\s+WebSocket\s*\(\s*["'`]wss?://(?P<h2>[^/"'`\s?#]+)"#),
            format!(r"\b(window|top|parent|self|globalThis)\s*\.\s*open\s*\(\s*{URL}"),
            format!(r"\blocation\s*(\.\s*href\s*)?=\s*{URL}"),
            format!(r"\blocation\s*\.\s*(assign|replace)\s*\(\s*{URL}"),
            format!(r"\.(src|href|action)\s*=\s*{URL}"),
            format!(r#"setAttribute\s*\(\s*["'](src|href|action|formaction)["']\s*,\s*{URL}"#),
        ]
        .iter()
        .map(|p| Regex::new(p).expect("send pattern"))
        .collect()
    })
}

fn snippet(text: &str, start: usize, end: usize) -> String {
    let s: String = text[start..end].chars().take(60).collect();
    s.replace('\n', " ")
}

/// Screen the script text. `site` is the tab's registrable domain ("" on
/// about:blank). Ok means it may run; Err carries the refusal for the model.
pub fn screen_code(src: &str, site: &str) -> Result<(), String> {
    let n = src.chars().count();
    if n > MAX_SOURCE_CHARS {
        return Err(format!(
            "run_script refused: the script is {n} characters, more than the {MAX_SOURCE_CHARS} limit - read \
             less per call"
        ));
    }
    let text = normalize_for_screen(src);
    for rule in theft_reads().iter().chain(disk_writes().iter()).chain(hiding().iter()) {
        if let Some(m) = rule.re.find(&text) {
            return Err(format!(
                "run_script refused: the script {} ({}); {}",
                rule.what,
                snippet(&text, m.start(), m.end()),
                rule.hint
            ));
        }
    }
    for re in send_patterns() {
        for caps in re.captures_iter(&text) {
            let host = caps
                .name("h")
                .or_else(|| caps.name("h2"))
                .map(|m| m.as_str())
                .unwrap_or("");
            if host.is_empty() || host.contains("${") {
                continue; // not a literal host: cannot be judged here
            }
            if registrable(host) != site {
                let whole = caps.get(0).map(|m| (m.start(), m.end())).unwrap_or((0, 0));
                return Err(format!(
                    "run_script refused: the script sends data to another site ({host}: {}); read the page \
                     here and return what you need - nothing leaves the site by script",
                    snippet(&text, whole.0, whole.1)
                ));
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------ detector

/// What one screening did, for the message and the audit line.
#[derive(Default, Debug, Clone)]
pub struct Report {
    /// Values redacted, by class.
    pub classes: BTreeMap<&'static str, usize>,
    /// Characters replaced by markers.
    pub redacted_chars: usize,
    /// Characters scanned.
    pub chars: usize,
    /// Set when the whole result is withheld, with the reason for the model.
    pub withheld: Option<String>,
    /// Largest number of session-like cookie pairs seen in one string.
    jar_pairs: usize,
    saw_private_key: bool,
    saw_card: bool,
    saw_cvv: bool,
    saw_otp: bool,
    leaves: usize,
    hit_leaves: usize,
}

impl Report {
    pub fn redacted(&self) -> usize {
        self.classes.values().sum()
    }

    /// "2 values redacted (cookie 1, jwt 1)".
    pub fn note(&self) -> String {
        let total = self.redacted();
        let parts: Vec<String> = self.classes.iter().map(|(k, v)| format!("{k} {v}")).collect();
        format!(
            "{total} value{} redacted ({}): cookies, tokens, passwords, one-time codes and card numbers never \
             leave the browser",
            if total == 1 { "" } else { "s" },
            parts.join(", ")
        )
    }

    pub fn classes_json(&self) -> Value {
        let map: serde_json::Map<String, Value> =
            self.classes.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
        json!({"count": self.redacted(), "classes": map})
    }

    fn absorb(&mut self, scan: &TextScan) {
        for (k, v) in &scan.classes {
            *self.classes.entry(k).or_insert(0) += v;
        }
        self.redacted_chars += scan.redacted_chars;
        self.chars += scan.chars;
        self.jar_pairs = self.jar_pairs.max(scan.jar_pairs);
        self.saw_private_key |= scan.saw_private_key;
        self.saw_card |= scan.saw_card;
        self.saw_cvv |= scan.saw_cvv;
        self.saw_otp |= scan.saw_otp;
    }

    /// The withhold rules, once everything is scanned.
    fn decide(&mut self) {
        let reason = if self.saw_private_key {
            Some("it contained a private key")
        } else if self.jar_pairs >= 3 {
            Some("it was a cookie jar (3 or more session cookies)")
        } else if self.saw_card && self.saw_cvv {
            Some("it contained a payment card with its security code")
        } else if self.saw_otp {
            Some("it contained a one-time code")
        } else if self.leaves >= 5 && self.hit_leaves * 2 >= self.leaves {
            Some("most of its values were credentials or tokens")
        } else if self.redacted() >= 3 && self.chars > 0 && self.redacted_chars * 4 > self.chars {
            // One or two tokens in a short result are redacted and the rest
            // handed over; three or more that make up most of it are the result.
            Some("more than a quarter of it was credentials or tokens")
        } else {
            None
        };
        if let Some(r) = reason {
            let n = self.redacted();
            self.withheld = Some(format!(
                "result withheld: {r} ({n} credential-like value{}). Cookies, tokens, passwords, one-time \
                 codes and card numbers never leave the browser; continue the task without them and do not \
                 try to read them another way",
                if n == 1 { "" } else { "s" }
            ));
        }
    }
}

#[derive(Default)]
struct TextScan {
    out: String,
    classes: BTreeMap<&'static str, usize>,
    redacted_chars: usize,
    chars: usize,
    jar_pairs: usize,
    saw_private_key: bool,
    saw_card: bool,
    saw_cvv: bool,
    saw_otp: bool,
}

impl TextScan {
    fn hit(&self) -> bool {
        !self.classes.is_empty()
    }
}

/// A span to replace, by byte offsets into the scanned text.
struct Span {
    start: usize,
    end: usize,
    class: &'static str,
    /// For cards: keep the first six and last four digits around the marker.
    keep_edges: bool,
}

fn re(pat: &str) -> Regex {
    Regex::new(pat).expect("detector pattern")
}

struct Detectors {
    pair: Regex,
    session: Regex,
    jwt: Regex,
    bearer: Regex,
    authorization: Regex,
    key_prefix: Regex,
    azure_key: Regex,
    private_key: Regex,
    private_key_json: Regex,
    card: Regex,
    cvv: Regex,
    iban: Regex,
    bank: Regex,
    otp: Regex,
    otp_tail: Regex,
    ssn: Regex,
    nino: Regex,
    pan_india: Regex,
    aadhaar: Regex,
    sin: Regex,
    userinfo: Regex,
    url_param: Regex,
    base64_run: Regex,
    hex_run: Regex,
    percent: Regex,
    card_context: Regex,
    aadhaar_context: Regex,
}

fn detectors() -> &'static Detectors {
    static D: OnceLock<Detectors> = OnceLock::new();
    D.get_or_init(|| Detectors {
        pair: re(r"^\s*([A-Za-z0-9_.\-]{1,64})=(\S{8,})\s*$"),
        session: re(
            r#"(?i)\b(PHPSESSID|JSESSIONID|ASP\.NET_SessionId|ASPSESSIONID[A-Z]*|sessionid|session_id|connect\.sid|express:sess|laravel_session|_session_id|rack\.session|ci_session|wordpress_logged_in_[a-f0-9]+|wp_woocommerce_session_[a-f0-9]+|auth_token|access_token|refresh_token|id_token|csrftoken|XSRF-TOKEN|_csrf|__Secure-[^=\s:]+|__Host-[^=\s:]+|SID|HSID|SSID|APISID|SAPISID|NID|li_at|c_user|datr|ds_user_id|remember_token|remember_me)\s*[=:]\s*["']?([^;\s"',}]{6,})"#,
        ),
        jwt: re(r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}(\.[A-Za-z0-9_-]{10,})?"),
        bearer: re(r"(?i)\b(bearer|basic)\s+([A-Za-z0-9\-._~+/]{16,}=*)"),
        authorization: re(r#"(?i)\bauthorization\b\s*[:=]\s*["']?([^"'\s]{8,})"#),
        key_prefix: re(
            r"\b(sk-ant-[A-Za-z0-9_\-]{20,}|sk-proj-[A-Za-z0-9_\-]{20,}|sk-or-v1-[a-f0-9]{64}|sk-[A-Za-z0-9_\-]{20,}|sk_live_[A-Za-z0-9]{16,}|rk_live_[A-Za-z0-9]{16,}|gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{60,}|glpat-[A-Za-z0-9_\-]{20,}|xox[abprs]-[A-Za-z0-9\-]{10,}|AKIA[0-9A-Z]{16}|ASIA[0-9A-Z]{16}|ya29\.[0-9A-Za-z_\-]{20,}|GOCSPX-[A-Za-z0-9_\-]{20,}|EAA[A-Za-z0-9]{60,}|SG\.[A-Za-z0-9_\-]{22}\.[A-Za-z0-9_\-]{43}|npm_[A-Za-z0-9]{36}|hf_[A-Za-z0-9]{30,}|pplx-[A-Za-z0-9]{40,}|gsk_[A-Za-z0-9]{40,}|csk-[A-Za-z0-9]{40,}|r8_[A-Za-z0-9]{30,}|shpat_[a-f0-9]{32}|shpss_[a-f0-9]{32}|sq0atp-[A-Za-z0-9_\-]{22}|AIza[0-9A-Za-z_\-]{35})",
        ),
        azure_key: re(r"AccountKey=([A-Za-z0-9+/]{80,}={0,2})"),
        private_key: re(r"-----BEGIN [A-Z ]*PRIVATE KEY( BLOCK)?-----|PuTTY-User-Key-File"),
        private_key_json: re(r#""private_key"\s*:\s*"[^"]{20,}""#),
        card: re(r"\b(?:\d[ \-]?){12,18}\d\b"),
        cvv: re(r"(?i)\b(cvv|cvc|cvv2|csc|security code)\b\D{0,12}(\d{3,4})\b"),
        iban: re(r"\b([A-Z]{2}\d{2}[A-Z0-9]{11,30})\b"),
        bank: re(
            r"(?i)\b(account (?:number|no\.?)|acct(?:\.|ount)? ?(?:number|no\.?)|routing (?:number|no\.?)|sort code|swift|bic)\b\D{0,20}(\d[\d \-]{5,}\d)",
        ),
        otp: re(
            r"(?i)\b(otp|one[- ]time (?:code|password|passcode)|verification code|verify code|security code|login code|authentication code|2fa code|two[- ]factor code|passcode|confirmation code)\b[^0-9\n]{0,40}\b(\d{6,8})\b",
        ),
        otp_tail: re(r"(?i)\b(\d{6,8})\b[^0-9\n]{0,40}\bis your (?:code|verification|otp|one[- ]time)"),
        ssn: re(r"\b(\d{3})-(\d{2})-(\d{4})\b"),
        nino: re(r"\b[A-CEGHJ-PR-TW-Z]{2}\d{6}[A-D]\b"),
        pan_india: re(r"\b[A-Z]{5}\d{4}[A-Z]\b"),
        aadhaar: re(r"\b([2-9]\d{3})\s?(\d{4})\s?(\d{4})\b"),
        sin: re(r"(?i)\b(?:sin|social insurance)\b\D{0,20}(\d{3}[ \-]?\d{3}[ \-]?\d{3})\b"),
        userinfo: re(r#"[a-zA-Z][a-zA-Z0-9+.\-]*://[^/\s:@"']+:([^/\s:@"']+)@"#),
        url_param: re(
            r#"(?i)[?&](access_token|token|auth|api_key|apikey|sig|signature|session|sessionid|otp|password|pwd|secret|X-Amz-Signature|X-Amz-Credential|X-Amz-Security-Token|code|key)=([^&\s"'<>]{8,})"#,
        ),
        base64_run: re(r"[A-Za-z0-9+/_\-]{24,}={0,2}"),
        hex_run: re(r"\b[0-9a-fA-F]{32,}\b"),
        percent: re(r"%[0-9A-Fa-f]{2}"),
        card_context: re(r"(?i)\b(card|visa|mastercard|amex|american express|expiry|exp\.?|cvv|cvc|credit|debit)\b"),
        aadhaar_context: re(r"(?i)\b(aadhaar|aadhar|uidai|uid)\b"),
    })
}

/// Cookie names that are not secrets (analytics and consent cookies).
fn ignored_cookie(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "_ga"
        || n == "_gid"
        || n == "_gat"
        || n.starts_with("_gat_")
        || n.starts_with("_gcl_")
        || n == "_fbp"
        || n == "_fbc"
        || n.starts_with("_hj")
        || n == "_uetsid"
        || n == "_uetvid"
        || n == "optanonconsent"
        || n == "optanonalertboxclosed"
        || n == "euconsent-v2"
        || n == "consent"
        || n == "cookieconsent_status"
        || n == "next_locale"
        || n == "lang"
        || n == "locale"
        || n == "theme"
        || n == "timezone"
}

fn luhn_ok(digits: &str) -> bool {
    let mut sum = 0;
    let mut double = false;
    for ch in digits.chars().rev() {
        let d = match ch.to_digit(10) {
            Some(d) => d,
            None => return false,
        };
        let d = if double {
            let x = d * 2;
            if x > 9 {
                x - 9
            } else {
                x
            }
        } else {
            d
        };
        sum += d;
        double = !double;
    }
    digits.len() >= 2 && sum % 10 == 0
}

/// Issuer ranges: a Luhn-valid number is a card only with a known prefix.
fn card_iin_ok(digits: &str) -> bool {
    let len = digits.len();
    let p2: u32 = digits[..2].parse().unwrap_or(0);
    let p3: u32 = digits[..3].parse().unwrap_or(0);
    let p4: u32 = digits[..4].parse().unwrap_or(0);
    match len {
        13 | 16 | 19 if digits.starts_with('4') => true,
        16 if (51..=55).contains(&p2) || (2221..=2720).contains(&p4) => true,
        15 if p2 == 34 || p2 == 37 => true,
        16..=19 if p4 == 6011 || p2 == 65 || (644..=649).contains(&p3) => true,
        16..=19 if (3528..=3589).contains(&p4) => true,
        16..=19 if p2 == 62 => true,
        14 if p2 == 36 || p2 == 38 || (300..=305).contains(&p3) => true,
        _ => false,
    }
}

fn verhoeff_ok(digits: &str) -> bool {
    const D: [[u8; 10]; 10] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        [1, 2, 3, 4, 0, 6, 7, 8, 9, 5],
        [2, 3, 4, 0, 1, 7, 8, 9, 5, 6],
        [3, 4, 0, 1, 2, 8, 9, 5, 6, 7],
        [4, 0, 1, 2, 3, 9, 5, 6, 7, 8],
        [5, 9, 8, 7, 6, 0, 4, 3, 2, 1],
        [6, 5, 9, 8, 7, 1, 0, 4, 3, 2],
        [7, 6, 5, 9, 8, 2, 1, 0, 4, 3],
        [8, 7, 6, 5, 9, 3, 2, 1, 0, 4],
        [9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
    ];
    const P: [[u8; 10]; 8] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        [1, 5, 7, 6, 2, 8, 3, 0, 9, 4],
        [5, 8, 0, 3, 7, 9, 6, 1, 4, 2],
        [8, 9, 1, 6, 0, 4, 3, 5, 2, 7],
        [9, 4, 5, 3, 1, 2, 6, 8, 7, 0],
        [4, 2, 8, 6, 5, 7, 3, 9, 0, 1],
        [2, 7, 9, 3, 8, 0, 6, 4, 1, 5],
        [7, 0, 4, 6, 9, 1, 3, 2, 5, 8],
    ];
    let mut c = 0usize;
    for (i, ch) in digits.chars().rev().enumerate() {
        let d = match ch.to_digit(10) {
            Some(d) => d as usize,
            None => return false,
        };
        c = D[c][P[i % 8][d] as usize] as usize;
    }
    c == 0
}

fn iban_ok(s: &str) -> bool {
    if s.len() < 15 || s.len() > 34 {
        return false;
    }
    let rearranged = format!("{}{}", &s[4..], &s[..4]);
    let mut rem: u32 = 0;
    for ch in rearranged.chars() {
        let v = match ch {
            '0'..='9' => ch as u32 - '0' as u32,
            'A'..='Z' => ch as u32 - 'A' as u32 + 10,
            _ => return false,
        };
        rem = if v >= 10 { (rem * 100 + v) % 97 } else { (rem * 10 + v) % 97 };
    }
    rem == 1
}

/// Part of a longer identifier (a UUID segment, a path, a version): the
/// character next to the match is a joiner, not a space.
fn id_adjacent(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    matches!(before, Some('-' | '_' | '.' | '/' | ':' | '#'))
        || matches!(after, Some('-' | '_' | '/' | ':' | '#'))
}

fn has_letter_and_digit(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_alphabetic()) && s.chars().any(|c| c.is_ascii_digit())
}

fn is_marker(s: &str) -> bool {
    s.starts_with("[REDACTED:")
}

/// Mostly text once decoded: a UTF-8 string with few control characters.
fn printable_text(bytes: &[u8]) -> Option<String> {
    let s = String::from_utf8(bytes.to_vec()).ok()?;
    let total = s.chars().count().max(1);
    let control = s.chars().filter(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')).count();
    if control * 10 > total {
        return None;
    }
    Some(s)
}

fn decode_base64(run: &str) -> Option<Vec<u8>> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    let trimmed = run.trim_end_matches('=');
    for engine in [&STANDARD, &URL_SAFE] {
        if let Ok(b) = engine.decode(run) {
            return Some(b);
        }
    }
    for engine in [&STANDARD_NO_PAD, &URL_SAFE_NO_PAD] {
        if let Ok(b) = engine.decode(trimmed) {
            return Some(b);
        }
    }
    None
}

fn decode_hex(run: &str) -> Option<Vec<u8>> {
    if run.len() % 2 != 0 {
        return None;
    }
    (0..run.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&run[i..i + 2], 16).ok())
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Default)]
struct Spans(Vec<Span>);

impl Spans {
    fn add(&mut self, start: usize, end: usize, class: &'static str, keep_edges: bool) {
        if end > start {
            self.0.push(Span { start, end, class, keep_edges });
        }
    }
}

/// Find everything to redact in one text. `depth` bounds the decoding of
/// encoded runs (one level).
fn find_spans(text: &str, depth: u8, scan: &mut TextScan) -> Vec<Span> {
    let d = detectors();
    let mut spans = Spans::default();

    // Cookie jars: parts split on ';', each a name=value with a long value.
    if text.contains(';') {
        let mut pairs = 0usize;
        let mut offset = 0usize;
        for part in text.split(';') {
            if let Some(c) = d.pair.captures(part) {
                let name = c.get(1).map(|m| m.as_str()).unwrap_or("");
                if !ignored_cookie(name) {
                    let value = c.get(2).unwrap();
                    pairs += 1;
                    spans.add(offset + value.start(), offset + value.end(), "cookie", false);
                }
            }
            offset += part.len() + 1;
        }
        if pairs >= 2 {
            scan.jar_pairs = scan.jar_pairs.max(pairs);
        } else {
            // One pair alone is not a jar: drop what the loop pushed.
            spans.0.retain(|s| s.class != "cookie");
        }
    }
    for c in d.session.captures_iter(text) {
        let v = c.get(2).unwrap();
        spans.add(v.start(), v.end(), "session", false);
    }
    for m in d.jwt.find_iter(text) {
        spans.add(m.start(), m.end(), "jwt", false);
    }
    for c in d.bearer.captures_iter(text) {
        let v = c.get(2).unwrap();
        if v.as_str().chars().any(|ch| !ch.is_ascii_alphabetic()) {
            spans.add(v.start(), v.end(), "bearer", false);
        }
    }
    for c in d.authorization.captures_iter(text) {
        let v = c.get(1).unwrap();
        spans.add(v.start(), v.end(), "bearer", false);
    }
    for m in d.key_prefix.find_iter(text) {
        let s = m.as_str();
        if s.starts_with("AKIA") || s.starts_with("ASIA") || has_letter_and_digit(s) {
            spans.add(m.start(), m.end(), "api_key", false);
        }
    }
    for c in d.azure_key.captures_iter(text) {
        let v = c.get(1).unwrap();
        spans.add(v.start(), v.end(), "api_key", false);
    }
    if d.private_key.is_match(text) || d.private_key_json.is_match(text) {
        scan.saw_private_key = true;
        spans.add(0, text.len(), "private_key", false);
    }
    for m in d.card.find_iter(text) {
        let digits: String = m.as_str().chars().filter(|c| c.is_ascii_digit()).collect();
        if !(13..=19).contains(&digits.len()) || !luhn_ok(&digits) || !card_iin_ok(&digits) {
            continue;
        }
        // A 15-digit 35-prefixed number is as likely an IMEI: ask for context.
        if digits.len() == 15 && digits.starts_with("35") {
            let lo = m.start().saturating_sub(80);
            let hi = (m.end() + 80).min(text.len());
            if !d.card_context.is_match(&text[lo..hi]) {
                continue;
            }
        }
        scan.saw_card = true;
        spans.add(m.start(), m.end(), "card", true);
    }
    for c in d.cvv.captures_iter(text) {
        let v = c.get(2).unwrap();
        scan.saw_cvv = true;
        spans.add(v.start(), v.end(), "cvv", false);
    }
    for c in d.iban.captures_iter(text) {
        let v = c.get(1).unwrap();
        if iban_ok(v.as_str()) {
            spans.add(v.start(), v.end(), "bank", false);
        }
    }
    for c in d.bank.captures_iter(text) {
        let v = c.get(2).unwrap();
        spans.add(v.start(), v.end(), "bank", false);
    }
    for c in d.otp.captures_iter(text) {
        let v = c.get(2).unwrap();
        scan.saw_otp = true;
        spans.add(v.start(), v.end(), "otp", false);
    }
    for c in d.otp_tail.captures_iter(text) {
        let v = c.get(1).unwrap();
        scan.saw_otp = true;
        spans.add(v.start(), v.end(), "otp", false);
    }
    for c in d.ssn.captures_iter(text) {
        let (a, g, s) = (c.get(1).unwrap(), c.get(2).unwrap(), c.get(3).unwrap());
        let area: u32 = a.as_str().parse().unwrap_or(0);
        if area == 0 || area == 666 || area >= 900 || g.as_str() == "00" || s.as_str() == "0000" {
            continue;
        }
        spans.add(a.start(), s.end(), "national_id", false);
    }
    for m in d.nino.find_iter(text) {
        spans.add(m.start(), m.end(), "national_id", false);
    }
    for m in d.pan_india.find_iter(text) {
        spans.add(m.start(), m.end(), "national_id", false);
    }
    for c in d.aadhaar.captures_iter(text) {
        let whole = c.get(0).unwrap();
        let digits: String = whole.as_str().chars().filter(|ch| ch.is_ascii_digit()).collect();
        if digits.len() != 12 || !verhoeff_ok(&digits) || id_adjacent(text, whole.start(), whole.end()) {
            continue;
        }
        // Unspaced, a 12-digit run passes the checksum one time in ten
        // (a UUID segment, an order id): ask for the word next to it.
        if !whole.as_str().contains(' ') {
            let lo = whole.start().saturating_sub(40);
            let hi = (whole.end() + 40).min(text.len());
            if !d.aadhaar_context.is_match(&text[lo..hi]) {
                continue;
            }
        }
        spans.add(whole.start(), whole.end(), "national_id", false);
    }
    for c in d.sin.captures_iter(text) {
        let v = c.get(1).unwrap();
        let digits: String = v.as_str().chars().filter(|ch| ch.is_ascii_digit()).collect();
        if luhn_ok(&digits) {
            spans.add(v.start(), v.end(), "national_id", false);
        }
    }
    for c in d.userinfo.captures_iter(text) {
        let v = c.get(1).unwrap();
        spans.add(v.start(), v.end(), "url_credential", false);
    }
    for c in d.url_param.captures_iter(text) {
        let v = c.get(2).unwrap();
        spans.add(v.start(), v.end(), "url_credential", false);
    }

    // Encoded forms: a base64, hex or percent-encoded run that hides a hit.
    if depth == 0 {
        for m in d.base64_run.find_iter(text) {
            if spans.0.iter().any(|s| s.start <= m.start() && m.end() <= s.end) {
                continue;
            }
            if let Some(decoded) = decode_base64(m.as_str()).and_then(|b| printable_text(&b)) {
                let mut inner = TextScan::default();
                if !find_spans(&decoded, 1, &mut inner).is_empty() {
                    spans.add(m.start(), m.end(), "encoded", false);
                }
            }
        }
        for m in d.hex_run.find_iter(text) {
            if let Some(decoded) = decode_hex(m.as_str()).and_then(|b| printable_text(&b)) {
                let mut inner = TextScan::default();
                if !find_spans(&decoded, 1, &mut inner).is_empty() {
                    spans.add(m.start(), m.end(), "encoded", false);
                }
            }
        }
        if d.percent.is_match(text) {
            let decoded = percent_decode(text);
            if decoded != text {
                let mut inner = TextScan::default();
                if !find_spans(&decoded, 1, &mut inner).is_empty() {
                    // Redact every token that carries an escape.
                    let mut pos = 0usize;
                    for token in text.split(char::is_whitespace) {
                        let start = text[pos..].find(token).map(|i| pos + i).unwrap_or(pos);
                        let end = start + token.len();
                        if d.percent.is_match(token) {
                            spans.add(start, end, "encoded", false);
                        }
                        pos = end;
                    }
                }
            }
        }
    }
    spans.0
}

/// Screen one text: redact every span, count by class.
fn screen_text(text: &str) -> TextScan {
    let mut scan = TextScan { chars: text.chars().count(), ..Default::default() };
    if text.is_empty() || is_marker(text) {
        scan.out = text.to_string();
        return scan;
    }
    let mut spans = find_spans(text, 0, &mut scan);
    if spans.is_empty() {
        scan.out = text.to_string();
        return scan;
    }
    // Byte offsets must sit on char boundaries; widen a span that does not.
    for s in spans.iter_mut() {
        while s.start > 0 && !text.is_char_boundary(s.start) {
            s.start -= 1;
        }
        while s.end < text.len() && !text.is_char_boundary(s.end) {
            s.end += 1;
        }
    }
    spans.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
    let mut merged: Vec<Span> = Vec::new();
    for s in spans {
        if let Some(last) = merged.last_mut() {
            if s.start < last.end {
                if s.end > last.end {
                    last.end = s.end;
                }
                last.keep_edges = false;
                continue;
            }
        }
        merged.push(s);
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    let mut counters: BTreeMap<&'static str, usize> = BTreeMap::new();
    for s in merged {
        out.push_str(&text[cursor..s.start]);
        let k = counters.entry(s.class).or_insert(0);
        *k += 1;
        let piece = &text[s.start..s.end];
        if s.keep_edges {
            let digits: Vec<char> = piece.chars().filter(|c| c.is_ascii_digit()).collect();
            let head: String = digits.iter().take(6).collect();
            let tail: String = digits.iter().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
            out.push_str(&format!("{head}[REDACTED:{}#{k}]{tail}", s.class));
            scan.redacted_chars += piece.chars().count().saturating_sub(10);
        } else {
            out.push_str(&format!("[REDACTED:{}#{k}]", s.class));
            scan.redacted_chars += piece.chars().count();
        }
        cursor = s.end;
    }
    out.push_str(&text[cursor..]);
    scan.classes = counters;
    scan.out = out;
    scan
}

/// A key whose string value is a secret by name alone.
fn secret_key(key: &str, value: &str) -> bool {
    let k: String = key.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    let v = value.trim();
    if v.len() < 4 || v.chars().all(|c| matches!(c, '*' | '•' | '●' | 'x' | 'X')) || is_marker(v) {
        return false;
    }
    let lower = v.to_ascii_lowercase();
    if matches!(lower.as_str(), "true" | "false" | "null" | "undefined" | "none" | "[object object]") {
        return false;
    }
    const STRONG: &[&str] = &[
        "password", "passwd", "pwd", "passphrase", "secret", "clientsecret", "apikey", "privatekey",
        "accesstoken", "refreshtoken", "idtoken", "authtoken", "sessiontoken", "bearertoken", "authorization",
        "cardnumber", "ccnumber", "iban", "ssn",
    ];
    const DIGITS: &[&str] = &["cvv", "cvc", "otp", "onetimecode", "securitycode"];
    const TOKEN_LIKE: &[&str] = &["session", "sessionid", "cookie", "cookies", "token", "csrftoken", "jwt"];
    if STRONG.contains(&k.as_str()) {
        return true;
    }
    if DIGITS.contains(&k.as_str()) {
        return v.chars().all(|c| c.is_ascii_digit()) && (3..=8).contains(&v.len());
    }
    if TOKEN_LIKE.contains(&k.as_str()) {
        return !v.contains(' ') && v.len() >= 8 && (v.len() >= 20 || v.chars().any(|c| c.is_ascii_digit()));
    }
    false
}

fn char_code_array(items: &[Value]) -> Option<String> {
    if items.len() < 16 {
        return None;
    }
    let mut s = String::with_capacity(items.len());
    for v in items {
        let n = v.as_u64()?;
        if !(32..=126).contains(&n) {
            return None;
        }
        s.push(n as u8 as char);
    }
    Some(s)
}

fn walk(v: &mut Value, report: &mut Report) {
    // An array of character codes is a string in disguise.
    let as_codes = match &*v {
        Value::Array(items) => char_code_array(items),
        _ => None,
    };
    if let Some(text) = as_codes {
        report.leaves += 1;
        let scan = screen_text(&text);
        if scan.hit() {
            report.hit_leaves += 1;
            report.absorb(&scan);
            *report.classes.entry("encoded").or_insert(0) += 1;
            report.redacted_chars += text.chars().count();
            *v = Value::String("[REDACTED:encoded#1]".into());
        } else {
            report.chars += text.chars().count();
        }
        return;
    }
    match v {
        Value::String(s) => {
            report.leaves += 1;
            let scan = screen_text(s);
            if scan.hit() {
                report.hit_leaves += 1;
                *s = scan.out.clone();
            }
            report.absorb(&scan);
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                walk(item, report);
            }
        }
        Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                let by_name = match val {
                    Value::String(s) => secret_key(key, s),
                    _ => false,
                };
                if by_name {
                    report.leaves += 1;
                    report.hit_leaves += 1;
                    let n = val.as_str().map(|s| s.chars().count()).unwrap_or(0);
                    report.chars += n;
                    report.redacted_chars += n;
                    *report.classes.entry("secret_field").or_insert(0) += 1;
                    *val = Value::String("[REDACTED:secret_field]".into());
                    continue;
                }
                walk(val, report);
            }
        }
        _ => {}
    }
}

/// The thrown error's text, screened: redacted, or withheld when it is itself
/// credential-shaped, then capped.
pub fn screen_error(text: &str) -> (String, Report) {
    let scan = screen_text(text);
    let mut report = Report::default();
    report.absorb(&scan);
    let shaped = scan.saw_private_key
        || scan.jar_pairs >= 2
        || scan.classes.contains_key("jwt")
        || scan.classes.contains_key("api_key")
        || scan.classes.contains_key("card")
        || (scan.chars > 0 && scan.redacted_chars * 2 > scan.chars);
    let out = if scan.hit() && shaped {
        report.withheld = Some("error text withheld".into());
        "error text withheld (it contained credential-like values)".to_string()
    } else {
        scan.out
    };
    let capped: String = out.chars().take(MAX_ERROR_CHARS).collect();
    (capped, report)
}

/// The result value, screened in place. A page-cut result ({__cut, text})
/// loses its last CUT_MARGIN_CHARS first and keeps its shape.
pub fn screen_result(value: Value) -> (Value, Report) {
    let mut report = Report::default();
    let mut value = value;
    let page_cut = value.get("__cut").and_then(Value::as_u64).is_some()
        && value.get("text").and_then(Value::as_str).is_some();
    if page_cut {
        let total = value["__cut"].clone();
        let text = value["text"].as_str().unwrap_or("");
        let keep = text.chars().count().saturating_sub(CUT_MARGIN_CHARS);
        let trimmed: String = text.chars().take(keep).collect();
        let scan = screen_text(&trimmed);
        report.leaves = 1;
        if scan.hit() {
            report.hit_leaves = 1;
        }
        report.absorb(&scan);
        value = json!({"__cut": total, "text": scan.out});
    } else {
        walk(&mut value, &mut report);
    }
    // Values split across fields: a token that only shows once joined.
    if !page_cut && report.leaves > 1 {
        let joined = value.to_string();
        let mut extra = TextScan::default();
        let spans = find_spans(&joined, 1, &mut extra);
        if spans.iter().any(|s| matches!(s.class, "jwt" | "api_key" | "private_key")) {
            report.withheld = Some(
                "result withheld: it carried a token split across fields. Cookies, tokens, passwords, one-time \
                 codes and card numbers never leave the browser; continue the task without them"
                    .into(),
            );
            return (value, report);
        }
    }
    report.decide();
    (value, report)
}

// ------------------------------------------------------------ glue (scanner, Python, audit)

#[cfg(not(feature = "guard_standalone"))]
mod glue {
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use pyo3::prelude::*;
    use serde_json::json;

    use super::{check_page, screen_code, Report};
    use crate::browser::ScannerInner;
    use crate::controller::service::scan_op;

    /// What the pre-check hands on to the post-check and the audit line.
    pub struct Gate {
        pub idx: i64,
        pub origin: String,
        pub mode: &'static str,
        pub code_len: usize,
        pub code_hash: String,
        pub code_head: String,
        pub audit_path: Option<PathBuf>,
        pub started: Instant,
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
        digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// AutoCua_data/audit, asked of Python (AutoCua.data_root knows where
    /// AutoCua_data lives; a second definition here would drift).
    fn audit_dir(py: Python<'_>) -> Option<PathBuf> {
        let module = py.import("AutoCua").ok()?;
        let root = module.call_method0("data_root").ok()?;
        let root = PathBuf::from(root.str().ok()?.extract::<String>().ok()?);
        Some(root.join("audit"))
    }

    fn gate(py: Python<'_>, idx: i64, origin: String, mode: &'static str, source: &str) -> Gate {
        Gate {
            idx,
            origin,
            mode,
            code_len: source.chars().count(),
            code_hash: sha256_hex(source.as_bytes()),
            code_head: source.chars().take(120).collect::<String>().replace('\n', " "),
            audit_path: audit_dir(py).map(|d| d.join("run_script.jsonl")),
            started: Instant::now(),
        }
    }

    /// Before anything executes: the tab, the page, the script text. Err is
    /// the refusal for the model; every failure to resolve a fact is a refusal.
    pub fn before(
        py: Python<'_>,
        scanner: &Arc<Mutex<ScannerInner>>,
        idx: i64,
        source: &str,
    ) -> Result<Gate, String> {
        let facts = scan_op(py, scanner, move |s| {
            let current = s.current_target_id()?;
            let listed = s.listed_tab(idx);
            let url = match &listed {
                Some(id) => s.tab_url_now(id),
                None => String::new(),
            };
            Ok((current, listed, url, s.bridged()))
        });
        let (current, listed, url, bridged) = match facts {
            Ok(f) => f,
            Err(_) => {
                let g = gate(py, idx, String::new(), "unknown", source);
                let m = "run_script is not available right now: the tab could not be resolved".to_string();
                audit(&g, "refused", &Report::default(), Some(&m), 0, 0);
                return Err(m);
            }
        };
        let mode = if bridged { "extension" } else { "port" };
        let refuse = |py: Python<'_>, origin: String, m: String| -> Result<Gate, String> {
            let g = gate(py, idx, origin, mode, source);
            audit(&g, "refused", &Report::default(), Some(&m), 0, 0);
            Err(m)
        };
        let listed = match listed {
            Some(id) => id,
            None => {
                return refuse(
                    py,
                    String::new(),
                    format!("tab [{idx}] is not on the tab list - read the fresh <all_tabs> before acting on a tab"),
                )
            }
        };
        if listed != current {
            return refuse(
                py,
                String::new(),
                format!(
                    "run_script runs only in the tab you are working in: switch_tab to [{idx}] first, or read \
                     the current tab"
                ),
            );
        }
        let (origin, site) = match check_page("run_script", &url) {
            Ok(x) => x,
            Err(m) => return refuse(py, String::new(), m),
        };
        if let Err(m) = screen_code(source, &site) {
            return refuse(py, origin, m);
        }
        Ok(gate(py, idx, origin, mode, source))
    }

    /// One JSON line per call: hashes, sizes and counts, never the data.
    /// Best effort: a failed write is printed once and never fails the call.
    pub fn audit(
        gate: &Gate,
        outcome: &str,
        report: &Report,
        message: Option<&str>,
        result_chars: usize,
        dropped: usize,
    ) {
        let Some(path) = &gate.audit_path else { return };
        let classes: serde_json::Map<String, serde_json::Value> =
            report.classes.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
        let line = json!({
            "time": chrono::Utc::now().to_rfc3339(),
            "tool": "run_script",
            "mode": gate.mode,
            "tab": gate.idx,
            "origin": gate.origin,
            "code_sha256": gate.code_hash,
            "code_chars": gate.code_len,
            "code_head": gate.code_head,
            "outcome": outcome,
            "message": message.map(|m| m.chars().take(200).collect::<String>()),
            "result_chars": result_chars,
            "dropped": dropped,
            "redacted": classes,
            "withheld": report.withheld.as_deref().map(|w| w.chars().take(120).collect::<String>()),
            "ms": gate.started.elapsed().as_millis() as u64,
        });
        let attempt = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            if let Ok(meta) = std::fs::metadata(path) {
                if meta.len() > 5 * 1024 * 1024 {
                    let _ = std::fs::rename(path, path.with_extension("jsonl.1"));
                }
            }
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
            writeln!(file, "{line}")
        })();
        if let Err(e) = attempt {
            println!("run_script audit not written: {e}");
        }
    }
}

#[cfg(not(feature = "guard_standalone"))]
pub use glue::{audit, before};
