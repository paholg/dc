//! Integration tests for the image API.
//!
//! Gated behind the `docker-tests` feature; needs a live daemon.

#![cfg(feature = "docker-tests")]

use std::process::Command;

use docker::test_support::{TEST_LABEL, repo_tag_names, unique_name};
use docker::{Docker, Error, build_single_file_tar};

const IMAGE: &str = "alpine:3.20";

#[tokio::test(flavor = "multi_thread")]
async fn inspect_returns_not_found_for_unknown_image() {
    let client = Docker::connect().await.expect("connect");
    let err = client
        .inspect_image("docker-crate-test/does-not-exist:zzz")
        .await
        .expect_err("missing image should error");
    assert!(
        matches!(err, Error::NotFound { .. }),
        "expected NotFound, got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pull_then_inspect_succeeds() {
    let client = Docker::connect().await.expect("connect");
    client.pull_image(IMAGE).await.expect("pull");
    let details = client.inspect_image(IMAGE).await.expect("inspect");
    assert!(
        details.repo_tags.iter().any(|t| t.contains("alpine")),
        "repo_tags should include the alpine tag, got {:?}",
        details.repo_tags
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pull_unknown_image_returns_error() {
    let client = Docker::connect().await.expect("connect");
    let err = client
        .pull_image("docker-crate-test/no-such-image:zzz")
        .await
        .expect_err("pull of non-existent image should fail");
    // Either:
    // - the daemon returns 404 directly (mapped to NotFound), or
    // - it returns 200 and emits an error event mid-stream (mapped to Api).
    // Both are legitimate outcomes; the test only cares that the pull failed.
    assert!(
        matches!(err, Error::Api { .. } | Error::NotFound { .. }),
        "expected Api or NotFound, got {err:?}",
    );
}

/// RAII cleanup of an image the test created. Best-effort, ignores errors.
struct ImageCleanup(String);

impl Drop for ImageCleanup {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["image", "rm", "-f", &self.0])
            .output();
    }
}

/// Tag `alpine` under a fresh name carrying `labels`.
async fn build_labelled_image(client: &Docker, tag: &str, labels: &[(&str, &str)]) {
    let mut build = client.build_image(tag).context(
        build_single_file_tar("Dockerfile", format!("FROM {IMAGE}\n").as_bytes()).expect("tar"),
    );
    for (key, value) in labels {
        build = build.with_label(*key, *value);
    }
    build.call().await.expect("build the labelled image");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_by_label_then_remove() {
    let client = Docker::connect().await.expect("connect");
    client.ensure_image(IMAGE).await.expect("ensure base image");

    // `unique_name` mixes case, which is fine for a container but not for an
    // image repository.
    let tag = unique_name().to_lowercase();
    let _cleanup = ImageCleanup(tag.clone());
    let (key, value) = TEST_LABEL.split_once('=').expect("TEST_LABEL is key=value");
    // A second, tag-unique label so the assertion can't be satisfied by some
    // other image left over from a previous run.
    build_labelled_image(
        &client,
        &tag,
        &[(key, value), ("devconcurrent-image-test", &tag)],
    )
    .await;

    let listed = client
        .list_images()
        .with_label("devconcurrent-image-test", &tag)
        .call()
        .await
        .expect("list");

    let found = listed
        .iter()
        .find(|image| {
            image
                .repo_tags
                .iter()
                .any(|repo_tag| repo_tag_names(repo_tag, &tag))
        })
        .unwrap_or_else(|| {
            panic!(
                "labelled image should be listed; got {:?}",
                listed.iter().map(|i| &i.repo_tags).collect::<Vec<_>>()
            )
        });
    assert_eq!(found.labels.get(key).map(String::as_str), Some(value));

    client
        .remove_image(&tag)
        .force(true)
        .call()
        .await
        .expect("remove");

    assert!(
        matches!(
            client.inspect_image(&tag).await,
            Err(Error::NotFound { .. })
        ),
        "the tag should be gone after removal"
    );
}

/// A failed build is reported in the response stream, with a 200 status, so it
/// only surfaces as an error if the stream is actually inspected.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_build_is_an_error() {
    let client = Docker::connect().await.expect("connect");
    client.ensure_image(IMAGE).await.expect("ensure base image");

    let tag = unique_name().to_lowercase();
    let _cleanup = ImageCleanup(tag.clone());
    let mut output = Vec::new();

    let err = client
        .build_image(&tag)
        .context(
            build_single_file_tar(
                "Dockerfile",
                format!("FROM {IMAGE}\nRUN exit 3\n").as_bytes(),
            )
            .expect("tar"),
        )
        .on_output(&mut |line: &str| output.push(line.to_string()))
        .call()
        .await
        .expect_err("a build whose RUN fails should error");

    assert!(
        matches!(err, Error::Api { .. }),
        "expected Api, got {err:?}"
    );
    assert!(
        !output.is_empty(),
        "the steps leading up to the failure should have been reported",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_missing_image_returns_not_found() {
    let client = Docker::connect().await.expect("connect");
    let err = client
        .remove_image("docker-crate-test/no-such-image:zzz")
        .call()
        .await
        .expect_err("missing image should error");
    assert!(
        matches!(err, Error::NotFound { .. }),
        "expected NotFound, got {err:?}",
    );
}
