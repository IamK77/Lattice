# Changelog

Notable user-facing changes are recorded here. The project uses semantic versions;
`Cargo.toml` is the version authority. `Unreleased` entries are not a published
release. Published artifacts and their publication dates are recorded in
[GitHub Releases](https://github.com/IamK77/Lattice/releases).

## [Unreleased]

### Added

- Terminal AI agent with configurable model endpoints, project instructions,
  file and command tools, and resumable, auditable conversation history.
- Composable components connected through serializable event contracts, with
  optional browser, language-service, desktop, and frontend integrations.
- English and Simplified Chinese product and setup guides.

### Changed

- Adopt Apache-2.0 and provide contribution, security-reporting, and support paths.
- Make `Cargo.toml` the formal version source. Development builds are visibly
  distinguished from release builds; commit counts no longer change the version.
- Use English Conventional Commits without a custom `Type:` trailer requirement.

### Security

- Document that tool execution is not a sandbox, interruption is not rollback,
  and retained or model-bound data can contain secrets.
