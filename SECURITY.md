# Security policy

## Supported versions

Only the latest development state on the default branch is currently supported. There are no stable release branches yet.

## Reporting a vulnerability

Do not report security vulnerabilities in public issues. Use GitHub's private vulnerability reporting feature for this repository where available. Include the affected version or commit, reproduction steps, impact, and any suggested mitigation.

The CLI reads schematic/project files and invokes the configured KiCad CLI as a child process. Do not feed it untrusted files in privileged automation environments without applying the same isolation and resource limits used for other file parsers and subprocesses.
