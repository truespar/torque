//! The realtime write path against the server binary, as docs/realtime-write-path.md reports it
//! (ignored: `cargo test --release --test realtime_load_test -- --ignored --nocapture
//! --test-threads 1`):
//! - acknowledged writes survive a kill: writers upsert and delete over HTTP, the server is killed
//!   hard while writes are in flight (TerminateProcess, SIGKILL), started again on the same data
//!   directory, and every acknowledged write checked (`TORQUE_LOAD_ROUNDS` rounds, default 3);
//! - write latency of one writer, 16 and 64 at once (group commit), and search latency while 4
//!   writers write;
//! - a load over HTTP (`TORQUE_LOAD_DOCS` documents of `TORQUE_LOAD_WORDS` words, default 150,000 of
//!   30, in imports of 5,000), 5 rounds of 500 updates each followed by a flush and its save, the
//!   data directory's size, a restart.
//!
//! The data directory is made in `TORQUE_LOAD_DIR` (default: the temporary directory): its drive's
//! sync decides every acknowledged write's latency.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const KEY: &str = "realtime-load-test-key";

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// A data directory for the server, on the drive `TORQUE_LOAD_DIR` names.
fn work_dir() -> tempfile::TempDir {
    let dir = match std::env::var_os("TORQUE_LOAD_DIR") {
        Some(base) => tempfile::tempdir_in(base).unwrap(),
        None => tempfile::tempdir().unwrap(),
    };
    println!("data directory: {}", dir.path().display());
    dir
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The server's HTTP API.
#[derive(Clone)]
struct Api {
    base: String,
    http: reqwest::Client,
}

/// The server binary on a data directory.
struct Server {
    child: Child,
    api: Api,
}

impl Server {
    async fn start(dir: &Path) -> Server {
        let (api, ingest) = (free_port(), free_port());
        let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("server.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_torque-server"))
            .args(["--api-key", KEY, "--api-addr", &format!("127.0.0.1:{api}"), "--ingest-addr", &format!("127.0.0.1:{ingest}")])
            .arg("--data-dir")
            .arg(dir.join("data"))
            .args(["--log-level", "warn"])
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let server = Server { child, api: Api { base: format!("http://127.0.0.1:{api}"), http: reqwest::Client::new() } };
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(60) {
            if let Ok(r) = server.api.http.get(server.api.url("/health")).send().await {
                if r.status().is_success() {
                    return server;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the server did not answer within 60 s (see {})", dir.join("server.log").display());
    }

    /// Killed hard: no shutdown.
    fn kill(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

impl Api {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<(u16, String), reqwest::Error> {
        let mut req = self.http.request(method, self.url(path)).header("X-TYPESENSE-API-KEY", KEY);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await?;
        let status = r.status().as_u16();
        Ok((status, r.text().await?))
    }

    async fn create(&self, name: &str) {
        let schema = json!({"name": name, "index_mode": "realtime", "fields": [{"name": "title", "type": "string"}, {"name": "n", "type": "int32"}]});
        let (s, b) = self.send(reqwest::Method::POST, "/collections", Some(schema)).await.unwrap();
        assert!((200..300).contains(&s), "create {name}: {s} {b}");
    }

    async fn upsert(&self, collection: &str, doc: Value) -> Result<u16, reqwest::Error> {
        Ok(self.send(reqwest::Method::POST, &format!("/collections/{collection}/documents?action=upsert"), Some(doc)).await?.0)
    }
}

fn quantile(xs: &mut [Duration], q: f64) -> f64 {
    xs.sort_unstable();
    xs[((xs.len() as f64 * q) as usize).min(xs.len() - 1)].as_secs_f64() * 1000.0
}

fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| if e.path().is_dir() { dir_bytes(&e.path()) } else { e.metadata().map_or(0, |m| m.len()) })
        .sum()
}

/// What a writer acknowledged, and the write under way when the server went (done or not).
#[derive(Default)]
struct Acked {
    upserted: Vec<String>,
    deleted: Vec<String>,
    in_flight: Option<String>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn acknowledged_writes_survive_a_kill() {
    let dir = work_dir();
    let mut server = Server::start(dir.path()).await;
    server.api.create("crash").await;
    let (mut present, mut absent) = (HashSet::<String>::new(), HashSet::<String>::new());
    println!("| round | upserts acknowledged | deletes acknowledged | restart | expected present | expected gone | missing | back |");
    println!("|---|---|---|---|---|---|---|---|");
    for round in 0..env("TORQUE_LOAD_ROUNDS", 3) {
        let api = server.api.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let writers: Vec<_> = (0..12)
            .map(|t| {
                let (s, stop) = (api.clone(), Arc::clone(&stop));
                let acked = Arc::new(Mutex::new(Acked::default()));
                let mine = Arc::clone(&acked);
                let task = tokio::spawn(async move {
                    let mut i = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        let victim = {
                            let a = mine.lock().unwrap();
                            (i % 7 == 6).then(|| a.upserted.get(a.upserted.len() / 2).cloned()).flatten().filter(|v| !a.deleted.contains(v))
                        };
                        match victim {
                            Some(v) => {
                                mine.lock().unwrap().in_flight = Some(v.clone());
                                let Ok((status, _)) = s.send(reqwest::Method::DELETE, &format!("/collections/crash/documents/{v}"), None).await else { break };
                                let mut a = mine.lock().unwrap();
                                a.in_flight = None;
                                if (200..300).contains(&status) {
                                    a.deleted.push(v);
                                }
                            }
                            None => {
                                let id = format!("r{round}-t{t}-{i}");
                                mine.lock().unwrap().in_flight = Some(id.clone());
                                let Ok(status) = s.upsert("crash", json!({"id": id, "title": format!("writer {t}"), "n": i})).await else { break };
                                let mut a = mine.lock().unwrap();
                                a.in_flight = None;
                                if (200..300).contains(&status) {
                                    a.upserted.push(id);
                                }
                            }
                        }
                        i += 1;
                    }
                });
                (task, acked)
            })
            .collect();
        tokio::time::sleep(Duration::from_secs(4 + round)).await;
        // writes in flight
        server.kill();
        stop.store(true, Ordering::Relaxed);
        let (mut ups, mut dels, mut undetermined) = (0, 0, HashSet::new());
        for (task, acked) in writers {
            task.await.unwrap();
            let a = acked.lock().unwrap();
            ups += a.upserted.len();
            dels += a.deleted.len();
            present.extend(a.upserted.iter().cloned());
            for d in &a.deleted {
                present.remove(d);
                absent.insert(d.clone());
            }
            undetermined.extend(a.in_flight.clone());
        }
        for u in &undetermined {
            present.remove(u);
            absent.remove(u);
        }
        let t = Instant::now();
        server = Server::start(dir.path()).await;
        let restart = t.elapsed();
        let (mut missing, mut back) = (0, 0);
        for id in &present {
            let (s, _) = server.api.send(reqwest::Method::GET, &format!("/collections/crash/documents/{id}"), None).await.unwrap();
            missing += usize::from(s != 200);
        }
        for id in &absent {
            let (s, _) = server.api.send(reqwest::Method::GET, &format!("/collections/crash/documents/{id}"), None).await.unwrap();
            back += usize::from(s == 200);
        }
        println!("| {} | {ups} | {dels} | {:.2} s | {} | {} | {missing} | {back} |", round + 1, restart.as_secs_f64(), present.len(), absent.len());
        assert_eq!((missing, back), (0, 0), "acknowledged writes lost or deletes undone");
    }
    server.kill();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn write_and_search_latency() {
    let dir = work_dir();
    let mut server = Server::start(dir.path()).await;
    let api = server.api.clone();
    api.create("lat").await;
    async fn timed(s: &Api, id: String) -> Duration {
        let t = Instant::now();
        let status = s.upsert("lat", json!({"id": id, "title": "x", "n": 1})).await.unwrap();
        assert!((200..300).contains(&status), "{status}");
        t.elapsed()
    }
    for i in 0..50 {
        timed(&api, format!("warm-{i}")).await;
    }
    println!("| writers | p50 | p99 | writes/s |");
    println!("|---|---|---|---|");
    let t = Instant::now();
    let mut one = Vec::new();
    for i in 0..1000 {
        one.push(timed(&api, format!("s-{i}")).await);
    }
    let rate = one.len() as f64 / t.elapsed().as_secs_f64();
    println!("| 1 | {:.2} ms | {:.2} ms | {rate:.0} |", quantile(&mut one, 0.5), quantile(&mut one, 0.99));
    for n in [16u64, 64] {
        let t = Instant::now();
        let tasks: Vec<_> = (0..n)
            .map(|w| {
                let s = api.clone();
                tokio::spawn(async move {
                    let mut mine = Vec::new();
                    for i in 0..200 {
                        mine.push(timed(&s, format!("c{n}-{w}-{i}")).await);
                    }
                    mine
                })
            })
            .collect();
        let mut all = Vec::new();
        for task in tasks {
            all.extend(task.await.unwrap());
        }
        let rate = all.len() as f64 / t.elapsed().as_secs_f64();
        println!("| {n} | {:.2} ms | {:.2} ms | {rate:.0} |", quantile(&mut all, 0.5), quantile(&mut all, 0.99));
    }
    // searches while 4 writers write
    api.create("mix").await;
    let stop = Arc::new(AtomicBool::new(false));
    let writers: Vec<_> = (0..4)
        .map(|w| {
            let (s, stop) = (api.clone(), Arc::clone(&stop));
            tokio::spawn(async move {
                let mut i = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    s.upsert("mix", json!({"id": format!("m-{w}-{i}"), "title": format!("word{} writer{w}", i % 50), "n": i})).await.unwrap();
                    i += 1;
                }
                i
            })
        })
        .collect();
    let mut searches = Vec::new();
    let end = Instant::now() + Duration::from_secs(8);
    while Instant::now() < end {
        let t = Instant::now();
        let (s, b) = api.send(reqwest::Method::GET, "/collections/mix/documents/search?q=word7&query_by=title&per_page=10", None).await.unwrap();
        assert_eq!(s, 200, "{b}");
        searches.push(t.elapsed());
    }
    stop.store(true, Ordering::Relaxed);
    let mut written = 0;
    for w in writers {
        written += w.await.unwrap();
    }
    println!();
    println!("search while 4 writers write: {} searches during {written} writes, p50 {:.2} ms, p99 {:.2} ms", searches.len(), quantile(&mut searches, 0.5), quantile(&mut searches, 0.99));
    server.kill();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn load_flush_and_restart() {
    let dir = work_dir();
    let mut server = Server::start(dir.path()).await;
    server.api.create("big").await;
    let (n, words) = (env("TORQUE_LOAD_DOCS", 150_000), env("TORQUE_LOAD_WORDS", 30));
    let mut rng = 7u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let doc = |next: &mut dyn FnMut() -> u64, i: u64, v: u64| {
        let text: Vec<String> = (0..words).map(|_| format!("w{}", next() % 5000)).collect();
        json!({"id": format!("d{i}"), "title": format!("item {i} {}", text.join(" ")), "n": i + v})
    };
    let t = Instant::now();
    let mut refused = 0;
    for start in (0..n).step_by(5000) {
        let body = (start..n.min(start + 5000)).map(|i| doc(&mut next, i, 0).to_string()).collect::<Vec<_>>().join("\n");
        loop {
            let r = server
                .api
                .http
                .post(server.api.url("/collections/big/documents/import?action=upsert"))
                .header("X-TYPESENSE-API-KEY", KEY)
                .body(body.clone())
                .send()
                .await
                .unwrap();
            let status = r.status().as_u16();
            let out = r.text().await.unwrap();
            if status == 429 {
                refused += 1;
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
            assert!(status == 200 && !out.contains("\"success\":false"), "{status} {}", &out[..out.len().min(300)]);
            break;
        }
    }
    let load = t.elapsed().as_secs_f64();
    server.api.send(reqwest::Method::POST, "/collections/big/documents/compact", None).await.unwrap();
    println!("load: {n} documents of {words} words in {load:.1} s ({:.0}/s), imports refused while flushing: {refused}", n as f64 / load);
    let mut flushes = Vec::new();
    for _ in 0..5 {
        for _ in 0..500 {
            let i = next() % n;
            let s = server.api.upsert("big", doc(&mut next, i, 1)).await.unwrap();
            assert!((200..300).contains(&s), "{s}");
        }
        let t = Instant::now();
        let (s, b) = server.api.send(reqwest::Method::POST, "/collections/big/documents/compact", None).await.unwrap();
        assert_eq!(s, 200, "{b}");
        flushes.push(t.elapsed());
    }
    println!("flush and save after 500 updates: median {:.0} ms, min {:.0} ms, max {:.0} ms", quantile(&mut flushes, 0.5), quantile(&mut flushes, 0.0), quantile(&mut flushes, 1.0));
    let (_, info) = server.api.send(reqwest::Method::GET, "/collections/big", None).await.unwrap();
    let info: Value = serde_json::from_str(&info).unwrap();
    println!("documents {}, realtime {}, data directory {:.0} MB", info["num_documents"], info["realtime"], dir_bytes(&dir.path().join("data")) as f64 / (1 << 20) as f64);
    server.kill();
    let t = Instant::now();
    server = Server::start(dir.path()).await;
    let restart = t.elapsed();
    let (_, info) = server.api.send(reqwest::Method::GET, "/collections/big", None).await.unwrap();
    let info: Value = serde_json::from_str(&info).unwrap();
    println!("restart: answering in {:.2} s, documents {}", restart.as_secs_f64(), info["num_documents"]);
    assert_eq!(info["num_documents"].as_u64(), Some(n));
    server.kill();
}
