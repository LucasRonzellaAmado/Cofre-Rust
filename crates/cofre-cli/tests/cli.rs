#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn cofre(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cofre"))
        .args(args)
        .env("COFRE_REPO", repo)
        .env("COFRE_PASSWORD", "senha-muito-boa")
        .env_remove("COFRE_PASSWORD_FILE")
        .output()
        .unwrap()
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn full_cycle_through_the_binary() {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = dir.path().join("repo");
    let src = dir.path().join("dados");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("a.txt"), b"conteudo a").unwrap();
    fs::write(src.join("sub/b.txt"), b"conteudo b").unwrap();

    ok(&cofre(
        &repo,
        &["init", "--kdf-memory-kib", "1024", "--kdf-iterations", "1"],
    ));
    let again = cofre(
        &repo,
        &["init", "--kdf-memory-kib", "1024", "--kdf-iterations", "1"],
    );
    assert!(!again.status.success());

    let out = ok(&cofre(&repo, &["backup", src.to_str().unwrap(), "--json"]));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["summary"]["files"], 2);

    let list = ok(&cofre(&repo, &["snapshots", "--json"]));
    let rows: Vec<serde_json::Value> = serde_json::from_str(&list).unwrap();
    assert_eq!(rows.len(), 1);

    let ls = ok(&cofre(&repo, &["ls", "latest"]));
    assert!(ls.contains("sub/b.txt"));

    let target = dir.path().join("volta");
    ok(&cofre(
        &repo,
        &["restore", "latest", "--target", target.to_str().unwrap()],
    ));
    let canon = fs::canonicalize(&src).unwrap();
    let rel = canon.strip_prefix("/").unwrap_or(&canon);
    assert_eq!(
        fs::read(target.join(rel).join("sub/b.txt")).unwrap(),
        b"conteudo b"
    );

    ok(&cofre(&repo, &["check", "--read-data"]));

    fs::write(src.join("a.txt"), b"mudou").unwrap();
    ok(&cofre(&repo, &["backup", src.to_str().unwrap()]));
    let forget = cofre(&repo, &["forget"]);
    assert!(!forget.status.success());
    ok(&cofre(&repo, &["forget", "--keep-last", "1", "--prune"]));
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&ok(&cofre(&repo, &["snapshots", "--json"]))).unwrap();
    assert_eq!(rows.len(), 1);
    ok(&cofre(&repo, &["check", "--read-data"]));
}

#[test]
fn wrong_password_is_reported() {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = dir.path().join("repo");
    ok(&cofre(
        &repo,
        &["init", "--kdf-memory-kib", "1024", "--kdf-iterations", "1"],
    ));
    let out = Command::new(env!("CARGO_BIN_EXE_cofre"))
        .args(["snapshots"])
        .env("COFRE_REPO", &repo)
        .env("COFRE_PASSWORD", "senha-errada")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("senha incorreta"));
}
