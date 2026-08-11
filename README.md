# dig-release-resolver

Resolve the latest / by-tag / prerelease GitHub release and the matching per-OS/arch asset name
for a DIG component, then decide **Install** / **Update** / **Skip** against whatever is already
on disk. Shared by [`dig-installer`](https://github.com/DIG-Network/dig-installer) and the
[`dig-updater`](https://github.com/DIG-Network/dig-updater) auto-update beacon
(issues [#504](https://github.com/DIG-Network/dig_ecosystem/issues/504)/[#508](https://github.com/DIG-Network/dig_ecosystem/issues/508))
so both speak the exact same resolution + decision logic — never two hand-rolled copies that could
drift apart.

## What it does

```rust
use dig_release_resolver::{decision, github, repo, target};

let component = repo::Repo::dig_node();
let host = target::Target::current()?;
let latest = github::latest_version(&component)?;

let dest = std::path::Path::new("/usr/local/bin").join(host.exe_name(&component.stem));
let decision = decision::decide(&decision::detect_installed_version(&dest), &latest);

println!("{}", decision.summary); // e.g. "v0.14.0 → v0.15.0 (update)"
```

- **`repo`** — `Repo` (owner/name/binary-stem) + the GitHub release/download URL builders, with a
  canonical constructor per tracked DIG-Network component (`digstore`, `dig-node`, `dig-dns`,
  `dig-relay`, `dig-browser`, `dig-updater`, …).
- **`target`** — `Target`/`Os`/`Arch` + the per-OS/arch release-asset filename template
  (`digstore-0.6.0-linux-x64`, …) every DIG-Network `release.yml` publishes under.
- **`github`** — a thin `ureq`-based GitHub Releases API client: `latest_release`/`latest_version`
  (with the `/releases/latest` → releases-list prerelease fallback), `release_by_tag`.
- **`decision`** — the pure Install/Update/Skip decision matrix (`decide`/`decide_with_force`) plus
  the one I/O boundary that detects what's on disk (`detect_installed_version`).
- **`loadability`** — will a resolved artifact actually LOAD on this host? Reads the ELF's own
  dynamic-linking requirements out of its bytes (never executing it) and returns a three-valued,
  deliberately asymmetric `Loadability` (`host_checker`/`inspect_artifact`/`decide_loadability`), so
  the install-time and update-time selectors reach the byte-identical verdict.

See [`SPEC.md`](./SPEC.md) for the full normative contract.

## Explicitly out of scope

This crate resolves releases; it does not download, checksum, or write the binary (each consumer
verifies against its own trust root), and it does not speak the signed `updates.dig.net` feed
(the beacon's own concern) — it is the documented GitHub-native fallback source every consumer can
rely on independently of that feed.

## Development

```sh
cargo test              # unit tests
cargo fmt --all         # format
cargo clippy --all-targets --all-features -- -D warnings
cargo llvm-cov --fail-under-lines 80 --summary-only   # coverage gate
```
