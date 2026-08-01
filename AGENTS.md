# Agent Guidelines

Follow these guidelines when authoring code in this repo.

## Documentation and Comments

Keep the epistemic state of the session (steering, feedback, mid-exploration uncertainty) out of the artifact. Docs must reflect the final evidence, not the journey.

- Bad: "We're not sure yet whether X handles Y."
- Good: "X does not handle Y; see issue #123."

References in docs must point to durable locations a future reader can reach — never to session-local or throwaway artifacts (prototypes, PoCs, superseded implementations). If a fact from a throwaway matters, restate it in self-contained form.

Throwaways may contain salvage: data, generic test scripts, and reusable instruments with forward utility can be promoted into the repo on their own merits. Never preserve a throwaway wholesale, and never preserve anything solely to make a doc reference resolve.

- Bad: "This is a port of `prototype.py` from the PoC, with two things the Python could not do:"
- Also bad: committing `prototype.py` into the repo so the reference resolves.
- Good: "Two constraints shaped this design that a Python implementation could not satisfy:" — with the prototype's test data promoted to `tests/fixtures/` because the tests use it.

Comments earn their place where the code cannot speak for itself. Two sites require them:

- **Error-log emission sites.** The log message is product: it carries verified facts only. The *why* is speculative — it needs a repro to confirm — so it lives in a comment at the emission site instead: enumerate the reasons this error can occur, cheapest-to-rule-out first. An investigator greps the message, lands here, and starts working backwards; the comment is their head start, and the simple cause checked first is often the answer.
- **External behavior claims.** Any claim about a protocol, dependency behavior, or system behavior cites its source in the comment — RFC, manual, official docs. When the source is itself versioned software (kernel source, a dependency's code), pin the version or commit; a file path alone floats while the behavior drifts. Uncited claims about external behavior are speculation; the citation is what makes them verifiable when versions drift.

Keep technical writing brief and concise. When presenting data, use tables and let the reader draw conclusions rather than editorializing. Prefer mermaid charts over ASCII art.

## Debugging

Avoid speculating or reasoning about runtime state abstractly — observe it. Use concrete data (log output or throwaway prints) to form conclusions, diagnose issues, and validate hypotheses. Prefer capturing invariants in unit tests over one-off verification: a test outlives the investigation. Throwaway prints that prove useful should graduate into permanent logs at the appropriate severity; delete the rest. Never ship a raw print — it bypasses the logging infra.

A debugger is a last resort: it can perturb the state under observation and behaves inconsistently across versions. Before reaching for one, isolate the suspect behavior in a unit test; if a debugger is still needed, run it against that test.

Trust but verify dependencies: read their source as a guide, but confirm actual behavior with a sanity check rather than concluding from the source alone.

## Logging

Emit log messages in key places. Code with no logging is a defect — when in doubt, add the log; the severity guidelines below govern where it goes.

Logging infra is zero-cost when disabled. Never omit a log for performance reasons; pick the right severity instead (high-frequency sites go to trace).

### Customer-facing severities: error, warning, info

These logs are part of the product. Assume they are shared when an external user hits an issue outside this repo, and that they persist after handoff, when no one is left to explain them. Keep them correct and tight. Balance emission frequency against each severity's purpose — a severity emitted too often stops serving its purpose.

- **info** — The prime audit severity. Auditable actions are system or file operations usable as an attack surface (creating, removing, or modifying a file). Leave as much of a trail as possible. Litmus test: if the audit log would be emitted too frequently, it is likely debug, not info.

- **warning** — Purpose: demonstrate defensive code behaved correctly under a corner case or fallback. Use infrequently. Trap to avoid: a support engineer reading a benign warning will investigate it. NEVER emit warnings where errors are used for control flow (e.g., container registry auth flows begin with a 401 or token expiration — debug is valid there, warning is not).

- **error** — Critical, non-recoverable runtime errors only. A 401 from an HTTP request is typically recoverable; exhausting the retries of an auth loop is not. Errors can expose state, stack traces, or object internals — do not rely on library Debug implementations to censor sensitive data (reqwest response objects censor sensitive headers; most types do not). Syscall-adjacent functions returning system errors are especially useful edges, as these typically require remediation outside the application process. An error log must carry enough information to produce the next theory to investigate.

### Developer-facing severities: debug, trace

Internal instruments, optimized for developer experience. We never ask a customer to enable these — we get a repro and enable them ourselves. Debug tells us the flow of a program; trace tells us about performance at key sites.

- **debug** — Log state transitions/mutations deeper in the stack, useful for reasoning about runtime state or validating that assumed invariants hold. Write full sentences with variables formatted in place, so someone new to the codebase understands the message with minimal context. Log entire objects with Debug formatting, for types known not to contain secrets.

- **trace** — For metric gathering and high-frequency data used to identify trends. Message is a one-word event description; state (durations, counts) goes in parameters. Noisier than debug by design. Keep traces at the deepest parts of the stack, ideally concentrated in single performance-critical files so they can be targeted when enabled.

## API Design

All public APIs must have a doc-header. All public types must document each field.

## Source Control Conventions

Never push without explicit prior approval.

Follow the Conventional Commits specification. Keep messages brief, preferably one sentence. Do not inventory the diff in the message — the diff is the inventory. State the intent or the most important aspect and let the commit itself do the rest of the work.

Prefer rebase over merge to keep history linear when updating a branch from upstream. Rebase only your own feature branches; when a push is approved for a rebased branch, use `--force-with-lease`, never `--force`.

## Housekeeping

This is required before a branch is ready for a PR. Mid-session mess is fine; a PR is not where it lives.

### Objective

The original request is the root objective for the branch, and it does not move. Bugs and placeholders encountered along the way are subtasks in service of it — fixing them never redefines what the branch is for. Detours must return: after resolving a side issue, resume the root objective from where you left it.

Completion and the final report are judged against the root objective, stated in its original wording — not against the most recent thing worked on. A session that resolved detours but not the root objective is *not done*, however productive it was.

If a subtask is substantial enough to risk displacing the root objective, prefer delegating it to a subagent working in its own worktree — one of sufficient quality for the subtask's stakes. The subagent is held to this document; its work merges back only when it meets the Completion bar.

You are accountable for delegated work: once merged, it is yours, and the PR answers for all of it. Review subagent work at merge time and fix issues you spot directly — you hold the review context; re-delegating discards it. Only if the work is fundamentally wrong does it go back, rerun with a corrected brief. Sequence integration last: complete the root objective first, then review and merge delegated work — integration is itself a subtask and does not displace the root.

### Repository Layout

Only these top-level directories exist; do not create new ones.

- `src/` — product source (standard Cargo layout; tests inline and in `tests/`).
- `docs/` — developer-facing documentation.
- `book/` — the user manual; end-user facing.
- `lib/` — non-product code and assets that ship with the product (e.g., shell scripts a Rust binary is packaged with), organized by artifact type (`lib/shell`, `lib/<domain>/<thing>`). Nested sub-projects live here too (`lib/elm`, `lib/html`).
- `tools/` — non-product code that is not shipped: anything useful during development or called by the Makefile. A tool that needs to ship gets promoted to `lib/`; never place things in `lib/` speculatively.

### Languages

Scripts are bash. Anything that outgrows shell becomes a small Rust project in `tools/` — never Python. Rust is the only language beyond bash in this repo.

### Known Bugs

Solve problems; don't inventory them. A bug encountered during the session is in scope regardless of your assignment — fix it as a real engineer would, even when the fix cascades. Large diffs are acceptable; a simple task legitimately becomes a large change sometimes. The constraint is coherence, not size: everything you start must be finished, and the branch must tell one story.

Deferring a bug is the exception and carries a burden of proof: it needs a concrete blocker a reviewer would accept (requires a design decision, blocked on external input) — "it wasn't related to my work" is never one. A deferred bug is surfaced once, explicitly, with the blocker named. Never leave a laundry list.

### Lint

The branch passes lint cleanly. Do not suppress warnings to get there; fix the cause or surface why it can't be fixed.

### Completion

"Done" means verified by execution: the feature runs end-to-end against real behavior, observed working — not "the code exists." A placeholder (`todo!()`, stub, hardcoded value, mocked data) in any requested path means the feature is *not done*, and must never be reported as done.

Placeholders that remain at PR time are declared loudly: what is stubbed, where, and what real behavior is missing. Never let a report look green because a stub returned green.

The final report distinguishes *verified* (executed, observed) from *implemented but unverified* from *not done*. Overstating completion is worse than incomplete work: incomplete work costs a session; a false "done" costs trust in every future report.



## Tests

Tests are not free — each one incurs maintenance. The goal is not maximal tests but sufficient confidence: enough coverage that an "easy" miss (the one line you forgot) surfaces before a bug report does. Treat test code as a discipline of its own, distinct from product code; tests are the first line of defense and must cover actual runtime conditions as closely as possible.

### Placement

Test system edges with integration-style tests. Test invariants and control logic with unit tests.

### The Cheapest Test Is the One You Don't Own

Every line we write is a line we must test; a maintained crate arrives with its testing already paid for. Code we write is a liability we own forever — correctness we lease is not. This applies at three layers:

- **Design.** Design product code with testability in mind. A design decision that makes testing natural beats any volume of tests compensating for one that doesn't — e.g., adopting `tower` to make backends swappable/mockable is worth the dependency, versus a thousand tests and a custom mock harness simulating the environment.
- **Product code.** Adding a dependency is not a last resort — hand-rolling a solved problem is. Before implementing anything that is a known, named problem (async runtimes, caches, eviction policies, parsers, retry/backoff, data structures, protocol clients), search the ecosystem first. Rolling our own requires justification a reviewer would accept (unmaintained crate, genuinely novel problem, demonstrably wrong fit); "avoiding a dependency" is never one. A hand-rolled solved problem is a defect: it ships unreviewed, unfuzzed, and unbenchmarked where the crate ships battle-tested. Genuinely beating the crate is allowed — but "better" is demonstrated, not asserted: it means matching the crate's evidence (tests, fuzzing, benchmarks) plus a shown fit advantage, and owning that bill permanently. In practice this is credible for small purpose-specific components and nearly never for general frameworks — write a better narrow layer, not a better tower.
- **Harnesses.** Prefer cargo's ecosystem for test, bench, and fuzz harnesses. We do not own the problem of writing a correct harness. If nothing exists, that is the signal to create an instrument — a proper tool in `tools/`, not ad-hoc code.

This mandates evaluation, not any particular crate: choose deliberately among candidates. It also cuts both ways — do not add dependencies speculatively for problems we don't have.

### Impossible Tests: Defer, Never Skip

If a critical test cannot be written inline, defer it — never drop it silently. Platform- or runtime-specific validation uses heavier setups (Dockerfile, kind cluster, Lima VM, Azure VM), with one hard requirement: re-running the validation must be reproducible without a human or agent writing ad-hoc scripts and commands. A deferred test is declared under Completion like any other gap.

### Performance

For performance-sensitive code, establish a benchmark baseline early. On unexpected degradation, re-run the benchmarks under a profiler to locate the cause. Document baseline results early, per the Documentation rules.

### Fuzzing

Any surface that handles user input requires fuzz testing — parsing above all. These entrypoints are defended at all cost; correctness here is held to the strictest standard in the repo. This is the one place test economics do not apply.