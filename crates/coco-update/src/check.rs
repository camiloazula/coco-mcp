//! The request: GitHub's latest release, sent with the last answer's
//! `ETag`.
//!
//! GitHub allows 60 unauthenticated requests an hour per address, and a 304
//! does not count against them; once a day per install never comes near.
//! A check that fails is recorded as made, so an install that cannot reach
//! GitHub asks again the next day rather than on every run.

use std::path::Path;

use reqwest::StatusCode;
use reqwest::header::{ACCEPT, ETAG, HeaderValue, IF_NONE_MATCH};
use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{Release, State};

/// Why a check found nothing.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The request failed or timed out.
    #[error("cannot reach {0}")]
    Request(#[from] reqwest::Error),
    /// GitHub answered something other than 200 or 304.
    #[error("GitHub answered {0}")]
    Status(StatusCode),
    /// The answer was not a release this can read.
    #[error("unexpected answer: {0}")]
    Answer(String),
    /// The state could not be written.
    #[error("cannot write the update state: {0}")]
    Write(#[from] std::io::Error),
}

/// The client every check uses: rustls, [`crate::TIMEOUT`], and a
/// `User-Agent` naming `version`, which GitHub requires.
pub fn client(version: &str) -> Result<reqwest::Client, Error> {
    Ok(reqwest::Client::builder()
        .timeout(crate::TIMEOUT)
        .connect_timeout(crate::TIMEOUT)
        .user_agent(format!("coco-mcp/{version}"))
        .build()?)
}

/// The fields read from the answer.
#[derive(Debug, Deserialize)]
struct Answer {
    tag_name: String,
    html_url: String,
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// Ask `url` for the latest release and record the answer in the state at
/// `path`. Returns the state written, which also holds the user's choices.
pub async fn check(client: &reqwest::Client, url: &str, path: &Path) -> Result<State, Error> {
    // An answer that named no stable release is not worth keeping: a 304
    // to it would leave nothing to show.
    let known = State::load(path);
    let etag = known.etag.filter(|_| known.latest.is_some());
    let answer = ask(client, url, etag.as_deref()).await;
    let now = crate::now();
    let state = State::update(path, |state| {
        state.checked_at = Some(now);
        if let Ok(Some((release, etag))) = &answer {
            state.latest = release.clone();
            state.etag.clone_from(etag);
        }
    })?;
    answer.map(|_| state)
}

/// `Ok(None)` when unchanged since `etag`; otherwise the release (or none,
/// for one that is not stable) and the answer's `ETag`.
async fn ask(
    client: &reqwest::Client,
    url: &str,
    etag: Option<&str>,
) -> Result<Option<(Option<Release>, Option<String>)>, Error> {
    let mut request = client
        .get(url)
        .header(ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(etag) = etag.and_then(|e| HeaderValue::from_str(e).ok()) {
        request = request.header(IF_NONE_MATCH, etag);
    }
    let response = request.send().await?;
    match response.status() {
        StatusCode::NOT_MODIFIED => return Ok(None),
        StatusCode::OK => {}
        status => return Err(Error::Status(status)),
    }
    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|e| e.to_str().ok())
        .map(str::to_owned);
    let answer: Answer = response.json().await?;
    Ok(Some((release(answer)?, etag)))
}

/// The release an answer names, `None` for a draft or a prerelease.
fn release(answer: Answer) -> Result<Option<Release>, Error> {
    if answer.draft || answer.prerelease {
        return Ok(None);
    }
    let version = answer.tag_name.trim_start_matches('v');
    semver::Version::parse(version)
        .map_err(|e| Error::Answer(format!("tag {}: {e}", answer.tag_name)))?;
    let published_at = match &answer.published_at {
        Some(at) => OffsetDateTime::parse(at, &Rfc3339)
            .map_err(|e| Error::Answer(format!("published_at {at}: {e}")))?
            .unix_timestamp(),
        None => return Err(Error::Answer("no published_at".into())),
    };
    Ok(Some(Release {
        version: version.to_owned(),
        published_at,
        url: answer.html_url,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const BODY: &str = r#"{
        "tag_name": "v0.5.0",
        "html_url": "https://github.com/camiloazula/coco-mcp/releases/tag/v0.5.0",
        "published_at": "2026-10-10T12:00:00Z",
        "draft": false,
        "prerelease": false,
        "assets": []
    }"#;

    /// Answer each connection with the next of `responses`, and hand back
    /// the requests as they came.
    async fn serve(responses: Vec<String>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/latest", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let mut seen = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0; 8192];
                let n = socket.read(&mut buf).await.unwrap();
                seen.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
            seen
        });
        (url, task)
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\netag: \"abc\"\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn status(line: &str) -> String {
        format!("HTTP/1.1 {line}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
    }

    /// A state file in a directory of the test's own: tests run in
    /// parallel, and each removes its directory when done.
    fn temp_state(test: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("coco-update-{test}-{}", std::process::id()))
            .join("update.json")
    }

    #[tokio::test]
    async fn an_answer_is_recorded_and_its_etag_sent_back() {
        let (url, server) = serve(vec![ok(BODY), status("304 Not Modified")]).await;
        let path = temp_state("answer");
        let client = client("0.4.0").unwrap();

        let state = check(&client, &url, &path).await.unwrap();
        let latest = state.latest.clone().unwrap();
        assert_eq!(latest.version, "0.5.0");
        assert_eq!(latest.published_at, 1_791_633_600);
        assert_eq!(state.etag.as_deref(), Some("\"abc\""));
        assert!(state.checked_at.is_some());

        let again = check(&client, &url, &path).await.unwrap();
        assert_eq!(again.latest, Some(latest), "a 304 keeps the release");

        let requests = server.await.unwrap();
        let first = requests[0].to_ascii_lowercase();
        assert!(first.contains("user-agent: coco-mcp/0.4.0"), "{first}");
        assert!(!first.contains("if-none-match"), "{first}");
        assert!(
            requests[1]
                .to_ascii_lowercase()
                .contains("if-none-match: \"abc\""),
            "{}",
            requests[1]
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn a_failed_check_counts_as_made_and_keeps_what_was_known() {
        let (url, server) = serve(vec![ok(BODY), status("403 Forbidden")]).await;
        let path = temp_state("failed");
        let client = client("0.4.0").unwrap();
        check(&client, &url, &path).await.unwrap();

        let error = check(&client, &url, &path).await.unwrap_err();
        assert!(
            matches!(error, Error::Status(StatusCode::FORBIDDEN)),
            "{error}"
        );
        let state = State::load(&path);
        assert_eq!(state.latest.as_ref().unwrap().version, "0.5.0");
        assert!(!state.due(crate::now()), "not asked again today");
        server.await.unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn an_answer_with_no_stable_release_sends_no_etag_next_time() {
        let prerelease = BODY.replace("\"prerelease\": false", "\"prerelease\": true");
        let (url, server) = serve(vec![ok(&prerelease), ok(BODY)]).await;
        let path = temp_state("no-stable");
        let client = client("0.4.0").unwrap();
        let first = check(&client, &url, &path).await.unwrap();
        assert_eq!(first.latest, None);
        let second = check(&client, &url, &path).await.unwrap();
        assert_eq!(second.latest.unwrap().version, "0.5.0");
        let requests = server.await.unwrap();
        assert!(
            !requests[1].to_ascii_lowercase().contains("if-none-match"),
            "{}",
            requests[1]
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn drafts_prereleases_and_odd_tags_are_not_releases() {
        let answer = |tag: &str, prerelease: bool| Answer {
            tag_name: tag.into(),
            html_url: String::new(),
            published_at: Some("2026-10-10T12:00:00Z".into()),
            draft: false,
            prerelease,
        };
        assert_eq!(release(answer("v0.6.0-rc.1", true)).unwrap(), None);
        assert!(release(answer("nightly", false)).is_err());
        assert_eq!(
            release(answer("0.5.0", false)).unwrap().unwrap().version,
            "0.5.0",
            "a tag without the v"
        );
    }
}
