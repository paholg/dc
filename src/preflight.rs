use eyre::bail;

pub fn check() -> eyre::Result<()> {
    if duct::cmd!("docker", "version", "--format", "{{.Client.Version}}")
        .stderr_null()
        .read()
        .is_err()
    {
        bail!(
            "docker is not installed or the daemon is not running.\nInstall Docker: https://docs.docker.com/get-docker/"
        );
    }

    if duct::cmd!("docker", "compose", "version", "--short")
        .stderr_null()
        .read()
        .is_err()
    {
        bail!(
            "docker compose (v2) is not available.\nInstall the Compose plugin: https://docs.docker.com/compose/install/"
        );
    }

    if duct::cmd!("docker", "buildx", "version")
        .stderr_null()
        .read()
        .is_err()
    {
        bail!(
            "docker buildx is not available.\nInstall the Buildx plugin: https://docs.docker.com/build/install-buildx/"
        );
    }

    Ok(())
}
