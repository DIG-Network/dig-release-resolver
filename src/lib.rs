//! # dig-release-resolver
//!
//! Resolve the latest / by-tag / prerelease GitHub release and the matching per-OS/arch asset
//! name for a DIG component, then decide **Install** / **Update** / **Skip** against whatever is
//! already on disk. Shared by `dig-installer` and the `dig-updater` auto-update beacon (issues
//! #504/#508) so both speak the exact same resolution + decision logic — never two hand-rolled
//! copies that could drift apart.
//!
//! ## Shape
//!
//! Two independent halves, composed by the caller:
//!
//! - **Resolution** ([`repo`], [`target`], [`github`]) — a [`repo::Repo`] names a component's
//!   GitHub repo + binary stem; a [`target::Target`] names the running host's OS/arch. Together
//!   they build the download URL for a specific tag ([`repo::Repo::binary_url`]), while
//!   [`github::latest_version`] asks GitHub which version is newest.
//! - **Decision** ([`decision`]) — a pure function, [`decision::decide`], that takes what was
//!   [`decision::detect_installed_version`] at the destination plus the string
//!   [`github::latest_version`] resolved, and returns an [`decision::UpdateDecision`].
//!
//! ```no_run
//! use dig_release_resolver::{decision, github, repo, target};
//!
//! let component = repo::Repo::dig_node();
//! let host = target::Target::current().unwrap();
//! let latest = github::latest_version(&component).unwrap();
//! let dest = std::path::Path::new("/usr/local/bin").join(host.exe_name(&component.stem));
//! let decision = decision::decide(&decision::detect_installed_version(&dest), &latest);
//! println!("{}", decision.summary);
//! ```
//!
//! ## Explicitly out of scope
//!
//! This crate speaks the **public GitHub Releases API only**. It does not download, checksum, or
//! write the release binary — each consumer verifies against its own trust root (dig-installer's
//! plain SHA-256 check vs. the beacon's signed manifest), so that stays with the consumer. It also
//! does not speak the signed `updates.dig.net` feed — that is the beacon's own concern (issue
//! #513); this crate is the documented GitHub-native fallback/bootstrap source every consumer can
//! rely on independently of that feed.

pub mod decision;
pub mod github;
pub mod repo;
pub mod target;

pub use decision::{
    decide, decide_with_force, detect_installed_version, DetectedVersion, UpdateAction,
    UpdateDecision,
};
pub use github::{latest_release, latest_tag, latest_version, release_by_tag, Release};
pub use repo::{tag_from_input, version_from_tag, Repo};
pub use target::{Arch, Os, Target};
