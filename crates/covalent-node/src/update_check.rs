//! Notify-only release check.
//!
//! A node reports the version it was built from and, unless the operator has
//! turned the check off, asks GitHub once a day whether a newer release has
//! been published. That is all it does: nothing is downloaded and nothing is
//! executed. Applying an update stays with whatever already replaces the
//! package (a container updater, Unraid, or a manual install).
//!
//! The result lives in memory only. Every failure ends in a reported state;
//! none of them can reach transfers, persisted state or the status endpoint,
//! which only ever reads a small snapshot.

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use http_body_util::{BodyExt as _, Empty, LengthLimitError, Limited};
use hyper::header::{ACCEPT, CONNECTION, HOST, USER_AGENT};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::RootCertStore;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, ServerName};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Version of the running node, taken from the workspace at build time.
pub const NODE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The one URL this module ever requests.
pub const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/thekozugroup/Covalent/releases/latest";
const RELEASE_API_HOST: &str = "api.github.com";
const RELEASE_API_PATH: &str = "/repos/thekozugroup/Covalent/releases/latest";
/// Release pages are built from a validated version, never copied from a response.
const RELEASE_PAGE_PREFIX: &str = "https://github.com/thekozugroup/Covalent/releases/tag/v";
const REQUEST_USER_AGENT: &str = concat!("covalent-node/", env!("CARGO_PKG_VERSION"));

/// Lets the network settle after start and keeps a restart loop from asking at all.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(60);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Covers name lookup, connect, TLS and the whole response.
const CHECK_DEADLINE: Duration = Duration::from_secs(20);
/// A release with every asset listed is about 70 KiB today.
const MAX_RELEASE_BODY_BYTES: usize = 1_024 * 1_024;
const MAX_TRUST_BUNDLE_BYTES: u64 = 4 * 1_024 * 1_024;
const MAX_VERSION_PART_DIGITS: usize = 9;

/// System certificate bundles, in the order they are tried.
const TRUST_BUNDLE_CANDIDATES: [&str; 5] = [
    // Alpine (the published container image), Debian, Ubuntu.
    "/etc/ssl/certs/ca-certificates.crt",
    // macOS, including inside the App Sandbox, and the BSDs.
    "/etc/ssl/cert.pem",
    // Fedora, RHEL.
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    // openSUSE.
    "/etc/ssl/ca-bundle.pem",
];

/// Whether this node may ask GitHub about newer releases.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UpdateCheckPolicy {
    /// One request shortly after start, then one every 24 hours.
    Enabled,
    /// Never contact GitHub. The node still reports its own version.
    #[default]
    Disabled,
}

/// What a client can show about updates.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateState {
    /// The operator turned the check off.
    Disabled,
    /// This platform's node cannot make the check.
    Unsupported,
    /// The check is on and has not completed since the node started.
    Pending,
    /// The running version is the latest published release, or newer.
    UpToDate,
    /// A newer release is published.
    UpdateAvailable,
    /// The latest attempt did not produce an answer.
    CheckFailed,
}

/// Stable reason a check produced no answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateCheckFailureCode {
    /// GitHub could not be reached: no route, no name resolution, or a dropped connection.
    Network,
    /// GitHub did not finish answering before the deadline.
    Timeout,
    /// No trusted certificate list was found, or GitHub's certificate was not accepted.
    SecureConnection,
    /// GitHub refused the request because of its request limit.
    RateLimited,
    /// GitHub answered with something this node does not understand.
    UnexpectedResponse,
}

impl UpdateCheckFailureCode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Timeout => "timeout",
            Self::SecureConnection => "secureConnection",
            Self::RateLimited => "rateLimited",
            Self::UnexpectedResponse => "unexpectedResponse",
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::Network => {
                "Covalent could not reach GitHub to check for updates. \
                 Check this device's internet connection."
            }
            Self::Timeout => "GitHub took too long to answer the update check.",
            Self::SecureConnection => {
                "Covalent could not verify a secure connection to GitHub, \
                 so it did not check for updates."
            }
            Self::RateLimited => "GitHub is limiting update checks from this network for now.",
            Self::UnexpectedResponse => {
                "GitHub sent an answer about the latest release that Covalent did not understand."
            }
        }
    }
}

impl fmt::Display for UpdateCheckFailureCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why the latest check failed, for a client to show.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckFailure {
    /// Machine-readable reason. New values may be added.
    pub code: UpdateCheckFailureCode,
    /// The same reason in plain language.
    pub message: &'static str,
}

/// Secret-free update snapshot returned by the status route.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// Which of the states a client should show.
    pub state: UpdateState,
    /// Latest published release, without a leading `v`, when a check has succeeded.
    pub latest_version: Option<String>,
    /// Release page for `latest_version`, only when it is newer than this node.
    pub release_url: Option<String>,
    /// When the check behind this state finished.
    pub checked_at_unix_ms: Option<u64>,
    /// Present only in the `checkFailed` state.
    pub failure: Option<UpdateCheckFailure>,
}

impl UpdateStatus {
    const fn idle(state: UpdateState) -> Self {
        Self {
            state,
            latest_version: None,
            release_url: None,
            checked_at_unix_ms: None,
            failure: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Schedule {
    first_delay: Duration,
    interval: Duration,
    deadline: Duration,
}

const PRODUCTION_SCHEDULE: Schedule = Schedule {
    first_delay: FIRST_CHECK_DELAY,
    interval: CHECK_INTERVAL,
    deadline: CHECK_DEADLINE,
};

/// In-memory owner of the update snapshot.
#[derive(Debug)]
pub struct UpdateCheck {
    active: bool,
    status: Mutex<UpdateStatus>,
}

impl UpdateCheck {
    /// Creates the snapshot for one runtime. Nothing is contacted here.
    #[must_use]
    pub fn new(policy: UpdateCheckPolicy) -> Self {
        // The Android app links this node in-process and has no certificate
        // bundle on disk for it to read. The app reports the version instead.
        let supported = !cfg!(target_os = "android");
        let state = match (supported, policy) {
            (false, _) => UpdateState::Unsupported,
            (true, UpdateCheckPolicy::Disabled) => UpdateState::Disabled,
            (true, UpdateCheckPolicy::Enabled) => UpdateState::Pending,
        };
        Self {
            active: state == UpdateState::Pending,
            status: Mutex::new(UpdateStatus::idle(state)),
        }
    }

    /// Current snapshot. Never blocks on the network.
    #[must_use]
    pub fn status(&self) -> UpdateStatus {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Starts the daily check, or returns `None` when this node must not make it.
    pub(crate) fn spawn(
        self: &Arc<Self>,
        shutdown: watch::Receiver<bool>,
    ) -> Option<JoinHandle<()>> {
        // Stated as a compile-time constant as well, so the Android library
        // does not link an HTTPS client it can never use.
        if cfg!(target_os = "android") || !self.active {
            return None;
        }
        Some(tokio::spawn(Arc::clone(self).run(
            shutdown,
            PRODUCTION_SCHEDULE,
            fetch_latest_tag_from_github,
        )))
    }

    async fn run<F, Fut>(
        self: Arc<Self>,
        mut shutdown: watch::Receiver<bool>,
        schedule: Schedule,
        fetch: F,
    ) where
        F: Fn() -> Fut + Send,
        Fut: Future<Output = Result<String, UpdateCheckFailureCode>> + Send + 'static,
    {
        let mut wait = schedule.first_delay;
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = stopped(&mut shutdown) => return,
            }
            let outcome = tokio::select! {
                outcome = attempt(fetch(), schedule.deadline) => outcome,
                () = stopped(&mut shutdown) => return,
            };
            self.record(outcome, unix_ms_now());
            wait = schedule.interval;
        }
    }

    #[cfg(test)]
    pub(crate) fn record_failure_for_test(&self, code: UpdateCheckFailureCode) {
        self.record(Err(code), 1);
    }

    fn record(&self, outcome: Result<Comparison, UpdateCheckFailureCode>, now_unix_ms: u64) {
        let mut status = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        match outcome {
            Ok(comparison) => {
                let next = comparison.into_status(now_unix_ms);
                if status.state != next.state || status.latest_version != next.latest_version {
                    info!(
                        running = NODE_VERSION,
                        latest = next.latest_version.as_deref().unwrap_or_default(),
                        update_available = next.state == UpdateState::UpdateAvailable,
                        "update check completed"
                    );
                }
                *status = next;
            }
            Err(code) => {
                let repeated = status.failure.is_some_and(|failure| failure.code == code);
                if repeated {
                    debug!(%code, "update check failed again");
                } else {
                    warn!(%code, "update check failed; Covalent keeps running and retries in a day");
                }
                // A release that was already found stays published, so a later
                // failure must not hide it.
                if status.state != UpdateState::UpdateAvailable {
                    *status = UpdateStatus {
                        state: UpdateState::CheckFailed,
                        latest_version: None,
                        release_url: None,
                        checked_at_unix_ms: Some(now_unix_ms),
                        failure: Some(UpdateCheckFailure {
                            code,
                            message: code.message(),
                        }),
                    };
                }
            }
        }
    }
}

async fn stopped(shutdown: &mut watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        // A dropped sender means the runtime that owned this check is gone.
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Runs one fetch in its own task so that a deadline, a shutdown or even a
/// panic inside it ends as a reported failure and nothing else.
async fn attempt<Fut>(fetch: Fut, deadline: Duration) -> Result<Comparison, UpdateCheckFailureCode>
where
    Fut: Future<Output = Result<String, UpdateCheckFailureCode>> + Send + 'static,
{
    let mut task = AbortOnDrop(tokio::spawn(fetch));
    let tag = match tokio::time::timeout(deadline, &mut task.0).await {
        Err(_) => return Err(UpdateCheckFailureCode::Timeout),
        Ok(Err(_)) => return Err(UpdateCheckFailureCode::UnexpectedResponse),
        Ok(Ok(fetched)) => fetched?,
    };
    compare(NODE_VERSION, &tag)
}

fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or_default()
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ReleaseVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl ReleaseVersion {
    /// Exactly `MAJOR.MINOR.PATCH` in plain decimal digits.
    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let version = Self {
            major: parse_version_part(parts.next()?)?,
            minor: parse_version_part(parts.next()?)?,
            patch: parse_version_part(parts.next()?)?,
        };
        parts.next().is_none().then_some(version)
    }

    /// A published release tag: `vMAJOR.MINOR.PATCH`, with the `v` optional.
    /// Anything else, including a pre-release or container-only tag, is not a
    /// version this node can compare itself with.
    fn parse_tag(tag: &str) -> Option<Self> {
        Self::parse(tag.strip_prefix('v').unwrap_or(tag))
    }

    /// The running build. Returns whether it carries a pre-release suffix,
    /// which sorts before the release of the same number.
    fn parse_running(version: &str) -> Option<(Self, bool)> {
        let without_build = version.split_once('+').map_or(version, |(core, _)| core);
        match without_build.split_once('-') {
            Some((core, suffix)) if !suffix.is_empty() => Some((Self::parse(core)?, true)),
            Some(_) => None,
            None => Some((Self::parse(without_build)?, false)),
        }
    }
}

impl fmt::Display for ReleaseVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn parse_version_part(part: &str) -> Option<u32> {
    let plain = !part.is_empty()
        && part.len() <= MAX_VERSION_PART_DIGITS
        && part.bytes().all(|byte| byte.is_ascii_digit())
        && (part.len() == 1 || !part.starts_with('0'));
    plain.then(|| part.parse().ok()).flatten()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Comparison {
    UpToDate { latest: ReleaseVersion },
    UpdateAvailable { latest: ReleaseVersion },
}

impl Comparison {
    fn into_status(self, now_unix_ms: u64) -> UpdateStatus {
        let (state, latest, release_url) = match self {
            Self::UpToDate { latest } => (UpdateState::UpToDate, latest, None),
            Self::UpdateAvailable { latest } => (
                UpdateState::UpdateAvailable,
                latest,
                Some(format!("{RELEASE_PAGE_PREFIX}{latest}")),
            ),
        };
        UpdateStatus {
            state,
            latest_version: Some(latest.to_string()),
            release_url,
            checked_at_unix_ms: Some(now_unix_ms),
            failure: None,
        }
    }
}

fn compare(running: &str, latest_tag: &str) -> Result<Comparison, UpdateCheckFailureCode> {
    let latest =
        ReleaseVersion::parse_tag(latest_tag).ok_or(UpdateCheckFailureCode::UnexpectedResponse)?;
    let (running, running_is_prerelease) =
        ReleaseVersion::parse_running(running).ok_or(UpdateCheckFailureCode::UnexpectedResponse)?;
    if latest > running || (latest == running && running_is_prerelease) {
        Ok(Comparison::UpdateAvailable { latest })
    } else {
        Ok(Comparison::UpToDate { latest })
    }
}

#[derive(Deserialize)]
struct LatestRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

fn parse_latest_release(body: &[u8]) -> Result<String, UpdateCheckFailureCode> {
    let release: LatestRelease =
        serde_json::from_slice(body).map_err(|_| UpdateCheckFailureCode::UnexpectedResponse)?;
    if release.draft || release.prerelease {
        return Err(UpdateCheckFailureCode::UnexpectedResponse);
    }
    Ok(release.tag_name)
}

async fn fetch_latest_tag_from_github() -> Result<String, UpdateCheckFailureCode> {
    let roots = tokio::task::spawn_blocking(load_trust_roots)
        .await
        .map_err(|_| UpdateCheckFailureCode::SecureConnection)??;
    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_client_config(roots)?));
    let server = ServerName::try_from(RELEASE_API_HOST)
        .map_err(|_| UpdateCheckFailureCode::SecureConnection)?;
    let stream = tokio::net::TcpStream::connect((RELEASE_API_HOST, 443))
        .await
        .map_err(|_| UpdateCheckFailureCode::Network)?;
    let stream = connector.connect(server, stream).await.map_err(|error| {
        // rustls reports a rejected certificate through the I/O error it wraps.
        if error
            .get_ref()
            .is_some_and(|inner| inner.is::<rustls::Error>())
        {
            UpdateCheckFailureCode::SecureConnection
        } else {
            UpdateCheckFailureCode::Network
        }
    })?;
    request_latest_tag(stream).await
}

fn tls_client_config(roots: RootCertStore) -> Result<rustls::ClientConfig, UpdateCheckFailureCode> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| UpdateCheckFailureCode::SecureConnection)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

fn load_trust_roots() -> Result<RootCertStore, UpdateCheckFailureCode> {
    TRUST_BUNDLE_CANDIDATES
        .iter()
        .find_map(|path| read_trust_bundle(Path::new(path)))
        .ok_or(UpdateCheckFailureCode::SecureConnection)
}

fn read_trust_bundle(path: &Path) -> Option<RootCertStore> {
    use std::io::Read as _;

    let mut pem = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_TRUST_BUNDLE_BYTES + 1)
        .read_to_end(&mut pem)
        .ok()?;
    if u64::try_from(pem.len()).ok()? > MAX_TRUST_BUNDLE_BYTES {
        return None;
    }
    trust_roots_from_pem(&pem)
}

fn trust_roots_from_pem(pem: &[u8]) -> Option<RootCertStore> {
    let mut roots = RootCertStore::empty();
    roots.add_parsable_certificates(CertificateDer::pem_slice_iter(pem).filter_map(Result::ok));
    (!roots.is_empty()).then_some(roots)
}

/// One HTTP/1.1 exchange over an already secured stream. No redirect is
/// followed and no credential, cookie or identifier is sent.
async fn request_latest_tag<IO>(stream: IO) -> Result<String, UpdateCheckFailureCode>
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|_| UpdateCheckFailureCode::Network)?;
    let _driver = AbortOnDrop(tokio::spawn(async move {
        let _ = connection.await;
    }));
    let request = Request::get(RELEASE_API_PATH)
        .header(HOST, RELEASE_API_HOST)
        .header(USER_AGENT, REQUEST_USER_AGENT)
        .header(ACCEPT, "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28")
        .header(CONNECTION, "close")
        .body(Empty::<Bytes>::new())
        .map_err(|_| UpdateCheckFailureCode::UnexpectedResponse)?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|_| UpdateCheckFailureCode::Network)?;
    let out_of_quota = response
        .headers()
        .get("x-ratelimit-remaining")
        .is_some_and(|remaining| remaining.as_bytes() == b"0");
    match response.status() {
        StatusCode::OK => {}
        StatusCode::TOO_MANY_REQUESTS => return Err(UpdateCheckFailureCode::RateLimited),
        StatusCode::FORBIDDEN if out_of_quota => return Err(UpdateCheckFailureCode::RateLimited),
        _ => return Err(UpdateCheckFailureCode::UnexpectedResponse),
    }
    let body = Limited::new(response.into_body(), MAX_RELEASE_BODY_BYTES)
        .collect()
        .await
        .map_err(|error| {
            if error.is::<LengthLimitError>() {
                UpdateCheckFailureCode::UnexpectedResponse
            } else {
                UpdateCheckFailureCode::Network
            }
        })?
        .to_bytes();
    parse_latest_release(&body)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    const HOUR: Duration = Duration::from_secs(60 * 60);
    const FAST: Schedule = Schedule {
        first_delay: Duration::from_millis(5),
        interval: HOUR,
        deadline: Duration::from_millis(250),
    };

    fn available(version: &str) -> Result<Comparison, UpdateCheckFailureCode> {
        Ok(Comparison::UpdateAvailable {
            latest: ReleaseVersion::parse(version).unwrap(),
        })
    }

    fn up_to_date(version: &str) -> Result<Comparison, UpdateCheckFailureCode> {
        Ok(Comparison::UpToDate {
            latest: ReleaseVersion::parse(version).unwrap(),
        })
    }

    #[test]
    fn newer_release_numbers_are_compared_as_numbers() {
        assert_eq!(compare("0.2.12", "v0.2.13"), available("0.2.13"));
        assert_eq!(compare("0.2.12", "0.2.13"), available("0.2.13"));
        // 0.2.9 sorts after 0.2.12 as text; it is the older release.
        assert_eq!(compare("0.2.12", "v0.2.9"), up_to_date("0.2.9"));
        assert_eq!(compare("0.2.9", "v0.2.12"), available("0.2.12"));
        assert_eq!(compare("0.2.12", "v0.3.0"), available("0.3.0"));
        assert_eq!(compare("0.9.9", "v1.0.0"), available("1.0.0"));
        assert_eq!(compare("0.2.12", "v0.2.12"), up_to_date("0.2.12"));
        // A build ahead of the latest release is not offered a downgrade.
        assert_eq!(compare("0.3.0", "v0.2.12"), up_to_date("0.2.12"));
        assert_eq!(compare("1.0.0", "v0.99.99"), up_to_date("0.99.99"));
    }

    #[test]
    fn a_prerelease_build_is_older_than_its_release() {
        assert_eq!(compare("0.2.13-rc.1", "v0.2.13"), available("0.2.13"));
        assert_eq!(compare("0.2.13-rc.1", "v0.2.12"), up_to_date("0.2.12"));
        assert_eq!(compare("0.2.13+build.5", "v0.2.13"), up_to_date("0.2.13"));
    }

    #[test]
    fn tags_that_are_not_plain_releases_are_refused() {
        for tag in [
            "",
            "v",
            "latest",
            "v0.2",
            "v0.2.13.1",
            "v0.2.13-rc.1",
            "container-v0.2.13",
            "V0.2.13",
            "vv0.2.13",
            "v0.2.x",
            "v0.02.13",
            "v+0.2.13",
            "v0.2.-13",
            " v0.2.13",
            "v0.2.13\n",
            "v0.2.99999999999",
            "v0.2.13/../../evil",
        ] {
            assert_eq!(
                compare("0.2.12", tag),
                Err(UpdateCheckFailureCode::UnexpectedResponse),
                "{tag:?}"
            );
        }
        assert_eq!(
            compare("not-a-version", "v0.2.13"),
            Err(UpdateCheckFailureCode::UnexpectedResponse)
        );
    }

    #[test]
    fn the_running_version_and_the_release_endpoint_belong_to_this_project() {
        assert!(ReleaseVersion::parse_running(NODE_VERSION).is_some());
        let repository = env!("CARGO_PKG_REPOSITORY");
        assert_eq!(
            LATEST_RELEASE_URL,
            format!(
                "https://{RELEASE_API_HOST}/repos/{}/releases/latest",
                repository.strip_prefix("https://github.com/").unwrap()
            )
        );
        assert_eq!(
            LATEST_RELEASE_URL,
            format!("https://{RELEASE_API_HOST}{RELEASE_API_PATH}")
        );
        assert_eq!(RELEASE_PAGE_PREFIX, format!("{repository}/releases/tag/v"));
    }

    #[test]
    fn release_answers_are_read_strictly() {
        assert_eq!(
            parse_latest_release(
                br#"{"tag_name":"v0.2.13","draft":false,"prerelease":false,"assets":[{"name":"x"}]}"#
            ),
            Ok("v0.2.13".to_owned())
        );
        assert_eq!(
            parse_latest_release(br#"{"tag_name":"v0.2.13"}"#),
            Ok("v0.2.13".to_owned())
        );
        for body in [
            &b""[..],
            b"<html>Sign in to continue</html>",
            b"{}",
            br#"{"tag_name":7}"#,
            br#"{"tag_name":"v0.2.13","draft":true}"#,
            br#"{"tag_name":"v0.2.13","prerelease":true}"#,
            br#"{"message":"Not Found"}"#,
        ] {
            assert_eq!(
                parse_latest_release(body),
                Err(UpdateCheckFailureCode::UnexpectedResponse)
            );
        }
    }

    #[test]
    fn states_and_failure_codes_use_the_documented_spelling() {
        let status = Comparison::UpdateAvailable {
            latest: ReleaseVersion::parse("0.2.13").unwrap(),
        }
        .into_status(1_794_360_000_000);
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({
                "state": "updateAvailable",
                "latestVersion": "0.2.13",
                "releaseUrl": "https://github.com/thekozugroup/Covalent/releases/tag/v0.2.13",
                "checkedAtUnixMs": 1_794_360_000_000_u64,
                "failure": null,
            })
        );
        for (state, text) in [
            (UpdateState::Disabled, "disabled"),
            (UpdateState::Unsupported, "unsupported"),
            (UpdateState::Pending, "pending"),
            (UpdateState::UpToDate, "upToDate"),
            (UpdateState::UpdateAvailable, "updateAvailable"),
            (UpdateState::CheckFailed, "checkFailed"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), text);
        }
        for code in [
            UpdateCheckFailureCode::Network,
            UpdateCheckFailureCode::Timeout,
            UpdateCheckFailureCode::SecureConnection,
            UpdateCheckFailureCode::RateLimited,
            UpdateCheckFailureCode::UnexpectedResponse,
        ] {
            assert_eq!(serde_json::to_value(code).unwrap(), code.as_str());
            assert!(code.message().ends_with('.'));
            // People read these; they use the glossary's words, not internal ones.
            let lower = code.message().to_ascii_lowercase();
            for retired in ["node", "peer", "share", "backup server"] {
                assert!(!lower.contains(retired), "{code}: {}", code.message());
            }
        }
    }

    #[test]
    fn a_disabled_check_reports_only_that_it_is_off() {
        let check = UpdateCheck::new(UpdateCheckPolicy::Disabled);
        assert!(!check.active);
        assert_eq!(check.status(), UpdateStatus::idle(UpdateState::Disabled));
        assert_eq!(
            UpdateCheck::new(UpdateCheckPolicy::default())
                .status()
                .state,
            UpdateState::Disabled
        );
        assert_eq!(
            UpdateCheck::new(UpdateCheckPolicy::Enabled).status(),
            UpdateStatus::idle(UpdateState::Pending)
        );
    }

    #[tokio::test]
    async fn a_disabled_check_never_starts_a_task() {
        let check = Arc::new(UpdateCheck::new(UpdateCheckPolicy::Disabled));
        let (_shutdown, receiver) = watch::channel(false);
        assert!(check.spawn(receiver).is_none());
    }

    async fn settled(check: &UpdateCheck) -> UpdateStatus {
        for _ in 0..400 {
            let status = check.status();
            if status.state != UpdateState::Pending {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("update check never left the pending state");
    }

    #[tokio::test]
    async fn one_answer_is_recorded_and_no_second_request_follows_within_the_interval() {
        let check = Arc::new(UpdateCheck::new(UpdateCheckPolicy::Enabled));
        let (shutdown, receiver) = watch::channel(false);
        let requests = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&requests);
        let task = tokio::spawn(Arc::clone(&check).run(receiver, FAST, move || {
            counted.fetch_add(1, Ordering::SeqCst);
            async { Ok("v999.0.0".to_owned()) }
        }));

        let status = settled(&check).await;
        assert_eq!(status.state, UpdateState::UpdateAvailable);
        assert_eq!(status.latest_version.as_deref(), Some("999.0.0"));
        assert_eq!(
            status.release_url.as_deref(),
            Some("https://github.com/thekozugroup/Covalent/releases/tag/v999.0.0")
        );
        assert!(status.checked_at_unix_ms.is_some_and(|at| at > 0));
        assert_eq!(status.failure, None);

        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(requests.load(Ordering::SeqCst), 1);

        shutdown.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("the check stops with the runtime")
            .unwrap();
    }

    #[tokio::test]
    async fn the_running_release_is_reported_as_up_to_date_without_a_link() {
        let check = Arc::new(UpdateCheck::new(UpdateCheckPolicy::Enabled));
        let (_shutdown, receiver) = watch::channel(false);
        tokio::spawn(
            Arc::clone(&check).run(receiver, FAST, || async { Ok(format!("v{NODE_VERSION}")) }),
        );
        let status = settled(&check).await;
        assert_eq!(status.state, UpdateState::UpToDate);
        assert_eq!(status.latest_version.as_deref(), Some(NODE_VERSION));
        assert_eq!(status.release_url, None);
        assert_eq!(status.failure, None);
    }

    #[tokio::test]
    async fn a_request_that_never_answers_times_out_while_status_stays_readable() {
        let check = Arc::new(UpdateCheck::new(UpdateCheckPolicy::Enabled));
        let (_shutdown, receiver) = watch::channel(false);
        let slow = Schedule {
            deadline: Duration::from_millis(40),
            ..FAST
        };
        tokio::spawn(Arc::clone(&check).run(receiver, slow, || {
            std::future::pending::<Result<String, _>>()
        }));
        // The snapshot is readable the whole time the request hangs.
        assert_eq!(check.status().state, UpdateState::Pending);
        tokio::time::sleep(Duration::from_millis(15)).await;
        assert_eq!(check.status().state, UpdateState::Pending);

        let status = settled(&check).await;
        assert_eq!(status.state, UpdateState::CheckFailed);
        assert_eq!(
            status.failure.map(|failure| failure.code),
            Some(UpdateCheckFailureCode::Timeout)
        );
        assert_eq!(status.latest_version, None);
        assert_eq!(status.release_url, None);
        assert!(status.checked_at_unix_ms.is_some());
    }

    #[tokio::test]
    async fn every_failure_becomes_a_reported_state() {
        for code in [
            UpdateCheckFailureCode::Network,
            UpdateCheckFailureCode::SecureConnection,
            UpdateCheckFailureCode::RateLimited,
            UpdateCheckFailureCode::UnexpectedResponse,
        ] {
            let check = Arc::new(UpdateCheck::new(UpdateCheckPolicy::Enabled));
            let (_shutdown, receiver) = watch::channel(false);
            tokio::spawn(Arc::clone(&check).run(receiver, FAST, move || async move { Err(code) }));
            let status = settled(&check).await;
            assert_eq!(status.state, UpdateState::CheckFailed);
            assert_eq!(
                status.failure,
                Some(UpdateCheckFailure {
                    code,
                    message: code.message()
                })
            );
        }

        let check = Arc::new(UpdateCheck::new(UpdateCheckPolicy::Enabled));
        let (_shutdown, receiver) = watch::channel(false);
        tokio::spawn(Arc::clone(&check).run(receiver, FAST, || async {
            Ok("container-v0.2.13".to_owned())
        }));
        assert_eq!(
            settled(&check).await.failure.map(|failure| failure.code),
            Some(UpdateCheckFailureCode::UnexpectedResponse)
        );
    }

    async fn faulty_request() -> Result<String, UpdateCheckFailureCode> {
        panic!("simulated fault inside the release request")
    }

    #[tokio::test]
    async fn a_panic_inside_the_request_is_contained() {
        let check = Arc::new(UpdateCheck::new(UpdateCheckPolicy::Enabled));
        let (_shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(Arc::clone(&check).run(receiver, FAST, faulty_request));
        let status = settled(&check).await;
        assert_eq!(status.state, UpdateState::CheckFailed);
        assert_eq!(
            status.failure.map(|failure| failure.code),
            Some(UpdateCheckFailureCode::UnexpectedResponse)
        );
        // The schedule survives and the snapshot is still readable.
        assert!(!task.is_finished());
        assert_eq!(check.status(), status);
    }

    #[test]
    fn a_later_failure_does_not_hide_an_update_that_was_already_found() {
        let check = UpdateCheck::new(UpdateCheckPolicy::Enabled);
        check.record(available("0.2.13"), 10);
        let found = check.status();
        check.record(Err(UpdateCheckFailureCode::Network), 20);
        assert_eq!(check.status(), found);

        // An "up to date" answer is only as good as the latest attempt.
        check.record(up_to_date("0.2.12"), 30);
        assert_eq!(check.status().state, UpdateState::UpToDate);
        check.record(Err(UpdateCheckFailureCode::Timeout), 40);
        let failed = check.status();
        assert_eq!(failed.state, UpdateState::CheckFailed);
        assert_eq!(failed.checked_at_unix_ms, Some(40));
        assert_eq!(failed.latest_version, None);

        // The next good answer clears the failure.
        check.record(up_to_date("0.2.12"), 50);
        assert_eq!(check.status().failure, None);
        assert_eq!(check.status().state, UpdateState::UpToDate);
    }

    #[tokio::test]
    async fn shutdown_ends_a_check_that_is_still_waiting_for_an_answer() {
        let check = Arc::new(UpdateCheck::new(UpdateCheckPolicy::Enabled));
        let (shutdown, receiver) = watch::channel(false);
        let unhurried = Schedule {
            deadline: HOUR,
            ..FAST
        };
        let task = tokio::spawn(Arc::clone(&check).run(receiver, unhurried, || {
            std::future::pending::<Result<String, _>>()
        }));
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!task.is_finished());
        drop(shutdown);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("a dropped runtime ends the check")
            .unwrap();
        assert_eq!(check.status().state, UpdateState::Pending);
    }

    /// Serves one canned HTTP response and returns the request it received.
    async fn exchange(response: Vec<u8>) -> (Result<String, UpdateCheckFailureCode>, String) {
        let (client, mut server) = tokio::io::duplex(64 * 1_024);
        let served = tokio::spawn(async move {
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1_024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = server.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            // The client may stop reading early; that is its decision to make.
            let _ = server.write_all(&response).await;
            let _ = server.shutdown().await;
            String::from_utf8(request).unwrap()
        });
        let result = request_latest_tag(client).await;
        (result, served.await.unwrap())
    }

    fn http_response(status: &str, headers: &[&str], body: &[u8]) -> Vec<u8> {
        let mut response = format!("HTTP/1.1 {status}\r\ncontent-length: {}\r\n", body.len());
        for header in headers {
            response.push_str(header);
            response.push_str("\r\n");
        }
        response.push_str("\r\n");
        let mut response = response.into_bytes();
        response.extend_from_slice(body);
        response
    }

    #[tokio::test]
    async fn the_request_names_only_the_release_endpoint_and_carries_no_identity() {
        let (result, request) = exchange(http_response(
            "200 OK",
            &["content-type: application/json; charset=utf-8"],
            br#"{"tag_name":"v0.2.13","draft":false,"prerelease":false}"#,
        ))
        .await;
        assert_eq!(result, Ok("v0.2.13".to_owned()));

        let request = request.to_ascii_lowercase();
        let mut lines = request.split("\r\n");
        assert_eq!(
            lines.next(),
            Some("get /repos/thekozugroup/covalent/releases/latest http/1.1")
        );
        let mut headers: Vec<&str> = lines.filter(|line| !line.is_empty()).collect();
        headers.sort_unstable();
        let user_agent = format!("user-agent: covalent-node/{NODE_VERSION}");
        assert_eq!(
            headers,
            [
                "accept: application/vnd.github+json",
                "connection: close",
                "host: api.github.com",
                user_agent.as_str(),
                "x-github-api-version: 2022-11-28",
            ]
        );
    }

    #[tokio::test]
    async fn a_chunked_answer_is_read_like_any_other() {
        let body = br#"{"tag_name":"v0.2.13"}"#;
        let (first, second) = body.split_at(9);
        let mut response = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec();
        for chunk in [first, second] {
            response.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            response.extend_from_slice(chunk);
            response.extend_from_slice(b"\r\n");
        }
        response.extend_from_slice(b"0\r\n\r\n");
        assert_eq!(exchange(response).await.0, Ok("v0.2.13".to_owned()));
    }

    #[tokio::test]
    async fn refusals_redirects_and_errors_are_classified_without_being_followed() {
        let cases: [(&str, &[&str], UpdateCheckFailureCode); 7] = [
            (
                "403 Forbidden",
                &["x-ratelimit-remaining: 0"],
                UpdateCheckFailureCode::RateLimited,
            ),
            (
                "429 Too Many Requests",
                &[],
                UpdateCheckFailureCode::RateLimited,
            ),
            (
                "403 Forbidden",
                &["x-ratelimit-remaining: 41"],
                UpdateCheckFailureCode::UnexpectedResponse,
            ),
            (
                "404 Not Found",
                &[],
                UpdateCheckFailureCode::UnexpectedResponse,
            ),
            (
                "500 Internal Server Error",
                &[],
                UpdateCheckFailureCode::UnexpectedResponse,
            ),
            (
                "302 Found",
                &["location: https://example.invalid/elsewhere"],
                UpdateCheckFailureCode::UnexpectedResponse,
            ),
            (
                "204 No Content",
                &[],
                UpdateCheckFailureCode::UnexpectedResponse,
            ),
        ];
        for (status, headers, expected) in cases {
            let body: &[u8] = if status.starts_with("204") {
                b""
            } else {
                br#"{"tag_name":"v9.9.9"}"#
            };
            assert_eq!(
                exchange(http_response(status, headers, body)).await.0,
                Err(expected),
                "{status}"
            );
        }
    }

    #[tokio::test]
    async fn oversized_truncated_and_non_http_answers_fail_closed() {
        let mut oversized = br#"{"tag_name":"v9.9.9","body":""#.to_vec();
        oversized.resize(MAX_RELEASE_BODY_BYTES + 64, b'a');
        oversized.extend_from_slice(br#""}"#);
        assert_eq!(
            exchange(http_response("200 OK", &[], &oversized)).await.0,
            Err(UpdateCheckFailureCode::UnexpectedResponse)
        );

        // The peer promises more than it sends, then closes.
        let truncated =
            b"HTTP/1.1 200 OK\r\ncontent-length: 4096\r\n\r\n{\"tag_name\":\"v9.9.9\"".to_vec();
        assert_eq!(
            exchange(truncated).await.0,
            Err(UpdateCheckFailureCode::Network)
        );

        assert_eq!(
            exchange(b"SSH-2.0-not-http\r\n\r\n".to_vec()).await.0,
            Err(UpdateCheckFailureCode::Network)
        );
        assert_eq!(
            exchange(Vec::new()).await.0,
            Err(UpdateCheckFailureCode::Network)
        );
    }

    #[test]
    fn only_a_bundle_with_certificates_becomes_a_trust_store() {
        assert!(trust_roots_from_pem(b"").is_none());
        assert!(trust_roots_from_pem(b"not a certificate bundle").is_none());
        assert!(
            trust_roots_from_pem(b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n")
                .is_none()
        );

        let key = rcgen::KeyPair::generate().unwrap();
        let mut parameters = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        parameters.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let authority = parameters.self_signed(&key).unwrap();
        let bundle = format!("# a comment line\n{}\n", authority.pem());
        let roots = trust_roots_from_pem(bundle.as_bytes()).unwrap();
        assert_eq!(roots.len(), 1);
        assert!(tls_client_config(roots).is_ok());

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bundle.pem");
        std::fs::write(&path, &bundle).unwrap();
        assert_eq!(read_trust_bundle(&path).unwrap().len(), 1);
        assert!(read_trust_bundle(&directory.path().join("missing.pem")).is_none());
        assert!(read_trust_bundle(directory.path()).is_none());
    }

    /// Manual probe: `cargo test -p covalent-node --lib live_release_check -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "contacts api.github.com"]
    async fn live_release_check_reaches_github_with_the_system_trust_store() {
        let tag = tokio::time::timeout(CHECK_DEADLINE, fetch_latest_tag_from_github())
            .await
            .expect("GitHub answers before the deadline")
            .expect("latest release tag");
        println!("latest published release: {tag}");
        println!(
            "compared with {NODE_VERSION}: {:?}",
            compare(NODE_VERSION, &tag)
        );
        assert!(ReleaseVersion::parse_tag(&tag).is_some());
    }
}
