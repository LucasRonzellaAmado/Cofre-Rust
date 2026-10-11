use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use cofre_server::{AppState, app};
use tracing_subscriber::EnvFilter;
use zeroize::Zeroizing;

#[derive(Parser, Debug)]
#[command(
    name = "cofre-server",
    version,
    about = "Servidor de destino remoto do Cofre"
)]
struct Args {
    #[arg(long, env = "COFRE_LISTEN", default_value = "127.0.0.1:8000")]
    listen: SocketAddr,

    #[arg(long, env = "COFRE_DATA", default_value = "/data")]
    data: PathBuf,

    #[arg(long, env = "COFRE_APPEND_ONLY", default_value_t = false)]
    append_only: bool,

    #[arg(long, env = "COFRE_TOKEN_FILE")]
    token_file: Option<PathBuf>,

    #[arg(long, default_value_t = false)]
    no_auth: bool,
}

fn load_token(args: &Args) -> Result<Option<Zeroizing<String>>> {
    if args.no_auth {
        return Ok(None);
    }
    let raw = if let Some(path) = &args.token_file {
        Zeroizing::new(
            std::fs::read_to_string(path)
                .with_context(|| format!("lendo token em {}", path.display()))?,
        )
    } else if let Ok(t) = std::env::var("COFRE_SERVER_TOKEN") {
        Zeroizing::new(t)
    } else {
        bail!("defina COFRE_SERVER_TOKEN ou --token-file (ou use --no-auth apenas em testes)");
    };
    let token = Zeroizing::new(raw.trim().to_string());
    if token.len() < 32 {
        bail!("o token precisa ter ao menos 32 caracteres");
    }
    Ok(Some(token))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = term => {},
    }
    tracing::info!("encerrando");
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let args = Args::parse();
    let token = load_token(&args)?;
    std::fs::create_dir_all(&args.data)
        .with_context(|| format!("criando {}", args.data.display()))?;
    if token.is_none() {
        tracing::warn!("autenticação DESLIGADA");
    }
    let state = AppState::new(&args.data, args.append_only, token);
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("escutando em {}", args.listen))?;
    tracing::info!(
        listen = %args.listen,
        data = %args.data.display(),
        append_only = args.append_only,
        "cofre-server iniciado"
    );
    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}
