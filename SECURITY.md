# Security Policy

## Supported versions

Security fixes go into the latest release of sge, and its packages: the deb and rpm
from apt.dbinvent.com, the tarball, and the `dbinvent/sge` image. Upgrade to the
latest release to get them.

| Version | Supported |
|---|---|
| 2.3.x (latest) | yes |
| older | no |

## Reporting a vulnerability

Do not open a public issue for a security problem. Report it by email to
**support@dbinvent.com**, with "sge security" in the subject, and include:

- the sge version (`sge --version`) and how it was installed;
- what an attacker can do, and the steps or a proof of concept that show it;
- any fix or mitigation you know of.

We acknowledge the report, keep you informed while we fix it, and credit you in the
release notes unless you ask us not to. Please give us the time to release a fix
before you disclose the problem publicly.

## Dependencies

The dependencies sge is built with are listed in [lib-licenses.txt](lib-licenses.txt)
and pinned in `Cargo.lock`; `cargo audit` checks them against the RustSec advisory
database.
