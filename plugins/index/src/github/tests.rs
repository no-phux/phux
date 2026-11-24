use std::sync::atomic::AtomicU32;

use super::curl::curl_config;
use super::*;

#[test]
fn search_url_includes_the_topic_and_forks() {
    assert!(SEARCH_URL.contains("topic%3Aphux-plugin"));
    assert!(SEARCH_URL.contains("fork%3Atrue"));
    assert!(SEARCH_URL.starts_with(API_ROOT));
}

#[test]
fn curl_config_sends_a_token_only_when_one_was_provided() {
    let plain = curl_config("application/vnd.github+json", None);
    assert!(!plain.contains("Authorization"));
    let authed = curl_config("application/vnd.github.raw", Some("ghp_example"));
    assert!(authed.contains("Authorization: Bearer ghp_example"));
    assert!(authed.contains("phux-plugin-index"));
}

#[test]
fn license_ids_drop_github_placeholders() {
    assert_eq!(
        license_id(Some(RawLicense {
            spdx_id: Some("NOASSERTION".to_owned())
        })),
        None
    );
    assert_eq!(
        license_id(Some(RawLicense {
            spdx_id: Some("MIT".to_owned())
        }))
        .as_deref(),
        Some("MIT")
    );
}

#[test]
fn next_link_is_the_rel_next_url() {
    let block = "Link: <https://api.github.com/search/repositories?q=a&page=2>; rel=\"next\", <https://api.github.com/search/repositories?q=a&page=3>; rel=\"last\"";
    assert_eq!(
        next_link(header(block, "link").unwrap()),
        Some("https://api.github.com/search/repositories?q=a&page=2")
    );
}

struct Scripted {
    calls: std::sync::Mutex<Vec<String>>,
    pages: Vec<(&'static str, u16, &'static str, &'static str)>,
}

impl Get for Scripted {
    fn get(&self, url: &str, _accept: &str, _limit: u64) -> Result<Response, Error> {
        self.calls.lock().expect("lock").push(url.to_owned());
        let page = self
            .pages
            .iter()
            .find(|(want, _, _, _)| *want == url)
            .ok_or_else(|| Error::new(format!("unexpected url {url}")))?;
        Ok(Response {
            status: page.1,
            headers: page.2.to_owned(),
            body: page.3.as_bytes().to_vec(),
        })
    }
}

#[test]
fn search_follows_the_next_link_and_stops() {
    let second = "https://api.github.com/search/repositories?q=topic%3Aphux-plugin+fork%3Atrue&per_page=100&page=2";
    let client = Scripted {
        calls: std::sync::Mutex::new(Vec::new()),
        pages: vec![
            (
                SEARCH_URL,
                200,
                "Link: <https://api.github.com/search/repositories?q=topic%3Aphux-plugin+fork%3Atrue&per_page=100&page=2>; rel=\"next\"\n",
                r#"{"incomplete_results":false,"items":[{"full_name":"acme/one","archived":false,"fork":false,"default_branch":"main","stargazers_count":1,"pushed_at":"2026-01-01T00:00:00Z","license":null}]}"#,
            ),
            (
                second,
                200,
                "\n",
                r#"{"incomplete_results":false,"items":[{"full_name":"acme/two","archived":false,"fork":true,"default_branch":"trunk","stargazers_count":2,"pushed_at":"2026-01-02T00:00:00Z","license":{"spdx_id":"Apache-2.0"}}]}"#,
            ),
        ],
    };
    let mut slept = 0;
    let repos = collect_search(&client, &mut |_| slept += 1).expect("search");
    assert_eq!(slept, 0);
    assert_eq!(repos.len(), 2);
    assert!(repos[1].fork);
    assert_eq!(repos[1].license.as_deref(), Some("Apache-2.0"));
    assert_eq!(client.calls.lock().expect("lock").len(), 2);
}

#[test]
fn incomplete_search_is_an_error() {
    let err = parse_search_page(r#"{"incomplete_results":true,"items":[]}"#).unwrap_err();
    assert!(err.to_string().contains("incomplete_results"), "{err}");
}

#[test]
fn a_long_rate_limit_wait_is_refused() {
    let wait = retry_wait(403, "retry-after: 60\n", b"", 0);
    assert!(wait.is_some_and(|delay| delay > MAX_WAIT));
}

#[test]
fn a_plain_403_is_a_skip() {
    let skipped = classify_body(403, "", b"forbidden".to_vec(), FILE_LIMIT).expect("skip");
    assert!(matches!(skipped, Fetched::Skip(reason) if reason == "HTTP 403"));
}

#[test]
fn the_request_cap_fails_the_build_without_calling_curl() {
    let source = Source::Github {
        token: None,
        budget: Arc::new(AtomicU32::new(0)),
    };
    let repo = RepoMeta {
        full_name: "acme/widgets".to_owned(),
        archived: false,
        fork: false,
        private: false,
        default_branch: "main".to_owned(),
        stars: 0,
        pushed_at: "2026-01-01T00:00:00Z".to_owned(),
        license: None,
    };
    let err = read_text(&source, &repo, "phux-plugin.toml").expect_err("cap is fatal");
    assert!(err.to_string().contains("request cap reached"), "{err}");
}

/// Repository metadata that could not be read fails the build; it is not a
/// per-repository skip like a bad manifest (#1861).
#[test]
fn a_skipped_repository_document_is_an_error_not_a_missing_repo() {
    let err = repo_from_fetched(Fetched::Skip("HTTP 403".to_owned())).expect_err("fatal");
    assert!(
        err.to_string()
            .contains("could not read repository metadata: HTTP 403"),
        "{err}"
    );
    assert!(repo_from_fetched(Fetched::Missing).expect("ok").is_none());
    assert!(
        repo_from_fetched(Fetched::Text("not json".to_owned()))
            .expect("ok")
            .is_none(),
        "an unparseable document drops the example, not the build"
    );
}

/// The cap can trip on the GET after a clean HEAD, and on the GET that
/// confirms a body-less HEAD 403. Both are fatal, like the HEAD site.
#[test]
fn the_request_cap_is_fatal_on_either_get_path() {
    let err = read_online_with(
        |head| {
            if head {
                Ok(Call::Done(response(200, "content-length: 2\n", b"")))
            } else {
                Ok(Call::Capped)
            }
        },
        FILE_LIMIT,
    )
    .unwrap_err();
    assert!(err.to_string().contains("request cap reached"), "{err}");

    let err = read_online_with(
        |head| {
            if head {
                Ok(Call::Done(response(403, "\n", b"")))
            } else {
                Ok(Call::Capped)
            }
        },
        FILE_LIMIT,
    )
    .unwrap_err();
    assert!(err.to_string().contains("request cap reached"), "{err}");
}

fn response(status: u16, headers: &str, body: &[u8]) -> Response {
    Response {
        status,
        headers: headers.to_owned(),
        body: body.to_vec(),
    }
}

#[test]
fn a_curl_failure_fails_the_build() {
    let err = read_online_with(|_| Err(Error::new("curl failed")), FILE_LIMIT).unwrap_err();
    assert!(err.to_string().contains("curl failed"), "{err}");
}

#[test]
fn head_retry_after_is_fatal_before_get() {
    let mut gets = 0;
    let err = read_online_with(
        |head| {
            if head {
                Ok(Call::Done(response(403, "retry-after: 60\n", b"")))
            } else {
                gets += 1;
                Ok(Call::Done(response(200, "content-length: 2\n", b"ok")))
            }
        },
        FILE_LIMIT,
    )
    .unwrap_err();
    assert_eq!(gets, 0, "the header rate-limit check must fail before GET");
    assert!(err.to_string().contains("rate limit"), "{err}");
}

#[test]
fn head_403_without_retry_after_reads_the_body_and_a_rate_limit_there_is_fatal() {
    let mut gets = 0;
    let err = read_online_with(
        |head| {
            if head {
                Ok(Call::Done(response(403, "\n", b"")))
            } else {
                gets += 1;
                Ok(Call::Done(response(
                    403,
                    "\n",
                    b"You have exceeded a secondary rate limit",
                )))
            }
        },
        FILE_LIMIT,
    )
    .unwrap_err();
    assert_eq!(gets, 1, "a body-less HEAD 403 must be confirmed with GET");
    assert!(err.to_string().contains("rate limit"), "{err}");
}

#[test]
fn a_rate_limit_body_after_a_clean_head_is_fatal() {
    let err = read_online_with(
        |head| {
            if head {
                Ok(Call::Done(response(200, "content-length: 20\n", b"")))
            } else {
                Ok(Call::Done(response(
                    403,
                    "\n",
                    b"You have exceeded a secondary rate limit",
                )))
            }
        },
        FILE_LIMIT,
    )
    .unwrap_err();
    assert!(err.to_string().contains("rate limit"), "{err}");
}
