# herdr task runner

# Run tests
test:
    cargo nextest run --locked --status-level fail --final-status-level fail --failure-output final --success-output never
    python3 -m unittest scripts.test_agent_detection_manifest_check scripts.test_branch_policy scripts.test_changelog scripts.test_preview scripts.test_vendor_libghostty_vt scripts.test_vendor_portable_pty
    just plugin-marketplace-test

# Run one nextest filter, e.g. `just test-one codex_stale_working`
test-one filter:
    cargo nextest run --locked "{{filter}}" --status-level fail --final-status-level fail --failure-output final --success-output never

# Run fast local lint checks
lint:
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings

# Run PR CI checks
ci filter='all()': lint
    cargo nextest run --locked -E "{{filter}}" --status-level fail --final-status-level slow --failure-output final --success-output never
    python3 -m unittest scripts.test_branch_policy
    just plugin-marketplace-test

# Run Windows target lint from Unix/macOS to catch cfg(windows) compile and clippy failures before CI
windows-lint:
    rustup target add x86_64-pc-windows-msvc
    LIBGHOSTTY_VT_SIMD=false cargo clippy --bin herdr --locked --target x86_64-pc-windows-msvc -- -D warnings

# Check formatting + run unit tests + Windows target lint + maintenance script tests
check: ci windows-lint
    python3 -m unittest scripts.test_agent_detection_manifest_check scripts.test_branch_policy scripts.test_changelog scripts.test_preview scripts.test_vendor_libghostty_vt scripts.test_vendor_portable_pty
    @echo "docs reminder: if this changes user-facing behavior, make sure the relevant release docs are updated or called out before release."

# Install repo-local git hooks
install-hooks:
    git config core.hooksPath .githooks
    chmod +x .githooks/pre-commit
    chmod +x .githooks/commit-msg
    @echo "installed git hooks from .githooks"

# Build release binary
build:
    cargo build --release --locked

# Build the website and documentation
website-build:
    cd website && bun install --frozen-lockfile && bun run build

# Run plugin marketplace Worker tests
plugin-marketplace-test:
    cd workers/plugin-marketplace && bun test

# Build the vendored libghostty-vt source dist
build-libghostty-vt:
    scripts/build_vendored_libghostty_vt.sh

# Check that release docs and changelog have been finalized from docs/next before release
release-docs-check:
    python3 scripts/agent_detection_manifest_check.py --require-website
    @for file in README.md CHANGELOG.md; do \
        if ! diff -u "$file" "docs/next/$file"; then \
            echo "error: $file differs from docs/next/$file; finalize release docs before releasing"; \
            exit 1; \
        fi; \
    done
    @for file in CONFIGURATION.md INTEGRATIONS.md SOCKET_API.md; do \
        if [ -e "$file" ]; then \
            echo "error: $file was replaced by website docs; remove the root copy"; \
            exit 1; \
        fi; \
    done
    @test -d docs/next/website/src/content/docs
    @for file in website/src/content/docs/*.mdx; do \
        staged="docs/next/website/src/content/docs/$(basename "$file")"; \
        if [ ! -f "$staged" ]; then \
            echo "error: $staged is missing; docs/next/website/src/content/docs must mirror website/src/content/docs"; \
            exit 1; \
        fi; \
        if ! diff -u "$file" "$staged"; then \
            echo "error: $file differs from $staged; finalize website docs before releasing"; \
            exit 1; \
        fi; \
    done
    @for file in docs/next/website/src/content/docs/*.mdx; do \
        released="website/src/content/docs/$(basename "$file")"; \
        if [ ! -f "$released" ]; then \
            echo "error: $file has no matching released website doc"; \
            exit 1; \
        fi; \
    done

# Prepare the release commit without tagging or pushing (usage: just release-prepare 0.1.1)
release-prepare version:
    @printf '%s\n' '{{version}}' | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || { \
        echo "error: version must look like 0.6.6 without a v prefix"; \
        exit 1; \
    }
    @branch="$(git branch --show-current)"; \
    if [ "$branch" != "release/{{version}}" ]; then \
        echo "error: release-prepare must run from release/{{version}}, got $branch"; \
        exit 1; \
    fi
    @if [ -n "$(git status --porcelain)" ]; then \
        echo "error: commit your changes first"; \
        exit 1; \
    fi
    @git fetch origin '+refs/heads/master:refs/remotes/origin/master' '+refs/heads/dev:refs/remotes/origin/dev' --tags
    @if ! git merge-base --is-ancestor origin/master HEAD; then \
        echo "error: release/{{version}} does not contain current origin/master release metadata"; \
        exit 1; \
    fi
    @if ! git merge-base --is-ancestor origin/dev HEAD; then \
        echo "error: release/{{version}} does not contain current origin/dev"; \
        exit 1; \
    fi
    @if git rev-parse "v{{version}}" >/dev/null 2>&1; then \
        echo "error: tag v{{version}} already exists"; \
        exit 1; \
    fi
    just release-docs-check
    python3 scripts/changelog.py prepare --version {{version}}
    cp CHANGELOG.md docs/next/CHANGELOG.md
    sed -i.bak 's/^version = ".*"/version = "{{version}}"/' Cargo.toml && rm -f Cargo.toml.bak
    cargo update -p herdr --offline
    just check
    git add CHANGELOG.md docs/next/CHANGELOG.md Cargo.toml Cargo.lock
    git diff --cached --quiet || git commit -m "release: v{{version}}"
    @echo "v{{version}} release commit prepared. Review it, then run: just release-publish {{version}}"

# Tag and push an already-prepared release commit (usage: just release-publish 0.1.1)
release-publish version:
    @printf '%s\n' '{{version}}' | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || { \
        echo "error: version must look like 0.6.6 without a v prefix"; \
        exit 1; \
    }
    @if [ -n "$(git status --porcelain)" ]; then \
        echo "error: working tree must be clean before publishing"; \
        exit 1; \
    fi
    @branch="$(git branch --show-current)"; \
    if [ "$branch" != "release/{{version}}" ]; then \
        echo "error: release-publish must run from release/{{version}}, got $branch"; \
        exit 1; \
    fi
    @git fetch origin '+refs/heads/master:refs/remotes/origin/master' '+refs/heads/dev:refs/remotes/origin/dev' --tags
    @if git rev-parse "v{{version}}" >/dev/null 2>&1; then \
        echo "error: tag v{{version}} already exists"; \
        exit 1; \
    fi
    @cargo_version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"; \
    if [ "$cargo_version" != "{{version}}" ]; then \
        echo "error: Cargo.toml version $cargo_version does not match {{version}}"; \
        exit 1; \
    fi
    just release-docs-check
    python3 scripts/changelog.py extract --version {{version}} --output /tmp/herdr-release-notes-check.md
    rm -f /tmp/herdr-release-notes-check.md
    @local_head="$(git rev-parse HEAD)"; \
    master_head="$(git rev-parse origin/master)"; \
    dev_head="$(git rev-parse origin/dev)"; \
    if ! git merge-base --is-ancestor "$master_head" "$local_head"; then \
        echo "error: origin/master is not an ancestor of the release commit"; \
        exit 1; \
    fi; \
    if ! git merge-base --is-ancestor "$dev_head" "$local_head"; then \
        echo "error: origin/dev is not an ancestor of the release commit"; \
        exit 1; \
    fi; \
    if [ "$local_head" != "$master_head" ]; then \
        echo "promoting reviewed release commit to origin/master"; \
        git push origin HEAD:master; \
    fi
    git tag -a v{{version}} -m "v{{version}}"
    git push origin v{{version}}
    @echo "v{{version}} released — GitHub Actions building binaries and updating website/latest.json"

# The release flow is deliberately two-step so review/approval separates preparation from publication.
release version:
    @echo "error: combined release is disabled; run 'just release-prepare {{version}}', review/approve, then 'just release-publish {{version}}'" >&2
    @exit 1

# Print default config
default-config:
    cargo run --release --locked -- --default-config
