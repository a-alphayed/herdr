from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = ROOT / ".github" / "workflows"


class BranchPolicyTests(unittest.TestCase):
    def read(self, relative: str) -> str:
        return (ROOT / relative).read_text(encoding="utf-8")

    def test_development_ci_targets_dev(self) -> None:
        ci = self.read(".github/workflows/ci.yml")
        self.assertIn("pull_request:\n    branches: [dev]", ci)
        self.assertIn("push:\n    branches: [dev, windows]", ci)
        self.assertIn("github.ref_name == 'dev'", ci)
        self.assertNotIn("branches: [master, windows]", ci)

        nix = self.read(".github/workflows/nix.yml")
        self.assertIn("pull_request:\n    branches: [dev]", nix)
        self.assertIn("push:\n    branches: [dev]", nix)
        self.assertNotIn("branches: [master]", nix)

    def test_preview_selects_dev_but_writes_only_release_metadata_to_master(self) -> None:
        workflow = self.read(".github/workflows/preview.yml")
        self.assertIn("description: Optional dev commit SHA to publish", workflow)
        self.assertIn("ref: dev", workflow)
        self.assertIn("refs/heads/dev:refs/remotes/origin/dev", workflow)
        self.assertIn('git merge-base --is-ancestor "$commit" origin/dev', workflow)
        self.assertIn("select-commit --ref origin/dev", workflow)
        self.assertIn("git show origin/master:website/preview.json", workflow)
        self.assertIn("git checkout --detach origin/master", workflow)
        self.assertIn('test "$(git diff --cached --name-only)" = "website/preview.json"', workflow)
        self.assertIn("git add website/preview.json", workflow)
        self.assertIn("HEAD:master", workflow)
        self.assertNotIn("reachable from origin/master", workflow)

        preview = self.read("scripts/preview.py")
        self.assertIn('default="origin/dev"', preview)
        self.assertIn("on `dev`", preview)
        self.assertNotIn("current master branch", preview)

    def test_release_guards_require_release_branch_and_both_long_lived_refs(self) -> None:
        justfile = self.read("justfile")
        self.assertIn('release-prepare must run from release/{{version}}', justfile)
        self.assertIn('release-publish must run from release/{{version}}', justfile)
        self.assertGreaterEqual(justfile.count("refs/remotes/origin/dev"), 2)
        self.assertGreaterEqual(justfile.count("merge-base --is-ancestor origin/dev"), 1)
        self.assertIn('dev_head="$(git rev-parse origin/dev)"', justfile)
        self.assertIn("combined release is disabled", justfile)
        self.assertIn("herdr-test-env :=", justfile)
        self.assertEqual(justfile.count("{{herdr-test-env}} cargo nextest run"), 3)
        self.assertIn("-u HERDR_SOCKET_PATH", justfile)
        self.assertIn("-u HERDR_CONFIG_PATH", justfile)
        self.assertIn("-u HERDR_STARTUP_CWD", justfile)

        release = self.read(".github/workflows/release.yml")
        self.assertIn("Verify tagged commit is on production master", release)
        self.assertIn('git merge-base --is-ancestor "$tag_commit" origin/master', release)

    def test_dev_integration_marks_but_does_not_close_pending_issues(self) -> None:
        self.assertFalse((WORKFLOWS / "label-next-release-issues.yml").exists())
        workflow = self.read(".github/workflows/mark-pending-release-issues.yml")
        self.assertIn("name: Mark pending-release issues", workflow)
        self.assertIn("branches: [dev]", workflow)
        self.assertIn("Implemented on dev and pending a published release.", workflow)
        self.assertNotIn("gh issue close", workflow)

    def test_policy_documents_the_branch_and_metadata_boundaries(self) -> None:
        agents = self.read("AGENTS.md")
        decision = self.read("BRANCHING.md")
        contributing = self.read("CONTRIBUTING.md")

        self.assertIn("`dev` as its development integration branch", agents)
        self.assertIn("`master` as its production/release branch", agents)
        self.assertIn("Release-channel metadata exception", decision)
        self.assertIn("Normal task branches start from", decision)
        self.assertIn("reviewed work lands on `dev`", decision)
        self.assertIn("Steam Deck live-dev profile", decision)
        self.assertIn("Require exact normalized pre/post identity equality", decision)
        self.assertIn("Linux live-dev command", agents)
        self.assertIn("do not start an empty parallel dev server", agents.lower())
        self.assertIn("Open normal pull requests against `dev`", contributing)


if __name__ == "__main__":
    unittest.main()
