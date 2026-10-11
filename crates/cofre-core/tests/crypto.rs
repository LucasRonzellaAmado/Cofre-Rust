#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use cofre_core::crypto::{KdfParams, MasterKeys, open_with, seal_with};
use cofre_core::{Backend, Error, Kind, LocalBackend, Repository};
use common::{Fixture, PASSWORD, test_options};

#[test]
fn seal_and_open_round_trip() {
    let key = [7u8; 32];
    let blob = seal_with(&key, b"aad", b"conteudo secreto").unwrap();
    let plain = open_with(&key, b"aad", &blob, "teste").unwrap();
    assert_eq!(plain.as_slice(), b"conteudo secreto");
}

#[test]
fn same_plaintext_gives_different_ciphertext() {
    let key = [1u8; 32];
    let a = seal_with(&key, b"", b"igual").unwrap();
    let b = seal_with(&key, b"", b"igual").unwrap();
    assert_ne!(a, b);
}

#[test]
fn tampered_ciphertext_is_rejected() {
    let key = [9u8; 32];
    let mut blob = seal_with(&key, b"aad", b"dados importantes").unwrap();
    let last = blob.len() - 1;
    blob[last] ^= 0x01;
    assert!(matches!(
        open_with(&key, b"aad", &blob, "teste"),
        Err(Error::Corrupted(_))
    ));
}

#[test]
fn wrong_aad_is_rejected() {
    let key = [3u8; 32];
    let blob = seal_with(&key, b"bloco-a", b"x").unwrap();
    assert!(open_with(&key, b"bloco-b", &blob, "teste").is_err());
}

#[test]
fn truncated_blob_is_rejected() {
    let key = [3u8; 32];
    assert!(matches!(
        open_with(&key, b"", &[1, 2, 3], "teste"),
        Err(Error::Corrupted(_))
    ));
}

#[test]
fn unknown_blob_version_is_rejected() {
    let key = [3u8; 32];
    let mut blob = seal_with(&key, b"", b"x").unwrap();
    blob[0] = 99;
    assert!(matches!(
        open_with(&key, b"", &blob, "teste"),
        Err(Error::UnsupportedVersion(99))
    ));
}

#[test]
fn kdf_is_deterministic_per_salt() {
    let params = KdfParams::new(1024, 1, 1);
    let a = params.derive(b"senha").unwrap();
    let b = params.derive(b"senha").unwrap();
    let c = params.derive(b"outra").unwrap();
    assert_eq!(*a, *b);
    assert_ne!(*a, *c);
    let other_salt = KdfParams::new(1024, 1, 1);
    assert_ne!(*a, *other_salt.derive(b"senha").unwrap());
}

#[test]
fn wrapped_keys_need_right_kek() {
    let keys = MasterKeys::generate();
    let blob = keys.wrap(&[5u8; 32]).unwrap();
    let back = MasterKeys::unwrap_with(&[5u8; 32], &blob).unwrap();
    assert_eq!(keys.chunk_id(b"abc"), back.chunk_id(b"abc"));
    assert!(matches!(
        MasterKeys::unwrap_with(&[6u8; 32], &blob),
        Err(Error::WrongPassword)
    ));
}

#[test]
fn open_with_wrong_password_fails() {
    let fx = Fixture::new();
    let err = Repository::open(fx.backend.clone(), b"senha-errada")
        .err()
        .unwrap();
    assert!(matches!(err, Error::WrongPassword));
    assert!(Repository::open(fx.backend.clone(), PASSWORD).is_ok());
}

#[test]
fn init_twice_fails() {
    let fx = Fixture::new();
    let err = Repository::init(fx.backend.clone(), PASSWORD, test_options())
        .err()
        .unwrap();
    assert!(matches!(err, Error::AlreadyInitialized));
}

#[test]
fn open_uninitialized_fails() {
    let dir = tempfile::TempDir::new().unwrap();
    let backend: Arc<dyn Backend> = Arc::new(LocalBackend::new(dir.path()));
    assert!(matches!(
        Repository::open(backend, PASSWORD).err().unwrap(),
        Error::NotInitialized
    ));
}

#[test]
fn config_never_contains_plain_keys_or_password() {
    let fx = Fixture::new();
    let raw = fx.backend.get(Kind::Config, "config").unwrap();
    let text = String::from_utf8(raw).unwrap();
    assert!(!text.contains("senha-de-teste"));
    assert!(text.contains("argon2id"));
}

#[test]
fn chunk_ids_are_keyed_per_repository() {
    let a = Fixture::new();
    let b = Fixture::new();
    assert_ne!(
        a.repo.chunk_id(b"mesmo dado"),
        b.repo.chunk_id(b"mesmo dado")
    );
    assert_ne!(
        a.repo.chunk_id(b"mesmo dado").0,
        *blake3::hash(b"mesmo dado").as_bytes()
    );
}

#[test]
fn chunk_encode_decode_and_tamper_detection() {
    let fx = Fixture::new();
    let data = common::pseudo_random(10_000, 1);
    let id = fx.repo.chunk_id(&data);
    let blob = fx.repo.encode_chunk(&id, &data).unwrap();
    assert_eq!(fx.repo.decode_chunk(&id, &blob).unwrap(), data);

    let other = fx.repo.chunk_id(b"outro");
    assert!(fx.repo.decode_chunk(&other, &blob).is_err());

    let mut bad = blob.clone();
    bad[30] ^= 0xff;
    assert!(matches!(
        fx.repo.decode_chunk(&id, &bad),
        Err(Error::Corrupted(_))
    ));
}

#[test]
fn compressible_chunks_get_smaller() {
    let fx = Fixture::new();
    let data = vec![b'a'; 20_000];
    let id = fx.repo.chunk_id(&data);
    let blob = fx.repo.encode_chunk(&id, &data).unwrap();
    assert!(blob.len() < 1_000);
}
