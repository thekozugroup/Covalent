//! First-run ownership claim for a headless node the operator cannot log into.
//!
//! # Why this exists
//!
//! A container has no keyboard and no window. Before this module, taking
//! ownership of an Unraid deployment meant opening a shell inside a running
//! container, `cat`-ing a bearer token out of `/data/local-api-token`, copying a
//! root certificate out of `/config/caddy/...`, and verifying that certificate
//! against the host by hand. Four terminal steps, on the platform this product
//! calls Tier 1, for a person whose only tool is a web browser.
//!
//! The node already knows both secrets. It only lacked a way to hand them over.
//! This module is that handoff: at first start the node mints a short code,
//! prints it to stdout — which the Unraid log viewer already shows, with no
//! shell — and serves exactly one exchange that trades proof of that code for
//! the API token and the CA certificate to pin.
//!
//! # The code is a credential, so it is treated as one
//!
//! Printing a secret to a log is not free, and the reasoning is written down
//! here rather than assumed.
//!
//! **Who can read the log.** On Unraid, the container log is reachable through
//! the Unraid web interface, the Docker socket, or the host filesystem. Every
//! one of those is already root-equivalent on that machine, and every one of
//! them can read `/data/local-api-token` directly — which is exactly what the
//! old instructions told the operator to do. So the code discloses nothing to a
//! log reader that a log reader did not already have. It is strictly weaker
//! than the access required to observe it, which is the property that makes
//! this safe to print at all.
//!
//! **What a LAN attacker can do with an observed code.** They cannot observe
//! it: the code is never transmitted. The client proves knowledge of it with a
//! MAC over a nonce, and the node's reply is sealed to a key derived from it.
//! An attacker who is merely on the network sees neither the code nor the
//! token.
//!
//! **An attacker on the path, terminating TLS.** This is the real threat, and
//! the reason the exchange is shaped the way it is. The node's TLS is served by
//! a same-container proxy using a CA the client has not yet trusted, so the very
//! first connection is unauthenticated by construction. A machine-in-the-middle
//! can therefore relay the whole exchange. It gains: the client's proof (a MAC
//! over a nonce), the CA certificate (public), and a sealed token it cannot
//! open. It does not gain the token, because the token is sealed under
//! [`seal_key`] and the CA's digest is bound in as associated data — swap the
//! CA and decryption fails. The legitimate client decrypts, pins the CA it was
//! handed, and every subsequent connection is validated against it, at which
//! point the attacker is locked out. A relay can still *drop* the exchange, but
//! dropping traffic is available to any on-path attacker regardless and is
//! plain denial of service, not disclosure.
//!
//! **Offline attack on the code.** Both the client proof and the sealed token
//! are oracles: an attacker holding either can test candidate codes offline. The
//! code carries [`CLAIM_CODE_ENTROPY_BITS`] bits, which is transcribable but not
//! by itself beyond a determined adversary inside the window. So the key is
//! stretched by [`CLAIM_KEY_STRETCH_ROUNDS`] sequential BLAKE3 compressions
//! before use. The node pays that cost exactly once, at mint, because it knows
//! the code; an attacker pays it per guess. That asymmetry puts an offline
//! search far beyond the code's lifetime. See [`stretch_claim_code`].
//!
//! **Online guessing.** Attempts are serialised by
//! [`MIN_CLAIM_ATTEMPT_INTERVAL_MS`] and capped by [`MAX_CLAIM_FAILURES`], after
//! which the window closes permanently for this process. Verification itself is
//! a single keyed hash, so the endpoint is not a CPU amplifier.
//!
//! **If the code is never claimed.** The window expires after
//! [`CLAIM_WINDOW_MS`] and the node keeps running, unclaimed and unreachable by
//! this route. Restarting the container mints a fresh code. The code is held in
//! memory only and never written to disk, so a restart is genuinely a new
//! secret rather than a redisplay of an old one — which is also why an expired
//! or exhausted window has a recovery path that does not require a shell.
//!
//! **After a successful claim** the window is closed for good: a durable marker
//! records that this node has an owner, and later starts mint nothing. A second
//! presentation of the same code is refused whether or not the process
//! restarted.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use blake3::Hasher;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use covalent_core::CoreError;
use rand_core::{OsRng, RngCore};
use zeroize::{Zeroize, Zeroizing};

/// Transcription alphabet, in Crockford Base32 order.
///
/// `I`, `L`, `O` and `U` are absent: the first three because they are read back
/// as `1`, `1` and `0`, and `U` because excluding it keeps accidental profanity
/// out of a code a person has to read aloud over the phone. Input is normalised
/// through [`normalise_claim_code`], so someone who types `O` for `0` or `l`
/// for `1` is still admitted rather than silently refused.
const CLAIM_CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Characters in a minted code, excluding the grouping separator.
const CLAIM_CODE_LENGTH: usize = 10;

/// Entropy in one minted code: ten symbols drawn uniformly from 32.
pub const CLAIM_CODE_ENTROPY_BITS: u32 = 50;

/// How long a minted code stays presentable.
///
/// Long enough that an operator can install the template, read the log, and
/// reach the console without racing a timer; short enough that a code sitting
/// in a log the operator never read is not a standing credential.
pub const CLAIM_WINDOW_MS: u64 = 30 * 60 * 1_000;

/// Failed presentations tolerated before the window closes for good.
///
/// Far above a mistyped code, far below anything that assists a search of a
/// 50-bit space. Closing rather than merely throttling is the fail-closed
/// direction; the recovery is a container restart, which mints a new code.
pub const MAX_CLAIM_FAILURES: u32 = 16;

/// Floor on the spacing between presentations, whatever their outcome.
pub const MIN_CLAIM_ATTEMPT_INTERVAL_MS: u64 = 500;

/// Sequential BLAKE3 compressions folded into the claim key before use.
///
/// This is a deliberate work factor, not a hash of convenience. The node pays
/// it once per boot, at mint, because it is the only party that starts from the
/// code itself. An attacker holding a captured proof or sealed token pays it for
/// every candidate code, turning a 2^50 search into roughly 2^68 compressions —
/// beyond reach inside the thirty-minute window the code is alive for.
pub const CLAIM_KEY_STRETCH_ROUNDS: u32 = 1 << 18;

/// Bytes of client-chosen nonce required in a presentation.
pub const CLAIM_NONCE_BYTES: usize = 32;

const STRETCH_CONTEXT: &str = "covalent/first-run-claim/stretch/v1";
const STRETCH_STEP_DOMAIN: &[u8] = b"covalent/first-run-claim/stretch-step/v1";
const CLIENT_PROOF_DOMAIN: &[u8] = b"covalent/first-run-claim/client-proof/v1";
const SEAL_CONTEXT: &str = "covalent/first-run-claim/seal/v1";
const SEAL_AAD_DOMAIN: &[u8] = b"covalent/first-run-claim/seal-aad/v1";

/// Why a presentation of a claim code was refused.
///
/// Carried out of the state machine so the HTTP layer can map each case onto a
/// distinct status without the state machine knowing what HTTP is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimRefusal {
    /// This node already has an owner. Terminal, and durable across restarts.
    AlreadyClaimed,
    /// The window closed: the code expired, or too many codes were wrong.
    WindowClosed(ClaimClosure),
    /// Presented sooner than [`MIN_CLAIM_ATTEMPT_INTERVAL_MS`] after the last.
    TooSoon,
    /// The proof did not verify under the minted code.
    IncorrectCode,
    /// The presentation was malformed — wrong nonce or proof length.
    Malformed,
    /// The node could not read the certificate it must hand over. Checked
    /// before the code is spent, so this never costs the operator their code.
    CertificateUnavailable,
}

/// The reason an open window stopped accepting presentations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimClosure {
    /// [`CLAIM_WINDOW_MS`] elapsed with no successful claim.
    Expired,
    /// [`MAX_CLAIM_FAILURES`] wrong codes were presented.
    Exhausted,
}

impl fmt::Display for ClaimRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::AlreadyClaimed => "this node already has an owner",
            Self::WindowClosed(ClaimClosure::Expired) => "the setup code expired",
            Self::WindowClosed(ClaimClosure::Exhausted) => {
                "too many incorrect setup codes were presented"
            }
            Self::TooSoon => "setup codes were presented too quickly",
            Self::IncorrectCode => "the setup code was incorrect",
            Self::Malformed => "the setup request was malformed",
            Self::CertificateUnavailable => "the node's certificate is not available yet",
        };
        formatter.write_str(text)
    }
}

/// A minted first-run code, held only in memory and never written to disk.
///
/// `Debug` is permanently redacted and the type implements neither `Clone` nor
/// `Serialize`, so a code cannot reach a structured log or a state file by
/// accident. The only intended disclosure is [`Self::grouped`], called once by
/// the startup banner.
pub struct ClaimCode(Zeroizing<String>);

impl ClaimCode {
    /// Draws a fresh code from the operating system's entropy source.
    ///
    /// Rejection sampling is unnecessary: the alphabet is exactly 32 symbols, so
    /// masking five bits off a random byte is already uniform.
    #[must_use]
    pub fn mint() -> Self {
        let mut random = [0_u8; CLAIM_CODE_LENGTH];
        OsRng.fill_bytes(&mut random);
        let mut code = String::with_capacity(CLAIM_CODE_LENGTH);
        for byte in random {
            let index = usize::from(byte & 0b0001_1111);
            code.push(char::from(CLAIM_CODE_ALPHABET[index]));
        }
        random.zeroize();
        Self(Zeroizing::new(code))
    }

    /// Rebuilds a code from known characters. Test and client-side use only.
    #[must_use]
    pub fn from_normalised(code: &str) -> Self {
        Self(Zeroizing::new(code.to_owned()))
    }

    /// The code as a person reads it, split into two groups of five.
    ///
    /// Grouping is transcription ergonomics only; the separator is discarded on
    /// the way back in by [`normalise_claim_code`].
    #[must_use]
    pub fn grouped(&self) -> String {
        let (left, right) = self.0.split_at(CLAIM_CODE_LENGTH / 2);
        format!("{left}-{right}")
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ClaimCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ClaimCode([REDACTED])")
    }
}

/// Folds a typed code back to the exact characters that were minted.
///
/// Case is lifted, separators and whitespace are dropped, and the three
/// Crockford confusions are resolved the way a reader would resolve them. A
/// string that still contains anything outside the alphabet is rejected rather
/// than coerced, so a wrong code fails as a wrong code rather than as a
/// silently different one.
#[must_use]
pub fn normalise_claim_code(supplied: &str) -> Option<Zeroizing<String>> {
    // Bounded before iteration so an oversized body cannot be walked at all.
    if supplied.len() > 64 {
        return None;
    }
    let mut normalised = Zeroizing::new(String::with_capacity(CLAIM_CODE_LENGTH));
    for character in supplied.chars() {
        let folded = match character.to_ascii_uppercase() {
            '-' | ' ' | '\t' => continue,
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        };
        if !CLAIM_CODE_ALPHABET.contains(&u8::try_from(folded).ok()?) {
            return None;
        }
        normalised.push(folded);
    }
    (normalised.len() == CLAIM_CODE_LENGTH).then_some(normalised)
}

/// Derives the claim key by folding the code through a long sequential chain.
///
/// The chain is intentionally serial: each round consumes the previous digest,
/// so a candidate cannot be tested faster by splitting the work. Parallelism
/// across *candidates* is still available to an attacker, which is why the round
/// count is chosen against the whole 2^50 space rather than against one guess.
#[must_use]
pub fn stretch_claim_code(code: &str) -> Zeroizing<[u8; 32]> {
    let mut digest = Zeroizing::new(blake3::derive_key(STRETCH_CONTEXT, code.as_bytes()));
    for _ in 0..CLAIM_KEY_STRETCH_ROUNDS {
        let next = blake3::keyed_hash(&digest, STRETCH_STEP_DOMAIN);
        *digest = *next.as_bytes();
    }
    digest
}

/// The MAC a client presents to prove it holds the code without disclosing it.
#[must_use]
pub fn client_proof(claim_key: &[u8; 32], client_nonce: &[u8]) -> [u8; 32] {
    let mut hasher = Hasher::new_keyed(claim_key);
    hasher.update(CLIENT_PROOF_DOMAIN);
    hasher.update(client_nonce);
    *hasher.finalize().as_bytes()
}

/// Derives the one-shot key the API token is sealed under.
#[must_use]
pub fn seal_key(claim_key: &[u8; 32], client_nonce: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut material = Zeroizing::new(Vec::with_capacity(32 + client_nonce.len()));
    material.extend_from_slice(claim_key);
    material.extend_from_slice(client_nonce);
    Zeroizing::new(blake3::derive_key(SEAL_CONTEXT, &material))
}

/// Binds the delivered CA to the sealed token.
///
/// The CA digest travels as associated data rather than as plaintext, so a relay
/// that substitutes its own certificate produces a ciphertext that will not
/// open. Successful decryption is therefore simultaneously proof that the
/// responder held the code and proof that the CA came from that same responder —
/// which is the entire verification the operator used to perform by hand.
fn seal_aad(client_nonce: &[u8], ca_digest: &[u8; 32]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(SEAL_AAD_DOMAIN.len() + client_nonce.len() + 32);
    aad.extend_from_slice(SEAL_AAD_DOMAIN);
    aad.extend_from_slice(client_nonce);
    aad.extend_from_slice(ca_digest);
    aad
}

/// Seals the API token for exactly one client presentation.
pub fn seal_token(
    claim_key: &[u8; 32],
    client_nonce: &[u8],
    ca_digest: &[u8; 32],
    token: &str,
) -> Result<(Vec<u8>, Vec<u8>), CoreError> {
    let key = seal_key(claim_key, client_nonce);
    let mut nonce_bytes = [0_u8; 24];
    OsRng.fill_bytes(&mut nonce_bytes);
    let aad = seal_aad(client_nonce, ca_digest);
    let ciphertext = XChaCha20Poly1305::new(Key::from_slice(key.as_ref()))
        .encrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload {
                msg: token.as_bytes(),
                aad: &aad,
            },
        )
        .map_err(|_| CoreError::InvalidState("seal first-run claim token".to_owned()))?;
    Ok((nonce_bytes.to_vec(), ciphertext))
}

/// Opens a sealed token. The client half of the exchange, kept here so the
/// property the client relies on is tested against the exact bytes the node
/// produces rather than against a reimplementation of them.
pub fn open_sealed_token(
    claim_key: &[u8; 32],
    client_nonce: &[u8],
    ca_digest: &[u8; 32],
    seal_nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<String>, CoreError> {
    if seal_nonce.len() != 24 {
        return Err(CoreError::InvalidKeyMaterial);
    }
    let key = seal_key(claim_key, client_nonce);
    let aad = seal_aad(client_nonce, ca_digest);
    let plaintext = XChaCha20Poly1305::new(Key::from_slice(key.as_ref()))
        .decrypt(
            XNonce::from_slice(seal_nonce),
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| CoreError::AuthenticationFailed)?;
    let token = String::from_utf8(plaintext).map_err(|_| CoreError::InvalidKeyMaterial)?;
    Ok(Zeroizing::new(token))
}

/// Constant-time comparison over two equal-length digests.
fn digests_equal(left: &[u8], right: &[u8; 32]) -> bool {
    let mut difference = u8::from(left.len() != right.len());
    for (index, expected) in right.iter().enumerate() {
        difference |= left.get(index).copied().unwrap_or(0) ^ expected;
    }
    difference == 0
}

/// The lifecycle of one node's first-run window.
///
/// Deliberately holds no clock and performs no I/O: every transition takes an
/// explicit `now_unix_ms`, which is what makes expiry, rate limiting and
/// exhaustion testable as arithmetic rather than as a sleep.
#[derive(Debug)]
pub enum ClaimWindow {
    /// A code is minted and presentations are being accepted.
    Open {
        /// The stretched code. The code itself is held by the caller for
        /// display and dropped as soon as the banner is printed.
        key: Zeroizing<[u8; 32]>,
        expires_at_unix_ms: u64,
        failures: u32,
        last_attempt_unix_ms: Option<u64>,
    },
    /// A claim succeeded. Terminal.
    Claimed,
    /// The window ended without a claim. Terminal for this process.
    Closed(ClaimClosure),
}

impl ClaimWindow {
    /// Opens a window around an already-minted code.
    #[must_use]
    pub fn open(code: &ClaimCode, now_unix_ms: u64) -> Self {
        Self::Open {
            key: stretch_claim_code(code.as_str()),
            expires_at_unix_ms: now_unix_ms.saturating_add(CLAIM_WINDOW_MS),
            failures: 0,
            last_attempt_unix_ms: None,
        }
    }

    /// True once no presentation can ever succeed again.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Claimed | Self::Closed(_))
    }

    /// Verifies one presentation and, on success, consumes the window.
    ///
    /// Ordering matters and is not incidental. Expiry is evaluated before the
    /// proof, so a stale code is refused as stale rather than leaking whether it
    /// was also correct. Spacing is evaluated before the proof so a caller
    /// cannot use the endpoint as a fast oracle. The failure counter is charged
    /// only for a genuinely wrong code — a malformed body or a too-fast retry
    /// does not burn the operator's budget, or an attacker could close the
    /// window with garbage and force a restart at will.
    pub fn present(
        &mut self,
        client_nonce: &[u8],
        proof: &[u8],
        now_unix_ms: u64,
    ) -> Result<Zeroizing<[u8; 32]>, ClaimRefusal> {
        match self {
            Self::Claimed => return Err(ClaimRefusal::AlreadyClaimed),
            Self::Closed(closure) => return Err(ClaimRefusal::WindowClosed(*closure)),
            Self::Open { .. } => {}
        }

        let Self::Open {
            key,
            expires_at_unix_ms,
            failures,
            last_attempt_unix_ms,
        } = self
        else {
            unreachable!("the terminal states returned above")
        };

        if now_unix_ms >= *expires_at_unix_ms {
            *self = Self::Closed(ClaimClosure::Expired);
            return Err(ClaimRefusal::WindowClosed(ClaimClosure::Expired));
        }
        if let Some(previous) = *last_attempt_unix_ms
            && now_unix_ms.saturating_sub(previous) < MIN_CLAIM_ATTEMPT_INTERVAL_MS
        {
            return Err(ClaimRefusal::TooSoon);
        }
        if client_nonce.len() != CLAIM_NONCE_BYTES || proof.len() != 32 {
            return Err(ClaimRefusal::Malformed);
        }
        *last_attempt_unix_ms = Some(now_unix_ms);

        let expected = client_proof(key, client_nonce);
        if !digests_equal(proof, &expected) {
            *failures += 1;
            if *failures >= MAX_CLAIM_FAILURES {
                *self = Self::Closed(ClaimClosure::Exhausted);
                return Err(ClaimRefusal::WindowClosed(ClaimClosure::Exhausted));
            }
            return Err(ClaimRefusal::IncorrectCode);
        }

        let key = key.clone();
        *self = Self::Claimed;
        Ok(key)
    }
}

/// Everything one claim exchange hands the client.
pub struct ClaimGrant {
    /// The one-shot sealing nonce for [`Self::sealed_token`].
    pub seal_nonce: Vec<u8>,
    /// The API token, sealed so an on-path relay cannot read it.
    pub sealed_token: Vec<u8>,
    /// The CA to pin, in PEM, when this deployment terminates TLS in a proxy.
    pub ca_certificate: Option<String>,
    /// Hex SHA-256 of the CA's DER, for display and for out-of-band checking.
    pub ca_fingerprint: Option<String>,
}

/// Durable record that this node has an owner.
#[must_use]
pub fn owner_marker_path(data_directory: &Path) -> PathBuf {
    data_directory.join("owner-claimed")
}

/// True when a previous run recorded an owner.
///
/// Errors read as "claimed": an unreadable marker must never reopen a claim
/// window, because the safe direction is to refuse a second owner rather than
/// to admit one.
#[must_use]
pub fn is_claimed(marker_path: &Path) -> bool {
    !matches!(
        std::fs::symlink_metadata(marker_path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

/// Records an owner. Written with the same private permissions as node state.
pub fn mark_claimed(marker_path: &Path, now_unix_ms: u64) -> Result<(), CoreError> {
    let record = format!("{{\"schemaVersion\":1,\"claimedAtUnixMs\":{now_unix_ms}}}\n");
    crate::persist_private_file(marker_path, record.as_bytes())
}

/// Reads the CA certificate this deployment wants clients to pin.
///
/// Bounded, symlink-refusing, and validated as a real certificate rather than
/// echoed back as bytes: handing a client a malformed CA would leave it unable
/// to pin and, depending on the client, tempted to proceed without pinning.
fn read_ca_certificate(path: &Path) -> Result<(String, [u8; 32]), CoreError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| CoreError::Io {
        operation: "inspect TLS CA certificate",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 64 * 1_024 {
        return Err(CoreError::InvalidState(
            "invalid TLS CA certificate file".to_owned(),
        ));
    }
    let pem = std::fs::read_to_string(path).map_err(|source| CoreError::Io {
        operation: "read TLS CA certificate",
        path: path.to_path_buf(),
        source,
    })?;
    let der = decode_certificate_pem(&pem)
        .ok_or_else(|| CoreError::InvalidState("malformed TLS CA certificate".to_owned()))?;
    let digest: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(&der).into();
    Ok((pem, digest))
}

/// Extracts the DER of the first certificate in a PEM document.
fn decode_certificate_pem(pem: &str) -> Option<Vec<u8>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let start = pem.find(BEGIN)? + BEGIN.len();
    let end = pem[start..].find(END)? + start;
    let body: String = pem[start..end]
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let der = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, body).ok()?;
    (!der.is_empty()).then_some(der)
}

/// The node-side owner of one first-run window, with its durable marker.
pub struct FirstRunClaim {
    window: Mutex<ClaimWindow>,
    marker_path: PathBuf,
    ca_certificate_path: Option<PathBuf>,
}

impl FirstRunClaim {
    /// Opens a window around a freshly minted code.
    #[must_use]
    pub fn new(
        code: &ClaimCode,
        marker_path: PathBuf,
        ca_certificate_path: Option<PathBuf>,
        now_unix_ms: u64,
    ) -> Self {
        Self {
            window: Mutex::new(ClaimWindow::open(code, now_unix_ms)),
            marker_path,
            ca_certificate_path,
        }
    }

    /// Verifies one presentation and, on success, hands over the sealed token.
    ///
    /// The CA is read *before* the window is consumed, so a deployment whose
    /// certificate is missing refuses without burning the operator's single-use
    /// code. Order matters here: the opposite order would leave a user holding a
    /// spent code and no token.
    pub fn present(
        &self,
        client_nonce: &[u8],
        proof: &[u8],
        api_token: &str,
        now_unix_ms: u64,
    ) -> Result<ClaimGrant, ClaimRefusal> {
        let certificate = match self.ca_certificate_path.as_deref() {
            Some(path) => match read_ca_certificate(path) {
                Ok(certificate) => Some(certificate),
                Err(_) => return Err(ClaimRefusal::CertificateUnavailable),
            },
            None => None,
        };
        let (ca_certificate, ca_digest) = match certificate {
            Some((pem, digest)) => (Some(pem), digest),
            // A node with no proxy still seals against a fixed digest, so the
            // sealing path is identical whether or not a CA is delivered.
            None => (None, [0_u8; 32]),
        };

        let claim_key = {
            let mut window = self
                .window
                .lock()
                .map_err(|_| ClaimRefusal::CertificateUnavailable)?;
            window.present(client_nonce, proof, now_unix_ms)?
        };

        let (seal_nonce, sealed_token) =
            seal_token(&claim_key, client_nonce, &ca_digest, api_token)
                .map_err(|_| ClaimRefusal::CertificateUnavailable)?;

        // The window is already spent in memory, so a failure here must not
        // deny the caller the token it just earned. A missing marker means the
        // next start offers a fresh code, which is the recovery an operator
        // wants if this response never reached them.
        if let Err(error) = mark_claimed(&self.marker_path, now_unix_ms) {
            tracing::error!(
                %error,
                "claim succeeded but the durable owner marker could not be written; \
                 restarting this container will offer a new setup code"
            );
        }

        Ok(ClaimGrant {
            seal_nonce,
            sealed_token,
            ca_fingerprint: ca_certificate
                .as_ref()
                .map(|_| ca_digest.iter().map(|byte| format!("{byte:02x}")).collect()),
            ca_certificate,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_700_000_000_000;

    fn presentation(code: &ClaimCode) -> ([u8; CLAIM_NONCE_BYTES], [u8; 32]) {
        let mut nonce = [0_u8; CLAIM_NONCE_BYTES];
        OsRng.fill_bytes(&mut nonce);
        let key = stretch_claim_code(code.as_str());
        let proof = client_proof(&key, &nonce);
        (nonce, proof)
    }

    #[test]
    fn a_minted_code_is_transcribable_and_carries_the_entropy_it_claims() {
        let code = ClaimCode::mint();
        let grouped = code.grouped();
        assert_eq!(grouped.len(), CLAIM_CODE_LENGTH + 1);
        assert_eq!(grouped.chars().filter(|&c| c == '-').count(), 1);
        for character in grouped.chars().filter(|&c| c != '-') {
            assert!(
                CLAIM_CODE_ALPHABET.contains(&(character as u8)),
                "{character} is outside the transcription alphabet"
            );
        }
        // Ten symbols over a 32-symbol alphabet is exactly fifty bits, and the
        // stretch factor is chosen against that number.
        assert_eq!(
            CLAIM_CODE_ENTROPY_BITS,
            u32::try_from(CLAIM_CODE_LENGTH).expect("length") * 5
        );

        // Distinctness is not proof of uniformity, but a collision across a
        // small sample would prove the draw is broken.
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..64 {
            assert!(seen.insert(ClaimCode::mint().grouped()));
        }
    }

    #[test]
    fn transcription_confusions_normalise_but_foreign_characters_do_not() {
        let code = ClaimCode::from_normalised("0123456789");
        // `U` is the one letter that is neither minted nor folded, so it is the
        // sharpest test that a foreign character fails rather than being
        // coerced into a neighbouring symbol.
        assert_eq!(
            normalise_claim_code("oIu3456789").as_deref(),
            None,
            "U is outside the alphabet and must not be coerced into something else"
        );
        assert_eq!(
            normalise_claim_code("o1@3456789").as_deref(),
            None,
            "a foreign character fails the whole code"
        );
        // Guarding the guard: `Z` really is in the alphabet, so a test that used
        // it as the foreign character would assert nothing.
        assert_eq!(
            normalise_claim_code("01z3456789")
                .as_deref()
                .map(String::as_str),
            Some("01Z3456789"),
            "Z is a minted symbol and must survive normalisation"
        );
        assert_eq!(
            normalise_claim_code("OIL3456789")
                .as_deref()
                .map(String::as_str),
            Some("0113456789"),
            "Crockford confusions fold the way a reader resolves them"
        );
        assert_eq!(
            normalise_claim_code(" 01234-56789 ")
                .as_deref()
                .map(String::as_str),
            Some(code.as_str()),
            "grouping and stray whitespace are presentation only"
        );
        assert_eq!(normalise_claim_code("0123456789A").as_deref(), None);
        assert_eq!(normalise_claim_code("012345678").as_deref(), None);
        assert_eq!(
            normalise_claim_code(&"0".repeat(1_000)).as_deref(),
            None,
            "an oversized body is refused before it is walked"
        );
    }

    #[test]
    fn a_correct_presentation_claims_the_window_exactly_once() {
        let code = ClaimCode::mint();
        let mut window = ClaimWindow::open(&code, NOW);
        let (nonce, proof) = presentation(&code);

        let key = window
            .present(&nonce, &proof, NOW)
            .expect("the minted code claims the window");
        assert!(window.is_terminal());

        // Single use, proven with the exact bytes that just worked.
        assert_eq!(
            window.present(&nonce, &proof, NOW + 1_000),
            Err(ClaimRefusal::AlreadyClaimed),
            "replaying a successful presentation must be refused"
        );

        // A fresh presentation of the same code is refused too, so single-use is
        // a property of the window and not merely of the nonce.
        let (other_nonce, other_proof) = presentation(&code);
        assert_eq!(
            window.present(&other_nonce, &other_proof, NOW + 2_000),
            Err(ClaimRefusal::AlreadyClaimed)
        );

        // The key handed back is the one the token is sealed under.
        assert_eq!(*key, *stretch_claim_code(code.as_str()));
    }

    #[test]
    fn an_expired_window_refuses_the_correct_code() {
        let code = ClaimCode::mint();
        let mut window = ClaimWindow::open(&code, NOW);
        let (nonce, proof) = presentation(&code);

        // One millisecond inside the window still works, which is what makes
        // the boundary assertion below meaningful rather than vacuous.
        let mut alive = ClaimWindow::open(&code, NOW);
        alive
            .present(&nonce, &proof, NOW + CLAIM_WINDOW_MS - 1)
            .expect("the last millisecond of the window is still open");

        assert_eq!(
            window.present(&nonce, &proof, NOW + CLAIM_WINDOW_MS),
            Err(ClaimRefusal::WindowClosed(ClaimClosure::Expired)),
            "expiry is evaluated before the proof, so a stale correct code is still refused"
        );
        assert!(window.is_terminal());

        // Expiry is terminal: the clock moving back does not reopen it.
        assert_eq!(
            window.present(&nonce, &proof, NOW),
            Err(ClaimRefusal::WindowClosed(ClaimClosure::Expired)),
            "a backwards clock must not reopen an expired window"
        );
    }

    #[test]
    fn wrong_codes_are_spaced_then_exhaust_the_window() {
        let code = ClaimCode::mint();
        let mut window = ClaimWindow::open(&code, NOW);
        let (nonce, _) = presentation(&code);
        let wrong = [0_u8; 32];

        assert_eq!(
            window.present(&nonce, &wrong, NOW),
            Err(ClaimRefusal::IncorrectCode)
        );
        // Immediately again: refused on spacing, and crucially not counted as a
        // failure, or an attacker could exhaust the budget for free.
        assert_eq!(
            window.present(&nonce, &wrong, NOW + MIN_CLAIM_ATTEMPT_INTERVAL_MS - 1),
            Err(ClaimRefusal::TooSoon)
        );

        let mut at = NOW;
        for attempt in 1..MAX_CLAIM_FAILURES {
            at += MIN_CLAIM_ATTEMPT_INTERVAL_MS;
            let refusal = window.present(&nonce, &wrong, at);
            if attempt + 1 == MAX_CLAIM_FAILURES {
                assert_eq!(
                    refusal,
                    Err(ClaimRefusal::WindowClosed(ClaimClosure::Exhausted)),
                    "the budget closes the window rather than merely throttling"
                );
            } else {
                assert_eq!(refusal, Err(ClaimRefusal::IncorrectCode));
            }
        }
        assert!(window.is_terminal());

        // And the real code no longer helps, which is the point of closing.
        let (fresh_nonce, fresh_proof) = presentation(&code);
        assert_eq!(
            window.present(&fresh_nonce, &fresh_proof, at + 10_000),
            Err(ClaimRefusal::WindowClosed(ClaimClosure::Exhausted)),
            "an exhausted window refuses the correct code too"
        );
    }

    #[test]
    fn a_malformed_presentation_never_spends_the_failure_budget() {
        let code = ClaimCode::mint();
        let mut window = ClaimWindow::open(&code, NOW);

        let mut at = NOW;
        for _ in 0..MAX_CLAIM_FAILURES * 4 {
            at += MIN_CLAIM_ATTEMPT_INTERVAL_MS;
            assert_eq!(
                window.present(b"short", &[0_u8; 32], at),
                Err(ClaimRefusal::Malformed)
            );
            assert_eq!(
                window.present(&[0_u8; CLAIM_NONCE_BYTES], b"short", at),
                Err(ClaimRefusal::Malformed)
            );
        }
        assert!(
            !window.is_terminal(),
            "garbage must not let an attacker close the operator's window"
        );

        let (nonce, proof) = presentation(&code);
        window
            .present(&nonce, &proof, at + MIN_CLAIM_ATTEMPT_INTERVAL_MS)
            .expect("the window survived the garbage and still claims");
    }

    #[test]
    fn a_sealed_token_opens_only_under_the_right_code_and_the_delivered_ca() {
        let code = ClaimCode::mint();
        let key = stretch_claim_code(code.as_str());
        let nonce = [7_u8; CLAIM_NONCE_BYTES];
        let ca_digest = [9_u8; 32];
        let token = "a-local-api-token-with-at-least-32-bytes";

        let (seal_nonce, ciphertext) = seal_token(&key, &nonce, &ca_digest, token).expect("seal");
        assert!(
            !ciphertext
                .windows(token.len())
                .any(|w| w == token.as_bytes()),
            "the token must not appear in the clear anywhere in the response"
        );
        assert_eq!(
            *open_sealed_token(&key, &nonce, &ca_digest, &seal_nonce, &ciphertext).expect("open"),
            token
        );

        // A relay that swaps the CA cannot produce an openable response: the CA
        // digest is bound in as associated data, so decryption is simultaneously
        // the CA verification the operator used to do by hand.
        let substituted = [10_u8; 32];
        assert!(
            open_sealed_token(&key, &nonce, &substituted, &seal_nonce, &ciphertext).is_err(),
            "substituting the CA must break the seal"
        );

        // A different code cannot open it either, which is what leaves an
        // on-path relay holding ciphertext it cannot use.
        let other = stretch_claim_code(ClaimCode::mint().as_str());
        assert!(open_sealed_token(&other, &nonce, &ca_digest, &seal_nonce, &ciphertext).is_err());

        // Nor can a different nonce, so a captured response is not replayable
        // against a second exchange.
        assert!(
            open_sealed_token(
                &key,
                &[8_u8; CLAIM_NONCE_BYTES],
                &ca_digest,
                &seal_nonce,
                &ciphertext
            )
            .is_err()
        );
        assert!(open_sealed_token(&key, &nonce, &ca_digest, b"short", &ciphertext).is_err());
    }

    #[test]
    fn the_claim_code_never_reaches_a_log_through_debug() {
        let code = ClaimCode::mint();
        let rendered = format!("{code:?}");
        assert_eq!(rendered, "ClaimCode([REDACTED])");
        assert!(
            !rendered.contains(code.as_str()),
            "the Debug rendering must not disclose the code"
        );
    }

    #[test]
    fn a_stretched_key_is_deterministic_and_domain_separated() {
        let code = ClaimCode::from_normalised("0123456789");
        assert_eq!(
            *stretch_claim_code(code.as_str()),
            *stretch_claim_code(code.as_str()),
            "the same code must always derive the same key"
        );
        assert_ne!(
            *stretch_claim_code("0123456789"),
            *stretch_claim_code("0123456788")
        );

        // The proof and the seal must not be the same key, or one would be an
        // oracle for the other.
        let key = stretch_claim_code(code.as_str());
        let nonce = [3_u8; CLAIM_NONCE_BYTES];
        assert_ne!(client_proof(&key, &nonce), *seal_key(&key, &nonce));
    }
}
