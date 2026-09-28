# Contributing to vynkor

Thanks for looking. vynkor is a small kernel with strong opinions — reading
the rules below first saves a review round.

## The one rule: dumb core

The kernel does exactly four things: **transport** (frame bytes over UDS,
zero-parse, MAC-auth), **lifecycle** (spawn / supervise / sandbox / kill
plugins), **security** (JWT, per-frame HMAC, default-deny permissions) and
**plumbing** (API gateway, event bus, metrics, TLS, CLI).

Anything that knows an action name, talks to a model, or stores application
state belongs in a plugin ([vynkor-plugins](https://github.com/vynkor-core/vynkor-plugins)),
not here. If a change makes the kernel *understand* a payload, it is almost
certainly in the wrong repo. Current drift tracking: `docs/DUMB_CORE_AUDIT.md`.

## Where things live

| Change | Repo |
|---|---|
| kernel routing, supervision, auth, CLI `vyn` | this repo |
| wire format / protobuf schema | [vynkor-wire](https://github.com/vynkor-core/vynkor-wire) (`proto/vynkor_protocol.proto`) |
| plugin manager `vynm`, registry client | [vynkor-manager](https://github.com/vynkor-core/vynkor-manager) |
| a plugin | [vynkor-plugins](https://github.com/vynkor-core/vynkor-plugins) |
| SDKs | [Rust](https://github.com/vynkor-core/vynkor-sdk) · [C++](https://github.com/vynkor-core/vynkor-sdk-cpp) · [Python](https://github.com/vynkor-core/vynkor-sdk-python) |
| website / marketplace | vynkor-web |

A proto change touches every SDK: bump the `vynkor-wire` crate, run
`../vynkor-wire/scripts/sync-proto.sh`, bump the SDKs' pinned wire version.
CI's drift check (T-17) fails on any byte difference. Always add new fields
as new numbers and `reserved` removed ones.

**Open an issue before** changing the proto, plugin lifecycle
(`src/kernel/orchestrator/`, `src/plugins/supervisor/`), anything
cross-SDK, or a hot path (IPC, event bus, router).

## Setup

Sibling layout — CI clones the same way:

```bash
mkdir vynkor-core && cd vynkor-core
for r in vynkor vynkor-wire vynkor-sdk vynkor-sdk-cpp vynkor-sdk-python; do
  git clone https://github.com/vynkor-core/$r
done
cd vynkor
```

Needs Rust ≥ 1.85 and `protoc`. The C++ integration test also needs
`libprotobuf-dev libabsl-dev libssl-dev libzstd-dev libgtest-dev` and a
built `../vynkor-sdk-cpp` `echo_plugin` (see `.github/workflows/ci.yml`).

## Before you open a PR

```bash
cargo test --all --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

- Branch from `develop`, PR into `develop`.
- New behavior comes with a regression test (`tests/unit/` or
  `tests/integration/`; SDK unit tests live in the SDK repos).
- Operator-visible changes update `README.md` in the same PR.
- Conventional commits: `feat(ipc): …`, `fix(supervisor): …`, `docs: …`.
- Comments are lowercase, terse, and explain *why* — the code already says
  what. Audit tags like `T-04` or `R9-03` are indexed in
  `docs/COMMENT_TAGS.md`.

## Security issues

Never in a public issue — see [SECURITY.md](SECURITY.md).

## License

By contributing you agree your work is dual-licensed under
[MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at the user's option.
