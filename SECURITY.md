# Security

termvisor handles SSH logins, saved passwords and key passphrases, so
security problems get priority.

## Reporting a vulnerability

Please do not open a public issue for a security problem: it would show
the problem to everyone before a fix is out.

Report it privately instead: on the repository page open **Security** →
**Report a vulnerability**. Only the maintainer (and anyone he adds to
the report) can see it. Please include:

- the termvisor version and system (menu → About termvisor →
  Troubleshooting),
- what the problem is and what an attacker could do with it,
- steps or a proof of concept to reproduce it.

## What happens next

- You get an answer within a week.
- The fix is worked on in a temporary private fork that belongs to the
  report. If you want to send the fix yourself, a pull request there is
  welcome, even though public pull requests are only accepted from
  collaborators (see [CONTRIBUTING.md](CONTRIBUTING.md)).
- Once a release with the fix is out, the report is published as a
  security advisory, crediting you unless you ask not to be named.

## Supported versions

Only the latest release gets security fixes. The app checks for updates
on start and offers new releases.
