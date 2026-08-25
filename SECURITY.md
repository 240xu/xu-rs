# Security Policy

## Supported Versions

`xu` is pre-1.0. Security fixes target the latest released version.

## Secret Handling

Provider API keys are written to target tool config files when those tools require plaintext credentials. Diffs, command output, and previews redact common secret fields.

Backups may contain secrets because they copy the target config file. Treat `*.xu.*.bak` files and release/debug logs as sensitive local data.

## Reporting Issues

Open a private security advisory on GitHub if the repository supports it, or contact the maintainers through the repository issue tracker without posting API keys or private config files.
