# Run all checks
check: gen lint test check-schema

# Fix what can be auto-fixed; must stage files first
fix: _fix check

# Run the CLI
run *args:
    cargo run --bin devconcurrent -- {{args}}

# Serve the book locally, rebuilding on change
[parallel]
book: watch-gen watch-book watch-readme

[continue, private]
watch-book:
    # mdbook exits 130 on sigint :(
    trap ':' INT; mdbook serve docs --open || test $? -eq 130

[continue, private]
watch-gen:
    watchexec --watch crates --watch docs/snippets --exts rs,md -- just gen

[continue, private]
watch-readme:
    gh-markdown-preview README.md

# Build the proxy image, tag it, then run it.
proxy-up:
    nix run .#docker-service-image.copyToDockerDaemon
    v=$(cargo pkgid -p devconcurrent-proxy | sed 's/.*[@#]//'); \
    docker tag "devconcurrent-proxy:$v" "ghcr.io/paholg/devconcurrent-proxy:$v" && \
    echo "Tagged ghcr.io/paholg/devconcurrent-proxy:$v"
    just run proxy up

# Clear proxy images
proxy-clear:
    docker images --format '{{{{.Repository}}:{{{{.Tag}}' \
        | grep -E '(^|/)devconcurrent-proxy:' \
        | xargs -r docker rmi -f

[private]
test *args:
    cargo nextest run --workspace --all-features --no-fail-fast {{args}}
    # Cleanup test docker artifacts
    docker ps -aq --filter "label=devconcurrent-docker-crate-test=true" | xargs -r docker rm -f
    
# Update dependencies
up:
    nix flake update
    cargo upgrade -i

_fix:
    just gen
    cargo clippy --all-features --all-targets --workspace --fix --allow-staged
    cargo fmt
    tombi format
    rumdl fmt

[private]
gen:
    cargo run -q -p gen

# Validate the generated JSON Schema with Ajv in strict mode.
[private]
check-schema:
    npx --yes --package=ajv-cli ajv compile -s docs/src/devconcurrent.schema.json \
        -c ./ajv.config.js --spec=draft7 --strict=true

[private]
lint:
    cargo fmt --all -- --check
    cargo clippy --all-features --all-targets --workspace -- -D warnings
    tombi lint
    rumdl check

# Release; pass any valid `set-version` args. Example: just release --bump minor
release *args:
    git diff --exit-code
    cargo set-version {{args}}
    just check
    v=$(cargo pkgid -p devconcurrent | sed 's/.*[@#]//'); \
    git add -u && \
    git commit -m "Version $v" && \
    git tag "v$v" && \
    git push && \
    git push --tags

# Manage the external devcontainer.json schema
schema: schema-gen schema-open

[private]
schema-gen:
    npx @adobe/jsonschema2md -d schemas -o schemas/out -x schemas/out

    fd -e md . schemas/out -x pandoc {} --from=gfm --standalone \
        --lua-filter=schemas/md-to-html-links.lua \
        --css=https://cdn.simplecss.org/simple.min.css \
        -o {.}.html

[private]
schema-open:
    xdg-open schemas/out/devcontainer.html
