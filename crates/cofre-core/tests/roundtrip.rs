#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;

use cofre_core::backup::{BackupOptions, run_backup};
use cofre_core::restore::{RestoreOptions, run_restore};
use common::{Fixture, pseudo_random, restored_root, tree_contents};
use proptest::prelude::*;

fn populate(root: &std::path::Path) {
    fs::create_dir_all(root.join("docs/sub/vazio")).unwrap();
    fs::write(root.join("docs/a.txt"), b"ola mundo").unwrap();
    fs::write(root.join("docs/vazio.bin"), b"").unwrap();
    fs::write(root.join("docs/sub/grande.bin"), pseudo_random(300_000, 1)).unwrap();
    fs::write(root.join("repetido1.bin"), pseudo_random(100_000, 2)).unwrap();
    fs::write(root.join("repetido2.bin"), pseudo_random(100_000, 2)).unwrap();
    fs::write(root.join("acentuação ç.txt"), "conteúdo".as_bytes()).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("docs/a.txt", root.join("link")).unwrap();
}

#[test]
fn backup_then_restore_gives_identical_tree() {
    let fx = Fixture::new();
    let src = fx.source();
    populate(&src);
    let report = fx.backup(&src);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.summary.files, 6);

    let target = fx.restore_all(&report.snapshot_id);
    let restored = restored_root(&target, &src);
    assert_eq!(tree_contents(&src), tree_contents(&restored));
}

#[cfg(unix)]
#[test]
fn restore_preserves_mode_and_mtime() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let src = fx.source();
    let file = src.join("script.sh");
    fs::write(&file, b"#!/bin/sh\necho oi\n").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o750)).unwrap();
    let mtime = filetime::FileTime::from_unix_time(1_600_000_000, 123_000_000);
    filetime::set_file_mtime(&file, mtime).unwrap();

    let report = fx.backup(&src);
    let target = fx.restore_all(&report.snapshot_id);
    let restored = restored_root(&target, &src).join("script.sh");
    let meta = fs::metadata(&restored).unwrap();
    assert_eq!(meta.permissions().mode() & 0o7777, 0o750);
    assert_eq!(
        filetime::FileTime::from_last_modification_time(&meta),
        mtime
    );
}

#[test]
fn second_backup_without_changes_uploads_nothing() {
    let fx = Fixture::new();
    let src = fx.source();
    populate(&src);
    let first = fx.backup(&src);
    assert!(first.summary.new_chunks > 0);
    let second = fx.backup(&src);
    assert_eq!(second.summary.new_chunks, 0);
    assert_eq!(second.summary.new_bytes_stored, 0);
}

#[test]
fn identical_files_are_deduplicated() {
    let fx = Fixture::new();
    let src = fx.source();
    fs::write(src.join("a.bin"), pseudo_random(200_000, 3)).unwrap();
    let one = fx.backup(&src);
    fs::write(src.join("b.bin"), pseudo_random(200_000, 3)).unwrap();
    let two = fx.backup(&src);
    assert!(one.summary.new_chunks > 5);
    assert_eq!(two.summary.new_chunks, 0);
}

#[test]
fn change_in_middle_uploads_few_chunks() {
    let fx = Fixture::new();
    let src = fx.source();
    let mut data = pseudo_random(1_000_000, 4);
    fs::write(src.join("db.bin"), &data).unwrap();
    let first = fx.backup(&src);
    data.splice(
        400_000..400_000,
        b"registro novo inserido no meio".iter().copied(),
    );
    fs::write(src.join("db.bin"), &data).unwrap();
    let second = fx.backup(&src);
    assert!(first.summary.new_chunks > 50);
    assert!(
        second.summary.new_chunks <= 4,
        "novos blocos: {}",
        second.summary.new_chunks
    );
    let target = fx.restore_all(&second.snapshot_id);
    assert_eq!(
        fs::read(restored_root(&target, &src).join("db.bin")).unwrap(),
        data
    );
}

#[test]
fn old_snapshot_still_restores_old_version() {
    let fx = Fixture::new();
    let src = fx.source();
    fs::write(src.join("f.txt"), b"versao 1").unwrap();
    let v1 = fx.backup(&src);
    fs::write(src.join("f.txt"), b"versao 2").unwrap();
    let v2 = fx.backup(&src);
    let t1 = fx.restore_all(&v1.snapshot_id);
    let t2 = fx.restore_all(&v2.snapshot_id);
    assert_eq!(
        fs::read(restored_root(&t1, &src).join("f.txt")).unwrap(),
        b"versao 1"
    );
    assert_eq!(
        fs::read(restored_root(&t2, &src).join("f.txt")).unwrap(),
        b"versao 2"
    );
}

#[test]
fn restore_single_file_and_folder() {
    let fx = Fixture::new();
    let src = fx.source();
    populate(&src);
    let report = fx.backup(&src);
    let snap = fx.repo.load_snapshot(&report.snapshot_id).unwrap();
    let abs = fs::canonicalize(&src).unwrap();
    let key = cofre_core::paths::path_to_key(&abs).unwrap();

    let t = fx.dir.path().join("so-um");
    let r = run_restore(
        &fx.repo,
        &snap,
        &t,
        &RestoreOptions {
            include: Some(format!("/{key}/docs/a.txt")),
            overwrite: false,
        },
    )
    .unwrap();
    assert_eq!(r.files, 1);

    let t2 = fx.dir.path().join("pasta");
    let r2 = run_restore(
        &fx.repo,
        &snap,
        &t2,
        &RestoreOptions {
            include: Some(format!("{key}/docs")),
            overwrite: false,
        },
    )
    .unwrap();
    assert_eq!(r2.files, 3);
    assert!(!restored_root(&t2, &src).join("repetido1.bin").exists());
}

#[test]
fn restore_does_not_overwrite_unless_asked() {
    let fx = Fixture::new();
    let src = fx.source();
    fs::write(src.join("f.txt"), b"backup").unwrap();
    let report = fx.backup(&src);
    let snap = fx.repo.load_snapshot(&report.snapshot_id).unwrap();
    let target = fx.dir.path().join("dest");
    run_restore(&fx.repo, &snap, &target, &RestoreOptions::default()).unwrap();
    let file = restored_root(&target, &src).join("f.txt");
    fs::write(&file, b"alterado localmente").unwrap();

    let r = run_restore(&fx.repo, &snap, &target, &RestoreOptions::default()).unwrap();
    assert_eq!(r.skipped, 1);
    assert_eq!(fs::read(&file).unwrap(), b"alterado localmente");

    run_restore(
        &fx.repo,
        &snap,
        &target,
        &RestoreOptions {
            include: None,
            overwrite: true,
        },
    )
    .unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"backup");
}

#[test]
fn excludes_are_respected() {
    let fx = Fixture::new();
    let src = fx.source();
    fs::create_dir_all(src.join("node_modules/pkg")).unwrap();
    fs::write(src.join("node_modules/pkg/index.js"), b"x").unwrap();
    fs::write(src.join("app.js"), b"y").unwrap();
    fs::write(src.join("debug.log"), b"z").unwrap();
    let mut opts = BackupOptions::new("teste");
    opts.excludes = vec!["node_modules".into(), "*.log".into()];
    let report = run_backup(&fx.repo, std::slice::from_ref(&src), &opts).unwrap();
    let snap = fx.repo.load_snapshot(&report.snapshot_id).unwrap();
    let names: Vec<&str> = snap.nodes.iter().map(|n| n.path.as_str()).collect();
    assert!(names.iter().any(|n| n.ends_with("app.js")));
    assert!(!names.iter().any(|n| n.contains("node_modules")));
    assert!(!names.iter().any(|n| n.ends_with(".log")));
}

#[test]
fn backup_of_missing_path_fails_cleanly() {
    let fx = Fixture::new();
    let missing = fx.dir.path().join("nao-existe");
    assert!(run_backup(&fx.repo, &[missing], &BackupOptions::new("t")).is_err());
    assert!(fx.repo.list_snapshot_ids().unwrap().is_empty());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn prop_backup_restore_returns_same_bytes(
        files in proptest::collection::btree_map(
            "[a-z]{1,8}(/[a-z]{1,8}){0,2}",
            proptest::collection::vec(any::<u8>(), 0..60_000),
            1..8,
        )
    ) {
        let fx = Fixture::new();
        let src = fx.source();
        let mut written = std::collections::BTreeMap::new();
        for (name, data) in &files {
            let path = src.join(name);
            if written.keys().any(|k: &String| name.starts_with(&format!("{k}/")) || k.starts_with(&format!("{name}/"))) {
                continue;
            }
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, data).unwrap();
            written.insert(name.clone(), data.clone());
        }
        let report = fx.backup(&src);
        let target = fx.restore_all(&report.snapshot_id);
        let root = restored_root(&target, &src);
        for (name, data) in &written {
            let restored = fs::read(root.join(name)).unwrap();
            prop_assert_eq!(&restored, data);
        }
        prop_assert_eq!(tree_contents(&src), tree_contents(&root));
    }
}
