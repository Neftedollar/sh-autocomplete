#!/usr/bin/env python3
"""Additively upsert one platform's bottle sha256 into the tap Formula/shac.rb.

Run with the current working directory set to the cloned tap repo. Reads
PLATFORM, SHA and ROOT_URL from the environment. Idempotent: re-running for the
same platform replaces that platform's line; running for a second platform adds
to the existing block without clobbering the first. This lets each CI matrix leg
publish its own bottle independently, so a slow/stuck runner for one platform
never blocks another platform's users from getting a bottle.
"""
import os
import re

PATH = "Formula/shac.rb"


def main() -> None:
    platform = os.environ["PLATFORM"]
    sha = os.environ["SHA"]
    root_url = os.environ["ROOT_URL"]
    new_line = f'    sha256 cellar: :any_skip_relocation, {platform}: "{sha}"'

    src = open(PATH).read()
    m = re.search(r"\n  bottle do\n(.*?)\n  end\n", src, flags=re.DOTALL)
    if m:
        body = [
            line
            for line in m.group(1).split("\n")
            if line.strip() and f", {platform}:" not in line
        ]
        root = [line for line in body if "root_url" in line] or [
            f'    root_url "{root_url}"'
        ]
        shas = sorted(line for line in body if "root_url" not in line) + [new_line]
        block = "\n  bottle do\n" + "\n".join(root + shas) + "\n  end\n"
        src = src[: m.start()] + block + src[m.end() :]
    else:
        block = f'  bottle do\n    root_url "{root_url}"\n{new_line}\n  end\n'
        src = src.replace('  license "MIT"\n', '  license "MIT"\n\n' + block, 1)

    open(PATH, "w").write(src)
    print(f"merged bottle sha for {platform}")


if __name__ == "__main__":
    main()
