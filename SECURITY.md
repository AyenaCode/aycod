# Security

aycod is a security boundary between an AI agent and your repository, so a bypass of the guard or the gate is a vulnerability.

**Report it privately** through GitHub: *Security* tab → *Report a vulnerability*. Please do not open a public issue for a bypass. Include the command or payload, the agent and its version, and what aycod answered.

Fixed bypasses are added to [`tests/corpus.txt`](tests/corpus.txt) before the fix ships, so they can never come back.

Only the latest release receives fixes.

## Verify a release

Every archive and the `install.sh` of a release are built by the release workflow and carries a signed build provenance attestation:

```bash
gh attestation verify aycod-x86_64-unknown-linux-musl.tar.gz --repo AyenaCode/aycod
```

The npm packages are published from the same workflow with npm provenance.
