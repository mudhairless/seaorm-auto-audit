//! The soak writer: seeded randomized operations against random
//! api replicas, logging every operation as one JSONL `OpEntry`.
//!
//! One writer owns one actor identity for the whole run. Values are
//! unique per operation (titles carry the writer and the sequence
//! number) and every write plants fresh sentinel secrets in the
//! redacted and excluded columns, so the verifier can reconcile the
//! audit trail against this log exactly.
//!
//! Configuration is all environment: WRITER_NAME (writer-0, ...),
//! SEED, OPS, API_URLS (comma-separated), LOG_DIR, HOT_COUNT.
//! writer-0 also seeds the shared hot rows the other writers
//! contend over and publishes their ids as hot-rows.json in the
//! log directory; the other writers wait for that file.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::time::Duration;

use rand::rngs::StdRng;
use rand::seq::IndexedRandom;
use rand::{Rng, SeedableRng};
use reqwest::Client;
use serde_json::{json, Value as JsonValue};
use uuid::Uuid;

use seaorm_auto_audit_soak::OpEntry;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn env_num<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// A writer's actor: writer-N maps to the uuid
/// 00000000-0000-4000-8000-(N+1), so a trail row names its writer
/// at a glance.
fn actor_for(name: &str) -> String {
    let digits: String = name
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let n: u64 = digits.parse().unwrap_or(0);
    format!("00000000-0000-4000-8000-{:012}", n + 1)
}

struct Writer {
    name: String,
    actor: String,
    rng: StdRng,
    client: Client,
    apis: Vec<String>,
    log: std::io::BufWriter<std::fs::File>,
    seq: u64,
    own_rows: Vec<Uuid>,
    bulk_rows: Vec<Uuid>,
    hot_rows: Vec<Uuid>,
    counts: BTreeMap<String, u64>,
}

impl Writer {
    fn api(&mut self) -> String {
        let idx = self.rng.random_range(0..self.apis.len());
        self.apis[idx].clone()
    }

    fn fresh_sentinels(&self) -> (String, String) {
        (
            format!("SENTINEL-R-{}-{}", self.name, self.seq),
            format!("SENTINEL-E-{}-{}", self.name, self.seq),
        )
    }

    fn title(&self, op: &str) -> String {
        format!("{}-{}-{}", self.name, self.seq, op)
    }

    fn entry(
        &mut self,
        op: &str,
        row_ids: Vec<Uuid>,
        list_name: Option<String>,
        outcome: &str,
        values: Option<JsonValue>,
        sentinels: Vec<String>,
    ) {
        let entry = OpEntry {
            seq: self.seq,
            writer: self.name.clone(),
            actor: self.actor.clone(),
            op: op.to_owned(),
            row_ids: row_ids.iter().map(ToString::to_string).collect(),
            list_name,
            outcome: outcome.to_owned(),
            values,
            sentinels,
        };
        let line = serde_json::to_string(&entry).expect("an OpEntry serializes");
        writeln!(self.log, "{line}").expect("write the op log");
        self.log.flush().expect("flush the op log");
        *self.counts.entry(outcome.to_owned()).or_default() += 1;
        self.seq += 1;
    }

    /// One request with the writer's actor header. Transport errors
    /// come back as a synthetic 0 status so the op is logged as an
    /// error instead of aborting the run: a soak that dies on the
    /// first hiccup proves nothing.
    async fn send(
        &self,
        api: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<JsonValue>,
    ) -> (u16, JsonValue) {
        let mut req = self
            .client
            .request(method, format!("{api}{path}"))
            .header("x-actor-id", &self.actor);
        if let Some(body) = body {
            req = req.json(&body);
        }
        match req.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let value = resp.json::<JsonValue>().await.unwrap_or(JsonValue::Null);
                (status, value)
            }
            Err(error) => (0, json!({ "error": error.to_string() })),
        }
    }

    fn note_body(&self, title: &str, secret: &str, excluded: &str) -> JsonValue {
        json!({
            "title": title,
            "body": format!("body of {title}"),
            "secret_token": secret,
            "internal_note": excluded,
        })
    }

    async fn op_create(&mut self) {
        let api = self.api();
        let (secret, excluded) = self.fresh_sentinels();
        let title = self.title("create");
        let list = format!("own-{}", self.name);
        let mut body = self.note_body(&title, &secret, &excluded);
        body["list_name"] = json!(list);
        let (status, resp) = self
            .send(&api, reqwest::Method::POST, "/notes", Some(body))
            .await;
        let values = json!({ "title": title, "body": format!("body of {title}") });
        if status == 201 {
            let id = resp["id"].as_str().and_then(|s| s.parse().ok());
            if let Some(id) = id {
                self.own_rows.push(id);
                self.entry(
                    "create",
                    vec![id],
                    Some(list),
                    "committed",
                    Some(values),
                    vec![secret, excluded],
                );
                return;
            }
        }
        self.entry(
            "create",
            vec![],
            Some(list),
            "error",
            Some(json!({ "http_status": status })),
            vec![secret, excluded],
        );
    }

    async fn op_update(&mut self, hot: bool) {
        let pool = if hot { &self.hot_rows } else { &self.own_rows };
        let Some(&id) = pool.choose(&mut self.rng) else {
            return self.op_create().await;
        };
        let api = self.api();
        let (secret, excluded) = self.fresh_sentinels();
        let title = self.title("update");
        let body = self.note_body(&title, &secret, &excluded);
        let (status, _) = self
            .send(
                &api,
                reqwest::Method::PUT,
                &format!("/notes/{id}"),
                Some(body),
            )
            .await;
        let values = json!({ "title": title, "body": format!("body of {title}") });
        let outcome = match status {
            200 => "committed",
            404 => "missing",
            _ => "error",
        };
        self.entry(
            "update",
            vec![id],
            None,
            outcome,
            Some(values),
            vec![secret, excluded],
        );
    }

    async fn op_delete(&mut self) {
        let Some(&id) = self.own_rows.choose(&mut self.rng) else {
            return self.op_create().await;
        };
        let api = self.api();
        let (status, _) = self
            .send(&api, reqwest::Method::DELETE, &format!("/notes/{id}"), None)
            .await;
        match status {
            200 => {
                self.own_rows.retain(|row| *row != id);
                self.entry("delete", vec![id], None, "committed", None, vec![]);
            }
            404 => {
                self.own_rows.retain(|row| *row != id);
                self.entry("delete", vec![id], None, "missing", None, vec![]);
            }
            _ => self.entry(
                "delete",
                vec![id],
                None,
                "error",
                Some(json!({ "http_status": status })),
                vec![],
            ),
        }
    }

    async fn op_read(&mut self) {
        let mut pool = self.own_rows.clone();
        pool.extend(self.hot_rows.iter().copied());
        pool.extend(self.bulk_rows.iter().copied());
        let Some(&id) = pool.choose(&mut self.rng) else {
            return self.op_create().await;
        };
        let api = self.api();
        let (status, _) = self
            .send(&api, reqwest::Method::GET, &format!("/notes/{id}"), None)
            .await;
        let outcome = match status {
            200 => "committed",
            404 => "missing",
            _ => "error",
        };
        self.entry("read", vec![id], None, outcome, None, vec![]);
    }

    async fn op_bulk_create(&mut self) {
        let api = self.api();
        let list = format!("bulk-{}", self.name);
        let n = self.rng.random_range(2..=5usize);
        let mut items = Vec::new();
        let mut sentinels = Vec::new();
        for i in 0..n {
            let (secret, excluded) = self.fresh_sentinels();
            sentinels.push(secret.clone());
            sentinels.push(excluded.clone());
            let title = format!("{}-bulk-{i}", self.title("create"));
            items.push(self.note_body(&title, &secret, &excluded));
        }
        let body = json!({ "list_name": list, "items": items });
        let (status, resp) = self
            .send(&api, reqwest::Method::POST, "/notes/bulk", Some(body))
            .await;
        if status == 201 {
            let ids: Vec<Uuid> = resp["ids"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().and_then(|s| s.parse().ok()))
                        .collect()
                })
                .unwrap_or_default();
            self.bulk_rows.extend(ids.iter().copied());
            self.entry("bulk_create", ids, Some(list), "committed", None, sentinels);
        } else {
            self.entry(
                "bulk_create",
                vec![],
                Some(list),
                "error",
                Some(json!({ "http_status": status })),
                sentinels,
            );
        }
    }

    async fn op_bulk_update(&mut self) {
        let api = self.api();
        let list = format!("bulk-{}", self.name);
        let marker = format!("bulk-body-{}-{}", self.name, self.seq);
        let body = json!({ "list_name": list, "body": marker });
        let (status, resp) = self
            .send(
                &api,
                reqwest::Method::POST,
                "/notes/bulk-update",
                Some(body),
            )
            .await;
        if status == 200 {
            let reported = resp["updated"].as_u64().unwrap_or(0);
            self.entry(
                "bulk_update",
                self.bulk_rows.clone(),
                Some(list),
                "committed",
                Some(json!({ "body": marker, "reported": reported })),
                vec![],
            );
        } else {
            self.entry(
                "bulk_update",
                self.bulk_rows.clone(),
                Some(list),
                "error",
                Some(json!({ "http_status": status })),
                vec![],
            );
        }
    }

    async fn op_bulk_delete(&mut self) {
        let api = self.api();
        let list = format!("bulk-{}", self.name);
        let snapshot = self.bulk_rows.clone();
        let body = json!({ "list_name": list });
        let (status, resp) = self
            .send(
                &api,
                reqwest::Method::POST,
                "/notes/bulk-delete",
                Some(body),
            )
            .await;
        if status == 200 {
            let reported = resp["deleted"].as_u64().unwrap_or(0);
            self.bulk_rows.clear();
            self.entry(
                "bulk_delete",
                snapshot,
                Some(list),
                "committed",
                Some(json!({ "reported": reported })),
                vec![],
            );
        } else {
            self.entry(
                "bulk_delete",
                snapshot,
                Some(list),
                "error",
                Some(json!({ "http_status": status })),
                vec![],
            );
        }
    }

    async fn op_transact_fail(&mut self) {
        let mut pool = self.hot_rows.clone();
        pool.extend(self.own_rows.iter().copied());
        let Some(&id) = pool.choose(&mut self.rng) else {
            return self.op_create().await;
        };
        let api = self.api();
        let marker = self.title("rolled-back");
        let body = json!({ "title": marker });
        let (status, _) = self
            .send(
                &api,
                reqwest::Method::POST,
                &format!("/notes/{id}/transact-fail"),
                Some(body),
            )
            .await;
        let outcome = match status {
            500 => "rolled_back",
            404 => "missing",
            _ => "error",
        };
        self.entry(
            "transact_fail",
            vec![id],
            None,
            outcome,
            Some(json!({ "title": marker })),
            vec![],
        );
    }

    async fn op_import(&mut self) {
        let api = self.api();
        let list = format!("import-{}", self.name);
        let n = self.rng.random_range(1..=3usize);
        let mut items = Vec::new();
        let mut sentinels = Vec::new();
        for i in 0..n {
            let (secret, excluded) = self.fresh_sentinels();
            sentinels.push(secret.clone());
            sentinels.push(excluded.clone());
            let title = format!("{}-import-{i}", self.title("import"));
            items.push(self.note_body(&title, &secret, &excluded));
        }
        let body = json!({ "list_name": list, "items": items });
        let (status, resp) = self
            .send(&api, reqwest::Method::POST, "/import", Some(body))
            .await;
        if status == 201 {
            let ids: Vec<Uuid> = resp["ids"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().and_then(|s| s.parse().ok()))
                        .collect()
                })
                .unwrap_or_default();
            self.entry("import", ids, Some(list), "suspended", None, sentinels);
        } else {
            self.entry(
                "import",
                vec![],
                Some(list),
                "error",
                Some(json!({ "http_status": status })),
                sentinels,
            );
        }
    }

    /// Weighted pick over the op table.
    async fn step(&mut self) {
        match self.rng.random_range(0..100u32) {
            0..=23 => self.op_create().await,
            24..=41 => self.op_update(false).await,
            42..=55 => self.op_update(true).await,
            56..=62 => self.op_delete().await,
            63..=72 => self.op_read().await,
            73..=80 => self.op_bulk_create().await,
            81..=86 => self.op_bulk_update().await,
            87..=89 => self.op_bulk_delete().await,
            90..=95 => self.op_transact_fail().await,
            _ => self.op_import().await,
        }
    }
}

async fn wait_for_api(client: &Client, apis: &[String]) {
    for _ in 0..90 {
        for api in apis {
            if client
                .get(format!("{api}/healthz"))
                .send()
                .await
                .is_ok_and(|resp| resp.status().is_success())
            {
                return;
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    eprintln!("no api replica answered /healthz in 90s; starting anyway");
}

/// writer-0 seeds the shared hot rows and publishes their ids;
/// the other writers wait for the file.
async fn hot_rows(writer: &mut Writer, log_dir: &str, count: usize) -> Vec<Uuid> {
    let path = format!("{log_dir}/hot-rows.json");
    if writer.name != "writer-0" {
        for _ in 0..120 {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(ids) = serde_json::from_str::<Vec<Uuid>>(&text) {
                    return ids;
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        eprintln!("hot-rows.json never appeared; running without hot rows");
        return vec![];
    }
    let mut ids = Vec::new();
    for _ in 0..count {
        writer.op_create_hot().await;
    }
    ids.extend(writer.hot_rows.iter().copied());
    let text = serde_json::to_string(&ids).expect("ids serialize");
    std::fs::write(&path, text).expect("publish hot-rows.json");
    ids
}

impl Writer {
    /// One create into the shared "hot" list, tracked as a hot row
    /// instead of an own row.
    async fn op_create_hot(&mut self) {
        let api = self.api();
        let (secret, excluded) = self.fresh_sentinels();
        let title = self.title("hot");
        let mut body = self.note_body(&title, &secret, &excluded);
        body["list_name"] = json!("hot");
        let (status, resp) = self
            .send(&api, reqwest::Method::POST, "/notes", Some(body))
            .await;
        let values = json!({ "title": title, "body": format!("body of {title}") });
        if status == 201 {
            if let Some(id) = resp["id"].as_str().and_then(|s| s.parse().ok()) {
                self.hot_rows.push(id);
                self.entry(
                    "create",
                    vec![id],
                    Some("hot".to_owned()),
                    "committed",
                    Some(values),
                    vec![secret, excluded],
                );
                return;
            }
        }
        self.entry(
            "create",
            vec![],
            Some("hot".to_owned()),
            "error",
            Some(json!({ "http_status": status })),
            vec![secret, excluded],
        );
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let name = env_or("WRITER_NAME", "writer-0");
    let seed: u64 = env_num("SEED", 1);
    let ops: u64 = env_num("OPS", 200);
    let hot_count: usize = env_num("HOT_COUNT", 8);
    let log_dir = env_or("LOG_DIR", "logs");
    let apis: Vec<String> = env_or(
        "API_URLS",
        "http://127.0.0.1:18081,http://127.0.0.1:18082,http://127.0.0.1:18083",
    )
    .split(',')
    .map(|s| s.trim().trim_end_matches('/').to_owned())
    .collect();
    let actor = actor_for(&name);
    println!(
        "{name}: seed={seed} actor={actor} apis={} ops={ops}",
        apis.len()
    );

    std::fs::create_dir_all(&log_dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{log_dir}/{name}.jsonl"))?;
    let client = Client::builder().timeout(Duration::from_secs(10)).build()?;
    wait_for_api(&client, &apis).await;

    let mut writer = Writer {
        name,
        actor,
        rng: StdRng::seed_from_u64(seed),
        client,
        apis,
        log: std::io::BufWriter::new(file),
        seq: 0,
        own_rows: vec![],
        bulk_rows: vec![],
        hot_rows: vec![],
        counts: BTreeMap::new(),
    };
    writer.hot_rows = hot_rows(&mut writer, &log_dir, hot_count).await;

    for _ in 0..ops {
        writer.step().await;
    }
    println!("{}: done, outcomes {:?}", writer.name, writer.counts);
    Ok(())
}
