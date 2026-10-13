//! `roller sync`: rebuild the operation table from ROLLER's documentation repo
//! (github.com/rollersoftware/public-api-documentation), inlining every `$ref` so each
//! operation carries its full parameter, request-body and response schemas.

use crate::{die, Body, Guide, Op, Param, Spec};
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

pub const RAW: &str = "https://raw.githubusercontent.com/rollersoftware/public-api-documentation/master/";
const SPECS: [(&str, &str); 2] = [("rest", "reference/rest-api.yaml"), ("report", "reference/reporting-api.yaml")];
const METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

pub fn fetch_text(url: &str) -> String {
    ureq::get(url).call().unwrap_or_else(|e| die(format!("{url}: {e}"))).into_string().unwrap_or_else(|e| die(e))
}

/// Resolve a relative `$ref` file path against the file it appears in.
pub fn join(base_file: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = base_file.split('/').collect();
    parts.pop();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

struct Repo {
    files: HashMap<String, Value>,
}

impl Repo {
    fn load(&mut self, path: &str) -> Value {
        if let Some(v) = self.files.get(path) {
            return v.clone();
        }
        let text = fetch_text(&format!("{RAW}{path}"));
        let v: Value = if path.ends_with(".json") {
            serde_json::from_str(&text).unwrap_or_else(|e| die(format!("{path}: {e}")))
        } else {
            let y: serde_yaml::Value = serde_yaml::from_str(&text).unwrap_or_else(|e| die(format!("{path}: {e}")));
            serde_json::to_value(y).unwrap_or_else(|e| die(format!("{path}: {e}")))
        };
        self.files.insert(path.into(), v.clone());
        v
    }

    /// Inline every `$ref` under `node` (which lives in `file`). Cycles become {"$circular": "<ref>"}.
    fn resolve(&mut self, node: &Value, file: &str, stack: &mut Vec<String>) -> Value {
        match node {
            Value::Object(m) => {
                if let Some(r) = m.get("$ref").and_then(Value::as_str) {
                    let (target, pointer) = r.split_once('#').unwrap_or((r, ""));
                    let target = if target.is_empty() { file.to_string() } else { join(file, target) };
                    let key = format!("{target}#{pointer}");
                    if stack.contains(&key) {
                        return json!({ "$circular": key });
                    }
                    let found = self.load(&target).pointer(pointer).cloned().unwrap_or_else(|| die(format!("unresolved $ref {r} in {file}")));
                    stack.push(key);
                    let mut out = self.resolve(&found, &target, stack);
                    stack.pop();
                    // Sibling keys next to a $ref (e.g. a local description) override the target's.
                    if let Value::Object(o) = &mut out {
                        for (k, v) in m.iter().filter(|(k, _)| *k != "$ref") {
                            let v = self.resolve(v, file, stack);
                            o.insert(k.clone(), v);
                        }
                    }
                    return out;
                }
                Value::Object(m.iter().map(|(k, v)| (k.clone(), self.resolve(v, file, stack))).collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(|v| self.resolve(v, file, stack)).collect()),
            v => v.clone(),
        }
    }
}

fn first_content_schema(v: &Value) -> Value {
    v["content"].as_object().and_then(|c| c.values().next()).map(|c| c["schema"].clone()).unwrap_or(Value::Null)
}

fn guides(items: &Value, section: &str, out: &mut Vec<Guide>) {
    let mut section = section.to_string();
    for it in items.as_array().into_iter().flatten() {
        match (it["type"].as_str(), it["uri"].as_str()) {
            (Some("divider"), _) | (Some("group"), None) => section = it["title"].as_str().unwrap_or("").trim().to_string(),
            (_, Some(uri)) if uri.ends_with(".md") => out.push(Guide {
                slug: it["slug"].as_str().unwrap_or_default().into(),
                title: it["title"].as_str().unwrap_or_default().into(),
                section: section.clone(),
                path: uri.trim_start_matches('/').into(),
            }),
            _ => {}
        }
        guides(&it["items"], &section, out);
    }
}

pub fn sync(out: PathBuf) {
    let mut repo = Repo { files: HashMap::new() };
    let mut spec = Spec::default();
    for (api, file) in SPECS {
        let raw = repo.load(file);
        let doc = repo.resolve(&raw, file, &mut vec![]);
        let mut group = IndexMap::new();
        for (path, item) in doc["paths"].as_object().unwrap_or_else(|| die(format!("{file} has no paths"))) {
            let shared = item["parameters"].as_array().cloned().unwrap_or_default();
            for (method, o) in item.as_object().unwrap().iter().filter(|(m, _)| METHODS.contains(&m.as_str())) {
                let params = shared.iter().chain(o["parameters"].as_array().into_iter().flatten()).map(|p| Param {
                    name: p["name"].as_str().unwrap_or_default().into(),
                    loc: p["in"].as_str().unwrap_or_default().into(),
                    required: p["required"].as_bool().unwrap_or(false) || p["in"] == "path",
                    description: p["description"].as_str().unwrap_or("").trim().into(),
                    schema: p["schema"].clone(),
                }).collect();
                let id = o["operationId"].as_str().unwrap_or_else(|| die(format!("{method} {path} has no operationId")));
                group.insert(id.to_string(), Op {
                    method: method.to_uppercase(),
                    path: path.clone(),
                    summary: o["summary"].as_str().unwrap_or("").trim().into(),
                    description: o["description"].as_str().unwrap_or("").trim().into(),
                    body: o.get("requestBody").map(|b| Body { required: b["required"].as_bool().unwrap_or(false), schema: first_content_schema(b) }),
                    params,
                    responses: o["responses"].as_object().into_iter().flatten()
                        .map(|(code, r)| (code.clone(), json!({ "description": r["description"], "schema": first_content_schema(r) })))
                        .collect(),
                });
            }
        }
        spec.ops.insert(api.into(), group);
    }
    guides(&repo.load("toc.json")["items"], "", &mut spec.guides);
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).unwrap_or_else(|e| die(e));
    }
    fs::write(&out, serde_json::to_string_pretty(&spec).unwrap()).unwrap_or_else(|e| die(format!("{}: {e}", out.display())));
    let n: usize = spec.ops.values().map(IndexMap::len).sum();
    println!("wrote {} ({n} operations, {} guides)", out.display(), spec.guides.len());
}
