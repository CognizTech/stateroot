"""Publish the updater's existing release JSON shape as a public asset.

Run after checksums.txt generation, before uploading dist/* to GitHub.
The nightly display name must be the same one used by the release job.
"""

import json
import sys
from pathlib import Path
from urllib.parse import quote


def main():
    directory, repo, tag, name = sys.argv[1:]
    directory = Path(directory)
    filenames = [item.name for item in directory.iterdir() if item.is_file()]
    if "checksums.txt" not in filenames:
        raise SystemExit("checksums.txt must exist before publishing metadata")
    base = f"https://github.com/{repo}/releases/download/{quote(tag, safe='')}"
    body = {
        "tag_name": tag,
        "name": name,
        "assets": [
            {"name": filename, "browser_download_url": f"{base}/{quote(filename)}"}
            for filename in sorted(filenames)
            if filename != "stateroot-release.json"
        ],
    }
    (directory / "stateroot-release.json").write_text(
        json.dumps(body, indent=2) + "\n", encoding="utf-8"
    )


if __name__ == "__main__":
    main()
