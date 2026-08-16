use std::hint::black_box;
use std::io::Cursor;
use std::time::Instant;

use covalent_core::{BackupKey, ChunkingConfig, ContentDefinedChunker};
use covalent_protocol::BackupId;

fn main() {
    let mut state = 0x4d59_5df4_d0f3_3173_u64;
    let input: Vec<_> = (0..16 * 1_024 * 1_024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    let key = BackupKey::from_bytes([0x2d; 32]);
    let backup_id = BackupId::from_uuid(uuid::Uuid::from_u128(0x42));
    let started = Instant::now();
    let mut chunker = ContentDefinedChunker::new(Cursor::new(&input), ChunkingConfig::default());
    let mut processed = 0_usize;
    let mut chunks = 0_usize;
    while let Some(chunk) = chunker.next_chunk().expect("stream chunk") {
        let encrypted = key
            .encrypt_chunk(backup_id, 1, black_box(&chunk))
            .expect("encrypt chunk");
        let plaintext = key
            .decrypt_chunk(backup_id, &encrypted.plaintext_digest, &encrypted)
            .expect("decrypt chunk");
        assert_eq!(plaintext.as_slice(), chunk.as_slice());
        processed += plaintext.len();
        chunks += 1;
    }
    assert_eq!(processed, input.len());
    assert!(chunks > 1);
    let elapsed = started.elapsed();
    let mebibytes_per_second = input.len() as f64 / (1_024.0 * 1_024.0) / elapsed.as_secs_f64();
    eprintln!(
        "engine_smoke: {chunks} chunks, {processed} bytes, {:.2} MiB/s",
        mebibytes_per_second
    );
}
