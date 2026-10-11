#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;

use cofre_core::check::{CheckOptions, run_check};
use cofre_core::prune::{forget, prune};
use cofre_core::retention::{Candidate, RetentionPolicy, apply_policy};
use common::{Fixture, pseudo_random, restored_root};

const DAY: i64 = 86_400;
const BASE: i64 = 1_767_268_800;

fn daily(n: i64) -> Vec<Candidate> {
    (0..n)
        .map(|i| Candidate {
            id: format!("{i:04}"),
            time: BASE + i * DAY,
            group: "g".into(),
        })
        .collect()
}

#[test]
fn empty_policy_keeps_everything() {
    let d = apply_policy(&daily(10), &RetentionPolicy::default());
    assert_eq!(d.keep.len(), 10);
    assert!(d.remove.is_empty());
}

#[test]
fn keep_last_keeps_newest() {
    let d = apply_policy(
        &daily(10),
        &RetentionPolicy {
            keep_last: 3,
            ..Default::default()
        },
    );
    assert_eq!(d.keep, vec!["0007", "0008", "0009"]);
    assert_eq!(d.remove.len(), 7);
}

#[test]
fn daily_keeps_one_per_day() {
    let mut c = daily(5);
    c.push(Candidate {
        id: "extra".into(),
        time: BASE + 4 * DAY + 60,
        group: "g".into(),
    });
    let d = apply_policy(
        &c,
        &RetentionPolicy {
            keep_daily: 7,
            ..Default::default()
        },
    );
    assert_eq!(d.keep.len(), 5);
    assert!(d.keep.contains(&"extra".to_string()));
    assert!(d.remove.contains(&"0004".to_string()));
}

#[test]
fn typical_policy_over_a_year() {
    let d = apply_policy(
        &daily(365),
        &RetentionPolicy {
            keep_daily: 7,
            keep_weekly: 4,
            keep_monthly: 12,
            ..Default::default()
        },
    );
    assert!(d.keep.len() >= 12 && d.keep.len() <= 23, "{}", d.keep.len());
    assert!(d.keep.contains(&"0364".to_string()));
    assert_eq!(d.keep.len() + d.remove.len(), 365);
}

#[test]
fn groups_are_independent() {
    let mut c = daily(3);
    c.push(Candidate {
        id: "outra".into(),
        time: BASE,
        group: "h".into(),
    });
    let d = apply_policy(
        &c,
        &RetentionPolicy {
            keep_last: 1,
            ..Default::default()
        },
    );
    assert_eq!(d.keep.len(), 2);
    assert!(d.keep.contains(&"outra".to_string()));
}

#[test]
fn forget_and_prune_never_delete_chunks_in_use() {
    let fx = Fixture::new();
    let src = fx.source();
    let shared = pseudo_random(150_000, 1);
    fs::write(src.join("compartilhado.bin"), &shared).unwrap();
    fs::write(src.join("temp.bin"), pseudo_random(150_000, 2)).unwrap();
    fx.backup(&src);
    fs::remove_file(src.join("temp.bin")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let latest = fx.backup(&src);

    let fr = forget(
        &fx.repo,
        &RetentionPolicy {
            keep_last: 1,
            ..Default::default()
        },
        "t",
        false,
    )
    .unwrap();
    assert_eq!(fr.deleted, 1);
    assert_eq!(fr.decision.keep, vec![latest.snapshot_id.clone()]);

    let pr = prune(&fx.repo, "t", false).unwrap();
    assert!(pr.deleted > 0);
    let check = run_check(&fx.repo, &CheckOptions { read_data: true }).unwrap();
    assert!(check.is_ok());
    assert_eq!(check.unreferenced_chunks, 0);

    let target = fx.restore_all(&latest.snapshot_id);
    assert_eq!(
        fs::read(restored_root(&target, &src).join("compartilhado.bin")).unwrap(),
        shared
    );
}

#[test]
fn forget_with_empty_policy_is_refused() {
    let fx = Fixture::new();
    assert!(forget(&fx.repo, &RetentionPolicy::default(), "t", false).is_err());
}

#[test]
fn dry_run_changes_nothing() {
    let fx = Fixture::new();
    let src = fx.source();
    fs::write(src.join("a"), pseudo_random(10_000, 3)).unwrap();
    fx.backup(&src);
    fs::write(src.join("a"), pseudo_random(10_000, 4)).unwrap();
    fx.backup(&src);
    let before = fx.repo.list_chunks().unwrap().len();
    let fr = forget(
        &fx.repo,
        &RetentionPolicy {
            keep_last: 1,
            ..Default::default()
        },
        "t",
        true,
    )
    .unwrap();
    assert_eq!(fr.decision.remove.len(), 1);
    assert_eq!(fx.repo.list_snapshot_ids().unwrap().len(), 2);
    prune(&fx.repo, "t", true).unwrap();
    assert_eq!(fx.repo.list_chunks().unwrap().len(), before);
}
