//! roller - CLI for the ROLLER REST and Reporting APIs. Every operation comes from roller_ops.json,
//! generated from ROLLER's documentation repo by `roller sync`.

mod sync;

#[cfg(windows)]
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use clap::builder::PossibleValuesParser;
use clap::{Arg, ArgAction, ArgMatches, Command};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{env, fs, thread};

const GUIDE: &str = "roller - command line access to the ROLLER REST and Reporting APIs

Getting started
  roller login                      Add a venue (asks for its API client ID + secret)
  roller login --env live           Add a venue's Live credentials (Playground is the default)
  roller whoami                     Show which venue and environment commands will use

Venues
  roller profiles                   List saved venues (* = current)
  roller use NAME                   Switch the current venue
  roller --profile NAME ...         Use a different venue for one command
  roller logout [NAME]              Remove a saved venue

Finding operations
  roller ops [WORD] [--json]        List all 88 operations, or those matching WORD
  roller describe API OPERATION     Everything about an operation as JSON: params, body schema, responses
  roller example API OPERATION      A request-body template to edit and pass to --body
  roller docs [TOPIC]               List ROLLER's guides, or print one (e.g. roller docs checkout-workflow)

Calling operations
  roller rest OPERATION [options]   REST API: bookings, guests, products, capacity, discounts...
  roller report OPERATION [options] Reporting API: daily data exports (max 1-day ranges)
  roller rest OPERATION -h          Options for one operation

Examples
  roller ops booking
  roller rest get-booking-detail --uniqueId ABC123
  roller report get-bookings --startDate 2026-10-12 --endDate 2026-10-13 --all-pages
  roller example rest create-a-booking > booking.json
  roller rest create-a-booking --body @booking.json --dry-run

Options for any call
  --dry-run                         Print the request instead of sending it (no login needed)
  --yes                             Required for any non-GET call on a live venue
  --compact                         Print JSON on one line
  --env live|play, --profile NAME   Override the current venue for one command

Exit codes: 0 ok, 1 CLI/config/network error, 2 bad arguments, 4 HTTP 4xx, 5 HTTP 5xx
ROLLER_CLIENT_ID / ROLLER_CLIENT_SECRET env vars override saved venues (CI, `op run`)

";

/// GPL-3.0 "Appropriate Legal Notices": shown by bare `roller`, `roller -h` and `roller --version`.
const NOTICE: &str = "Roller CLI  Copyright (C) 2026  CasualBot
This program comes with ABSOLUTELY NO WARRANTY. This is free software, and you are
welcome to redistribute it under the terms of the GNU GPL v3 or later; see LICENSE
or <https://www.gnu.org/licenses/gpl-3.0.html>.
";

const HOSTS: [(&str, &str); 2] = [("live", "https://api.roller.app"), ("play", "https://api.play.roller.app")];
const EMBEDDED_OPS: &str = include_str!("../roller_ops.json");
const APIS: [(&str, &str, &str); 2] = [("rest", "REST API", "rest"), ("report", "Reporting API", "reporting")];

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("roller: {msg}");
    std::process::exit(1)
}

fn host(env_name: &str) -> &'static str {
    HOSTS.iter().find(|h| h.0 == env_name).map(|h| h.1).unwrap_or_else(|| die(format!("unknown env '{env_name}' (live|play)")))
}

fn roller_dir() -> PathBuf {
    if let Ok(d) = env::var("ROLLER_HOME") {
        return d.into();
    }
    let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")).unwrap_or_else(|| die("can't find home directory"));
    PathBuf::from(home).join(".roller")
}

/// Write a file only this user can read. On Unix the dir is 0700 and the file is created 0600
/// (not chmod-ed after writing, which would leave it briefly readable). Windows: the user-profile ACL applies.
fn write_private(path: &Path, text: &str) {
    let fail = |e: io::Error| -> ! { die(format!("{}: {e}", path.display())) };
    let dir = path.parent().unwrap();
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).unwrap_or_else(|e| fail(e));
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700)); // tighten a pre-existing dir
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600)); // tighten a pre-existing file before writing
        opts.mode(0o600);
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir).unwrap_or_else(|e| fail(e));
    opts.open(path).and_then(|mut f| f.write_all(text.as_bytes())).unwrap_or_else(|e| fail(e));
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

fn show(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}

// ---------- operations table ----------

#[derive(Serialize, Deserialize, Clone)]
pub struct Param {
    name: String,
    #[serde(rename = "in")]
    loc: String,
    required: bool,
    description: String,
    schema: Value,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Body {
    required: bool,
    schema: Value,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Op {
    method: String,
    path: String,
    summary: String,
    description: String,
    body: Option<Body>,
    params: Vec<Param>,
    responses: IndexMap<String, Value>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Guide {
    slug: String,
    title: String,
    section: String,
    path: String,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Spec {
    ops: IndexMap<String, IndexMap<String, Op>>,
    guides: Vec<Guide>,
}

fn load_spec() -> Spec {
    // `roller sync` writes ~/.roller/ops.json; an unreadable or outdated one falls back to the table baked in at build time.
    let synced = fs::read_to_string(roller_dir().join("ops.json")).ok().and_then(|t| serde_json::from_str::<Spec>(&t).ok());
    synced.filter(|s| !s.ops.is_empty()).unwrap_or_else(|| serde_json::from_str(EMBEDDED_OPS).unwrap_or_else(|e| die(format!("bad embedded ops table: {e}"))))
}

fn type_of(s: &Value) -> Option<&str> {
    // OpenAPI 3.1 allows "type": ["string", "null"]
    s["type"].as_str().or_else(|| s["type"].as_array()?.iter().filter_map(Value::as_str).find(|t| *t != "null"))
}

/// A plausible value for a schema: its own example/default/first enum, else a typed placeholder.
fn example(s: &Value) -> Value {
    for k in ["example", "default"] {
        if let Some(e) = s.get(k) {
            return e.clone();
        }
    }
    if let Some(e) = s["enum"].as_array().and_then(|a| a.first()) {
        return e.clone();
    }
    // ponytail: ROLLER's specs use only allOf; add oneOf/anyOf here if they ever appear
    if let Some(all) = s["allOf"].as_array() {
        let mut o = serde_json::Map::new();
        for part in all {
            if let Value::Object(m) = example(part) {
                o.extend(m);
            }
        }
        return Value::Object(o);
    }
    if let Some(props) = s["properties"].as_object() {
        return Value::Object(props.iter().map(|(k, v)| (k.clone(), example(v))).collect());
    }
    match type_of(s) {
        Some("array") => json!([example(&s["items"])]),
        Some("object") => json!({}),
        Some("integer") | Some("number") => json!(0),
        Some("boolean") => json!(false),
        Some("string") => json!(match s["format"].as_str() {
            Some("date") => "2026-01-01",
            Some("date-time") => "2026-01-01T09:00:00",
            Some("uuid") => "00000000-0000-0000-0000-000000000000",
            _ => "string",
        }),
        _ => Value::Null,
    }
}

fn usage(api: &str, id: &str, op: &Op) -> String {
    let mut u = format!("roller {api} {id}");
    for p in op.params.iter().filter(|p| p.required) {
        u += &format!(" --{} <{}>", p.name, type_of(&p.schema).unwrap_or("value"));
    }
    if op.body.as_ref().is_some_and(|b| b.required) {
        u += " --body @body.json";
    }
    u
}

fn describe(api: &str, id: &str, op: &Op) -> Value {
    let docs_api = APIS.iter().find(|a| a.0 == api).map(|a| a.2).unwrap_or(api);
    json!({
        "api": api,
        "operation": id,
        "method": op.method,
        "path": op.path,
        "summary": op.summary,
        "description": op.description,
        "docs_url": format!("https://docs.roller.app/docs/api/{docs_api}/operations/{id}"),
        "usage": usage(api, id, op),
        "writes": op.method != "GET",
        "paginated": op.params.iter().any(|p| p.name == "pageNumber"),
        "parameters": op.params.iter().map(|p| json!({
            "name": p.name, "in": p.loc, "required": p.required, "schema": p.schema, "description": p.description,
        })).collect::<Vec<_>>(),
        "body": op.body.as_ref().map(|b| json!({ "required": b.required, "schema": b.schema, "example": example(&b.schema) })),
        "responses": op.responses,
    })
}

// ---------- HTTP + auth ----------

type Resp = (u16, Value, Option<String>);
type Transport = Box<dyn Fn(&str, &str, &[(String, String)], Option<&Value>) -> Result<Resp, String>>;

fn http(method: &str, url: &str, headers: &[(String, String)], body: Option<&Value>) -> Result<Resp, String> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(120)).build();
    let mut req = agent.request(method, url).set("Accept", "application/json");
    for (k, v) in headers {
        req = req.set(k, v);
    }
    let res = match body {
        Some(b) => req.send_json(b),
        None => req.call(),
    };
    let resp = match res {
        Ok(r) | Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(e.to_string()),
    };
    let (status, retry_after) = (resp.status(), resp.header("Retry-After").map(String::from));
    let mut text = String::new();
    resp.into_reader().read_to_string(&mut text).map_err(|e| e.to_string())?;
    let data = if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::String(text)) };
    Ok((status, data, retry_after))
}

fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn build_path(template: &str, values: &[(String, String)]) -> String {
    values.iter().fold(template.to_string(), |out, (k, v)| out.replace(&format!("{{{k}}}"), &enc(v)))
}

fn build_url(base: &str, path: &str, query: &[(String, String)]) -> String {
    let q: Vec<String> = query.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect();
    if q.is_empty() { format!("{base}{path}") } else { format!("{base}{path}?{}", q.join("&")) }
}

fn token_cache(env_name: &str, cid: &str) -> PathBuf {
    // The client ID isn't secret (it's stored as-is in config.json); keep it filename-safe.
    let id: String = cid.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    roller_dir().join(format!("token-{env_name}-{id}.json"))
}

fn base_url(env_name: &str) -> String {
    match env::var("ROLLER_BASE_URL").ok().filter(|u| !u.is_empty()) {
        // The client secret and bearer token go to this host, so never over plain HTTP.
        Some(u) if !u.starts_with("https://") => die(format!("ROLLER_BASE_URL must start with https:// (got '{u}')")),
        Some(u) => u.trim_end_matches('/').to_string(),
        None => host(env_name).into(),
    }
}

/// The live-write guard keys on where requests actually go: anything but the Playground host
/// (including custom ROLLER_BASE_URL domains) is treated as live.
fn is_live(base: &str) -> bool {
    base != host("play")
}

struct Client {
    cid: String,
    secret: String,
    base: String,
    cache: PathBuf,
    send: Transport,
}

impl Client {
    fn new(env_name: &str, cid: &str, secret: &str) -> Self {
        Self::with(env_name, cid, secret, Box::new(http))
    }

    fn with(env_name: &str, cid: &str, secret: &str, send: Transport) -> Self {
        // Cache per env+client: ROLLER suspends credentials that request a token per call.
        Client { cid: cid.into(), secret: secret.into(), base: base_url(env_name), cache: token_cache(env_name, cid), send }
    }

    fn token(&self, fresh: bool) -> String {
        if !fresh {
            let cached: Option<Value> = fs::read_to_string(&self.cache).ok().and_then(|t| serde_json::from_str(&t).ok());
            // An expired, unreadable or old-format cache just means one fresh token request.
            if let Some(tok) = cached.filter(|t| t["expires_at"].as_f64().unwrap_or(0.0) > now() + 60.0).and_then(|t| unseal(&t["token"])) {
                return tok;
            }
        }
        let body = json!({"client_id": self.cid, "client_secret": self.secret});
        let (status, v, _) = (self.send)("POST", &format!("{}/token", self.base), &[], Some(&body)).unwrap_or_else(|e| die(e));
        let tok = match (status, v["access_token"].as_str()) {
            (200, Some(t)) => t.to_string(),
            _ => die(format!("token request failed ({status}): {} - check client_id/secret and env (play and live credentials differ)", show(&v))),
        };
        write_private(&self.cache, &json!({"token": seal(&tok), "expires_at": now() + v["expires_in"].as_f64().unwrap_or(3600.0)}).to_string());
        tok
    }

    fn call(&self, method: &str, path: &str, query: &[(String, String)], headers: &[(String, String)], body: Option<&Value>) -> (u16, Value) {
        let url = build_url(&self.base, path, query);
        let (mut refresh, mut refreshed, mut last) = (false, false, (0, Value::Null));
        for attempt in 0..4 {
            let mut h = vec![("Authorization".to_string(), format!("Bearer {}", self.token(refresh)))];
            h.extend_from_slice(headers);
            let (status, data, retry_after) = (self.send)(method, &url, &h, body).unwrap_or_else(|e| die(e));
            refresh = false;
            if status == 401 && !refreshed {
                // docs: reuse the token until a 401, then refresh once
                (refresh, refreshed, last) = (true, true, (status, data));
                continue;
            }
            if status == 429 && attempt < 3 {
                // limits: REST 600/min, Reporting 120/min
                let wait = retry_after.and_then(|s| s.trim().parse().ok()).unwrap_or(10u64);
                eprintln!("roller: 429, retrying in {wait}s");
                thread::sleep(Duration::from_secs(wait));
                last = (status, data);
                continue;
            }
            return (status, data);
        }
        last
    }
}

/// Call fetch(pageNumber) until a short/empty page. Reporting endpoints return a JSON array.
fn paginate(mut fetch: impl FnMut(u32) -> (u16, Value), page_size: usize) -> (u16, Value) {
    let (mut rows, mut page) = (Vec::new(), 1);
    loop {
        let (status, data) = fetch(page);
        let Value::Array(items) = data else { return (status, if page == 1 { data } else { Value::Array(rows) }) };
        if status >= 400 {
            return (status, if page == 1 { Value::Array(items) } else { Value::Array(rows) });
        }
        let n = items.len();
        rows.extend(items);
        if n < page_size {
            return (status, Value::Array(rows));
        }
        page += 1;
    }
}

// ---------- secret storage: Windows DPAPI, macOS Keychain, else a 0600 file ----------

#[cfg(windows)]
fn dpapi(data: &[u8], encrypt: bool) -> Option<Vec<u8>> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::*;
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: null_mut() };
    // Ciphertext only decrypts as this Windows user on this machine.
    let ok = unsafe {
        if encrypt {
            CryptProtectData(&input, null(), null(), null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        } else {
            CryptUnprotectData(&input, null_mut(), null(), null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        }
    };
    if ok == 0 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    unsafe { LocalFree(out.pbData as _) };
    Some(bytes)
}

/// Protect a value stored in a file: DPAPI on Windows; elsewhere the 0600 file is the protection.
#[cfg(windows)]
fn seal(s: &str) -> Value {
    json!({"dpapi": B64.encode(dpapi(s.as_bytes(), true).unwrap_or_else(|| die(format!("DPAPI encrypt failed: {}", io::Error::last_os_error()))))})
}

#[cfg(not(windows))]
fn seal(s: &str) -> Value {
    json!({"plain": s})
}

fn unseal(v: &Value) -> Option<String> {
    #[cfg(windows)]
    if let Some(b) = v["dpapi"].as_str() {
        return String::from_utf8(dpapi(&B64.decode(b).ok()?, false)?).ok();
    }
    v["plain"].as_str().map(String::from)
}

#[cfg(target_os = "macos")]
fn keychain(account: &str) -> keyring::Entry {
    keyring::Entry::new("roller-cli", account).unwrap_or_else(|e| die(e))
}

#[cfg(target_os = "macos")]
fn protect(secret: &str, account: &str) -> Value {
    keychain(account).set_password(secret).unwrap_or_else(|e| die(format!("keychain: {e}")));
    json!({"keychain": account})
}

// ponytail: Linux keeps the secret in the 0600 config file (no keyring daemon on servers/WSL); add secret-service if needed
#[cfg(not(target_os = "macos"))]
fn protect(secret: &str, _account: &str) -> Value {
    seal(secret)
}

fn unprotect(stored: &Value) -> String {
    #[cfg(target_os = "macos")]
    if let Some(a) = stored["keychain"].as_str() {
        return keychain(a).get_password().unwrap_or_else(|e| die(format!("keychain: {e} - run `roller login` again")));
    }
    unseal(stored).unwrap_or_else(|| die("saved secret can't be read on this machine - run `roller login` again"))
}

fn forget_secret(stored: &Value) {
    #[cfg(target_os = "macos")]
    if let Some(a) = stored["keychain"].as_str() {
        let _ = keychain(a).delete_credential();
    }
    let _ = stored;
}

// ---------- profiles ----------

#[derive(Serialize, Deserialize, Default)]
struct Config {
    current: Option<String>,
    #[serde(default)]
    profiles: IndexMap<String, Profile>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Profile {
    env: String,
    client_id: String,
    secret: Value,
    venue: String,
    venue_id: Value,
}

fn load_config() -> Config {
    fs::read_to_string(roller_dir().join("config.json")).ok().map(|t| serde_json::from_str(&t).unwrap_or_else(|e| die(format!("bad config: {e}")))).unwrap_or_default()
}

fn save_config(cfg: &Config) {
    write_private(&roller_dir().join("config.json"), &serde_json::to_string_pretty(cfg).unwrap());
}

fn env_creds(env_flag: Option<&str>) -> Option<(String, String, String, String)> {
    let var = |k: &str| env::var(k).ok().filter(|v| !v.is_empty());
    let (cid, secret) = (var("ROLLER_CLIENT_ID")?, var("ROLLER_CLIENT_SECRET")?);
    let e = env_flag.map(String::from).or(var("ROLLER_ENV")).unwrap_or_else(|| "play".into());
    Some(("(env vars)".into(), e, cid, secret))
}

/// -> (profile name, env) without touching the secret. Env vars win, then --profile / ROLLER_PROFILE, then current.
fn pick(profile: Option<&str>, env_flag: Option<&str>) -> Option<(String, Profile)> {
    let cfg = load_config();
    let name = profile.map(String::from).or(env::var("ROLLER_PROFILE").ok().filter(|v| !v.is_empty())).or(cfg.current.clone())?;
    let p = cfg.profiles.get(&name).cloned().unwrap_or_else(|| die(format!("no profile '{name}' - see `roller profiles`")));
    if env_flag.is_some_and(|e| e != p.env) {
        die(format!("profile '{name}' is for {}; credentials don't cross environments", p.env));
    }
    Some((name, p))
}

/// -> (profile, env, client_id, secret)
fn resolve(profile: Option<&str>, env_flag: Option<&str>) -> (String, String, String, String) {
    if let Some(c) = env_creds(env_flag) {
        return c;
    }
    let (name, p) = pick(profile, env_flag).unwrap_or_else(|| die("not logged in - run `roller login`"));
    let secret = unprotect(&p.secret);
    (name, p.env, p.client_id, secret)
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').into()
}

fn read_line() -> String {
    let mut s = String::new();
    io::stdin().read_line(&mut s).unwrap_or_else(|e| die(e));
    s.trim().to_string()
}

fn login(env_name: &str, profile: Option<&str>, cid: Option<&str>, secret_stdin: bool) {
    host(env_name);
    let cid = cid.map(String::from).unwrap_or_else(|| {
        println!("Create a key in {}Venue Manager > Settings > Integrations > API keys > Create client key.", if env_name == "play" { "Playground " } else { "" });
        print!("ROLLER {env_name} client_id: ");
        io::stdout().flush().ok();
        read_line()
    });
    // Secret never goes in argv (shell history / process list): prompt, or pipe e.g. `op read ... | roller login --secret-stdin`
    let secret = if secret_stdin { read_line() } else { rpassword::prompt_password("client_secret (hidden): ").unwrap_or_else(|e| die(e)).trim().to_string() };
    if cid.is_empty() || secret.is_empty() {
        die("client_id and client_secret are both required");
    }
    save_login(&Client::new(env_name, &cid, &secret), env_name, profile);
}

fn save_login(client: &Client, env_name: &str, profile: Option<&str>) {
    client.token(true); // dies with ROLLER's error if the pair is wrong or for the other environment
    let (status, venue) = client.call("GET", "/venues/me", &[], &[], None);
    if status != 200 {
        die(format!("credentials work but /venues/me failed ({status}): {}", show(&venue)));
    }
    let venue_name = venue["name"].as_str().unwrap_or("venue").to_string();
    let name = profile.map(String::from).unwrap_or_else(|| format!("{}-{env_name}", slug(&venue_name)));
    let mut cfg = load_config();
    if let Some(old) = cfg.profiles.get(&name) {
        forget_secret(&old.secret);
        // Don't leave the replaced credentials' bearer token (valid ~24h) on disk.
        let old_cache = token_cache(&old.env, &old.client_id);
        if old_cache != client.cache {
            let _ = fs::remove_file(old_cache);
        }
    }
    let secret = protect(&client.secret, &format!("{env_name}:{}", client.cid));
    cfg.profiles.insert(name.clone(), Profile { env: env_name.into(), client_id: client.cid.clone(), secret, venue: venue_name.clone(), venue_id: venue["id"].clone() });
    cfg.current = Some(name.clone());
    save_config(&cfg);
    println!("Logged in to {venue_name} (venue {}, {env_name}) as profile '{name}' - now the default.", venue["id"]);
}

// ---------- CLI ----------

fn api_arg() -> Arg {
    Arg::new("api").required(true).value_parser(["rest", "report"]).help("rest or report")
}

fn cli(spec: &Spec) -> Command {
    let global_flag = |id: &'static str, help: &'static str| Arg::new(id).long(id).global(true).action(ArgAction::SetTrue).help(help);
    let mut c = Command::new("roller")
        .version(format!("{}

{NOTICE}", env!("CARGO_PKG_VERSION")))
        .override_help(format!("{GUIDE}{NOTICE}"))
        .subcommand_required(true)
        .arg(Arg::new("env").long("env").global(true).value_parser(["live", "play"]).help("play (default for new logins) or live"))
        .arg(Arg::new("profile").long("profile").global(true).help("saved venue to use"))
        .arg(global_flag("compact", "single-line JSON output"))
        .arg(global_flag("dry-run", "print the request instead of sending it"))
        .arg(global_flag("yes", "allow non-GET calls on a live venue"))
        .subcommand(Command::new("login").visible_alias("auth").about("Add a venue's API credentials as a profile")
            .arg(Arg::new("client-id").long("client-id").help("prompted if omitted"))
            .arg(Arg::new("secret-stdin").long("secret-stdin").action(ArgAction::SetTrue).help("read client_secret from stdin instead of prompting")))
        .subcommand(Command::new("logout").about("Remove a saved venue").arg(Arg::new("name")))
        .subcommand(Command::new("use").about("Switch the current venue").arg(Arg::new("name").required(true)))
        .subcommand(Command::new("profiles").about("List saved venues").arg(Arg::new("json").long("json").action(ArgAction::SetTrue)))
        .subcommand(Command::new("whoami").about("Show which venue/env commands will use"))
        .subcommand(Command::new("ops").about("List operations")
            .arg(Arg::new("filter"))
            .arg(Arg::new("json").long("json").action(ArgAction::SetTrue).help("machine-readable list")))
        .subcommand(Command::new("describe").about("Full JSON description of an operation").arg(api_arg()).arg(Arg::new("operation").required(true)))
        .subcommand(Command::new("example").about("Request-body template for an operation").arg(api_arg()).arg(Arg::new("operation").required(true)))
        .subcommand(Command::new("docs").about("List or print ROLLER's API guides")
            .arg(Arg::new("topic"))
            .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)))
        .subcommand(Command::new("sync").about("Rebuild the operation table from ROLLER's documentation repo")
            .arg(Arg::new("out").long("out").help("output file (default ~/.roller/ops.json)")));
    for (api, title, _) in APIS {
        let mut sub = Command::new(api).about(title).subcommand_required(true);
        for (id, op) in spec.ops.get(api).into_iter().flatten() {
            let mut more = format!("{} {}\n{}\n\nFull details: roller describe {api} {id}", op.method, op.path, op.summary);
            if op.body.is_some() {
                more += &format!("\nBody template:  roller example {api} {id}");
            }
            let mut oc = Command::new(id.clone()).about(op.summary.clone()).long_about(more);
            for p in &op.params {
                let first = p.description.lines().next().unwrap_or("");
                let kind = [Some(p.loc.as_str()), type_of(&p.schema), p.schema["format"].as_str()].into_iter().flatten().collect::<Vec<_>>().join(", ");
                let mut arg = Arg::new(p.name.clone()).long(p.name.clone()).required(p.required).help(format!("[{kind}] {first}"));
                if let Some(values) = p.schema["enum"].as_array() {
                    arg = arg.value_parser(PossibleValuesParser::new(values.iter().map(show).collect::<Vec<_>>())).ignore_case(true);
                }
                oc = oc.arg(arg);
            }
            if let Some(b) = &op.body {
                oc = oc.arg(Arg::new("body").long("body").required(b.required).help("JSON string, @file.json, or - for stdin"));
            }
            if op.params.iter().any(|p| p.name == "pageNumber") {
                oc = oc.arg(Arg::new("all-pages").long("all-pages").action(ArgAction::SetTrue).help("follow pageNumber until the last page"));
            }
            sub = sub.subcommand(oc);
        }
        c = c.subcommand(sub);
    }
    c
}

fn read_body(arg: &str) -> Value {
    let text = match arg {
        "-" => {
            let mut s = String::new();
            io::stdin().read_to_string(&mut s).unwrap_or_else(|e| die(e));
            s
        }
        a if a.starts_with('@') => fs::read_to_string(&a[1..]).unwrap_or_else(|e| die(format!("{}: {e}", &a[1..]))),
        a => a.to_string(),
    };
    serde_json::from_str(&text).unwrap_or_else(|e| die(format!("--body is not valid JSON: {e}")))
}

/// Escape control characters (except newline/tab) so text from the API or the docs repo
/// can't drive the terminal with escape sequences. JSON output is already escaped by serde.
fn safe(s: &str) -> String {
    s.chars().map(|c| if c.is_control() && c != '\n' && c != '\t' { c.escape_default().to_string() } else { c.to_string() }).collect()
}

fn print_json(v: &Value, compact: bool) {
    match v {
        Value::String(s) => println!("{}", safe(s)),
        v if compact => println!("{v}"),
        v => println!("{}", serde_json::to_string_pretty(v).unwrap()),
    }
}

struct Flags<'a> {
    profile: Option<&'a str>,
    env: Option<&'a str>,
    compact: bool,
    dry_run: bool,
    yes: bool,
}

fn run_op(op: &Op, m: &ArgMatches, f: &Flags) {
    let vals: Vec<(&Param, String)> = op.params.iter().filter_map(|p| m.get_one::<String>(&p.name).map(|v| (p, v.clone()))).collect();
    let by = |loc: &str| -> Vec<(String, String)> { vals.iter().filter(|(p, _)| p.loc == loc).map(|(p, v)| (p.name.clone(), v.clone())).collect() };
    let (path, headers, query) = (build_path(&op.path, &by("path")), by("header"), by("query"));
    let body = m.try_get_one::<String>("body").ok().flatten().map(|b| read_body(b));
    let all_pages = m.try_get_one::<bool>("all-pages").ok().flatten() == Some(&true);

    if f.dry_run {
        // No credentials needed: show where the request would go, with auth redacted.
        let (who, env_name) = match env_creds(f.env) {
            Some((n, e, ..)) => (n, e),
            None => pick(f.profile, f.env).map(|(n, p)| (n, p.env)).unwrap_or_else(|| ("(not logged in)".into(), f.env.unwrap_or("play").into())),
        };
        let mut h: IndexMap<String, String> = IndexMap::from([("Authorization".to_string(), "Bearer <redacted>".to_string())]);
        h.extend(headers);
        let base = base_url(&env_name);
        return print_json(&json!({
            "dry_run": true, "profile": who, "env": env_name, "live": is_live(&base), "method": op.method,
            "needs_yes": is_live(&base) && op.method != "GET",
            "url": build_url(&base, &path, &query), "headers": h, "body": body, "all_pages": all_pages,
        }), f.compact);
    }

    let (name, env_name, cid, secret) = resolve(f.profile, f.env);
    // ponytail: every non-GET counts as a write, including read-like POSTs (booking costs, gift card balance)
    let base = base_url(&env_name);
    if is_live(&base) && op.method != "GET" && !f.yes {
        die(format!("refusing {} {} on live venue '{name}' ({base}) without --yes (this can change real data; try --dry-run first)", op.method, op.path));
    }
    let client = Client::new(&env_name, &cid, &secret);
    let fetch = |page: Option<u32>| {
        let mut q = query.clone();
        if let Some(p) = page {
            q.retain(|(k, _)| k != "pageNumber");
            q.push(("pageNumber".into(), p.to_string()));
        }
        client.call(&op.method, &path, &q, &headers, body.as_ref())
    };
    let (status, data) = if all_pages {
        let size = vals.iter().find(|(p, _)| p.name == "pageSize").and_then(|(_, v)| v.parse().ok()).unwrap_or(100);
        paginate(|p| fetch(Some(p)), size)
    } else {
        fetch(None)
    };
    print_json(&data, f.compact);
    if status >= 400 {
        eprintln!("roller: HTTP {status}");
        std::process::exit(if status >= 500 { 5 } else { 4 });
    }
}

fn find_op<'a>(spec: &'a Spec, m: &ArgMatches) -> (String, String, &'a Op) {
    let api = m.get_one::<String>("api").unwrap().clone();
    let id = m.get_one::<String>("operation").unwrap().clone();
    let op = spec.ops.get(&api).and_then(|g| g.get(&id)).unwrap_or_else(|| die(format!("no operation '{api} {id}' - see `roller ops`")));
    (api, id, op)
}

fn docs(spec: &Spec, topic: Option<&String>, as_json: bool) {
    let Some(topic) = topic else {
        if as_json {
            return print_json(&serde_json::to_value(&spec.guides).unwrap(), false);
        }
        let mut section = "";
        for g in &spec.guides {
            if g.section != section {
                section = &g.section;
                println!("\n{section}");
            }
            println!("  {:42} {}", g.slug, g.title);
        }
        return println!("\nroller docs TOPIC prints a guide (fetched live from ROLLER's documentation repo).");
    };
    let g = spec.guides.iter().find(|g| g.slug == *topic).unwrap_or_else(|| die(format!("no guide '{topic}' - run `roller docs` for the list")));
    let text = sync::fetch_text(&format!("{}{}", sync::RAW, g.path));
    // drop Stoplight front matter
    let text = text.strip_prefix("---").and_then(|t| t.split_once("\n---").map(|(_, rest)| rest)).unwrap_or(&text);
    println!("{}", safe(text.trim()));
}

fn main() {
    if env::args().len() == 1 {
        print!("{GUIDE}{NOTICE}");
        let cfg = load_config();
        match cfg.current.as_ref().and_then(|n| cfg.profiles.get(n).map(|p| (n, p))) {
            Some((n, p)) => println!("\nCurrent venue: {} ({n}, {})", p.venue, p.env),
            None => println!("\nNot logged in yet - start with `roller login`."),
        }
        return;
    }
    let spec = load_spec();
    let m = cli(&spec).get_matches();
    let f = Flags {
        profile: m.get_one::<String>("profile").map(String::as_str),
        env: m.get_one::<String>("env").map(String::as_str),
        compact: m.get_flag("compact"),
        dry_run: m.get_flag("dry-run"),
        yes: m.get_flag("yes"),
    };
    match m.subcommand().unwrap() {
        ("sync", s) => sync::sync(s.get_one::<String>("out").map(PathBuf::from).unwrap_or_else(|| roller_dir().join("ops.json"))),
        ("login", s) => login(f.env.unwrap_or("play"), f.profile, s.get_one::<String>("client-id").map(String::as_str), s.get_flag("secret-stdin")),
        ("profiles", s) => {
            let cfg = load_config();
            if s.get_flag("json") {
                let list: Vec<Value> = cfg.profiles.iter().map(|(n, p)| json!({"name": n, "env": p.env, "venue": p.venue, "venue_id": p.venue_id, "current": Some(n) == cfg.current.as_ref()})).collect();
                return print_json(&json!(list), f.compact);
            }
            for (n, p) in &cfg.profiles {
                println!("{} {n:35} {:5} {} ({})", if Some(n) == cfg.current.as_ref() { "*" } else { " " }, p.env, p.venue, p.venue_id);
            }
        }
        (cmd @ ("use" | "logout"), s) => {
            let mut cfg = load_config();
            let name = s.get_one::<String>("name").cloned().or(cfg.current.clone()).unwrap_or_else(|| die("no current profile"));
            let Some(p) = cfg.profiles.get(&name).cloned() else { die(format!("no profile '{name}' - see `roller profiles`")) };
            if cmd == "use" {
                cfg.current = Some(name.clone());
            } else {
                cfg.profiles.shift_remove(&name);
                forget_secret(&p.secret);
                let _ = fs::remove_file(token_cache(&p.env, &p.client_id));
                if cfg.current.as_ref() == Some(&name) {
                    cfg.current = None;
                }
            }
            save_config(&cfg);
            println!("{cmd}: {name}");
        }
        ("ops", s) => {
            let filter = s.get_one::<String>("filter").map(|f| f.to_lowercase()).unwrap_or_default();
            let rows: Vec<(&String, &String, &Op)> = spec.ops.iter().flat_map(|(api, g)| g.iter().map(move |(id, op)| (api, id, op)))
                .filter(|(api, id, op)| format!("{api} {id} {} {} {}", op.method, op.path, op.summary).to_lowercase().contains(&filter))
                .collect();
            if s.get_flag("json") {
                let list: Vec<Value> = rows.iter().map(|(api, id, op)| json!({"api": api, "operation": id, "method": op.method, "path": op.path, "summary": op.summary, "usage": usage(api, id, op)})).collect();
                return print_json(&json!(list), f.compact);
            }
            for (api, id, op) in rows {
                println!("{api:6} {id:40} {:6} {}", op.method, op.path);
            }
        }
        ("describe", s) => {
            let (api, id, op) = find_op(&spec, s);
            print_json(&describe(&api, &id, op), f.compact);
        }
        ("example", s) => {
            let (api, id, op) = find_op(&spec, s);
            let body = op.body.as_ref().unwrap_or_else(|| die(format!("{api} {id} takes no request body")));
            print_json(&example(&body.schema), f.compact);
        }
        ("docs", s) => docs(&spec, s.get_one::<String>("topic"), s.get_flag("json")),
        ("whoami", _) => {
            let (name, env_name, cid, secret) = resolve(f.profile, f.env);
            let client = Client::new(&env_name, &cid, &secret);
            let (status, venue) = client.call("GET", "/venues/me", &[], &[], None);
            if status != 200 {
                die(format!("/venues/me failed ({status}): {}", show(&venue)));
            }
            println!("profile {name} | {env_name} {} | {} ({})", client.base, venue["name"].as_str().unwrap_or("?"), venue["id"]);
        }
        (api, s) => {
            let (id, om) = s.subcommand().unwrap();
            run_op(&spec.ops[api][id], om, &f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    type Calls = Rc<RefCell<Vec<(String, Vec<(String, String)>)>>>;

    fn setup() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            env::set_var("ROLLER_HOME", env::temp_dir().join(format!("roller-test-{}", std::process::id())));
            for k in ["ROLLER_CLIENT_ID", "ROLLER_CLIENT_SECRET", "ROLLER_PROFILE", "ROLLER_BASE_URL"] {
                env::remove_var(k);
            }
        });
    }

    fn scripted(script: Vec<Resp>) -> (Transport, Calls) {
        let calls: Calls = Rc::default();
        let (log, script) = (calls.clone(), RefCell::new(script.into_iter()));
        let send: Transport = Box::new(move |_, url, h, _| {
            log.borrow_mut().push((url.to_string(), h.to_vec()));
            Ok(script.borrow_mut().next().expect("unexpected extra request"))
        });
        (send, calls)
    }

    #[test]
    fn paths_and_pages() {
        let vals = [("uniqueId".to_string(), "a b/c".to_string()), ("id".to_string(), "7".to_string())];
        assert_eq!(build_path("/bookings/{uniqueId}/payments/links/{id}", &vals), "/bookings/a%20b%2Fc/payments/links/7");
        let pages = [json!([1, 2]), json!([3, 4]), json!([5])];
        assert_eq!(paginate(|p| (200, pages[p as usize - 1].clone()), 2), (200, json!([1, 2, 3, 4, 5])));
        assert_eq!(paginate(|_| (400, json!({"error": "bad"})), 2), (400, json!({"error": "bad"})));
        assert_eq!(slug("Funland Demo 1"), "funland-demo-1");
        // live guard follows the host, not the profile's env name
        assert!(is_live("https://api.roller.app") && is_live("https://api.custom-domain.example") && !is_live("https://api.play.roller.app"));
        assert_eq!(safe("a\u{1b}[31mb\nc\td"), "a\\u{1b}[31mb\nc\td");
        assert_eq!(sync::join("reference/rest-api.yaml", "../models/Guest.yaml"), "models/Guest.yaml");
        assert_eq!(sync::join("models/Guest.yaml", "./CustomerFlag.yaml"), "models/CustomerFlag.yaml");
    }

    #[test]
    fn examples_follow_schema() {
        let s = json!({"type": "object", "properties": {
            "id": {"type": "integer"}, "when": {"type": "string", "format": "date"},
            "kind": {"type": "string", "enum": ["a", "b"]}, "tags": {"type": "array", "items": {"type": "string", "example": "vip"}},
            "note": {"type": ["string", "null"]}}});
        assert_eq!(example(&s), json!({"id": 0, "when": "2026-01-01", "kind": "a", "tags": ["vip"], "note": "string"}));
    }

    /// The embedded table is 1:1 with ROLLER's specs and every operation builds a valid command.
    #[test]
    fn embedded_table_covers_every_operation() {
        let spec: Spec = serde_json::from_str(EMBEDDED_OPS).unwrap();
        assert_eq!((spec.ops["rest"].len(), spec.ops["report"].len()), (63, 25));
        for (api, group) in &spec.ops {
            for (id, op) in group {
                if let Some(b) = &op.body {
                    assert!(b.schema.is_object(), "{api} {id} body has no schema");
                    assert!(!example(&b.schema).is_null(), "{api} {id} example is empty");
                }
                assert!(!op.responses.is_empty(), "{api} {id} has no responses");
            }
        }
        assert!(spec.guides.iter().any(|g| g.slug == "authentication"));
        cli(&spec).debug_assert();
    }

    #[test]
    fn token_cached_refreshed_once_on_401_and_429_retried() {
        setup();
        let (send, calls) = scripted(vec![
            (200, json!({"access_token": "t1", "expires_in": 86400}), None),
            (401, Value::Null, None),
            (200, json!({"access_token": "t2", "expires_in": 86400}), None),
            (429, Value::Null, Some("0".into())),
            (200, json!({"ok": true}), None),
        ]);
        let c = Client::with("play", "retry-id", "secret", send);
        assert_eq!(c.call("GET", "/venues/me", &[], &[], None), (200, json!({"ok": true})));
        let calls = calls.borrow();
        let auth: Vec<_> = calls.iter().filter(|(u, _)| !u.ends_with("/token")).map(|(_, h)| h[0].1.clone()).collect();
        assert_eq!(auth, ["Bearer t1", "Bearer t2", "Bearer t2"]);
        assert_eq!(calls.iter().filter(|(u, _)| u.ends_with("/token")).count(), 2);
        let (none, _) = scripted(vec![]);
        assert_eq!(Client::with("play", "retry-id", "secret", none).token(false), "t2"); // from cache
        let on_disk = fs::read_to_string(token_cache("play", "retry-id")).unwrap();
        assert_eq!(on_disk.contains("\"t2\""), !cfg!(windows), "token must be DPAPI-sealed on Windows");
    }

    #[test]
    fn login_saves_protected_profile() {
        setup();
        let (send, _) = scripted(vec![
            (200, json!({"access_token": "t9", "expires_in": 86400}), None),
            (200, json!({"id": 42, "name": "Funland Downtown"}), None),
        ]);
        save_login(&Client::with("play", "cid42", "venue-secret", send), "play", None);
        assert_eq!(load_config().current.as_deref(), Some("funland-downtown-play"));
        assert_eq!(resolve(None, None), ("funland-downtown-play".into(), "play".into(), "cid42".into(), "venue-secret".into()));
        let on_disk = fs::read_to_string(roller_dir().join("config.json")).unwrap();
        assert_eq!(on_disk.contains("venue-secret"), !(cfg!(windows) || cfg!(target_os = "macos")));

        // Re-login under the same profile with new credentials removes the old client's cached token.
        assert!(token_cache("play", "cid42").exists());
        let (send, _) = scripted(vec![
            (200, json!({"access_token": "t10", "expires_in": 86400}), None),
            (200, json!({"id": 42, "name": "Funland Downtown"}), None),
        ]);
        save_login(&Client::with("play", "cid43", "rotated-secret", send), "play", None);
        assert!(!token_cache("play", "cid42").exists() && token_cache("play", "cid43").exists());
    }
}
