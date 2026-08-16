check: lint test

fix: _fix check

run *args:
    cargo run --bin devconcurrent -- {{args}}

# Build the book.
docs:
    mdbook build docs

# Serve the book locally, rebuilding on change.
docs-serve:
    mdbook serve docs --open

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

test *args:
    cargo nextest run --all-features --no-fail-fast {{args}}
    docker ps -aq --filter "label=devconcurrent-docker-crate-test=true" | xargs -r docker rm -f
    
up:
    nix flake update
    cargo upgrade -i

_fix:
    cargo clippy --all-features --all-targets --fix --allow-staged
    cargo fmt
    tombi format
    rumdl fmt

lint:
    cargo fmt --all -- --check
    cargo clippy --all-features --all-targets -- -D warnings
    tombi lint
    rumdl check

release version:
    git diff --exit-code
    cargo set-version -p devconcurrent -p devconcurrent-proxy {{version}}
    just check
    git add -u
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
