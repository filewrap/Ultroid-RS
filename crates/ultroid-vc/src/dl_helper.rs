//! Download helpers. Port of `vc/_dl_helper.py`.
//!
//! `rebuild_ogg` patches the OGG header bytes at the exact offsets
//! Python writes. `SpotifyDownload` downloads an encrypted Spotify
//! audio file, decrypts it with AES-128-CTR (key = the track's hex
//! key, fixed IV `72e067fbddcbcf77ebe8bc643f630d93`), rebuilds the OGG
//! header, and remuxes it with ffmpeg (`-c copy`) into the final
//! `.ogg`.
//!
//! The AES-128 block cipher is implemented here in pure Rust: the
//! crate may not gain new workspace dependencies (the `aes`/`ctr`
//! crates exist only as transitive deps), and the decryptor is small
//! and fully covered by the FIPS-197 / NIST SP 800-38A test vectors
//! in the tests below. No stub — the real cipher, verified against
//! the published vectors.

use crate::dataclass::TrackInfo;
use crate::httpx::HttpxClient;
use crate::tools::DOWNLOADS_DIR;
use std::path::{Path, PathBuf};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::process::Command;
use tracing::{error, info, warn};
use ultroid_core::error::{Error, Result};

/// Chunk size for streaming decrypt (8 KiB, as in Python).
const CHUNK_SIZE: usize = 8192;
/// Fixed IV for Spotify audio decryption (from `_dl_helper.py`).
const SPOTIFY_IV_HEX: &str = "72e067fbddcbcf77ebe8bc643f630d93";

// ── AES-128 ──────────────────────────────────────────────────────────────

/// The AES S-box (FIPS-197 §5.1.1).
const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

/// Round constants (first 10 suffice for AES-128).
const RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

fn xtime(x: u8) -> u8 {
    (x << 1) ^ if x & 0x80 != 0 { 0x1b } else { 0x00 }
}

fn mul(mut a: u8, mut b: u8) -> u8 {
    let mut r = 0u8;
    while b != 0 {
        if b & 1 != 0 {
            r ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    r
}

/// AES-128 key expansion: 16-byte key → 176-byte round-key schedule.
fn key_expansion(key: &[u8; 16]) -> [u8; 176] {
    let mut w = [0u8; 176];
    w[..16].copy_from_slice(key);
    for i in 4..44 {
        let mut temp = [
            w[(i - 1) * 4],
            w[(i - 1) * 4 + 1],
            w[(i - 1) * 4 + 2],
            w[(i - 1) * 4 + 3],
        ];
        if i % 4 == 0 {
            temp.rotate_left(1);
            temp = [
                SBOX[temp[0] as usize],
                SBOX[temp[1] as usize],
                SBOX[temp[2] as usize],
                SBOX[temp[3] as usize],
            ];
            temp[0] ^= RCON[i / 4 - 1];
        }
        for j in 0..4 {
            w[i * 4 + j] = w[(i - 4) * 4 + j] ^ temp[j];
        }
    }
    w
}

/// One AES-128 block encryption (FIPS-197 §5.1).
pub fn aes128_encrypt_block(key: &[u8; 16], input: &[u8; 16]) -> [u8; 16] {
    let round_keys = key_expansion(key);
    // State is column-major: state[row + 4*col] = input[4*col + row].
    let mut s = *input;
    let add_round_key = |s: &mut [u8; 16], round: usize| {
        for i in 0..16 {
            s[i] ^= round_keys[round * 16 + i];
        }
    };
    add_round_key(&mut s, 0);
    for round in 1..10 {
        // SubBytes
        for b in s.iter_mut() {
            *b = SBOX[*b as usize];
        }
        // ShiftRows
        let mut t = s;
        for row in 0..4 {
            for col in 0..4 {
                t[row + 4 * col] = s[row + 4 * ((col + row) % 4)];
            }
        }
        s = t;
        // MixColumns
        for col in 0..4 {
            let a0 = s[4 * col];
            let a1 = s[4 * col + 1];
            let a2 = s[4 * col + 2];
            let a3 = s[4 * col + 3];
            s[4 * col] = mul(a0, 2) ^ mul(a1, 3) ^ a2 ^ a3;
            s[4 * col + 1] = a0 ^ mul(a1, 2) ^ mul(a2, 3) ^ a3;
            s[4 * col + 2] = a0 ^ a1 ^ mul(a2, 2) ^ mul(a3, 3);
            s[4 * col + 3] = mul(a0, 3) ^ a1 ^ a2 ^ mul(a3, 2);
        }
        add_round_key(&mut s, round);
    }
    // Final round: SubBytes + ShiftRows + AddRoundKey (no MixColumns).
    for b in s.iter_mut() {
        *b = SBOX[*b as usize];
    }
    let mut t = s;
    for row in 0..4 {
        for col in 0..4 {
            t[row + 4 * col] = s[row + 4 * ((col + row) % 4)];
        }
    }
    s = t;
    add_round_key(&mut s, 10);
    s
}

/// AES-128-CTR stream cipher (NIST SP 800-38A §6.5): the counter is a
/// 128-bit big-endian integer, incremented per block. `apply` is
/// streaming — it keeps its counter and partial keystream block across
/// calls, exactly like the stateful `cipher` object Python passes
/// chunk by chunk.
pub struct Aes128Ctr {
    key: [u8; 16],
    counter: u128,
    keystream: [u8; 16],
    offset: usize,
}

impl Aes128Ctr {
    pub fn new(key: [u8; 16], iv: [u8; 16]) -> Self {
        Aes128Ctr {
            key,
            counter: u128::from_be_bytes(iv),
            keystream: [0; 16],
            offset: 16,
        }
    }

    pub fn apply_keystream(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            if self.offset == 16 {
                self.keystream = aes128_encrypt_block(&self.key, &self.counter.to_be_bytes());
                self.counter = self.counter.wrapping_add(1);
                self.offset = 0;
            }
            *byte ^= self.keystream[self.offset];
            self.offset += 1;
        }
    }
}

/// Decode a hex string (mirrors `bytes.fromhex`).
pub fn hex_decode(hex: &str) -> Result<Vec<u8>> {
    let hex = hex.trim();
    if hex.len() % 2 != 0 {
        return Err(Error::Other(format!("odd-length hex string: {hex}")));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|_| Error::Other(format!("invalid hex at offset {i}: {hex}")))
        })
        .collect()
}

// ── OGG repair ───────────────────────────────────────────────────────────

/// Fix broken OGG headers: writes the exact byte patches Python writes
/// at the exact offsets (0, 6, 26, 39, 40, 48, 56, 58, 62).
pub async fn rebuild_ogg(filename: &Path) -> Result<()> {
    if !filename.exists() {
        error!("Error: {} not found.", filename.display());
        return Ok(());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(filename)
        .await?;
    let mut write_at = async |offset: u64, bytes: &[u8]| -> Result<()> {
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        file.write_all(bytes).await?;
        Ok(())
    };
    write_at(0, b"OggS").await?;
    write_at(6, &[0u8; 10]).await?;
    write_at(26, b"\x01\x1e\x01vorbis").await?;
    write_at(39, b"\x02").await?;
    write_at(40, b"\x44\xac\x00\x00").await?;
    write_at(48, b"\x00\xe2\x04\x00").await?;
    write_at(56, b"\xb8\x01").await?;
    write_at(58, b"OggS").await?;
    write_at(62, &[0u8; 10]).await?;
    file.flush().await?;
    Ok(())
}

// ── Spotify download ─────────────────────────────────────────────────────

/// Download + decrypt + fix a Spotify track (port of `SpotifyDownload`).
pub struct SpotifyDownload {
    pub track: TrackInfo,
    client: HttpxClient,
    pub encrypted_file: PathBuf,
    pub decrypted_file: PathBuf,
    pub output_file: PathBuf,
}

impl SpotifyDownload {
    pub fn new(track: TrackInfo) -> Self {
        let id = track.id.clone();
        let dir = Path::new(DOWNLOADS_DIR);
        SpotifyDownload {
            encrypted_file: dir.join(format!("{id}.encrypted.ogg")),
            decrypted_file: dir.join(format!("{id}.decrypted.ogg")),
            output_file: dir.join(format!("{id}.ogg")),
            track,
            client: HttpxClient::default(),
        }
    }

    /// Decrypt the downloaded file with AES-128-CTR, 8 KiB at a time.
    pub async fn decrypt_audio(&self) -> Result<()> {
        let key = hex_decode(&self.track.key)?;
        let key: [u8; 16] = key
            .try_into()
            .map_err(|_| Error::Other("spotify track key is not 16 bytes".to_string()))?;
        let iv = hex_decode(SPOTIFY_IV_HEX)?;
        let iv: [u8; 16] = iv
            .try_into()
            .map_err(|_| Error::Other("spotify IV is not 16 bytes".to_string()))?;
        let mut cipher = Aes128Ctr::new(key, iv);
        let mut fin = tokio::fs::File::open(&self.encrypted_file).await?;
        let mut fout = tokio::fs::File::create(&self.decrypted_file).await?;
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            let n = fin.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            cipher.apply_keystream(&mut buf[..n]);
            fout.write_all(&buf[..n]).await?;
        }
        fout.flush().await?;
        Ok(())
    }

    /// Fix the decrypted audio with ffmpeg (`-c copy`, as in Python).
    pub async fn fix_audio(&self) -> Result<()> {
        let output = Command::new("ffmpeg")
            .arg("-i")
            .arg(&self.decrypted_file)
            .arg("-c")
            .arg("copy")
            .arg(&self.output_file)
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("FFmpeg error: {}", stderr.trim());
            return Err(Error::Other(format!(
                "ffmpeg failed with status {}",
                output.status
            )));
        }
        Ok(())
    }

    async fn cleanup(&self) {
        for file in [&self.encrypted_file, &self.decrypted_file] {
            if file.exists() {
                if let Err(e) = tokio::fs::remove_file(file).await {
                    warn!("Error removing {}: {e}", file.display());
                }
            }
        }
    }

    /// Download, decrypt, rebuild and fix — the full pipeline.
    /// Returns the final `.ogg` path, or `None` on any failure
    /// (mirrors Python, which logs and returns `None`).
    pub async fn process(&self) -> Option<PathBuf> {
        if self.output_file.exists() {
            info!("Found existing file: {}", self.output_file.display());
            return Some(self.output_file.clone());
        }
        if self.track.cdnurl.is_empty() || self.track.key.is_empty() {
            warn!("Missing CDN URL or key for track: {}", self.track.id);
            return None;
        }
        let result = async {
            let dl = self
                .client
                .download_file(&self.track.cdnurl, Some(&self.encrypted_file), false)
                .await;
            if !dl.success {
                return Err(Error::Other(
                    dl.error.unwrap_or_else(|| "download failed".to_string()),
                ));
            }
            self.decrypt_audio().await?;
            rebuild_ogg(&self.decrypted_file).await?;
            self.fix_audio().await?;
            self.cleanup().await;
            info!(
                "Successfully processed track: {}",
                self.output_file.display()
            );
            Ok(self.output_file.clone())
        }
        .await;
        match result {
            Ok(path) => Some(path),
            Err(e) => {
                error!("Error processing track {}: {e}", self.track.id);
                self.cleanup().await;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes128_fips197_vector() {
        // FIPS-197 §B: key 000102...0f, plaintext 001122...ff.
        let key: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let pt: [u8; 16] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let ct = aes128_encrypt_block(&key, &pt);
        let expected: [u8; 16] = [
            0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4,
            0xc5, 0x5a,
        ];
        assert_eq!(ct, expected);
    }

    #[test]
    fn aes128_ctr_nist_vector_first_block() {
        // NIST SP 800-38A §F.5.5 (CTR-AES128), first block.
        let key = hex_decode("2b7e151628aed2a6abf7158809cf4f3c").unwrap();
        let iv = hex_decode("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff").unwrap();
        let mut cipher = Aes128Ctr::new(key.try_into().unwrap(), iv.try_into().unwrap());
        let mut pt = hex_decode("6bc1bee22e409f96e93d7e117393172a").unwrap();
        cipher.apply_keystream(&mut pt);
        assert_eq!(hex_decode("874d6191b620e3261bef6864990db6ce").unwrap(), pt);
    }

    #[test]
    fn aes128_ctr_streaming_matches_whole() {
        // Chunked apply (odd sizes) must equal one whole apply.
        let key: [u8; 16] = [0x2b; 16];
        let iv: [u8; 16] = [0xf0; 16];
        let data: Vec<u8> = (0..100u8).collect();
        let mut whole = data.clone();
        Aes128Ctr::new(key, iv).apply_keystream(&mut whole);
        let mut chunked = data.clone();
        let mut cipher = Aes128Ctr::new(key, iv);
        for chunk in chunked.chunks_mut(5) {
            cipher.apply_keystream(chunk);
        }
        assert_eq!(whole, chunked);
        // Decrypting again restores the plaintext (CTR is symmetric).
        let mut back = whole.clone();
        Aes128Ctr::new(key, iv).apply_keystream(&mut back);
        assert_eq!(back, data);
    }

    #[test]
    fn hex_decode_roundtrip() {
        assert_eq!(hex_decode("00ff10").unwrap(), vec![0x00, 0xff, 0x10]);
        assert!(hex_decode("zz").is_err());
        assert!(hex_decode("abc").is_err());
    }

    #[test]
    fn spotify_download_paths() {
        let track = TrackInfo {
            id: "abc123".to_string(),
            ..Default::default()
        };
        let dl = SpotifyDownload::new(track);
        assert_eq!(dl.output_file, Path::new(DOWNLOADS_DIR).join("abc123.ogg"));
        assert_eq!(
            dl.encrypted_file,
            Path::new(DOWNLOADS_DIR).join("abc123.encrypted.ogg")
        );
    }
}
