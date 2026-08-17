check: lint test check-schema

fix: _fix check

run *args:
    cargo run --bin devconcurrent -- {{args}}

# Render VHS tapes.
tape *names:
    # Put the current build of `devconcurrent` in the path.
    cargo build -q --bin devconcurrent
    bin_dir="$(cargo metadata --format-version=1 --no-deps | jq -r .target_directory)/debug"; \
    for tape in {{ if names == "" { "tapes/*.tape" } else { names } }}; do \
        [ -e "$tape" ] || tape="tapes/$tape.tape"; \
        PATH="$bin_dir:$PATH" vhs "$tape"; \
    done

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
    cargo nextest run --workspace --all-features --no-fail-fast {{args}}
    docker ps -aq --filter "label=devconcurrent-docker-crate-test=true" | xargs -r docker rm -f
    
up:
    nix flake update
    cargo upgrade -i

_fix:
    just gen
    cargo clippy --all-features --all-targets --workspace --fix --allow-staged
    cargo fmt
    tombi format
    rumdl fmt

gen:
    cargo run -q -p gen

# Validate the generated JSON Schema with Ajv in strict mode.
check-schema:
    npx --yes --package=ajv-cli ajv compile -s docs/src/devconcurrent.schema.json \
        -c ./ajv.config.js --spec=draft7 --strict=true

lint:
    cargo fmt --all -- --check
    cargo clippy --all-features --all-targets --workspace -- -D warnings
    tombi lint
    rumdl check

# Relase; pass any valid `set-version` args. Example: just release --bump minor
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

schema: schema-gen schema-open

schema-gen:
    npx @adobe/jsonschema2md -d schemas -o schemas/out -x schemas/out

    fd -e md . schemas/out -x pandoc {} --from=gfm --standalone \
        --lua-filter=schemas/md-to-html-links.lua \
        --css=https://cdn.simplecss.org/simple.min.css \
        -o {.}.html

schema-open:
    xdg-open schemas/out/devcontainer.html
