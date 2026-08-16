#![forbid(unsafe_code)]

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    // Keep writing to a closed pipe from panicking.
    sigpipe::reset();

    devconcurrent::cli_main().await
}
