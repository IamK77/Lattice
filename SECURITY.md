# Security policy

## Report privately

Please do not report a suspected vulnerability in a public issue or discussion.
Use [GitHub private vulnerability reporting](https://github.com/IamK77/Lattice/security/advisories/new).
The report is shared with repository security-advisory maintainers, not the public issue tracker.

Include the affected version or commit, operating system, relevant component,
impact, and a minimal synthetic reproduction. Remove credentials, personal
conversation records, screenshots, and unrelated local data. If an exploit
requires a real secret, describe the requirement rather than uploading it.

If the private reporting form is unavailable, do not fall back to posting
exploit details publicly. Open a neutral issue asking the maintainer to restore
the reporting channel, without the vulnerability details.

## Support and disclosure

Before the first stable release, report the exact commit. For published versions,
security fixes are provided for the latest stable release; older releases have no
promised backport support. Development builds are not a separate supported release
line.

@IamK77 coordinates triage, fixes, and disclosure through the private advisory.
There is no guaranteed response time, paid incident response, or bug-bounty
program. The maintainer and reporter should coordinate disclosure after a fix or
mitigation is available; do not promise a publication date on another person's
behalf. Confirmed advisories should explain affected versions, impact,
mitigations, and the fixed version.

## Important boundaries

Lattice runs tools that can read and write files, execute commands, and access the
network. It is **not a sandbox**. Tool-declared permissions help prevent mistakes;
they do not contain a malicious component. Review code and permissions before
installing a component or skill, and treat external content as untrusted data.

Tool results, conversation records, attachments, and screenshots can contain
secrets. They may be retained locally and sent to the selected model provider.
Do not assume all secrets are redacted. Cancellation is not a rollback, and an
interrupted operation may already have had effects.

See [data and permissions](docs/getting-started.md#data-and-permissions) before
using sensitive files or accounts. Documented limitations do not make a new
vulnerability report unwelcome; explain how the observed behavior differs from
the intended boundary.
