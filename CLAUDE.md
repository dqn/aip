# aip

## Release Procedure

Trigger: user says `release`

1. Check commits since last release to determine version bump (feat → minor, fix → patch)
2. Update `version` in `Cargo.toml` (Cargo.lock is updated automatically)
3. Run `cargo check` to verify
4. Commit with message `chore: release <version>`
5. Confirm with user, then:
   - `git push`
   - `cargo publish`
   - `gh release create v<version>` with changelog from commits since last release
