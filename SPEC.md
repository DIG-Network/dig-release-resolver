# dig-release-resolver — Release Resolution Specification

**Status:** Normative · **Crate:** `dig-release-resolver` · **API version:** `0.2`

This document is the authoritative contract for `dig-release-resolver`: given a DIG component's
release coordinates and the running host's platform, how to resolve the latest (or a named)
release, name its per-OS/arch asset, decide whether an install run should install, update, or
skip, and decide whether a resolved artifact can actually LOAD on this host. An independent
implementation built from this document alone MUST resolve the same URLs, the same asset names,
the same Install/Update/Skip decision, and the same three-valued loadability verdict as the
reference crate for the same inputs.

The key words **MUST**, **MUST NOT**, **SHOULD**, and **MAY** are to be interpreted as in RFC 2119.

---

## 1 · Purpose & scope

`dig-release-resolver` answers two questions every updater on the DIG component fleet needs
answered **identically**, so that `dig-installer` (a one-shot CLI run) and the `dig-updater`
auto-update beacon (a scheduled daemon, issue #504) never diverge on what "the latest release" or
"needs updating" means for the same component:

1. **Resolution** — what is the latest (or a specifically tagged) published release for a
   component, and what does its release asset for THIS host look like? (§2–§4)
2. **Decision** — given what is already installed and what is latest, should this run **Install**,
   **Update**, or **Skip**? (§5)
3. **Loadability** — will a resolved artifact's bytes actually LOAD on this host, or would it die in
   the dynamic linker before `main` despite a perfect signature and digest? (§6)

### 1.1 What this crate is NOT

- **Not a downloader.** It resolves a release and builds its download URL; it does not fetch,
  checksum, verify, or write the binary. Each consumer owns that against its own trust root —
  `dig-installer`'s plain SHA-256 check is not the same operation as the beacon's signed-manifest
  verification (issue #504's TUF-style trust chain), so neither is embedded here.
- **Not the signed feed.** `updates.dig.net` (issue #513) is a separate, signed JSON feed the
  beacon consults as its primary source. This crate speaks the **public GitHub Releases API
  only** — the documented fallback/bootstrap source available independently of that feed, and
  usable as a cross-check once it exists.
- **Not an asset MATCHER.** [`Release::asset_names`](#3-release-json-parsing) is the raw list of
  names GitHub actually published; this crate does not pick "the right one" out of that list by
  fuzzy OS/arch-token matching — a consumer with that need (`dig-installer`'s asset module) owns
  its own matcher. This crate only builds the CANONICAL templated name (§4) a component's release
  workflow is expected to publish under.

---

## 2 · Component identity — `Repo`

A `Repo` names one DIG component's release source:

| Field   | Meaning                                                         |
|---------|------------------------------------------------------------------|
| `owner` | The GitHub org, always `DIG-Network` for every canonical constructor. |
| `name`  | The GitHub repo name (e.g. `digstore`, `dig-node`).             |
| `stem`  | The binary/asset-name stem (e.g. `digstore`, `dig-node`) — see §4. |

`owner`/`name`/`stem` are independent: a component MAY publish its stem under a different repo
name (`Repo::digs()` — the `digs` alias binary — shares `DIG-Network/digstore`'s repo but resolves
under its own `digs` stem, §4.2).

### 2.1 Canonical constructors (the alpha component set)

The crate ships one constructor per DIG-Network component the fleet resolves today:

`Repo::digstore()` · `Repo::digs()` · `Repo::dig_node()` · `Repo::dig_node_legacy()` ·
`Repo::dig_dns()` · `Repo::dig_relay()` · `Repo::dig_browser()` · `Repo::dig_updater()`

`Repo::dig_updater()` names the auto-update beacon's OWN release source — the beacon is a member
of its own tracked set (issue #504's build order includes the beacon self-updating), so it
resolves through the exact same `Repo`/`Target` machinery it uses for every other component.

A caller resolving a component this crate has no canonical constructor for MAY build one directly
via `Repo::new(owner, name, stem)`.

### 2.2 Release API URLs

Given a `Repo`, three GitHub REST endpoints are built by simple string formatting (no
percent-encoding beyond what a plain owner/name/tag already satisfies — DIG-Network repo and tag
names never require it):

| Method                  | URL template                                                              |
|--------------------------|----------------------------------------------------------------------------|
| `latest_release_api()`  | `https://api.github.com/repos/{owner}/{name}/releases/latest`             |
| `release_by_tag_api(tag)` | `https://api.github.com/repos/{owner}/{name}/releases/tags/{tag}`       |
| `releases_list_api()`   | `https://api.github.com/repos/{owner}/{name}/releases`                    |
| `asset_download_url(tag, asset)` | `https://github.com/{owner}/{name}/releases/download/{tag}/{asset}` |

`binary_url(tag, version, target)` composes `asset_download_url` with the asset name §4 derives
for `target` at `version`.

### 2.3 Tag ⟷ version normalization

Every DIG-Network release tag is a bare `vMAJOR.MINOR.PATCH` (git-cliff's Conventional-Commit
bump, CLAUDE.md §3.6):

- `version_from_tag("v0.6.0") == "0.6.0"`; a tag with no `v` prefix is returned unchanged.
- `tag_from_input(s)`: adds a leading `v` unless `s` already has one, is empty, or is the literal
  `"latest"` (both of which a caller uses as sentinels and MUST get back unchanged).

---

## 3 · Release JSON parsing — the `github` module

A GitHub release JSON object (from any of §2.2's three endpoints, or one entry of the releases
LIST array) is reduced to a `Release { tag_name: String, asset_names: Vec<String> }`:

- `tag_name` MUST be present as a string; its absence is an error (`"release JSON had no
  tag_name"`), never a panic.
- `assets` (if present and an array) contributes one `asset_names` entry per element that has a
  string `name` field; an element without one is SILENTLY skipped (not an error, not an empty
  string) — a release with malformed asset metadata still yields whatever real names it has.
- `assets` absent, or present but not an array, yields an EMPTY `asset_names` — never a parse
  error, never a panic.

### 3.1 `latest_release` — the 404-then-releases-list fallback

`GET /releases/latest` **excludes** prerelease and draft releases. A component whose newest (or
only) published release IS prerelease-flagged — e.g. an alpha channel — therefore 404s there even
though a real, asset-bearing release exists.

`latest_release(repo)` MUST:

1. `GET` `latest_release_api()`.
2. On success, parse it as a single release (§3).
3. On a **404** response (recognized by the transport error containing `"404"` or `"Not Found"`),
   fall back to `GET releases_list_api()` and take the **first** (newest) entry of that array,
   **regardless of its `prerelease`/`draft` flags** — the list endpoint has no such filter, so
   position alone determines newest.
4. On any OTHER transport error (timeout, 5xx, DNS failure, …), propagate it unchanged — MUST NOT
   silently fall back to the list on a non-404 failure.

`latest_tag(repo)` is `latest_release(repo)?.tag_name`. `latest_version(repo)` is
`version_from_tag(latest_release(repo)?.tag_name)` — the single call most callers want when all
they need is a bare-semver string to feed into §5's `decide`.

`release_by_tag(repo, tag)` fetches one specific tagged release with NO fallback — a caller asking
for an exact tag gets that tag or an error, never a substitute.

### 3.2 Authentication

Every GitHub API request carries `User-Agent: dig-release-resolver/<crate-version>` and
`Accept: application/vnd.github+json`. If the process environment variable `GITHUB_TOKEN` is set
to a non-empty value, requests additionally carry `Authorization: Bearer <token>` (raises the
unauthenticated 60/hour-per-IP rate limit to 5,000/hour — material for CI runners sharing a
heavily-used IP pool). An unset or empty `GITHUB_TOKEN` sends the identical anonymous request as
if the feature did not exist.

---

## 4 · Per-OS/arch asset naming — the `target` module

### 4.1 Supported targets

| `Os`      | `Arch` supported |
|-----------|------------------|
| `Windows` | `X64` (an `Arm64` request maps to the same `x64` slug — Windows ships x64 only; ARM devices run it under emulation) |
| `Linux`   | `X64` (same ARM fallback behavior as Windows) |
| `MacOs`   | `X64`, `Arm64` (both published natively — no fallback) |

`Target::current()` resolves the actual host from `std::env::consts::OS`/`ARCH`; any other OS/arch
pair is an error, never a silent guess.

### 4.2 The asset-name template

For a component with stem `S` at bare-semver version `V` on a resolved `Target`:

```
asset_name = "{S}-{V}-{slug}{exe_ext}"
```

where `slug` is `windows-x64` / `linux-x64` / `macos-arm64` / `macos-x64` (exactly the release
workflows' build-matrix `out_name`s) and `exe_ext` is `.exe` on Windows, else empty.

Examples: `digstore-0.6.0-windows-x64.exe`, `dig-node-0.15.0-linux-x64`,
`dig-node-0.2.0-macos-arm64`.

This is the CANONICAL name a component's own `release.yml` is expected to publish under — every
DIG-Network component's release workflow MUST keep publishing under this exact template, or
every resolver on the fleet 404s. A producing repo whose asset naming needs to diverge from this
template (e.g. a native per-OS installer package rather than a raw binary, like DIG Browser) is
matched by a consumer's OWN asset-list matcher against `Release::asset_names` (§3) instead of this
template — outside this crate's scope (§1.1).

---

## 5 · The Install/Update/Skip decision — the `decision` module

### 5.1 Inputs

- **Detected** (`DetectedVersion`): `Absent` (nothing at the destination path) or
  `Present(raw_version_output)` (the destination exists; `raw_version_output` is its `--version`
  stdout, or an EMPTY string if the probe could not read it — a spawn failure or non-zero exit
  collapses to empty, never an error type).
- **Latest** (`&str`): a bare-semver string, normally `github::latest_version`'s output.

### 5.2 The decision matrix

| Detected                              | vs. latest           | Action  |
|----------------------------------------|-----------------------|---------|
| `Absent`                               | —                     | Install |
| `Present`, parses, **older**           | installed < latest    | Update  |
| `Present`, parses, **equal**           | installed == latest   | Skip    |
| `Present`, parses, **newer**           | installed > latest    | Skip    |
| `Present`, **does not parse**          | —                     | Update  |

- A locally newer build than the latest published release MUST NOT be downgraded — that cell is
  Skip, not Update.
- An unparseable installed version (a garbled `--version` output, or an empty probe result) MUST
  be treated as "can't prove it's current" and decided as Update (a safe reinstall), never as
  Install (the destination DOES exist) or Skip (unverified currency is not currency).
- The `latest` string failing to parse as a `SimpleVersion` is treated identically — every
  DIG-Network release tag fits `MAJOR.MINOR.PATCH` (§2.3), so this is theoretical, but the
  fallback is symmetric rather than an unhandled panic path.

### 5.3 Version comparison — `SimpleVersion`

Ordering is a bare 3-tuple `(major, minor, patch)`, each a `u64`, compared lexicographically — NOT
full SemVer precedence rules (no pre-release/build-metadata ordering). A string with a
pre-release/build suffix (`"0.15.0-rc.1"`), a wrong segment count, or a non-numeric segment is
UNPARSEABLE by design (§5.2's "does not parse" row), because every real DIG-Network release tag is
a bare `vX.Y.Z` and a string that doesn't fit is far more likely to be foreign/garbled
`--version` output than a genuine pre-release tag.

### 5.4 `UpdateDecision` — the caller-facing result

```
UpdateDecision {
    action: UpdateAction,             // Install | Update | Skip
    installed_version: Option<String>, // None only when detected == Absent
    latest_version: String,           // echoed verbatim from the input
    summary: String,                  // one human-readable line, stable enough to log/render as-is
}
```

`UpdateAction` serializes (`serde`, `rename_all = "snake_case"`) to `"install"` / `"update"` /
`"skip"` — the wire form a `--json` CLI surface or a GUI status pill consumes directly;
`UpdateAction::as_str()` returns the same three strings without a JSON round trip.

### 5.5 `--force-reinstall` — `decide_with_force`

`decide_with_force(detected, latest, force)` is `decide(detected, latest)` unchanged, EXCEPT: when
`force` is `true` and the plain decision was `Skip`, it is upgraded to `Update` (with `" — forced
reinstall"` appended to `summary`). An `Install` or `Update` decision is unaffected by `force` —
both are already replacing the artifact, so forcing adds nothing.

### 5.6 Detecting what's installed — `detect_installed_version`

`detect_installed_version(bin_path)`:

1. If `bin_path` does not exist, return `Absent` WITHOUT spawning anything.
2. Otherwise spawn `<bin_path> --version` and return `Present(trimmed_stdout)` on a successful
   (exit-0) run, or `Present("")` (empty) if the spawn failed or exited non-zero — never an `Err`;
   an unreadable version is data for §5.2's decision matrix, not a resolver-level failure.

This function is READ-ONLY: it MUST NOT create, modify, or delete anything at `bin_path`, so a
caller MAY call it purely to preview a decision (e.g. a GUI's pre-install Components screen)
before committing to any real install/update.

The `--version` output's LAST whitespace-separated token of its FIRST line is taken as the version
string (clap's default formatter prints `"<name> <version>"`, e.g. `"dig-node 0.15.0"`; a bare
`"0.15.0"` also satisfies this).

---

## 6 · Host loadability — the `loadability` module

A signature and a digest prove an artifact is the intended BYTES; they say nothing about whether
those bytes can start on THIS host. A `linux/x64` build linked against GTK sonames a headless
server lacks installs perfectly and then dies inside the dynamic linker before `main`; an `arm64`
build dropped into the `linux/x64` slot dies at `execve` with `Exec format error` while every
soname it names still resolves. The `loadability` module answers this question so that the
install-time selector (`dig-installer`) and the update-time selector (`dig-updater`'s beacon)
reach the **byte-identical** verdict — a host MUST never oscillate between calling a build loadable
and calling it unloadable depending on which selector looked.

### 6.1 The verdict is three-valued (`Loadability`)

- `Loadable` — every demand the image makes of the loader is satisfiable here; permit.
- `Unloadable { missing }` — the image needs shared libraries or a program interpreter this host
  does not provide; refuse, naming what is missing in the image's own order.
- `WrongMachine { artifact, host }` — the image's `e_machine` is not the host's; refuse.
- `Indeterminate { why }` — no answer could be established; **permit**.

### 6.2 The decision is deliberately asymmetric

A verdict MUST refuse ONLY when it can PROVE the host cannot load the artifact. A non-ELF artifact
(`.deb`/`.msi`/`.pkg`), an unparseable or truncated image, a host whose shared-library set cannot
be established, an architecture this crate does not name, and any non-Linux host all yield
`Indeterminate`, which permits. Refusing what cannot be proven would freeze every native-package
and musl host forever — including security updates and the updater's own update — so the check may
only ever make a selector do LESS, never more, and runs strictly AFTER signature + digest
verification.

### 6.3 The verdict is read from bytes, never by executing the artifact

Loadability MUST be answered by PARSING the artifact's bytes ([`parse_elf_needs`] →
[`decide_loadability`] / [`inspect_artifact`]) — the candidate is NEVER spawned. The component that
most needs the check parses no arguments and, run under a root beacon, would seal a master seed and
bind a signing socket; executing a candidate "to see if it runs" is itself the harm. The only
subprocess the module MAY spawn is the host's own `ldconfig -p`, and only to READ the linker cache.

- `ldconfig` MUST be invoked at a trusted ABSOLUTE path (`/usr/sbin/ldconfig`, `/sbin/ldconfig`,
  `/usr/bin/ldconfig`, `/bin/ldconfig`), never a bare name resolved through `$PATH`, with a cleared
  environment, an output cap, and a deadline after which it is killed and reaped.
- Three demands are checked, because each kills the process before `main` and each looks perfect to
  a digest: the machine (`e_machine`), the program interpreter (`PT_INTERP`, by absolute path), and
  the `DT_NEEDED` sonames (each resolved against the host set OR the image's own `$ORIGIN`-expanded
  `DT_RUNPATH`/`DT_RPATH`).

### 6.4 An enumerated host set is trusted to REFUSE only when COMPLETE

A host shared-library set MUST NOT be used to refuse an artifact unless it is anchored by a C
library (`libc.so*`, `libc-*`, `ld-musl-*`). A set without one was scanned in the wrong place (a
multiarch triplet not searched), not "found wanting"; refusing against it would name every real
library missing and freeze the fleet. The multiarch directories scanned are DERIVED from the
filesystem and scoped to the host's own architecture — another architecture's flavour of a soname
MUST NOT count as resolvable, since that would be a false `Loadable`.

---

## 7 · Conformance

An implementation conforms iff, for the same `Repo`/`Target`/tag/version inputs, it:

1. Builds byte-identical URLs to §2.2 and byte-identical asset names to §4.2.
2. Parses release JSON per §3's field rules (including the two silent-empty and one
   silent-skip cases — never erroring where this spec says "silently").
3. Falls back from `/releases/latest` to the releases list ONLY on a 404-shaped error, taking the
   list's first entry regardless of prerelease/draft flags, and propagates every other error
   without falling back (§3.1).
4. Reaches the identical cell of the §5.2 decision matrix for the same `(detected, latest)` pair,
   including the never-downgrade and unparseable-reinstall rules.
5. Returns the identical §6.1 loadability verdict for the same artifact bytes and host facts,
   honouring the §6.2 asymmetry (refuse only what is proven), the §6.3 never-execute rule, and the
   §6.4 completeness anchor.

This crate does not define a numeric versioning scheme beyond §5.3 — conformance is purely about
matching the STATED rules above, not reproducing internal representations.
