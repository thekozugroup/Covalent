use std::io::{self, Read};

/// Default content-defined target chunk size: 256 KiB.
pub const DEFAULT_AVERAGE_CHUNK_SIZE: usize = 256 * 1_024;
const READ_BUFFER_SIZE: usize = 64 * 1_024;

/// Validated streaming chunk-size limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkingConfig {
    /// Smallest non-final chunk.
    pub minimum_size: usize,
    /// Power-of-two target size controlling the content boundary mask.
    pub average_size: usize,
    /// Hard maximum allocation and chunk size.
    pub maximum_size: usize,
}

impl ChunkingConfig {
    /// Constructs safe content-defined chunking limits.
    pub fn new(
        minimum_size: usize,
        average_size: usize,
        maximum_size: usize,
    ) -> Result<Self, &'static str> {
        let config = Self {
            minimum_size,
            average_size,
            maximum_size,
        };
        if !config.is_valid() {
            return Err("chunk sizes require 4KiB <= min < power-of-two average < max <= 8MiB");
        }
        Ok(config)
    }

    /// Revalidates public fields after a contract or binding constructs this value.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.minimum_size >= 4 * 1_024
            && self.average_size.is_power_of_two()
            && self.minimum_size < self.average_size
            && self.average_size < self.maximum_size
            && self.maximum_size <= 8 * 1_024 * 1_024
    }
}

impl Default for ChunkingConfig {
    fn default() -> Self {
        Self {
            minimum_size: 64 * 1_024,
            average_size: DEFAULT_AVERAGE_CHUNK_SIZE,
            maximum_size: 1_024 * 1_024,
        }
    }
}

/// Streaming content-defined chunker with a hard bounded-memory ceiling.
pub struct ContentDefinedChunker<R> {
    reader: R,
    config: ChunkingConfig,
    read_buffer: Box<[u8; READ_BUFFER_SIZE]>,
    read_position: usize,
    read_length: usize,
    eof: bool,
}

impl<R: Read> ContentDefinedChunker<R> {
    /// Wraps a reader. At most `maximum_size + 64KiB` is resident per stream.
    #[must_use]
    pub fn new(reader: R, config: ChunkingConfig) -> Self {
        Self {
            reader,
            config,
            read_buffer: Box::new([0_u8; READ_BUFFER_SIZE]),
            read_position: 0,
            read_length: 0,
            eof: false,
        }
    }

    /// Reads the next deterministic chunk, or `None` at clean EOF.
    pub fn next_chunk(&mut self) -> io::Result<Option<Vec<u8>>> {
        if self.eof && self.read_position == self.read_length {
            return Ok(None);
        }

        let mut chunk = Vec::with_capacity(self.config.average_size);
        let mut fingerprint = 0_u64;
        let mask = (self.config.average_size - 1) as u64;

        loop {
            if self.read_position == self.read_length {
                self.refill()?;
                if self.eof && self.read_length == 0 {
                    return if chunk.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(chunk))
                    };
                }
            }

            let remaining_to_maximum = self.config.maximum_size - chunk.len();
            let available = self.read_length - self.read_position;
            let take = remaining_to_maximum.min(available);
            for (offset, byte) in self.read_buffer[self.read_position..self.read_position + take]
                .iter()
                .enumerate()
            {
                chunk.push(*byte);
                fingerprint = fingerprint.rotate_left(1) ^ gear(*byte);
                if chunk.len() >= self.config.minimum_size && fingerprint & mask == 0 {
                    self.read_position += offset + 1;
                    return Ok(Some(chunk));
                }
                if chunk.len() == self.config.maximum_size {
                    self.read_position += offset + 1;
                    return Ok(Some(chunk));
                }
            }
            self.read_position += take;
        }
    }

    fn refill(&mut self) -> io::Result<()> {
        self.read_position = 0;
        loop {
            match self.reader.read(self.read_buffer.as_mut()) {
                Ok(0) => {
                    self.read_length = 0;
                    self.eof = true;
                    return Ok(());
                }
                Ok(length) => {
                    self.read_length = length;
                    return Ok(());
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

fn gear(byte: u8) -> u64 {
    let mut value = u64::from(byte).wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn deterministic_bytes(length: usize) -> Vec<u8> {
        let mut state = 0x1234_5678_9abc_def0_u64;
        (0..length)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    fn chunk(data: &[u8], config: ChunkingConfig) -> Vec<Vec<u8>> {
        let mut chunker = ContentDefinedChunker::new(Cursor::new(data), config);
        let mut chunks = Vec::new();
        while let Some(value) = chunker.next_chunk().expect("chunk read") {
            chunks.push(value);
        }
        chunks
    }

    #[test]
    fn streaming_chunks_recombine_and_obey_bounds() {
        let config = ChunkingConfig::default();
        let input = deterministic_bytes(8 * 1_024 * 1_024 + 17);
        let chunks = chunk(&input, config);
        assert!(chunks.len() > 4);
        for value in chunks.iter().take(chunks.len() - 1) {
            assert!(value.len() >= config.minimum_size);
            assert!(value.len() <= config.maximum_size);
        }
        assert_eq!(chunks.concat(), input);
    }

    #[test]
    fn boundaries_are_content_defined_and_deterministic() {
        let config = ChunkingConfig::new(4 * 1_024, 16 * 1_024, 64 * 1_024).expect("config");
        let input = deterministic_bytes(512 * 1_024);
        let first: Vec<_> = chunk(&input, config)
            .into_iter()
            .map(|part| part.len())
            .collect();
        let second: Vec<_> = chunk(&input, config)
            .into_iter()
            .map(|part| part.len())
            .collect();
        assert_eq!(first, second);

        let mut inserted = vec![99_u8];
        inserted.extend_from_slice(&input);
        let shifted = chunk(&inserted, config);
        let original_digests: std::collections::HashSet<_> = chunk(&input, config)
            .into_iter()
            .map(|part| blake3::hash(&part))
            .collect();
        assert!(
            shifted
                .iter()
                .skip(1)
                .any(|part| original_digests.contains(&blake3::hash(part)))
        );
    }

    #[test]
    fn invalid_memory_limits_are_rejected() {
        assert!(ChunkingConfig::new(1, 2, usize::MAX).is_err());
        assert!(ChunkingConfig::new(4_096, 10_000, 20_000).is_err());
    }
}
