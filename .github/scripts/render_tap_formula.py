#!/usr/bin/env python3
"""Render the tap Formula/shac.rb from the repo's binary-install formula.

The repo's Formula/shac.rb is the single source of truth for the formula body
(desc, service, caveats, test). This script injects the release-specific
version and the per-platform download url + sha256 into it, then writes the
result to the tap. Substitution is block-scoped: the macOS sha is only ever
written inside the `on_macos do ... end` block and the Linux sha inside
`on_linux do ... end`, so the two 64-hex strings can never be swapped.

Reads from the environment:
  TAG         release tag, e.g. "v0.6.2"
  VERSION     bare version, e.g. "0.6.2"
  MACOS_SHA   sha256 of shac-macos-universal.tar.gz
  LINUX_SHA   sha256 of shac-linux-x86_64.tar.gz
  SRC         path to the repo formula (source of truth)
  OUT         path to write the rendered tap formula
"""
import os
import re

REPO = "Neftedollar/sh-autocomplete"


def patch_platform(src: str, os_kw: str, filename: str, tag: str, sha: str) -> str:
    """Replace the url + sha256 inside a single `on_<os_kw> do ... end` block."""
    # Last gate before the tap: refuse to render an empty/garbage sha256, which
    # would ship a formula that fails every `brew install` with a checksum error.
    if not re.fullmatch(r"[0-9a-f]{64}", sha):
        raise SystemExit(
            f"render_tap_formula: {os_kw} sha256 is not 64 hex chars: {sha!r}"
        )
    url = f"https://github.com/{REPO}/releases/download/{tag}/{filename}"

    def repl(match: "re.Match[str]") -> str:
        block = match.group(0)
        block = re.sub(r'url ".*"', f'url "{url}"', block, count=1)
        block = re.sub(r'sha256 ".*"', f'sha256 "{sha}"', block, count=1)
        return block

    pattern = rf"  on_{os_kw} do\n.*?\n  end"
    new_src, n = re.subn(pattern, repl, src, count=1, flags=re.DOTALL)
    if n != 1:
        raise SystemExit(f"render_tap_formula: on_{os_kw} block not found in template")
    return new_src


def main() -> None:
    tag = os.environ["TAG"]
    version = os.environ["VERSION"]
    src = open(os.environ["SRC"]).read()

    src, n = re.subn(r'^  version ".*"', f'  version "{version}"', src, count=1, flags=re.M)
    if n != 1:
        raise SystemExit("render_tap_formula: version line not found in template")

    src = patch_platform(src, "macos", "shac-macos-universal.tar.gz", tag, os.environ["MACOS_SHA"])
    src = patch_platform(src, "linux", "shac-linux-x86_64.tar.gz", tag, os.environ["LINUX_SHA"])

    with open(os.environ["OUT"], "w") as f:
        f.write(src)
    print(f"rendered tap formula for {tag}")


if __name__ == "__main__":
    main()
