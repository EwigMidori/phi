# phi-ext-tools

Host-bound tool executors with owned asynchronous shutdown. The registry owns each
schema and its executable binding together. The model adapter sends raw arguments;
unknown tools, invalid JSON, invalid code inputs, and interpreter failures return
typed tool outcomes.

Each generation obtains `registry.scope(cancel)`. It awaits `scope.execute(name,
arguments)` for each call. On interruption, `scope.close_and_join()` cancels that
generation and waits for its executions. On successful completion, `scope.join()`
waits without cancelling the turn. Dropping the consumer future does not abandon
the independently owned execution task.

`ProcessSupervisor` starts executables directly, drains bounded stdout/stderr,
and kills and reaps the process group / Windows Job Object before returning. The
Windows child is assigned to its Job while suspended, before user code can spawn
descendants. Both timeout and cancellation follow the same cleanup path. This is
process management, not a security sandbox.

`JavaScriptExecutor` uses the separately packaged `phi-code-worker`; each call has
fresh globals. `PythonExecutor` receives a host's `PythonEnvironment` and holds its
`PythonLease` through process cleanup. Runtime locations, downloads, environment
repair, dependency choices, and default execution limits belong to the host.

Tests cover real process-tree cancellation, deadlines, output flooding, cleanup
after a leader exits, and retained ownership after the consuming stream is dropped.
The desktop composition root additionally has a networked managed-Python smoke
example which checks fresh venv provisioning, shared preparation cancellation,
package installation/reuse, calculation, explicit repair, and strict existing files.

`scope_for(session, cancel)` supplies execution identity to host-bound artifact services. Python receives an `artifacts.publish(relative_path, name=...)` helper and optional `inputs` staging by immutable artifact ID. Output collection happens after process-tree cleanup and before temporary-directory removal, only on successful execution. It is bounded to 16 files / 32 MiB per invocation; paths outside the run directory are rejected. Host storage commits before metadata references are returned. Standalone executors without an artifact service still compute, but publishing/reading artifacts fails explicitly. Matplotlib uses the headless Agg backend. Library versions and supported output formats remain host choices.

Within `run_python`, `artifacts` is a preloaded, invocation-local Python module.
Direct `artifacts.publish(...)`, `import artifacts`, and `from artifacts import publish`
all use the same publisher and limits. No pip installation is needed. It is not a
package installed into the environment for separately launched Python processes.
Run the bootstrap behavior tests with
`python -I crates/phi-ext-tools/tests/python_bootstrap_test.py`.
