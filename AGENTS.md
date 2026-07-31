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

Keep technical writing brief and concise. When presenting data, use tables and let the reader draw conclusions rather than editorializing. Prefer mermaid charts over ASCII art.

## Debugging

Avoid speculating or reasoning about state abstractly. Use concrete data to form conclusions, diagnose issues, and validate hypotheses.

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