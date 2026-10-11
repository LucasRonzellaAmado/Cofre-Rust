#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::HashSet;

use cofre_core::chunker::ChunkerParams;
use common::{pseudo_random, test_options};

fn params() -> ChunkerParams {
    test_options().chunker
}

fn split(data: &[u8]) -> Vec<Vec<u8>> {
    params().chunks(data).collect::<Result<_, _>>().unwrap()
}

#[test]
fn chunks_concatenate_back_to_input() {
    let data = pseudo_random(500_000, 42);
    let chunks = split(&data);
    assert!(chunks.len() > 10);
    assert_eq!(chunks.concat(), data);
    let p = params();
    for c in &chunks[..chunks.len() - 1] {
        assert!(c.len() >= p.min_size as usize);
        assert!(c.len() <= p.max_size as usize);
    }
}

#[test]
fn empty_input_has_no_chunks() {
    assert!(split(&[]).is_empty());
}

#[test]
fn insertion_in_the_middle_keeps_most_chunks() {
    let original = pseudo_random(1_000_000, 7);
    let mut modified = original.clone();
    let extra = pseudo_random(100, 99);
    modified.splice(500_000..500_000, extra);

    let a: HashSet<Vec<u8>> = split(&original).into_iter().collect();
    let b = split(&modified);
    let shared = b.iter().filter(|c| a.contains(*c)).count();
    assert!(
        shared + 3 >= b.len(),
        "esperava quase todos os blocos reaproveitados: {shared} de {}",
        b.len()
    );
}

#[test]
fn chunking_is_deterministic() {
    let data = pseudo_random(300_000, 5);
    assert_eq!(split(&data), split(&data));
}

#[test]
fn invalid_params_are_rejected() {
    let mut p = params();
    p.min_size = p.max_size + 1;
    assert!(p.validate().is_err());
}
