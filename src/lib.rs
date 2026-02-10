#![forbid(unsafe_code)]

pub mod ansi;
pub mod bytes;
pub mod cli;
pub mod config;
pub mod devcontainer;
pub mod docker;
pub mod preflight;
pub mod run;
pub mod subscriber;
pub mod workspace;
pub mod worktree;

#[cfg(test)]
mod test {
    // We need at least 1 test to make cargo-nextest happy. Remove when we have
    // real tests.
    #[test]
    fn test() {}
}
