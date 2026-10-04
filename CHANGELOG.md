# Changelog

Notable user-facing changes are recorded here. The project uses semantic versions;
`Cargo.toml` is the version authority. `Unreleased` entries are not a published
release. Published artifacts and their publication dates are recorded in
[GitHub Releases](https://github.com/IamK77/Lattice/releases).

## [Unreleased]

## [0.1.0]

### Added

- Terminal AI agent with configurable model endpoints, project instructions,
  file and command tools, and resumable, auditable conversation history.
- Composable components connected through serializable event contracts, with
  optional browser, language-service, desktop, and frontend integrations.
- English and Simplified Chinese product and setup guides.
- initialize Lattice (d83a2c5c9514).
- establish Cargo-based release versioning (45d2d07d5000).
- prepare reviewable releases from protected source branches (532f608a763d).
- bind native release archives to verified build inputs (a8581d1c1044).
- publish approved releases with provenance and recoverable drafts (e90b6ca12f36).
- synchronize stable history through protected native pull requests (821102229599).

### Changed

- Adopt Apache-2.0 and provide contribution, security-reporting, and support paths.
- Make `Cargo.toml` the formal version source. Development builds are visibly
  distinguished from release builds; commit counts no longer change the version.
- Use English Conventional Commits without a custom `Type:` trailer requirement.

### Security

- Update the locked h2 dependency to 0.4.16 for RUSTSEC-2026-0258.
- Add dependency/source/license checks, pinned CI actions, and time-limited
  review records for existing transitive maintenance advisories.
- Document that tool execution is not a sandbox, interruption is not rollback,
  and retained or model-bound data can contain secrets.

### Fixed

- patch h2 and add supply-chain verification (b5f1761384d9).
- align the minimum Rust version with file locking (cafdc62f6146).
- correct command help and operational guidance (b3604403c8e2).
- allow exact commit comparison paths in repository API client (996e79574177).
- audit indirect advisories and replace unsound lru dependency (d8de7fcec32a).
- synchronize diverged histories through a reusable integration branch (7407dc109d22).

