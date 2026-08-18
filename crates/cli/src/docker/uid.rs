//! `updateRemoteUserUID`: remap the remote user's uid/gid to the host user's.
//!
//! Without this, a container user whose uid differs from the host user's writes
//! foreign-owned files into anything bind-mounted from the host — most sharply
//! the `.git` directory that `mountGit` shares with the host, where it makes
//! host-side git start failing on its own repository.
//!
//! Done as an image layer rather than a `docker exec` after start, matching the
//! reference implementation. That ordering is what makes the `chown -R` in
//! [`UPDATE_UID_DOCKERFILE`] safe: at build time there are no bind mounts or
//! volumes under the home folder, so there is no host data for it to reach.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use docker::{Docker, build_single_file_tar};
use eyre::WrapErr;

use crate::devcontainer::DevcontainerConfig;
use crate::docker::compose::to_project_name;
use crate::run::cmd::NamedCmd;
use crate::run::{Runnable, Runner, Token};

const UPDATE_UID_DOCKERFILE: &str = include_str!("updateUID.Dockerfile");

/// A decided remap: which image to build, from what, and to which ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UidUpdate {
    /// Tag for the derived image.
    pub(crate) fixed_image: String,
    /// The user whose entry in `/etc/passwd` gets rewritten.
    pub(crate) remote_user: String,
    /// The base image's own `USER`, restored at the end of the layer so the
    /// derived image starts as the base image would have.
    pub(crate) image_user: String,
    pub(crate) platform: Option<String>,
    pub(crate) new_uid: u32,
    pub(crate) new_gid: u32,
}

/// Everything [`plan`] needs to know about the base image.
#[derive(Debug, Clone)]
pub(crate) struct BaseImage<'a> {
    /// The image's `Config.User`, empty when it doesn't set one.
    pub(crate) user: &'a str,
    pub(crate) platform: Option<String>,
}

/// Decide whether to remap, and under what name.
///
/// Mirrors the reference's `getRemoteUserUIDUpdateDetails`, including its skips:
/// a root or numeric remote user has nothing to remap.
///
/// Linux-only, like the reference. Docker Desktop on macOS and Windows already
/// reconciles ownership in its VM, so the uids differing there is expected and
/// rewriting them would be wrong — which is why this can't just test whether
/// the ids differ.
///
/// Nothing here checks whether the remote user's ids *already* match the host's:
/// that lives in `/etc/passwd` inside the image, and reading it costs as much as
/// building the layer. The Dockerfile detects the case itself and does nothing,
/// and the build is cached, so the redundant path is close to free.
pub(crate) fn plan(
    config: &DevcontainerConfig,
    base: &BaseImage<'_>,
    fixed_image: String,
    container_user: Option<&str>,
    remote_user: Option<&str>,
    host: (u32, u32),
) -> Option<UidUpdate> {
    plan_on(
        config,
        base,
        fixed_image,
        container_user,
        remote_user,
        host,
        cfg!(target_os = "linux"),
    )
}

/// [`plan`] with the platform gate as a parameter, so tests exercise the real
/// decision on any host.
fn plan_on(
    config: &DevcontainerConfig,
    base: &BaseImage<'_>,
    fixed_image: String,
    container_user: Option<&str>,
    remote_user: Option<&str>,
    host: (u32, u32),
    linux: bool,
) -> Option<UidUpdate> {
    if !config.update_remote_user_uid() || !linux {
        return None;
    }

    let image_user = if base.user.is_empty() {
        "root"
    } else {
        base.user
    };
    let remote_user = remote_user.or(container_user).unwrap_or(image_user);
    // A numeric user has no /etc/passwd entry to rewrite, and root is already
    // uid 0 everywhere.
    if remote_user == "root" || remote_user.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    let (new_uid, new_gid) = host;

    Some(UidUpdate {
        fixed_image,
        remote_user: remote_user.to_string(),
        image_user: image_user.to_string(),
        platform: base.platform.clone(),
        new_uid,
        new_gid,
    })
}

/// Tag for the derived image.
///
/// Built from the compose project rather than from the base image's own name:
/// the result is specific to this workspace's remote user and this host's ids,
/// so two workspaces sharing a base image must not share a tag. Re-running `up`
/// then overwrites one tag instead of accreting them.
///
/// The service is sanitized because it may legally carry characters an image
/// reference may not.
pub(crate) fn derived_image_name(project_name: &str, service: &str) -> String {
    format!("{project_name}-{}-uid", to_project_name(service))
}

pub(crate) fn host_ids() -> (u32, u32) {
    (
        rustix::process::getuid().as_raw(),
        rustix::process::getgid().as_raw(),
    )
}

/// Build the derived image.
///
/// The context holds nothing but the Dockerfile: it only ever touches the base
/// image's own filesystem, so sending the project would be pure upload cost.
///
/// The layer is one `RUN` keyed on the build args, so an unchanged uid/gid over
/// an unchanged base is a cache hit — this runs on every `dc up`. Which builder
/// serves that cache depends on the daemon:
///
/// - docker: shell out to `docker build`, i.e. BuildKit. The API's `/build` is
///   the legacy builder, whose cache is keyed on the base image *ID* — and with
///   the containerd image store that ID is an OCI index digest that BuildKit's
///   default provenance attestation churns on every `docker compose build`, so
///   the legacy cache missed on every `up` and re-ran the `chown`. BuildKit's
///   cache is content-addressed and doesn't care.
/// - podman: the API's `/build`, which is buildah. Podman has no buildx for a
///   shell-out to reach (the reason builds moved to the API at all), and no
///   attestation churn either. The reference CLI lands on the same builder by
///   shelling out to `podman build`.
pub(crate) async fn build(
    client: &Docker,
    update: &UidUpdate,
    base_image: &str,
    working_dir: &Path,
    labels: &[(&str, &str)],
) -> eyre::Result<()> {
    if client.is_podman() {
        Runner::run(BuildDerivedImage {
            client,
            update,
            base_image,
            labels,
        })
        .await
    } else {
        build_via_cli(update, base_image, working_dir, labels).await
    }
}

/// Build the image by shelling out to `docker build`.
async fn build_via_cli(
    update: &UidUpdate,
    base_image: &str,
    working_dir: &Path,
    labels: &[(&str, &str)],
) -> eyre::Result<()> {
    let (dockerfile, context) = write_build_inputs(working_dir)?;

    let mut cmd = tokio::process::Command::new("docker");
    cmd.arg("build")
        .arg("-f")
        .arg(&dockerfile)
        .args(["-t", &update.fixed_image]);
    if let Some(platform) = &update.platform {
        cmd.args(["--platform", platform]);
    }
    for (key, value) in labels {
        cmd.args(["--label", &format!("{key}={value}")]);
    }
    for arg in [
        format!("BASE_IMAGE={base_image}"),
        format!("REMOTE_USER={}", update.remote_user),
        format!("NEW_UID={}", update.new_uid),
        format!("NEW_GID={}", update.new_gid),
        format!("IMAGE_USER={}", update.image_user),
    ] {
        cmd.args(["--build-arg", &arg]);
    }
    cmd.arg(&context);

    let cmd = cmd.into_std().into();
    Runner::run(NamedCmd {
        name: "updateRemoteUserUID",
        cmd: &cmd,
        dir: None,
        // The service is pinned to this image, so `compose up`'s recreate
        // decision keys on *its* ID staying stable, not just the base's.
        env: crate::docker::build_env(),
    })
    .await
}

fn write_build_inputs(working_dir: &Path) -> eyre::Result<(PathBuf, PathBuf)> {
    let dockerfile = working_dir.join("updateUID.Dockerfile");
    // Named with a leading dot so it can't collide with a worktree, which
    // `State::worktree_path` also puts directly in the working dir.
    let context = working_dir.join(".uid-build-context");

    std::fs::create_dir_all(&context)
        .wrap_err_with(|| format!("failed to create {}", context.display()))?;
    std::fs::write(&dockerfile, UPDATE_UID_DOCKERFILE)
        .wrap_err_with(|| format!("failed to write {}", dockerfile.display()))?;

    Ok((dockerfile, context))
}

/// Build the image using the docker API.
struct BuildDerivedImage<'a> {
    client: &'a Docker,
    update: &'a UidUpdate,
    base_image: &'a str,
    labels: &'a [(&'a str, &'a str)],
}

impl Runnable for BuildDerivedImage<'_> {
    fn name(&self) -> Cow<'_, str> {
        "updateRemoteUserUID".into()
    }

    fn description(&self) -> Cow<'_, str> {
        format!("build {}", self.update.fixed_image).into()
    }

    async fn run(self, _: Token) -> eyre::Result<()> {
        // Same level and field the process runner reports command output with,
        // so a build looks the same as it did when this shelled out.
        let mut report = |line: &str| tracing::trace!(stdout = true, "{line}");

        let mut build = self
            .client
            .build_image(&self.update.fixed_image)
            .context(build_single_file_tar(
                "Dockerfile",
                UPDATE_UID_DOCKERFILE.as_bytes(),
            ))
            .maybe_platform(self.update.platform.as_deref())
            .on_output(&mut report)
            .with_build_arg("BASE_IMAGE", self.base_image)
            .with_build_arg("REMOTE_USER", &self.update.remote_user)
            .with_build_arg("NEW_UID", self.update.new_uid.to_string())
            .with_build_arg("NEW_GID", self.update.new_gid.to_string())
            .with_build_arg("IMAGE_USER", &self.update.image_user);
        for (key, value) in self.labels {
            build = build.with_label(*key, *value);
        }

        build
            .call()
            .await
            .wrap_err_with(|| format!("failed to build {}", self.update.fixed_image))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXED: &str = "ws_devcontainer-app-uid";

    fn base(user: &str) -> BaseImage<'_> {
        BaseImage {
            user,
            platform: Some("linux/amd64".to_string()),
        }
    }

    fn config(update: Option<bool>) -> DevcontainerConfig {
        DevcontainerConfig {
            update_remote_user_uid: update,
            ..Default::default()
        }
    }

    #[test]
    fn plans_a_remap_for_a_named_user() {
        let update = plan_on(
            &config(None),
            &base(""),
            FIXED.to_string(),
            None,
            Some("vscode"),
            (501, 20),
            true,
        )
        .expect("named user should remap");

        assert_eq!(update.remote_user, "vscode");
        assert_eq!(update.image_user, "root");
        assert_eq!(update.fixed_image, FIXED);
        assert_eq!((update.new_uid, update.new_gid), (501, 20));
    }

    #[test]
    fn remote_user_wins_over_container_user_wins_over_image_user() {
        let b = base("imageuser");
        let cfg = config(None);
        let plan = |container, remote| {
            plan_on(&cfg, &b, FIXED.to_string(), container, remote, (1, 1), true)
                .map(|update| update.remote_user)
        };

        assert_eq!(
            plan(Some("containeruser"), Some("remoteuser")).as_deref(),
            Some("remoteuser")
        );
        assert_eq!(
            plan(Some("containeruser"), None).as_deref(),
            Some("containeruser")
        );
        assert_eq!(plan(None, None).as_deref(), Some("imageuser"));
    }

    #[test]
    fn skips_root_and_numeric_users() {
        let cfg = config(None);
        let plan = |image_user, remote| {
            plan_on(
                &cfg,
                &base(image_user),
                FIXED.to_string(),
                None,
                remote,
                (501, 20),
                true,
            )
        };

        assert_eq!(
            plan("", None),
            None,
            "an image with no USER runs as root, which is already uid 0"
        );
        assert_eq!(plan("", Some("root")), None, "explicit root, same reason");
        assert_eq!(
            plan("", Some("1000")),
            None,
            "a numeric user has no passwd entry to rewrite"
        );
    }

    #[test]
    fn opting_out_skips_the_remap() {
        assert_eq!(
            plan_on(
                &config(Some(false)),
                &base(""),
                FIXED.to_string(),
                None,
                Some("vscode"),
                (501, 20),
                true,
            ),
            None
        );
    }

    #[test]
    fn derived_tag_sanitizes_the_service() {
        assert_eq!(
            derived_image_name("ws_devcontainer", "app"),
            "ws_devcontainer-app-uid"
        );
        // A compose service may carry characters an image reference may not.
        assert_eq!(
            derived_image_name("ws_devcontainer", "My.Svc"),
            "ws_devcontainer-mysvc-uid"
        );
    }

    #[test]
    fn skips_off_linux() {
        assert_eq!(
            plan_on(
                &config(None),
                &base(""),
                FIXED.to_string(),
                None,
                Some("vscode"),
                (501, 20),
                false,
            ),
            None,
            "Docker Desktop reconciles ownership itself; remapping would be wrong"
        );
    }
}

/// Live-daemon coverage of the actual remap.
///
/// Gated behind the `docker-tests` feature; needs a live daemon.
///
/// Ownership is always read with `stat` from *inside* a container, never from
/// the host: CI runs these against rootless podman too, where a user namespace
/// makes the host-side uid of a container-owned file something else entirely.
#[cfg(all(test, feature = "docker-tests"))]
mod docker_tests {
    use docker::test_support::repo_tag_names;
    use rand::distr::{Alphanumeric, SampleString};

    use super::*;

    /// Same label the `docker` crate's tests use, so `just test`'s trailing
    /// sweep picks up anything a panic leaves behind.
    const TEST_LABEL: &str = "devconcurrent-docker-crate-test=true";
    const OLD_UID: u32 = 1000;
    const NEW_UID: u32 = 4242;

    /// Removes an image on drop, including on panic.
    struct ImageCleanup(String);

    impl Drop for ImageCleanup {
        fn drop(&mut self) {
            let _ = std::process::Command::new("docker")
                .args(["image", "rm", "-f", &self.0])
                .output();
        }
    }

    /// Panics with the daemon's stderr, which is the only useful part when a
    /// docker invocation in a test fails.
    fn docker(args: &[&str]) -> String {
        let out = std::process::Command::new("docker")
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("docker {args:?}: {e}"));
        assert!(
            out.status.success(),
            "docker {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn unique_tag(prefix: &str) -> String {
        let suffix = Alphanumeric
            .sample_string(&mut rand::rng(), 16)
            .to_lowercase();
        format!("devconcurrent-uid-test-{prefix}-{suffix}")
    }

    /// An alpine image with a uid-1000 `dev` user, a file in its home, and
    /// `USER dev` — i.e. the shape of devcontainer image this feature exists for.
    async fn build_base_image(client: &Docker) -> (String, ImageCleanup) {
        let tag = unique_tag("base");
        let dockerfile = format!(
            "FROM alpine:3.20\n\
             RUN adduser -D -u {OLD_UID} dev \\\n\
             && install -o dev -g dev /dev/null /home/dev/owned\n\
             USER dev\n"
        );
        let (key, value) = TEST_LABEL.split_once('=').expect("TEST_LABEL is key=value");

        client
            .build_image(&tag)
            .context(build_single_file_tar("Dockerfile", dockerfile.as_bytes()))
            .with_label(key, value)
            .call()
            .await
            .expect("build the base image");

        (tag.clone(), ImageCleanup(tag))
    }

    /// Read ownership from inside a container, with `mounts` applied as `-v`.
    fn owner_of(image: &str, mounts: &[String], path: &str) -> String {
        let mut args = vec!["run", "--rm", "--label", TEST_LABEL, "-u", "0"];
        for mount in mounts {
            args.extend(["-v", mount]);
        }
        args.extend([image, "stat", "-c", "%u:%g", path]);
        docker(&args)
    }

    fn test_labels() -> [(&'static str, &'static str); 1] {
        let (key, value) = TEST_LABEL.split_once('=').expect("TEST_LABEL is key=value");
        [(key, value)]
    }

    fn update(fixed_image: String) -> UidUpdate {
        UidUpdate {
            fixed_image,
            remote_user: "dev".to_string(),
            image_user: "dev".to_string(),
            platform: None,
            new_uid: NEW_UID,
            new_gid: NEW_UID,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remaps_the_remote_user_without_touching_mounted_host_data() {
        let client = Docker::connect().await.expect("connect");
        let (base_image, _base_guard) = build_base_image(&client).await;

        let fixed_tag = unique_tag("fixed");
        let _fixed_guard = ImageCleanup(fixed_tag.clone());
        let update = update(fixed_tag.clone());
        let working_dir = tempfile::tempdir().expect("tempdir");
        build(
            &client,
            &update,
            &base_image,
            working_dir.path(),
            &test_labels(),
        )
        .await
        .expect("build the uid layer");

        // A stand-in for a host directory bind-mounted into the container, at
        // the path a careless `chown -R $HOME` would sweep through. Its
        // contents are owned by the *old* container uid, which is the case a
        // uid-filtered chown would still have eaten.
        let mount_dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(mount_dir.path().join("host-file"), "").expect("write host file");
        let mount = format!(
            "{}:/home/dev/mounted",
            mount_dir.path().to_str().expect("utf-8 tempdir")
        );
        docker(&[
            "run",
            "--rm",
            "--label",
            TEST_LABEL,
            "-u",
            "0",
            "-v",
            &mount,
            &base_image,
            "chown",
            "-R",
            &format!("{OLD_UID}:{OLD_UID}"),
            "/home/dev/mounted",
        ]);

        let mounts = [mount];

        assert_eq!(
            docker(&["run", "--rm", "--label", TEST_LABEL, &fixed_tag, "id", "-u"]),
            NEW_UID.to_string(),
            "the image's default user should now be the remapped uid"
        );
        assert_eq!(
            owner_of(&fixed_tag, &[], "/home/dev/owned"),
            format!("{NEW_UID}:{NEW_UID}"),
            "the home folder should be chowned to the new ids"
        );

        assert_eq!(
            owner_of(&base_image, &[], "/home/dev/owned"),
            format!("{OLD_UID}:{OLD_UID}"),
            "the base image should not be mutated in place"
        );

        // The no-corruption assertion: the remap is a build-time layer, so it
        // never sees a mount. A `docker exec` implementation, or any runtime
        // `chown -R $HOME`, would rewrite both of these.
        assert_eq!(
            owner_of(&fixed_tag, &mounts, "/home/dev/mounted/host-file"),
            format!("{OLD_UID}:{OLD_UID}"),
            "mounted host data must keep its ownership"
        );
        assert_eq!(
            owner_of(&fixed_tag, &mounts, "/home/dev/mounted"),
            format!("{OLD_UID}:{OLD_UID}"),
            "the mount point itself must keep its ownership"
        );
    }

    /// `destroy` finds the derived image by label, not by name, so the build
    /// has to actually stamp them on.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_derived_image_is_labelled_for_destroy_to_find() {
        let client = Docker::connect().await.expect("connect");
        let (base_image, _base_guard) = build_base_image(&client).await;

        let fixed_tag = unique_tag("labelled");
        let _fixed_guard = ImageCleanup(fixed_tag.clone());
        let labels = [
            (docker::MANAGED_LABEL, "true"),
            (docker::PROJECT_LABEL, "uid-test-project"),
            (docker::WORKSPACE_LABEL, fixed_tag.as_str()),
        ];
        let working_dir = tempfile::tempdir().expect("tempdir");
        build(
            &client,
            &update(fixed_tag.clone()),
            &base_image,
            working_dir.path(),
            &labels,
        )
        .await
        .expect("build the uid layer");

        let found = client
            .list_images()
            .with_label(docker::PROJECT_LABEL, "uid-test-project")
            .with_label(docker::WORKSPACE_LABEL, &fixed_tag)
            .call()
            .await
            .expect("list images by label");

        assert!(
            found.iter().any(|image| image
                .repo_tags
                .iter()
                .any(|repo_tag| repo_tag_names(repo_tag, &fixed_tag))),
            "the labelled query should find the derived image; got {:?}",
            found.iter().map(|i| &i.repo_tags).collect::<Vec<_>>()
        );

        // And the round trip `destroy` performs actually removes it.
        for image in &found {
            client
                .remove_image(&image.id)
                .force(true)
                .call()
                .await
                .expect("remove by id");
        }
        assert!(
            client.inspect_image(&fixed_tag).await.is_err(),
            "the image should be gone after removal"
        );
    }

    /// The point of the whole feature: files the container writes into a
    /// bind-mounted host directory come out owned by the host user, which is
    /// what stops host-side git choking on the `.git` that `mountGit` shares.
    #[tokio::test(flavor = "multi_thread")]
    async fn container_writes_land_with_host_ownership() {
        let client = Docker::connect().await.expect("connect");
        let (base_image, _base_guard) = build_base_image(&client).await;

        let fixed_tag = unique_tag("fixed");
        let _fixed_guard = ImageCleanup(fixed_tag.clone());
        let working_dir = tempfile::tempdir().expect("tempdir");
        build(
            &client,
            &update(fixed_tag.clone()),
            &base_image,
            working_dir.path(),
            &test_labels(),
        )
        .await
        .expect("build the uid layer");

        // Stands in for the host's checkout: owned by the ids we remapped onto.
        let repo = tempfile::tempdir().expect("tempdir");
        let mount = format!("{}:/repo", repo.path().to_str().expect("utf-8 tempdir"));
        docker(&[
            "run",
            "--rm",
            "--label",
            TEST_LABEL,
            "-u",
            "0",
            "-v",
            &mount,
            &base_image,
            "chown",
            &format!("{NEW_UID}:{NEW_UID}"),
            "/repo",
        ]);

        docker(&[
            "run",
            "--rm",
            "--label",
            TEST_LABEL,
            "-v",
            &mount,
            &fixed_tag,
            "touch",
            "/repo/written-by-container",
        ]);

        assert_eq!(
            owner_of(&fixed_tag, &[mount], "/repo/written-by-container"),
            format!("{NEW_UID}:{NEW_UID}"),
            "without the remap this would be {OLD_UID}, which the host cannot own"
        );
    }
}
