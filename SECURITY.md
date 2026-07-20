# Security Policy

## Supported Versions

We release security updates for the following versions of Conductor:

| Version  | Supported          |
| -------- | ------------------ |
| 5.7.x    | :white_check_mark: |
| 5.6.x    | :white_check_mark: |
| < 5.6.0  | :x:                |

The supported line tracks the current published release stream. This table is
verified against the workspace version by an automated test
, which fails the build if a new
release train ships without updating the row above — so it can't silently fall
out of date again.

## Reporting a Vulnerability

We take the security of Conductor seriously. If you discover a security vulnerability, please follow these steps:

### 1. DO NOT Disclose Publicly

Please do not open a public GitHub issue or discussion about the vulnerability. This helps protect users while we work on a fix.

### 2. Report Privately

Send details of the vulnerability to: **security@amiable.dev**

Include in your report:
- Description of the vulnerability
- Steps to reproduce the issue
- Potential impact assessment
- Suggested fix (if you have one)
- Your name/handle for acknowledgment (optional)

### 3. Response Timeline

- **Initial Response**: Within 48 hours
- **Status Update**: Within 7 days with assessment and estimated fix timeline
- **Fix Released**: Critical issues within 14 days, others within 30 days
- **Public Disclosure**: After fix is released and users have had time to update (typically 7-14 days)

### 4. Security Update Process

1. We'll confirm the vulnerability and assess its severity
2. We'll develop a fix in a private repository
3. We'll release a patch version with the fix
4. We'll publish a security advisory with details and mitigation steps
5. We'll credit the reporter (if desired) in our security hall of fame

## Known Security Considerations

### HID Device Access

Conductor requires HID device access to control LED feedback on MIDI controllers. On macOS, this requires:
- Input Monitoring permissions granted in System Settings
- User consent for device access

**Mitigation**: Users should only grant permissions to official releases from trusted sources.

### Shell Command Execution

Conductor can execute shell commands as part of action mappings (e.g., launching applications, volume control). This is a powerful feature but requires careful configuration.

**Best Practices**:
- Review configuration files before loading them from untrusted sources
- Use absolute paths for shell commands
- Avoid running Conductor with elevated privileges unless necessary
- Validate all user-provided configuration inputs

### Configuration File Loading

Conductor loads TOML configuration files that define action mappings.

**Best Practices**:
- Only load configuration files from trusted sources
- Review configurations before applying them
- Keep backups of working configurations
- Use version control for configuration files

### Native Instruments Controller Editor Profiles

Conductor can load .ncmm3 profile files from Native Instruments Controller Editor.

**Best Practices**:
- Only load profiles from official Native Instruments sources or trusted creators
- XML parsing is done with a memory-safe Rust library (quick-xml)

## Security Hall of Fame

We acknowledge and thank the following security researchers for responsibly disclosing vulnerabilities:

*(No reports yet - be the first!)*

## Public Disclosure Policy

- We believe in coordinated disclosure to protect users
- Security patches are released before public disclosure
- We'll work with reporters to agree on disclosure timeline
- We'll credit reporters in release notes and this document (with permission)

## PGP Key

For encrypted vulnerability reports, use our PGP key:

```
(PGP key to be added if needed)
```

## Additional Resources

- [GitHub Security Advisories](https://github.com/monstrous-media/conductor/security/advisories)
- [SECURITY.md Guidelines](https://docs.github.com/en/code-security/security-advisories)
- [Common Weakness Enumeration (CWE)](https://cwe.mitre.org/)

---

**Last Updated**: 2026-06-02
