use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{Local, TimeZone};
use clap::{Args, Parser, Subcommand};
use cofre_cli::backends::open_backend;
use cofre_core::backup::{BackupOptions, run_backup};
use cofre_core::check::{CheckOptions, run_check};
use cofre_core::chunker::ChunkerParams;
use cofre_core::crypto::KdfParams;
use cofre_core::lock::{list_locks, remove_locks};
use cofre_core::paths::{key_matches_prefix, normalize_key};
use cofre_core::prune::{forget, forget_ids, prune};
use cofre_core::restore::{RestoreOptions, run_restore};
use cofre_core::retention::RetentionPolicy;
use cofre_core::{Backend, Error as CoreError, InitOptions, NodeKind, Repository};
use serde::Serialize;
use tracing_subscriber::EnvFilter;
use zeroize::Zeroizing;

const EXIT_WARNINGS: u8 = 3;

#[derive(Parser, Debug)]
#[command(
    name = "cofre",
    version,
    about = "Backup incremental, deduplicado e criptografado de ponta a ponta"
)]
struct Cli {
    #[arg(short, long, env = "COFRE_REPO", global = true)]
    repo: Option<String>,

    #[arg(long, env = "COFRE_PASSWORD_FILE", global = true)]
    password_file: Option<PathBuf>,

    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Init(InitArgs),
    Backup(BackupArgs),
    Snapshots {
        #[arg(long)]
        json: bool,
    },
    Ls {
        snapshot: String,
        path: Option<String>,
        #[arg(short, long)]
        long: bool,
    },
    Restore {
        snapshot: String,
        #[arg(short, long)]
        target: PathBuf,
        #[arg(short, long)]
        include: Option<String>,
        #[arg(long)]
        overwrite: bool,
    },
    Check {
        #[arg(long)]
        read_data: bool,
    },
    Forget(ForgetArgs),
    Prune {
        #[arg(long)]
        dry_run: bool,
    },
    Unlock {
        #[arg(long)]
        all: bool,
    },
}

#[derive(Args, Debug)]
struct InitArgs {
    #[arg(long, default_value_t = 64 * 1024)]
    kdf_memory_kib: u32,
    #[arg(long, default_value_t = 3)]
    kdf_iterations: u32,
    #[arg(long, default_value_t = 3)]
    compression: i32,
}

#[derive(Args, Debug)]
struct BackupArgs {
    #[arg(required = true)]
    paths: Vec<PathBuf>,
    #[arg(long)]
    tag: Vec<String>,
    #[arg(short, long)]
    exclude: Vec<String>,
    #[arg(long)]
    exclude_file: Option<PathBuf>,
    #[arg(long)]
    host: Option<String>,
    #[arg(long, env = "COFRE_HEARTBEAT_URL")]
    heartbeat_url: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct ForgetArgs {
    snapshots: Vec<String>,
    #[arg(long, default_value_t = 0)]
    keep_last: u32,
    #[arg(long, default_value_t = 0)]
    keep_hourly: u32,
    #[arg(long, default_value_t = 0)]
    keep_daily: u32,
    #[arg(long, default_value_t = 0)]
    keep_weekly: u32,
    #[arg(long, default_value_t = 0)]
    keep_monthly: u32,
    #[arg(long, default_value_t = 0)]
    keep_yearly: u32,
    #[arg(long)]
    prune: bool,
    #[arg(long)]
    dry_run: bool,
}

fn hostname() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

fn fmt_time(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .earliest()
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| ts.to_string())
}

fn read_password(cli: &Cli, confirm: bool) -> Result<Zeroizing<String>> {
    if let Ok(p) = std::env::var("COFRE_PASSWORD") {
        return Ok(Zeroizing::new(p));
    }
    if let Some(path) = &cli.password_file {
        let raw = Zeroizing::new(
            std::fs::read_to_string(path)
                .with_context(|| format!("lendo senha de {}", path.display()))?,
        );
        return Ok(Zeroizing::new(
            raw.trim_end_matches(['\r', '\n']).to_string(),
        ));
    }
    if !std::io::stdin().is_terminal() {
        bail!("sem terminal: defina COFRE_PASSWORD ou COFRE_PASSWORD_FILE");
    }
    let first = Zeroizing::new(rpassword::prompt_password("Senha do repositório: ")?);
    if confirm {
        let second = Zeroizing::new(rpassword::prompt_password("Repita a senha: ")?);
        if *first != *second {
            bail!("as senhas não conferem");
        }
    }
    Ok(first)
}

fn server_token() -> Option<Zeroizing<String>> {
    if let Ok(path) = std::env::var("COFRE_SERVER_TOKEN_FILE") {
        if let Ok(t) = std::fs::read_to_string(path) {
            return Some(Zeroizing::new(t.trim().to_string()));
        }
    }
    std::env::var("COFRE_SERVER_TOKEN")
        .ok()
        .map(|t| Zeroizing::new(t.trim().to_string()))
}

fn backend(cli: &Cli) -> Result<Arc<dyn Backend>> {
    let spec = cli
        .repo
        .as_deref()
        .ok_or_else(|| anyhow!("informe o repositório com --repo ou COFRE_REPO"))?;
    open_backend(spec, server_token())
}

fn open_repo(cli: &Cli) -> Result<Repository> {
    let backend = backend(cli)?;
    let password = read_password(cli, false)?;
    match Repository::open(backend, password.as_bytes()) {
        Ok(r) => Ok(r),
        Err(CoreError::WrongPassword) => bail!("senha incorreta"),
        Err(e) => Err(e.into()),
    }
}

fn cmd_init(cli: &Cli, args: &InitArgs) -> Result<()> {
    let backend = backend(cli)?;
    let password = read_password(cli, true)?;
    if password.chars().count() < 8 {
        bail!("use uma senha com pelo menos 8 caracteres");
    }
    let opts = InitOptions {
        kdf: KdfParams::new(args.kdf_memory_kib, args.kdf_iterations, 1),
        chunker: ChunkerParams::default(),
        compression_level: args.compression,
    };
    let repo = Repository::init(backend.clone(), password.as_bytes(), opts)?;
    println!(
        "repositório {} criado em {}",
        &repo.config().repo_id[..8],
        backend.describe()
    );
    println!("ATENÇÃO: sem a senha não há como recuperar os dados. Guarde-a em local seguro.");
    Ok(())
}

fn ping(url: &str) {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("heartbeat: {e}");
            return;
        }
    };
    if let Err(e) = client.get(url).send().and_then(|r| r.error_for_status()) {
        eprintln!("aviso: heartbeat para {url} falhou: {e}");
    }
}

#[derive(Serialize)]
struct BackupJson<'a> {
    snapshot_id: &'a str,
    summary: &'a cofre_core::Summary,
    warnings: &'a [String],
}

fn cmd_backup(cli: &Cli, args: &BackupArgs) -> Result<ExitCode> {
    if let Some(url) = &args.heartbeat_url {
        ping(&format!("{}/start", url.trim_end_matches('/')));
    }
    let result = (|| -> Result<ExitCode> {
        let repo = open_repo(cli)?;
        let mut opts = BackupOptions::new(args.host.clone().unwrap_or_else(hostname));
        opts.tags = args.tag.clone();
        opts.excludes = args.exclude.clone();
        if let Some(f) = &args.exclude_file {
            let text =
                std::fs::read_to_string(f).with_context(|| format!("lendo {}", f.display()))?;
            opts.excludes.extend(
                text.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(str::to_string),
            );
        }
        let report = run_backup(&repo, &args.paths, &opts)?;
        let s = &report.summary;
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&BackupJson {
                    snapshot_id: &report.snapshot_id,
                    summary: s,
                    warnings: &report.warnings,
                })?
            );
        } else {
            println!("snapshot {} salvo", &report.snapshot_id[..8]);
            println!(
                "  {} arquivos, {} pastas, {} links, {} lidos",
                s.files,
                s.dirs,
                s.symlinks,
                human_bytes(s.bytes)
            );
            println!(
                "  {} blocos novos, {} enviados, em {:.1}s",
                s.new_chunks,
                human_bytes(s.new_bytes_stored),
                s.duration_ms as f64 / 1000.0
            );
            for w in &report.warnings {
                eprintln!("aviso: {w}");
            }
        }
        if report.warnings.is_empty() {
            Ok(ExitCode::SUCCESS)
        } else {
            Ok(ExitCode::from(EXIT_WARNINGS))
        }
    })();
    if let Some(url) = &args.heartbeat_url {
        let url = url.trim_end_matches('/');
        match &result {
            Ok(_) => ping(url),
            Err(_) => ping(&format!("{url}/fail")),
        }
    }
    result
}

#[derive(Serialize)]
struct SnapshotRow<'a> {
    id: &'a str,
    time: i64,
    hostname: &'a str,
    paths: &'a [String],
    tags: &'a [String],
    summary: &'a cofre_core::Summary,
}

fn cmd_snapshots(cli: &Cli, json: bool) -> Result<()> {
    let repo = open_repo(cli)?;
    let snaps = repo.load_all_snapshots()?;
    if json {
        let rows: Vec<SnapshotRow> = snaps
            .iter()
            .map(|(id, s)| SnapshotRow {
                id,
                time: s.time,
                hostname: &s.hostname,
                paths: &s.paths,
                tags: &s.tags,
                summary: &s.summary,
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!(
        "{:<10} {:<20} {:<16} {:>8} {:>10}  caminhos",
        "id", "data", "máquina", "arquivos", "tamanho"
    );
    for (id, s) in &snaps {
        let tags = if s.tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", s.tags.join(","))
        };
        println!(
            "{:<10} {:<20} {:<16} {:>8} {:>10}  {}{}",
            &id[..8],
            fmt_time(s.time),
            s.hostname,
            s.summary.files,
            human_bytes(s.summary.bytes),
            s.paths
                .iter()
                .map(|p| format!("/{p}"))
                .collect::<Vec<_>>()
                .join(" "),
            tags
        );
    }
    println!("{} snapshot(s)", snaps.len());
    Ok(())
}

fn cmd_ls(cli: &Cli, spec: &str, path: Option<&str>, long: bool) -> Result<()> {
    let repo = open_repo(cli)?;
    let id = repo.resolve_snapshot(spec)?;
    let snap = repo.load_snapshot(&id)?;
    let prefix = path.map(normalize_key).unwrap_or_default();
    for node in snap
        .nodes
        .iter()
        .filter(|n| key_matches_prefix(&n.path, &prefix))
    {
        let suffix = match &node.kind {
            NodeKind::Dir => "/".to_string(),
            NodeKind::Symlink { target } => format!(" -> {target}"),
            NodeKind::File { .. } => String::new(),
        };
        if long {
            println!(
                "{:04o} {:>10} {} /{}{}",
                node.mode,
                human_bytes(node.size()),
                fmt_time(node.mtime),
                node.path,
                suffix
            );
        } else {
            println!("/{}{}", node.path, suffix);
        }
    }
    Ok(())
}

fn cmd_restore(
    cli: &Cli,
    spec: &str,
    target: &std::path::Path,
    include: Option<String>,
    overwrite: bool,
) -> Result<ExitCode> {
    let repo = open_repo(cli)?;
    let id = repo.resolve_snapshot(spec)?;
    let snap = repo.load_snapshot(&id)?;
    let report = run_restore(&repo, &snap, target, &RestoreOptions { include, overwrite })?;
    println!(
        "restaurado snapshot {} em {}: {} arquivos, {} pastas, {} links, {}",
        &id[..8],
        target.display(),
        report.files,
        report.dirs,
        report.symlinks,
        human_bytes(report.bytes)
    );
    for w in &report.warnings {
        eprintln!("aviso: {w}");
    }
    Ok(if report.warnings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_WARNINGS)
    })
}

fn cmd_check(cli: &Cli, read_data: bool) -> Result<()> {
    let repo = open_repo(cli)?;
    let report = run_check(&repo, &CheckOptions { read_data })?;
    println!(
        "{} snapshots, {} blocos referenciados, {} armazenados, {} órfãos",
        report.snapshots,
        report.referenced_chunks,
        report.stored_chunks,
        report.unreferenced_chunks
    );
    if read_data {
        println!("{} blocos lidos e verificados", report.chunks_read);
    }
    for (id, e) in &report.broken_snapshots {
        eprintln!("ERRO snapshot {}: {e}", &id[..8.min(id.len())]);
    }
    for id in &report.missing_chunks {
        eprintln!("ERRO bloco ausente: {id}");
    }
    for (id, e) in &report.corrupted_chunks {
        eprintln!("ERRO bloco corrompido {id}: {e}");
    }
    if report.is_ok() {
        println!("nenhum problema encontrado");
        Ok(())
    } else {
        bail!("o repositório tem problemas")
    }
}

fn cmd_forget(cli: &Cli, args: &ForgetArgs) -> Result<()> {
    let repo = open_repo(cli)?;
    let host = hostname();
    if !args.snapshots.is_empty() {
        if args.dry_run {
            for spec in &args.snapshots {
                println!("removeria {}", &repo.resolve_snapshot(spec)?[..8]);
            }
        } else {
            let n = forget_ids(&repo, &args.snapshots, &host)?;
            println!("{n} snapshot(s) removido(s)");
        }
    } else {
        let policy = RetentionPolicy {
            keep_last: args.keep_last,
            keep_hourly: args.keep_hourly,
            keep_daily: args.keep_daily,
            keep_weekly: args.keep_weekly,
            keep_monthly: args.keep_monthly,
            keep_yearly: args.keep_yearly,
        };
        let report = forget(&repo, &policy, &host, args.dry_run)?;
        let verb = if args.dry_run {
            "removeria"
        } else {
            "removido"
        };
        for id in &report.decision.keep {
            println!("mantido   {}", &id[..8]);
        }
        for id in &report.decision.remove {
            println!("{verb:<9} {}", &id[..8]);
        }
        println!(
            "{} mantidos, {} {}",
            report.decision.keep.len(),
            report.decision.remove.len(),
            if args.dry_run {
                "seriam removidos"
            } else {
                "removidos"
            }
        );
    }
    if args.prune {
        run_prune(&repo, &host, args.dry_run)?;
    }
    Ok(())
}

fn run_prune(repo: &Repository, host: &str, dry_run: bool) -> Result<()> {
    let r = prune(repo, host, dry_run)?;
    println!(
        "{} blocos em uso, {} {} de {} armazenados, {} temporários limpos",
        r.referenced,
        r.deleted,
        if dry_run {
            "seriam apagados"
        } else {
            "apagados"
        },
        r.stored,
        r.temp_files_removed
    );
    Ok(())
}

fn cmd_unlock(cli: &Cli, all: bool) -> Result<()> {
    let repo = open_repo(cli)?;
    let before = list_locks(&repo)?;
    let removed = remove_locks(&repo, all)?;
    println!("{removed} trava(s) removida(s) de {}", before.len());
    if !all && removed < before.len() {
        println!("travas ativas mantidas; use --all se tiver certeza de que nada está rodando");
    }
    Ok(())
}

fn run(cli: &Cli) -> Result<ExitCode> {
    match &cli.command {
        Command::Init(a) => cmd_init(cli, a).map(|()| ExitCode::SUCCESS),
        Command::Backup(a) => cmd_backup(cli, a),
        Command::Snapshots { json } => cmd_snapshots(cli, *json).map(|()| ExitCode::SUCCESS),
        Command::Ls {
            snapshot,
            path,
            long,
        } => cmd_ls(cli, snapshot, path.as_deref(), *long).map(|()| ExitCode::SUCCESS),
        Command::Restore {
            snapshot,
            target,
            include,
            overwrite,
        } => cmd_restore(cli, snapshot, target, include.clone(), *overwrite),
        Command::Check { read_data } => cmd_check(cli, *read_data).map(|()| ExitCode::SUCCESS),
        Command::Forget(a) => cmd_forget(cli, a).map(|()| ExitCode::SUCCESS),
        Command::Prune { dry_run } => {
            let repo = open_repo(cli)?;
            run_prune(&repo, &hostname(), *dry_run).map(|()| ExitCode::SUCCESS)
        }
        Command::Unlock { all } => cmd_unlock(cli, *all).map(|()| ExitCode::SUCCESS),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_env("COFRE_LOG").unwrap_or_else(|_| EnvFilter::new(level)),
        )
        .init();
    match run(&cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("erro: {e:#}");
            ExitCode::FAILURE
        }
    }
}
