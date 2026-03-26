# aip

## OAuth Parity with Claude Code

aip mirrors Claude Code's OAuth flow (token refresh, Keychain read/write, credential storage). When Claude Code updates its OAuth parameters, aip must match. After updating Claude Code, verify parity by extracting config from the binary:

```sh
# Extract OAuth config (CLIENT_ID, TOKEN_URL, scopes, Keychain service name)
strings /path/to/claude | grep -oE 'TOKEN_URL:"[^"]*"' | sort -u
strings /path/to/claude | grep -oE '"9d1c250a[^"]*"'
strings /path/to/claude | grep -oE 'OAUTH_FILE_SUFFIX:"[^"]*"' | sort -u
strings /path/to/claude | grep -oE 'grant_type:"refresh_token"[^}]+'
```

Key fields to keep in sync: `TOKEN_URL`, `CLIENT_ID`, refresh request parameters (`scope`), Keychain service name suffix (`-credentials`), credential JSON field names (`subscriptionType`, `scopes`, `rateLimitTier`).

## Release Procedure

Trigger: user says `release`

1. Check commits since last release to determine version bump (feat → minor, fix → patch)
2. Update `version` in `Cargo.toml` (Cargo.lock is updated automatically)
3. Run `cargo check` to verify
4. Commit with message `chore: release <version>`
5. Without waiting for confirmation:
   - `git push`
   - `git tag v<version>` and push the tag
   - `cargo publish`
   - `gh release create v<version>` with changelog from commits since last release
