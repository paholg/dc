schema: schema-gen schema-open

schema-gen:
    npx @adobe/jsonschema2md -d schemas -o schemas/out -x schemas/out

    fd -e md . schemas/out -x pandoc {} --from=gfm --standalone \
        --lua-filter=schemas/md-to-html-links.lua \
        --css=https://cdn.simplecss.org/simple.min.css \
        -o {.}.html

schema-open:
    xdg-open schemas/out/devcontainer.html
