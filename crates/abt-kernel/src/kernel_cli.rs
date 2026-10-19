//! `abt kernel` (P-3.4): the kernel service and its client commands.
//!
//! ```text
//! abt kernel serve    [--socket PATH] --data SOCKET
//! abt kernel check    [--kernel SOCKET] --strategy NAME <files...>
//! abt kernel contract [--kernel SOCKET] --strategy NAME <files...>
//! abt kernel show     [--kernel SOCKET] (--run RUN | --strategy NAME <files...>) [--what program|plan R|schedule|calendars|contract]
//! abt kernel run      [--kernel SOCKET] --strategy NAME [--param NAME=VALUE]... [--start DATE] [--end DATE] [--progress N] [--checkpoint-every month|N] [--resume KEY]
//!                     [--mode replay-sim|live-sim|live-broker] [--session S] [--deployment FILE] <files...>
//! abt kernel query    [--kernel SOCKET] --run RUN --text QUERY [--json]
//! abt kernel status   [--kernel SOCKET] [--run RUN]
//! abt kernel control  [--kernel SOCKET] --run RUN halt|checkpoint|flatten|release
//! ```
//!
//! The kernel's socket defaults to the layer's (`$XDG_RUNTIME_DIR/abt/kernel.sock`
//! or the temporary directory's); `serve` reaches the data service at `--data`.

use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;

use abt_wire::generated::abt::wire::{KernelOp, KernelRequest, KernelResponse, Message, ParamValue, SourceFile};
use abt_wire::rpc::Client;
use abt_wire::transport::{default_socket, Connection};
use abt_wire::Layer;

use crate::kernel::service::KernelService;
use crate::kernel::DataClient;

struct Args {
    cmd: String,
    positional: Vec<String>,
    opts: std::collections::BTreeMap<String, String>,
    params: Vec<String>,
    json: bool,
}

fn fail(m: &str) -> ! {
    eprintln!("{}", m);
    exit(2)
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  abt kernel serve    [--socket PATH] --data SOCKET\n  abt kernel check    [--kernel SOCKET] --strategy NAME <files...>\n  abt kernel contract [--kernel SOCKET] --strategy NAME <files...>\n  abt kernel show     [--kernel SOCKET] (--run RUN | --strategy NAME <files...>) [--what program|plan R|schedule|calendars|contract]\n  abt kernel run      [--kernel SOCKET] --strategy NAME [--param NAME=VALUE]... [--start DATE] [--end DATE] [--progress N] [--checkpoint-every month|N] [--resume KEY]\n                      [--mode replay-sim|live-sim|live-broker] [--session S] [--deployment FILE] <files...>\n  abt kernel query    [--kernel SOCKET] --run RUN --text QUERY [--json]\n  abt kernel status   [--kernel SOCKET] [--run RUN]\n  abt kernel control  [--kernel SOCKET] --run RUN halt|checkpoint|flatten|release"
    );
    exit(2)
}

fn parse(args: &[String]) -> Args {
    let mut out = Args {
        cmd: args.first().cloned().unwrap_or_else(|| usage()),
        positional: Vec::new(),
        opts: Default::default(),
        params: Vec::new(),
        json: false,
    };
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        match a.strip_prefix("--") {
            Some("json") => out.json = true,
            Some(name) => {
                let Some(v) = args.get(i + 1) else { fail(&format!("--{} takes a value", name)) };
                if name == "param" {
                    out.params.push(v.clone());
                } else {
                    out.opts.insert(name.to_string(), v.clone());
                }
                i += 1;
            }
            None => out.positional.push(a.clone()),
        }
        i += 1;
    }
    out
}

/// Every `.dsl` file under the named files and directories, in path order.
fn sources(files: &[String]) -> Vec<SourceFile> {
    let mut paths = Vec::new();
    fn collect(p: &Path, out: &mut Vec<PathBuf>) {
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    collect(&e.path(), out);
                }
            }
        } else if p.extension().is_some_and(|x| x == "dsl") {
            out.push(p.to_path_buf());
        }
    }
    for f in files {
        collect(Path::new(f), &mut paths);
    }
    paths.sort();
    paths
        .into_iter()
        .map(|p| SourceFile {
            text: std::fs::read_to_string(&p).unwrap_or_else(|e| fail(&format!("{}: {}", p.display(), e))),
            path: Some(p.to_string_lossy().into_owned()),
        })
        .collect()
}

fn request(op: KernelOp) -> KernelRequest {
    KernelRequest {
        op,
        sources: None,
        strategy: None,
        point: None,
        what: None,
        run_id: None,
        text: None,
        to_seq: None,
        start: None,
        end: None,
        memory_budget: None,
        pushdown: true,
        progress_every: None,
        checkpoint_every: None,
        from_checkpoint: None,
        horizon: None,
        received_by: None,
        plan_step: None,
        embargo_mask: None,
        observe: None,
        mode: None,
        session: None,
        deployment: None,
    }
}

fn client(a: &Args) -> Client {
    let path = a.opts.get("kernel").map(PathBuf::from).unwrap_or_else(|| default_socket("kernel"));
    let conn = Connection::connect(&path).unwrap_or_else(|e| fail(&format!("--kernel {}: {}", path.display(), e)));
    Client::connect(conn, Layer::Front, "abt").unwrap_or_else(|e| fail(&format!("--kernel {}: {}", path.display(), e)))
}

fn time(s: &str) -> i64 {
    crate::kernel::time::parse_timestamp(s).unwrap_or_else(|| fail(&format!("`{}` is not a time (YYYY-MM-DD[THH:MM[:SS]])", s)))
}

/// A request's program: its strategy and the workspace's sources.
fn with_program(a: &Args, op: KernelOp) -> KernelRequest {
    let mut r = request(op);
    r.strategy = Some(a.opts.get("strategy").cloned().unwrap_or_else(|| fail("--strategy NAME names the strategy")));
    r.sources = Some(sources(&a.positional));
    r
}

/// The kernel's answer to one request (its error printed, exit 1).
fn answer(c: &Client, r: KernelRequest) -> (KernelResponse, abt_wire::rpc::Reply) {
    let mut reply = c.call(Message::KernelRequest(Box::new(r))).unwrap_or_else(|e| fail(&e.to_string()));
    match reply.message() {
        Ok(Message::KernelResponse(m)) => (*m, reply),
        Ok(_) => fail("the kernel service answered with another message"),
        Err(e) => {
            eprintln!("{}", e);
            exit(1)
        }
    }
}

/// The table an answer announced (`has_table`): `TableFollows`, then its
/// frames.
pub fn read_table(reply: &mut abt_wire::rpc::Reply) -> Vec<arrow::record_batch::RecordBatch> {
    match reply.message() {
        Ok(Message::TableFollows(_)) => reply.table().map(|(_, b)| b).unwrap_or_else(|e| fail(&e.to_string())),
        Ok(_) => fail("the answer announced a table and sent another message"),
        Err(e) => fail(&e.to_string()),
    }
}

/// `abt kernel`.
pub fn main(args: &[String]) {
    let a = parse(args);
    match a.cmd.as_str() {
        "serve" => {
            let path = a.opts.get("socket").map(PathBuf::from).unwrap_or_else(|| default_socket("kernel"));
            let data = PathBuf::from(a.opts.get("data").cloned().unwrap_or_else(|| fail("--data SOCKET names the data service the kernel reads")));
            let connect = Arc::new(move || {
                let conn = Connection::connect(&data).map_err(|e| crate::kernel::RunError::Config(format!("--data {}: {}", data.display(), e)))?;
                DataClient::connect(conn)
            });
            let service = KernelService::new(connect);
            eprintln!("serving the kernel on {}", path.display());
            let h = crate::kernel::service::listen(&path, service).unwrap_or_else(|e| fail(&format!("{}: {}", path.display(), e)));
            let _ = h.join();
        }
        "check" => {
            let (m, _) = answer(&client(&a), with_program(&a, KernelOp::Check));
            for d in m.diagnostics.unwrap_or_default() {
                eprintln!("{}", d);
            }
            match m.program_hash {
                Some(h) => println!("{} {}: {}", a.opts["strategy"], h, m.degrees_of_freedom.unwrap_or_default()),
                None => exit(1),
            }
        }
        "contract" => {
            let (m, _) = answer(&client(&a), with_program(&a, KernelOp::Contract));
            print!("{}", m.text.unwrap_or_default());
        }
        "show" => {
            let mut r = match a.opts.get("run") {
                Some(run) => {
                    let mut r = request(KernelOp::Show);
                    r.run_id = Some(run.clone());
                    r
                }
                None => with_program(&a, KernelOp::Show),
            };
            r.what = a.opts.get("what").cloned();
            let (m, _) = answer(&client(&a), r);
            print!("{}", m.text.unwrap_or_default());
        }
        "run" => {
            let mut r = with_program(&a, KernelOp::Run);
            r.point = Some(
                a.params
                    .iter()
                    .map(|p| {
                        let (name, value) = p.split_once('=').unwrap_or_else(|| fail(&format!("--param takes NAME=VALUE, not `{}`", p)));
                        ParamValue {
                            name: name.trim().to_string(),
                            value: value.trim().to_string(),
                        }
                    })
                    .collect(),
            );
            r.start = a.opts.get("start").map(|s| time(s));
            r.end = a.opts.get("end").map(|s| time(s));
            r.progress_every = a.opts.get("progress").map(|n| n.parse().unwrap_or_else(|_| fail("--progress takes a number of barriers")));
            r.checkpoint_every = a.opts.get("checkpoint-every").cloned();
            r.from_checkpoint = a.opts.get("resume").cloned();
            // A live run (K-6.2, K-6.4): its mode, session and deployment
            // document, sent as its text.
            r.mode = a.opts.get("mode").cloned();
            r.session = a.opts.get("session").cloned();
            r.deployment = a
                .opts
                .get("deployment")
                .map(|p| std::fs::read_to_string(p).unwrap_or_else(|e| fail(&format!("--deployment {}: {}", p, e))));
            let c = client(&a);
            let (mut m, mut reply) = answer(&c, r);
            loop {
                match (m.status.as_deref(), &m.barrier) {
                    // A checkpoint, written through the data layer: its key.
                    (Some("running"), Some(t)) if m.text.as_deref().is_some_and(|x| x.starts_with("checkpoint ")) => {
                        eprintln!("{} of {}", m.text.as_deref().unwrap_or_default(), crate::kernel::time::format_timestamp(*t))
                    }
                    (Some("running"), Some(t)) => eprintln!("barrier {} decisions {}", crate::kernel::time::format_timestamp(*t), m.decisions.unwrap_or(0)),
                    (Some("running"), None) => println!("run {}", m.run_id.clone().unwrap_or_default()),
                    (status, _) => {
                        println!("{}: {}", m.run_id.clone().unwrap_or_default(), status.unwrap_or("?"));
                        println!("{}", m.report.clone().unwrap_or_default());
                        if status != Some("completed") {
                            exit(1)
                        }
                        return;
                    }
                }
                m = match reply.message() {
                    Ok(Message::KernelResponse(m)) => *m,
                    Ok(_) => fail("the kernel service answered with another message"),
                    Err(e) => {
                        eprintln!("{}", e);
                        exit(1)
                    }
                };
            }
        }
        "query" => {
            let mut r = request(KernelOp::Query);
            r.run_id = Some(a.opts.get("run").cloned().unwrap_or_else(|| fail("--run RUN names the run")));
            r.text = Some(a.opts.get("text").cloned().unwrap_or_else(|| fail("--text QUERY is the query")));
            let (m, mut reply) = answer(&client(&a), r);
            if a.json {
                let table = if m.has_table {
                    let batches = read_table(&mut reply);
                    Some(batches.iter().map(|b| b.num_rows()).sum::<usize>())
                } else {
                    None
                };
                println!("{}", serde_json::json!({"text": m.text, "rows": table}));
            } else {
                print!("{}", m.text.unwrap_or_default());
            }
        }
        "status" => {
            let mut r = request(KernelOp::Status);
            r.run_id = a.opts.get("run").cloned();
            let (m, _) = answer(&client(&a), r);
            print!("{}", m.text.unwrap_or_default());
        }
        "control" => {
            let mut r = request(KernelOp::Control);
            r.run_id = Some(a.opts.get("run").cloned().unwrap_or_else(|| fail("--run RUN names the run")));
            r.what = Some(a.positional.first().cloned().unwrap_or_else(|| usage()));
            let (m, _) = answer(&client(&a), r);
            // `checkpoint` answers with the checkpoint's key.
            match m.text {
                Some(key) => println!("{}: {} {}", m.run_id.unwrap_or_default(), m.status.unwrap_or_default(), key),
                None => println!("{}: {}", m.run_id.unwrap_or_default(), m.status.unwrap_or_default()),
            }
        }
        _ => usage(),
    }
}
