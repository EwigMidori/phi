1. Protect existing users' persisted data when changing formats or models. Provide explicit, tested migrations and rollback on failure. Keep one current runtime model rather than long-lived parallel implementations. Reject corrupt data or unsupported versions clearly; never silently reset or delete user files.

2. Pick the simplest implementation that meets the current needs. No premature abstraction, no unnecessary config layers.

3. Layer the system gradually. Get a minimal end-to-end version running first, then build on it. Never tear down working code for unfinished complexity.

4. Keep components modular, with separation of concerns.

5. Prioritize mature, maintained libraries. Don't rewrite unless there's a damn good reason.

6. First, check what your project's existing dependencies can do—then think about adding new packages or writing from scratch. Don't assume the libs are missing it right off the bat.

7. Make architecture decisions for the long haul. No "we'll swap it out later" half-measures.

8. See how mature products solve the same problem—use proven patterns, don't invent from scratch.

9. Implement changes in a dedicated Git worktree on a task branch (default prefix: `codex/`). After validation, commit and merge into the primary branch (`main` here). Never leave completed, effective code uncommitted unless the user explicitly requests it. When used as a submodule, commit and merge here before committing the parent repository's updated gitlink. Push only when requested.
