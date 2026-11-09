# Contributing to Regulus

Contributions are welcome: fixes, templates, modules, documentation and reproducible
bug reports. Start with a small change that can be reviewed and verified on its own.
Regulus is licensed under [Apache-2.0](LICENSE); intentionally submitted contributions
are covered by that license unless explicitly stated otherwise.

## Find your way

Read [the concepts](docs/contributing/concepts.md), then the
[module guide](docs/contributing/modules.md) if you are extending the node palette.
[AGENTS.md](AGENTS.md) maps the detailed design documents, currently in Spanish.
The principles in [docs/00-principles.md](docs/00-principles.md) govern design:
capabilities instead of module names, removable modules, configurable behavior,
measured performance, and CLI access to every desktop operation.

Open an issue to discuss a substantial contract or architecture change before building
it. A focused bug fix or documentation correction can go straight to a pull request.
Include reproduction steps, expected behavior and actual behavior in bug reports.
Never attach credentials, provider tokens or private project data.

## Set up a development checkout

Install Git, stable Rust and Cargo. For the desktop client, install Node.js, pnpm,
and the platform prerequisites linked from [Tauri](https://v2.tauri.app/start/prerequisites/).
Python 3 is used by some examples and tests. Dependencies are maintained in Cargo.toml
and the frontend package manifest; verified dependency notes live in docs/06-stack.md.

```sh
git clone https://github.com/Solar2004/Regulus.git
cd Regulus
scripts/install-hooks.sh
cargo build -p regulus-cli
cargo run -p regulus-cli -- serve
```

In another terminal, use `cargo run -p regulus-cli -- --help` to explore commands.
To develop the desktop client:

```sh
cd crates/regulus-tauri/frontend
pnpm install --frozen-lockfile
pnpm tauri:dev
```

The standalone frontend can also run with `pnpm dev`. Consult its configuration
before choosing between a mock and a real core. Do not use a production configuration
or provider account for deterministic tests.

## Make and verify a change

Use a short branch such as `fix/chat-history` or `feat/document-parser`. Keep changes
focused and include a regression test for changed behavior. Test the affected crate
first; for changes crossing modules, run integration tests and the ablation matrix.

```sh
cargo fmt --all -- --check
cargo test --workspace --exclude regulus-tauri
cargo test -p regulus-cli --test ablation
cargo clippy --workspace --all-targets -- -D warnings
```

Desktop checks, from `crates/regulus-tauri/frontend`:

```sh
pnpm exec tsc --noEmit
pnpm test
pnpm test:mock
pnpm build
```

Live agent tests may need installed engines, authentication or quota; report exactly
what you ran. A skipped live test is not evidence that an integration works. For UI
changes, include a screenshot or recording and exercise the behavior against the core.

Code, comments, logs, commits and public contributor guides use English. Put visible
UI strings in the translation resources rather than JSX literals. Preserve third-party
license and origin notices. Keep dependency-version documentation in docs/06-stack.md.

## Send a pull request

Use Conventional Commits: `type(scope): imperative subject`, lowercase initial,
no final period, subject at most 72 characters. Valid types and scopes are defined in
[.gitconventions](.gitconventions). Explain why in the body and cite the relevant design
section with `Refs: docs/...`. Do not add coauthor trailers or bypass hooks.

A PR should explain the problem, the resulting behavior, validation performed and
any limitations. Update documentation when a public contract or supported workflow
changes. Keep formatting-only changes separate from behavior changes. CI checks
Rust, frontend code, commit messages and secrets. Push changes to your fork and open
a PR against the default branch; maintainers review changes before merging.

## First contributions

A small template, a focused regression fix or an improvement to these guides is a
good starting point. New engines and modules often involve lifecycle, contracts and
ablation tests, so discuss their scope first. Candidate tasks are described in
[starter tasks](docs/contributing/starter-tasks.md); they are proposals, not a claim
that matching GitHub issues have already been published.
