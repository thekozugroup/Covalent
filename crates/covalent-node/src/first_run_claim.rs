//! First-run ownership claim for a headless node the operator cannot log into.
//!
//! # Why this exists
//!
//! A container has no keyboard and no window. Earlier deployments exposed a
//! plaintext bearer-token file and required several manual shell and
//! certificate-verification steps. The token is now stored only as a
//! path-bound wrapped secret, so ownership must be handed to a verified client
//! rather than recovered from node storage.
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
//! the Unraid web interface, the Docker socket, or the host filesystem. Those
//! are security-sensitive, root-equivalent surfaces. Anyone who sees a live
//! code can attempt the one-time claim, so operators must protect log access
//! and restart the container if the code may have been exposed.
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
//! records that this node has an owner, and later starts mint nothing. The exact
//! nonce-and-proof request is durably bound to its sealed response, so a client
//! that lost the response can retrieve the same bytes after a restart. Every
//! different presentation is refused, including a new nonce made from the same
//! code.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use blake3::Hasher;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use covalent_core::CoreError;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
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
const CLAIM_LIFECYCLE_SCHEMA_VERSION: u16 = 1;
const CLAIM_REPLAY_SCHEMA_VERSION: u16 = 1;
const MAX_CLAIM_LIFECYCLE_BYTES: u64 = 4 * 1_024;
const MAX_CLAIM_REPLAY_BYTES: u64 = 128 * 1_024;
const CLAIM_LIFECYCLE_FILE_NAME: &str = "owner-claim-state.json";
const CLAIM_REPLAY_FILE_NAME: &str = "owner-claim-replay.json";
const CLAIM_REPLAY_AUTH_CONTEXT: &str = "covalent/first-run-claim/replay-auth/v1";

#[cfg(test)]
thread_local! {
    static CLAIM_COMMIT_FAILPOINT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn claim_commit_failpoint(boundary: u8) -> Result<(), CoreError> {
    CLAIM_COMMIT_FAILPOINT.with(|failpoint| {
        if failpoint.get() == boundary {
            Err(CoreError::InvalidState(format!(
                "forced first-run claim commit failure at boundary {boundary}"
            )))
        } else {
            Ok(())
        }
    })
}

#[cfg(not(test))]
const fn claim_commit_failpoint(_boundary: u8) -> Result<(), CoreError> {
    Ok(())
}

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
    /// Ownership could not be committed durably. No token is returned. The
    /// caller preserves its exact request: when a replay receipt was committed,
    /// retrying that request recovers the sealed response; otherwise a restart
    /// reopens the still-unclaimed lifecycle with a fresh code.
    OwnershipStateUnavailable,
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
            Self::OwnershipStateUnavailable => "ownership could not be recorded durably",
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
/// Case is lifted, grouping plus bounded editor-style ASCII whitespace is
/// dropped, and the three Crockford confusions are resolved the way a reader
/// would resolve them. Only space, tab, CR, and LF are presentation whitespace;
/// a string containing any other byte or Unicode character is rejected rather
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
            '-' | ' ' | '\t' | '\r' | '\n' => continue,
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
fn digests_equal(left: &[u8], right: &[u8]) -> bool {
    let mut difference = u8::from(left.len() != right.len());
    for index in 0..left.len().max(right.len()) {
        difference |=
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0);
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

/// Everything one claim exchange hands the client and durably replays after an
/// interrupted response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaimGrant {
    /// Device name captured before ownership is committed.
    pub device_name: String,
    /// The CA to pin, in PEM, when this deployment terminates TLS in a proxy.
    pub ca_certificate: Option<String>,
    /// Hex SHA-256 of the CA's DER, for display and for out-of-band checking.
    pub ca_fingerprint_sha256: Option<String>,
    /// The one-shot sealing nonce for [`Self::sealed_token`], base64url encoded.
    pub seal_nonce: String,
    /// The API token, sealed so an on-path relay cannot read it, base64url encoded.
    pub sealed_token: String,
}

impl ClaimGrant {
    fn validate(&self) -> Result<(), CoreError> {
        if self.device_name.is_empty()
            || self.device_name.len() > 80
            || self.device_name.chars().any(char::is_control)
            || self.seal_nonce.len() != 32
            || self.sealed_token.len() > 1_024
        {
            return Err(CoreError::InvalidState(
                "invalid first-run claim replay response".to_owned(),
            ));
        }
        let seal_nonce = URL_SAFE_NO_PAD
            .decode(&self.seal_nonce)
            .map_err(|_| CoreError::AuthenticationFailed)?;
        let sealed_token = URL_SAFE_NO_PAD
            .decode(&self.sealed_token)
            .map_err(|_| CoreError::AuthenticationFailed)?;
        if seal_nonce.len() != 24 || !(48..=528).contains(&sealed_token.len()) {
            return Err(CoreError::AuthenticationFailed);
        }
        match (&self.ca_certificate, &self.ca_fingerprint_sha256) {
            (None, None) => Ok(()),
            (Some(pem), Some(fingerprint)) => {
                if pem.len() > 64 * 1_024
                    || fingerprint.len() != 64
                    || fingerprint
                        .bytes()
                        .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
                {
                    return Err(CoreError::AuthenticationFailed);
                }
                let der = validate_exact_ca_certificate_pem(pem)?;
                let actual: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(&der).into();
                let actual = actual
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                if !digests_equal(actual.as_bytes(), fingerprint.as_bytes()) {
                    return Err(CoreError::AuthenticationFailed);
                }
                Ok(())
            }
            _ => Err(CoreError::AuthenticationFailed),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaimRequestBinding {
    client_nonce: String,
    client_proof: String,
}

impl ClaimRequestBinding {
    fn new(client_nonce: &[u8], client_proof: &[u8]) -> Result<Self, CoreError> {
        if client_nonce.len() != CLAIM_NONCE_BYTES || client_proof.len() != 32 {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(Self {
            client_nonce: URL_SAFE_NO_PAD.encode(client_nonce),
            client_proof: URL_SAFE_NO_PAD.encode(client_proof),
        })
    }

    fn validate(&self) -> Result<(), CoreError> {
        let nonce = URL_SAFE_NO_PAD
            .decode(&self.client_nonce)
            .map_err(|_| CoreError::AuthenticationFailed)?;
        let proof = URL_SAFE_NO_PAD
            .decode(&self.client_proof)
            .map_err(|_| CoreError::AuthenticationFailed)?;
        if nonce.len() != CLAIM_NONCE_BYTES || proof.len() != 32 {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(())
    }

    fn matches(&self, client_nonce: &[u8], client_proof: &[u8]) -> bool {
        let nonce = URL_SAFE_NO_PAD.encode(client_nonce);
        let proof = URL_SAFE_NO_PAD.encode(client_proof);
        digests_equal(self.client_nonce.as_bytes(), nonce.as_bytes())
            && digests_equal(self.client_proof.as_bytes(), proof.as_bytes())
    }
}

impl Drop for ClaimRequestBinding {
    fn drop(&mut self) {
        self.client_nonce.zeroize();
        self.client_proof.zeroize();
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaimReplayReceipt {
    schema_version: u16,
    request: ClaimRequestBinding,
    response: ClaimGrant,
    claimed_at_unix_ms: u64,
    authenticator: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaimReplayAuthenticated<'a> {
    schema_version: u16,
    request: &'a ClaimRequestBinding,
    response: &'a ClaimGrant,
    claimed_at_unix_ms: u64,
}

impl ClaimReplayReceipt {
    fn new(
        request: ClaimRequestBinding,
        response: ClaimGrant,
        claimed_at_unix_ms: u64,
        api_token: &str,
    ) -> Result<Self, CoreError> {
        let mut receipt = Self {
            schema_version: CLAIM_REPLAY_SCHEMA_VERSION,
            request,
            response,
            claimed_at_unix_ms,
            authenticator: String::new(),
        };
        receipt.authenticator = URL_SAFE_NO_PAD.encode(receipt.authenticator(api_token)?);
        receipt.validate(api_token)?;
        Ok(receipt)
    }

    fn authenticated_bytes(&self) -> Result<Vec<u8>, CoreError> {
        Ok(serde_json::to_vec(&ClaimReplayAuthenticated {
            schema_version: self.schema_version,
            request: &self.request,
            response: &self.response,
            claimed_at_unix_ms: self.claimed_at_unix_ms,
        })?)
    }

    fn authenticator(&self, api_token: &str) -> Result<[u8; 32], CoreError> {
        let key = Zeroizing::new(blake3::derive_key(
            CLAIM_REPLAY_AUTH_CONTEXT,
            api_token.as_bytes(),
        ));
        Ok(*blake3::keyed_hash(&key, &self.authenticated_bytes()?).as_bytes())
    }

    fn validate(&self, api_token: &str) -> Result<(), CoreError> {
        if self.schema_version != CLAIM_REPLAY_SCHEMA_VERSION || self.claimed_at_unix_ms == 0 {
            return Err(CoreError::AuthenticationFailed);
        }
        self.request.validate()?;
        self.response.validate()?;
        let stored = URL_SAFE_NO_PAD
            .decode(&self.authenticator)
            .map_err(|_| CoreError::AuthenticationFailed)?;
        let expected = self.authenticator(api_token)?;
        if stored.len() != expected.len() || !digests_equal(&stored, &expected) {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(())
    }
}

impl Drop for ClaimReplayReceipt {
    fn drop(&mut self) {
        self.authenticator.zeroize();
    }
}

/// Durable ownership phase consulted before a setup code is minted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ClaimLifecyclePhase {
    Unclaimed,
    Claimed,
}

/// Versioned lifecycle record that distinguishes a new unclaimed node from a
/// legacy node whose token predates first-run claiming.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaimLifecycleRecord {
    schema_version: u16,
    phase: ClaimLifecyclePhase,
    initialized_at_unix_ms: u64,
    claimed_at_unix_ms: Option<u64>,
}

impl ClaimLifecycleRecord {
    fn unclaimed(now_unix_ms: u64) -> Self {
        Self {
            schema_version: CLAIM_LIFECYCLE_SCHEMA_VERSION,
            phase: ClaimLifecyclePhase::Unclaimed,
            initialized_at_unix_ms: now_unix_ms,
            claimed_at_unix_ms: None,
        }
    }

    fn claimed(initialized_at_unix_ms: u64, claimed_at_unix_ms: u64) -> Self {
        Self {
            schema_version: CLAIM_LIFECYCLE_SCHEMA_VERSION,
            phase: ClaimLifecyclePhase::Claimed,
            initialized_at_unix_ms,
            claimed_at_unix_ms: Some(claimed_at_unix_ms),
        }
    }

    fn validate(&self) -> Result<(), CoreError> {
        if self.schema_version != CLAIM_LIFECYCLE_SCHEMA_VERSION
            || self.initialized_at_unix_ms == 0
            || matches!(self.phase, ClaimLifecyclePhase::Unclaimed)
                && self.claimed_at_unix_ms.is_some()
            || matches!(self.phase, ClaimLifecyclePhase::Claimed)
                && self
                    .claimed_at_unix_ms
                    .is_none_or(|claimed_at| claimed_at == 0)
        {
            return Err(CoreError::InvalidState(
                "invalid first-run claim lifecycle".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Startup decision made from the explicit lifecycle record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimStartupState {
    /// A fresh code may be minted for this process.
    Unclaimed,
    /// A sealed replay receipt committed before the claimed lifecycle and must
    /// be authenticated with the persisted API token before startup continues.
    RecoveringReplay,
    /// The node already has an owner and must never mint another code.
    Claimed,
}

/// Path to the explicit first-run lifecycle record.
#[must_use]
pub fn claim_lifecycle_path(data_directory: &Path) -> PathBuf {
    data_directory.join(CLAIM_LIFECYCLE_FILE_NAME)
}

/// Path to the one bounded sealed-grant replay receipt.
#[must_use]
pub fn claim_replay_path(data_directory: &Path) -> PathBuf {
    data_directory.join(CLAIM_REPLAY_FILE_NAME)
}

fn replay_receipt_is_present(data_directory: &Path) -> bool {
    !matches!(
        std::fs::symlink_metadata(claim_replay_path(data_directory)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
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

fn read_claim_lifecycle(path: &Path) -> Result<Option<ClaimLifecycleRecord>, CoreError> {
    let Some(bytes) =
        crate::read_bounded_regular_file_optional(path, MAX_CLAIM_LIFECYCLE_BYTES, true)?
    else {
        return Ok(None);
    };
    let record: ClaimLifecycleRecord = serde_json::from_slice(bytes.as_ref())?;
    record.validate()?;
    Ok(Some(record))
}

fn persist_claim_lifecycle(path: &Path, record: &ClaimLifecycleRecord) -> Result<(), CoreError> {
    record.validate()?;
    crate::persist_private_file(path, &serde_json::to_vec_pretty(record)?)
}

/// Initializes or migrates the claim lifecycle before token loading can create
/// a token. This ordering is the durable distinction between a new unclaimed
/// node and a legacy deployment whose operator already owns its token.
pub fn prepare_claim_lifecycle(
    data_directory: &Path,
    token_already_exists: bool,
    now_unix_ms: u64,
) -> Result<ClaimStartupState, CoreError> {
    let lifecycle_path = claim_lifecycle_path(data_directory);
    let marker_path = owner_marker_path(data_directory);
    if let Some(record) = read_claim_lifecycle(&lifecycle_path)? {
        if record.phase == ClaimLifecyclePhase::Claimed || is_claimed(&marker_path) {
            if record.phase != ClaimLifecyclePhase::Claimed {
                persist_claim_lifecycle(
                    &lifecycle_path,
                    &ClaimLifecycleRecord::claimed(record.initialized_at_unix_ms, now_unix_ms),
                )?;
            }
            return Ok(ClaimStartupState::Claimed);
        }
        if replay_receipt_is_present(data_directory) {
            return Ok(ClaimStartupState::RecoveringReplay);
        }
        return Ok(ClaimStartupState::Unclaimed);
    }

    if token_already_exists || is_claimed(&marker_path) {
        persist_claim_lifecycle(
            &lifecycle_path,
            &ClaimLifecycleRecord::claimed(now_unix_ms, now_unix_ms),
        )?;
        if !is_claimed(&marker_path) {
            mark_claimed(&marker_path, now_unix_ms)?;
        }
        return Ok(ClaimStartupState::Claimed);
    }

    // This commit must precede token creation. If the process stops after this
    // write, the next start still knows that the on-disk token belongs to an
    // unclaimed first-run lifecycle rather than to a legacy owner.
    persist_claim_lifecycle(
        &lifecycle_path,
        &ClaimLifecycleRecord::unclaimed(now_unix_ms),
    )?;
    Ok(ClaimStartupState::Unclaimed)
}

fn complete_claim_lifecycle(
    lifecycle_path: &Path,
    marker_path: &Path,
    now_unix_ms: u64,
) -> Result<(), CoreError> {
    let record = read_claim_lifecycle(lifecycle_path)?.ok_or_else(|| {
        CoreError::InvalidState("first-run claim lifecycle is missing".to_owned())
    })?;
    if record.phase != ClaimLifecyclePhase::Claimed {
        persist_claim_lifecycle(
            lifecycle_path,
            &ClaimLifecycleRecord::claimed(record.initialized_at_unix_ms, now_unix_ms),
        )?;
    }
    // Retain the original marker as a rollback-safe compatibility signal. The
    // lifecycle record is authoritative, so failure here cannot reopen a claim.
    if let Err(error) = mark_claimed(marker_path, now_unix_ms) {
        tracing::error!(%error, "claimed lifecycle is durable but legacy owner marker could not be written");
    }
    Ok(())
}

fn read_claim_replay(
    path: &Path,
    api_token: &str,
) -> Result<Option<ClaimReplayReceipt>, CoreError> {
    let Some(bytes) =
        crate::read_bounded_regular_file_optional(path, MAX_CLAIM_REPLAY_BYTES, true)?
    else {
        return Ok(None);
    };
    let receipt: ClaimReplayReceipt = serde_json::from_slice(bytes.as_ref())?;
    receipt.validate(api_token)?;
    Ok(Some(receipt))
}

fn persist_claim_replay(path: &Path, receipt: &ClaimReplayReceipt) -> Result<(), CoreError> {
    let bytes = serde_json::to_vec_pretty(receipt)?;
    if bytes.len() as u64 > MAX_CLAIM_REPLAY_BYTES {
        return Err(CoreError::ResourceLimit("first-run claim replay receipt"));
    }
    crate::persist_private_file(path, &bytes)
}

/// Reads the CA certificate this deployment wants clients to pin.
///
/// Bounded, symlink-refusing, and validated as a real certificate rather than
/// echoed back as bytes: handing a client a malformed CA would leave it unable
/// to pin and, depending on the client, tempted to proceed without pinning.
fn read_ca_certificate(path: &Path) -> Result<(String, [u8; 32]), CoreError> {
    let bytes =
        crate::read_bounded_regular_file_optional(path, 64 * 1_024, false)?.ok_or_else(|| {
            CoreError::Io {
                operation: "open TLS CA certificate without following links",
                path: path.to_path_buf(),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            }
        })?;
    let pem = std::str::from_utf8(bytes.as_ref())
        .map_err(|_| CoreError::InvalidState("TLS CA certificate is not UTF-8 PEM".to_owned()))?
        .to_owned();
    let der = validate_exact_ca_certificate_pem(&pem)?;
    let digest: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(&der).into();
    Ok((pem, digest))
}

/// Validates one exact PEM-encoded X.509 CA certificate and returns its DER.
///
/// Only whitespace may surround the PEM block. Bundles, appended blocks,
/// arbitrary decoded bytes, and leaf certificates all fail closed.
pub fn validate_exact_ca_certificate_pem(pem: &str) -> Result<Vec<u8>, CoreError> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let begin = pem
        .find(BEGIN)
        .ok_or_else(|| CoreError::InvalidState("TLS CA PEM begin marker is missing".to_owned()))?;
    if !pem[..begin].chars().all(char::is_whitespace) {
        return Err(CoreError::InvalidState(
            "TLS CA PEM has non-whitespace prefix data".to_owned(),
        ));
    }
    let start = begin + BEGIN.len();
    let end = pem[start..]
        .find(END)
        .map(|offset| start + offset)
        .ok_or_else(|| CoreError::InvalidState("TLS CA PEM end marker is missing".to_owned()))?;
    let after_end = end + END.len();
    if !pem[after_end..].chars().all(char::is_whitespace) {
        return Err(CoreError::InvalidState(
            "TLS CA PEM has appended data or certificates".to_owned(),
        ));
    }
    let body: String = pem[start..end]
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let der = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, body)
        .map_err(|_| CoreError::InvalidState("TLS CA PEM body is invalid base64".to_owned()))?;

    let (remaining, certificate) = x509_parser::parse_x509_certificate(&der).map_err(|_| {
        CoreError::InvalidState("TLS CA is not a valid X.509 certificate".to_owned())
    })?;
    if !remaining.is_empty() || !certificate.is_ca() {
        return Err(CoreError::InvalidState(
            "TLS CA must contain exactly one X.509 CA certificate".to_owned(),
        ));
    }
    Ok(der)
}

/// The node-side owner of one first-run window, with its durable marker.
pub struct FirstRunClaim {
    state: Mutex<FirstRunClaimState>,
    lifecycle_path: PathBuf,
    replay_path: PathBuf,
    marker_path: PathBuf,
    ca_certificate_path: Option<PathBuf>,
}

enum FirstRunClaimState {
    Window(ClaimWindow),
    Replay {
        receipt: ClaimReplayReceipt,
        needs_commit: bool,
    },
}

impl FirstRunClaim {
    /// Opens a window around a freshly minted code.
    #[must_use]
    pub fn new(
        code: &ClaimCode,
        lifecycle_path: PathBuf,
        marker_path: PathBuf,
        ca_certificate_path: Option<PathBuf>,
        now_unix_ms: u64,
    ) -> Self {
        let replay_path = lifecycle_path
            .parent()
            .map_or_else(|| PathBuf::from(CLAIM_REPLAY_FILE_NAME), claim_replay_path);
        Self {
            state: Mutex::new(FirstRunClaimState::Window(ClaimWindow::open(
                code,
                now_unix_ms,
            ))),
            lifecycle_path,
            replay_path,
            marker_path,
            ca_certificate_path,
        }
    }

    /// Loads the sole authenticated sealed-grant replay receipt for a claimed or
    /// crash-interrupted node. Absence means a legacy/already-delivered owner.
    pub fn load_replay(
        data_directory: &Path,
        api_token: &str,
        now_unix_ms: u64,
    ) -> Result<Option<Self>, CoreError> {
        let replay_path = claim_replay_path(data_directory);
        let Some(receipt) = read_claim_replay(&replay_path, api_token)? else {
            return Ok(None);
        };
        let lifecycle_path = claim_lifecycle_path(data_directory);
        let marker_path = owner_marker_path(data_directory);
        complete_claim_lifecycle(&lifecycle_path, &marker_path, now_unix_ms)?;
        Ok(Some(Self {
            state: Mutex::new(FirstRunClaimState::Replay {
                receipt,
                needs_commit: false,
            }),
            lifecycle_path,
            replay_path,
            marker_path,
            ca_certificate_path: None,
        }))
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
        device_name: &str,
        api_token: &str,
        now_unix_ms: u64,
    ) -> Result<ClaimGrant, ClaimRefusal> {
        if now_unix_ms == 0
            || device_name.is_empty()
            || device_name.len() > 80
            || device_name.chars().any(char::is_control)
        {
            return Err(ClaimRefusal::OwnershipStateUnavailable);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| ClaimRefusal::CertificateUnavailable)?;
        if let FirstRunClaimState::Replay { receipt, .. } = &*state {
            if !receipt.request.matches(client_nonce, proof) {
                return Err(ClaimRefusal::AlreadyClaimed);
            }
            self.commit_pending_replay(&mut state, now_unix_ms)?;
            let FirstRunClaimState::Replay { receipt, .. } = &*state else {
                unreachable!("pending replay commit cannot reopen a claim window")
            };
            return Ok(receipt.response.clone());
        }

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

        let claim_key = match &mut *state {
            FirstRunClaimState::Window(window) => {
                window.present(client_nonce, proof, now_unix_ms)?
            }
            FirstRunClaimState::Replay { .. } => {
                unreachable!("replay state returned before reading the certificate")
            }
        };

        let (seal_nonce, sealed_token) =
            seal_token(&claim_key, client_nonce, &ca_digest, api_token)
                .map_err(|_| ClaimRefusal::CertificateUnavailable)?;

        let response = ClaimGrant {
            device_name: device_name.to_owned(),
            ca_fingerprint_sha256: ca_certificate
                .as_ref()
                .map(|_| ca_digest.iter().map(|byte| format!("{byte:02x}")).collect()),
            ca_certificate,
            seal_nonce: URL_SAFE_NO_PAD.encode(seal_nonce),
            sealed_token: URL_SAFE_NO_PAD.encode(sealed_token),
        };
        let receipt = ClaimReplayReceipt::new(
            ClaimRequestBinding::new(client_nonce, proof).map_err(|_| ClaimRefusal::Malformed)?,
            response.clone(),
            now_unix_ms,
            api_token,
        )
        .map_err(|_| ClaimRefusal::OwnershipStateUnavailable)?;
        // Retain the exact request and sealed response before attempting any
        // fallible commit. A transient write failure therefore cannot turn the
        // same request into a 409 inside this process; its next retry resumes
        // only this commit, while every different request remains refused.
        *state = FirstRunClaimState::Replay {
            receipt,
            needs_commit: true,
        };
        self.commit_pending_replay(&mut state, now_unix_ms)?;

        Ok(response)
    }

    fn commit_pending_replay(
        &self,
        state: &mut FirstRunClaimState,
        now_unix_ms: u64,
    ) -> Result<(), ClaimRefusal> {
        let FirstRunClaimState::Replay {
            receipt,
            needs_commit,
        } = state
        else {
            unreachable!("only a replay response can enter the claim commit path")
        };
        if !*needs_commit {
            return Ok(());
        }
        claim_commit_failpoint(3).map_err(|_| ClaimRefusal::OwnershipStateUnavailable)?;
        persist_claim_replay(&self.replay_path, receipt)
            .map_err(|_| ClaimRefusal::OwnershipStateUnavailable)?;
        claim_commit_failpoint(1).map_err(|_| ClaimRefusal::OwnershipStateUnavailable)?;

        // The replay receipt is already durable. A crash or persistence failure
        // from here can never mint a second owner: startup authenticates that
        // exact receipt, finishes the lifecycle, and serves only the same request.
        complete_claim_lifecycle(&self.lifecycle_path, &self.marker_path, now_unix_ms)
            .map_err(|_| ClaimRefusal::OwnershipStateUnavailable)?;
        claim_commit_failpoint(2).map_err(|_| ClaimRefusal::OwnershipStateUnavailable)?;
        *needs_commit = false;
        Ok(())
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

    fn test_ca_pem() -> String {
        let mut parameters =
            rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
        parameters.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate().expect("CA key");
        parameters.self_signed(&key).expect("CA certificate").pem()
    }

    fn test_claim(
        directory: &tempfile::TempDir,
        code: &ClaimCode,
        now_unix_ms: u64,
    ) -> FirstRunClaim {
        prepare_claim_lifecycle(directory.path(), false, now_unix_ms)
            .expect("prepare unclaimed lifecycle");
        let ca_path = directory.path().join("root.crt");
        std::fs::write(&ca_path, test_ca_pem()).expect("write CA");
        FirstRunClaim::new(
            code,
            claim_lifecycle_path(directory.path()),
            owner_marker_path(directory.path()),
            Some(ca_path),
            now_unix_ms,
        )
    }

    #[cfg(unix)]
    #[test]
    fn first_run_ca_reader_uses_one_nofollow_handle_and_requires_one_real_ca() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::TempDir::new().expect("directory");
        let path = directory.path().join("root.crt");
        let link = directory.path().join("linked-root.crt");
        let pem = test_ca_pem();
        std::fs::write(&path, &pem).expect("CA PEM");
        let (loaded, digest) = read_ca_certificate(&path).expect("valid CA file");
        assert_eq!(loaded, pem);
        assert_ne!(digest, [0_u8; 32]);

        symlink(&path, &link).expect("CA symlink");
        assert!(read_ca_certificate(&link).is_err());
        assert_eq!(std::fs::read_to_string(&path).expect("CA unchanged"), pem);

        std::fs::write(&path, format!("{pem}{pem}")).expect("appended CA");
        assert!(read_ca_certificate(&path).is_err());
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
        assert_eq!(
            normalise_claim_code(" \t\r\n01234-\r\n56789 \t\r\n")
                .as_deref()
                .map(String::as_str),
            Some(code.as_str()),
            "common editor line endings and ASCII spacing are presentation only"
        );
        for foreign_whitespace in ['\u{000b}', '\u{000c}', '\u{0085}', '\u{00a0}'] {
            assert_eq!(
                normalise_claim_code(&format!("01234{foreign_whitespace}56789")).as_deref(),
                None,
                "only space, tab, CR, and LF are accepted as presentation whitespace"
            );
        }
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
    fn an_exact_claim_request_replays_byte_identically_and_a_different_one_never_does() {
        const TOKEN: &str = "test-local-api-token-with-at-least-thirty-two-bytes";
        let directory = tempfile::TempDir::new().expect("directory");
        let code = ClaimCode::mint();
        let claim = test_claim(&directory, &code, NOW);
        let (nonce, proof) = presentation(&code);

        let first = claim
            .present(&nonce, &proof, "Covalent test", TOKEN, NOW + 1)
            .expect("first presentation");
        let replayed = claim
            .present(&nonce, &proof, "Covalent test", TOKEN, NOW + 2)
            .expect("same-process replay");
        assert_eq!(replayed, first);

        let (other_nonce, other_proof) = presentation(&code);
        assert_eq!(
            claim.present(&other_nonce, &other_proof, "Covalent test", TOKEN, NOW + 3,),
            Err(ClaimRefusal::AlreadyClaimed)
        );
        drop(claim);

        assert_eq!(
            prepare_claim_lifecycle(directory.path(), true, NOW + 4).expect("claimed startup"),
            ClaimStartupState::Claimed
        );
        let restarted = FirstRunClaim::load_replay(directory.path(), TOKEN, NOW + 4)
            .expect("load authenticated replay")
            .expect("replay receipt");
        assert_eq!(
            restarted
                .present(&nonce, &proof, "Covalent test", TOKEN, NOW + 5)
                .expect("restart replay"),
            first
        );
        assert_eq!(
            restarted.present(&other_nonce, &other_proof, "Covalent test", TOKEN, NOW + 6,),
            Err(ClaimRefusal::AlreadyClaimed)
        );
    }

    #[test]
    fn every_receipt_to_lifecycle_crash_boundary_recovers_only_the_exact_request() {
        const TOKEN: &str = "test-local-api-token-with-at-least-thirty-two-bytes";
        for boundary in [1_u8, 2] {
            let directory = tempfile::TempDir::new().expect("directory");
            let code = ClaimCode::mint();
            let claim = test_claim(&directory, &code, NOW);
            let (nonce, proof) = presentation(&code);
            CLAIM_COMMIT_FAILPOINT.with(|failpoint| failpoint.set(boundary));
            assert_eq!(
                claim.present(&nonce, &proof, "Covalent test", TOKEN, NOW + 1),
                Err(ClaimRefusal::OwnershipStateUnavailable)
            );
            CLAIM_COMMIT_FAILPOINT.with(|failpoint| failpoint.set(0));
            drop(claim);

            let startup = prepare_claim_lifecycle(directory.path(), true, NOW + 2)
                .expect("recover startup state");
            assert_eq!(
                startup,
                if boundary == 1 {
                    ClaimStartupState::RecoveringReplay
                } else {
                    ClaimStartupState::Claimed
                }
            );
            let restarted = FirstRunClaim::load_replay(directory.path(), TOKEN, NOW + 2)
                .expect("authenticate crash receipt")
                .expect("crash receipt");
            let response = restarted
                .present(&nonce, &proof, "Covalent test", TOKEN, NOW + 3)
                .expect("exact crash-boundary replay");
            response.validate().expect("valid replayed response");

            let (other_nonce, other_proof) = presentation(&code);
            assert_eq!(
                restarted.present(&other_nonce, &other_proof, "Covalent test", TOKEN, NOW + 4,),
                Err(ClaimRefusal::AlreadyClaimed)
            );
        }
    }

    #[test]
    fn a_transient_pre_receipt_commit_failure_keeps_only_the_exact_request_retryable() {
        const TOKEN: &str = "test-local-api-token-with-at-least-thirty-two-bytes";
        let directory = tempfile::TempDir::new().expect("directory");
        let code = ClaimCode::mint();
        let claim = test_claim(&directory, &code, NOW);
        let (nonce, proof) = presentation(&code);

        CLAIM_COMMIT_FAILPOINT.with(|failpoint| failpoint.set(3));
        assert_eq!(
            claim.present(&nonce, &proof, "Covalent test", TOKEN, NOW + 1),
            Err(ClaimRefusal::OwnershipStateUnavailable)
        );
        let (other_nonce, other_proof) = presentation(&code);
        assert_eq!(
            claim.present(&other_nonce, &other_proof, "Covalent test", TOKEN, NOW + 2,),
            Err(ClaimRefusal::AlreadyClaimed),
            "a commit retry cannot be replaced by a different presentation"
        );

        CLAIM_COMMIT_FAILPOINT.with(|failpoint| failpoint.set(0));
        claim
            .present(&nonce, &proof, "Covalent test", TOKEN, NOW + 3)
            .expect("exact request resumes its pending durable commit")
            .validate()
            .expect("committed replay response");
        assert_eq!(
            prepare_claim_lifecycle(directory.path(), true, NOW + 4).expect("claimed lifecycle"),
            ClaimStartupState::Claimed
        );
    }

    #[test]
    fn a_tampered_or_oversized_replay_receipt_fails_closed() {
        const TOKEN: &str = "test-local-api-token-with-at-least-thirty-two-bytes";
        let directory = tempfile::TempDir::new().expect("directory");
        let code = ClaimCode::mint();
        let claim = test_claim(&directory, &code, NOW);
        let (nonce, proof) = presentation(&code);
        claim
            .present(&nonce, &proof, "Covalent test", TOKEN, NOW + 1)
            .expect("claim");
        drop(claim);

        let path = claim_replay_path(directory.path());
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("receipt")).expect("JSON");
        value["response"]["deviceName"] = serde_json::json!("tampered");
        crate::persist_private_file(&path, &serde_json::to_vec_pretty(&value).expect("JSON"))
            .expect("tamper receipt");
        assert!(FirstRunClaim::load_replay(directory.path(), TOKEN, NOW + 2).is_err());

        crate::persist_private_file(&path, &vec![b' '; MAX_CLAIM_REPLAY_BYTES as usize + 1])
            .expect("oversized receipt");
        assert!(FirstRunClaim::load_replay(directory.path(), TOKEN, NOW + 3).is_err());
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
