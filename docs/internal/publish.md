# Publishing obd-rs to crates.io

Maintainer operation. A release also builds the deb and the rpm — that half is
in [the README](../../README.md#releases) — and this is the crate half.

## No token is stored in this repository

`.github/workflows/publish.yml` proves its identity to crates.io over OIDC and
receives a token that lasts for the run. crates.io calls this **Trusted
Publishing**, and `rust-lang/crates-io-auth-action` revokes the token in its
own post step (rust-lang/crates-io-auth-action v1, README, "Sequence Diagram").

The trade is configuration instead of a secret: crates.io has to be told which
repository, which workflow file and which environment to trust. A token in a
repository secret — under any name, dated or not — is a usable credential for
as long as it sits there. This has no such window.

## One-time setup

crates.io requires a crate's first release to be published by hand before a
trusted publisher can be configured for it ("crates.io: development update",
[Rust Blog, 2025-07-11](https://blog.rust-lang.org/2025/07/11/crates-io-development-update-2025-07/)).
So the bootstrap is manual, and its token never reaches GitHub:

1. **Publish once from a laptop.**

   ```bash
   make publish-check                     # packages and builds from the packaged tree
   cargo login                            # a token scoped to publish-new, from crates.io
   cargo publish --locked
   ```

   Then revoke that token on crates.io. It has done its only job.

2. **Configure the trusted publisher.** On crates.io, under the crate's
   Settings → Trusted Publishing, add:

   | Field | Value |
   | --- | --- |
   | Repository owner / name | the GitHub repository this is published from |
   | Workflow filename | `publish.yml` |
   | Environment | `release` |

3. **Create the environment.** In the repository, Settings → Environments →
   `release`. Add a required reviewer there if publishing should pause for a
   human. Publishing is irreversible: a version can be yanked, never replaced.

## Releasing after that

```bash
git tag -a v0.2.0 -m "obd-rs 0.2.0" && git push origin v0.2.0
```

That one tag drives both workflows: `release.yml` builds the packages and
publishes the GitHub release, and `publish.yml` publishes the crate. Running
`publish.yml` by hand from the Actions tab is the retry path when the upload
fails but the tag is already out.

### Why the tag, and not the release

`release.yml` creates the GitHub release with `GITHUB_TOKEN`, and events raised
by `GITHUB_TOKEN` do not start another workflow run — `workflow_dispatch` is
one of the two documented exceptions ([GitHub Actions docs, "Triggering a
workflow from a workflow"](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow)).
A `release: published` trigger would therefore never fire, silently.

## What is checked before anything is uploaded

| Check | Catches |
| --- | --- |
| The tag matches the manifest version | A crate published under a version nobody can find from the release it came out of |
| The version is absent from crates.io | A duplicate upload, reported in one line instead of a publish failure |
| `make publish-check` | A file the tarball omits — it builds the crate from the packaged tree, so this fails before a version number is spent |

CI runs the last of those on every pull request, because by the time a tag
exists a packaging mistake costs a version number.

## What ships in the crate

`exclude` in `Cargo.toml` keeps the development scaffolding out: the
devcontainer, the workflows, `AGENTS.md`, the Lima VM definition and these
internal docs. None of it is actionable from an unpacked tarball, and
`repository` in the manifest points at where it does live.

The library, `obdctl`, `build.rs`, the packaging assets under `lib/`, the
scripts the README refers to and the test suites all ship: 35 files, 74 KiB
compressed at v0.1.0.
