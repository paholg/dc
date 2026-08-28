"use strict";

// Render a `filename=...` code-fence annotation as a header bar on the block.
//
// Usage in markdown:
//
//     ```json,filename=devcontainer.json
//
// mdbook splits the fence info string on commas into classes on the <code>
// element, so the annotation arrives as a `filename=...` class here. Because
// it travels as a single class, the filename cannot contain spaces.
(function () {
    for (const code of document.querySelectorAll("pre > code")) {
        for (const cls of code.classList) {
            if (cls.startsWith("filename=")) {
                const header = document.createElement("div");
                header.className = "code-filename";
                header.textContent = cls.slice("filename=".length);
                code.parentElement.insertBefore(header, code);
                break;
            }
        }
    }
})();
