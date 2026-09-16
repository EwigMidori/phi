# JavaScript console inspection

`object-inspect/index.js` and `object-inspect/LICENSE` are unmodified files from
[object-inspect v1.13.4](https://github.com/inspect-js/object-inspect/tree/v1.13.4),
licensed under MIT. The library formats objects, arrays, cycles and JavaScript
special values; the worker does not implement its own object printer.

SHA-256 of the upstream files:

- `index.js`: `9aad508be54fbe29d82145d54352e7a19c01e19b70e1202b0123449fe9afcde6`
- `LICENSE`: `bd40cc437e28a3ad7bef2ad34e6b72e757b182e67bda1acadbab4ef0476f8232`

The worker loads the CommonJS export inside a private closure. Its only require,
`./util.inspect`, receives an empty module, matching upstream's `browser` mapping
to `false`. No Node APIs, global `require`, npm installation or bundling step is
needed to build or run the Rust worker, including the embedded mobile engine.

Upgrade by replacing both files from an explicit upstream release, updating the
hashes, checking the browser mapping, and running the worker behavior tests. Do
not patch vendored sources. Integration and output policy live in `src/runtime.js`.

Console inspection is text, with depth 8 and no colors. Plain string arguments
remain unquoted. It is not JSON serialization or a full Node console API (for
example, printf-style placeholders are not expanded). A value that throws during
inspection produces `[Inspection failed]`; other arguments and the computation
continue. The existing runtime memory/time limits and UTF-8 output cap still apply.
