run *args: 
    cargo run -- {{args}}

test *args:
    cargo nextest run --no-fail-fast {{args}}

up:
    nix flake update
    cargo upgrade -i

fix: clippy-fix lint test

clippy-fix:
    cargo clippy --fix --allow-staged
    cargo fmt

check: lint test

lint: fmt-check clippy

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy -- -D warnings

release version:
    git diff --exit-code
    cargo set-version {{version}}
    just lint
    just test
    git add Cargo.toml Cargo.lock
    git commit -m "Version {{version}}"
    git tag v{{version}}
    git push
    git push --tags

schema: schema-gen schema-open

schema-gen:
    npx @adobe/jsonschema2md -d schemas -o schemas/out -x schemas/out

    fd -e md . schemas/out -x pandoc {} --from=gfm --standalone \
        --lua-filter=schemas/md-to-html-links.lua \
        --css=https://cdn.simplecss.org/simple.min.css \
        -o {.}.html

schema-open:
    xdg-open schemas/out/devcontainer.html
