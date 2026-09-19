# Releases with release-plz

Work directly on `main`, or use short-lived feature branches. Ordinary pushes
run checks but do not publish. Release-plz handles version changes, changelogs,
release PRs, Git tags, and GitHub Releases; there are no custom release scripts.

## Daily use

- Write Conventional Commits: `feat: ...` for a minor release, `fix: ...` for a
  patch, and `feat!: ...` / `BREAKING CHANGE:` for breaking changes. The minor
  bump for `feat:` also applies before version 1.0. Use `docs:`, `build:`,
  `refactor:`, and `test:` where appropriate. Release-plz decides the final bump
  from all changes; breaking changes never qualify for patch auto-merge.
- Renovate checks throughout the weekend in Europe/Zurich, groups compatible
  Cargo, Actions, and Docker updates, and auto-merges them after tests. Its
  `fix(deps): ...` commits participate in patch releases. Breaking dependency
  upgrades need review; mark their squash commit `fix(deps)!:` if they introduce
  a breaking change to the application. Renovate never bumps our crate version.
- **Actions → Release-plz → Run workflow** on `main` creates or refreshes the
  release PR. Review its version and generated `CHANGELOG.md`, then merge it
  when ready. Manual runs do not enable auto-merge.
- On Mondays at 06:00 UTC, release-plz prepares the same PR. The workflow enables
  auto-merge only for the next patch version, and only if the current version
  has a published GitHub Release at least seven days old. Minor/major releases
  remain for you to merge. Required checks still apply; CI delays can delay
  the merge and publication beyond the scheduled run.
- A weekly patch can include ordinary fixes and maintenance as well as
  dependency changes. A pending feature bumps the release to minor and keeps
  it manual. This intentionally replaces strict dependency-only releases.
- Merging a release PR publishes `vX.Y.Z` and a GitHub Release with the generated
  changelog. That event builds and tests the exact tag, then publishes
  `ghcr.io/freopen/sat-tracker:vX.Y.Z`, its full commit SHA, and `latest`.
  Nothing is published to crates.io.

Keep the `release-plz-` branch prefix reserved for release-plz. To refresh a stale
release PR, run the workflow again rather than manually merging `main` into its
branch. The workflow clears earlier auto-merge decisions before refreshing it,
so a PR that becomes a minor release is not left with patch auto-merge enabled.

After a release, use a normal `git pull`. If you have no local commits, use
`git pull --ff-only`. There is no back-merge or version-reset procedure.
Concurrent local edits to the manifests or changelog can still conflict.

## One-time GitHub setup

1. Create a dedicated GitHub App under your account's **Settings → Developer
   settings → GitHub Apps**. Use a unique name, the repository URL as its
   homepage, disable the webhook, and restrict installation to your account.
   Grant repository **Contents: Read and write** and **Pull requests: Read and
   write**. Install it only on `freopen/sat-tracker` and generate a private key.
2. In this repository's **Settings → Secrets and variables → Actions**, add
   repository variable **`RELEASE_APP_CLIENT_ID`** with the **Client ID** shown
   in the App's settings (not its numeric App ID), and secret
   **`RELEASE_APP_PRIVATE_KEY`** with the complete PEM key. Keep the key outside
   the repository. No PAT or crates.io credential is needed. The App token lets
   release PRs trigger CI and published Releases trigger the container workflow.
   If migrating from `RELEASE_APP_ID`, add the new variable before pushing the
   workflow change; the old variable can then be deleted. The private key stays
   the same.
3. Under **Settings → Rules → Rulesets**, retain the required **`test`** check
   from **GitHub Actions**, with branches required to be up to date. Add only
   **Repository admin** to its bypass list with **Always** bypass, allowing
   your direct pushes. **Do not grant either release-plz's App or Renovate a
   bypass.** Do not require an approving review or require your pushes to be PRs.
4. Put **Block force pushes** and **Restrict deletions** in a separate ruleset
   for the default branch, without bypass actors. Remove these two rules from
   the checks ruleset so the administrator bypass does not bypass history
   protection too.
5. Keep **Allow auto-merge** and **Allow squash merging** enabled under
   **Settings → General → Pull Requests**. Keep Renovate installed; do not
   enable another dependency updater. Close obsolete Renovate PRs that contain
   version bumps and let the new configuration recreate them.
6. Keep Actions enabled. For the existing GHCR package, confirm this repository
   has **Write** permission under **Manage Actions access** if access is not
   inherited. Container publication uses `GITHUB_TOKEN`, not the release App.
7. Leave release tags unprotected initially. If you later add tag rules, permit
   the App to create `v*` tags. Never move or reuse published version tags.

See [release-plz's GitHub App instructions](https://release-plz.dev/docs/github/token).

## Publish this local migration

Local `main` includes the existing `dev` commits and tracks `origin/main`.
The old local `dev` branch is retained. Review and commit the new configuration
on `main`; no commit or push is performed automatically for this migration.
Use a `feat:` commit for the new release system if you want the first release
to be `0.6.0`.

A local `v0.5.0` baseline tag points to
`e825dae5649cb5676519f95f30263e741ea34a15` (`Version 0.5.0`), rather than the
development tip. Once the GitHub settings above are ready, inspect and push:

```sh
git status -sb
git log --oneline origin/main..main
git show v0.5.0:Cargo.toml
git push --atomic origin main:main refs/tags/v0.5.0:refs/tags/v0.5.0
```

The baseline is just a tag; no historical image or GitHub Release is created.
Run **Release-plz** manually and merge its first release PR after checks pass.
Until that first GitHub Release exists, weekly runs leave PRs for manual merge.
Verify the GitHub Release and the separate **Publish container** run. Retire
remote `dev` in GitHub when ready; other workstations should switch to `main`.

## Recovery and local tools

If a release PR fails tests, fix the problem and run **Release-plz** again.
If publishing fails before a tag is created, rerun the failed publication job.
If the tag exists but creating the GitHub Release failed, release-plz will skip
that tag: create the missing GitHub Release from the existing tag in the UI,
using the changelog entry as its notes. Publishing it starts the container build.

If the container build fails, rerun it, or run **Publish container** manually
with the existing release tag. It rebuilds that source commit; mutable build
inputs can produce a new image digest. Retrying an older release does not move
`latest` back. A GitHub Release and its image are separate operations; check
the container run before assuming an image is available.

The Dockerfile provides release-plz, actionlint, GitHub CLI, and jq to both the
devcontainer and CI image. Python is not required. Release-plz is pinned in the
Dockerfile and workflow; update its version and architecture checksums together.
`make workflows` runs actionlint; `make check` includes it alongside all existing
Rust checks. Docker builds still require a Docker-capable host.
