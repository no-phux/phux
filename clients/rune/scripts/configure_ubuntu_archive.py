"""Use Ubuntu's canonical HTTPS archive on ephemeral Linux CI runners.

Keep the runner's suites, components, architectures and signing keys intact.
Only the observed EC2 archive URL changes; ARM ports and other sources do not.
"""
from pathlib import Path
import re


EC2_ARCHIVE = re.compile(
    r"(?<!\S)https?://us-east-2\.ec2\.archive\.ubuntu\.com/ubuntu(?=[/\s]|$)"
)


def canonical_archive(text):
    lines = []
    for line in text.splitlines(keepends=True):
        if not line.lstrip().startswith("#"):
            line = EC2_ARCHIVE.sub("https://archive.ubuntu.com/ubuntu", line)
        lines.append(line)
    return "".join(lines)


def configure_sources(root):
    # Hosted runners can select their archive through mirror+file: indirection.
    # Rewriting only ubuntu.sources would leave the slow first-priority route.
    sources = [root / "apt-mirrors.txt", root / "sources.list",
               *sorted((root / "sources.list.d").glob("*.list")),
               *sorted((root / "sources.list.d").glob("*.sources"))]
    for source in sources:
        if not source.is_file():
            continue
        original = source.read_bytes().decode("utf-8")
        updated = canonical_archive(original)
        if updated != original:
            source.write_bytes(updated.encode("utf-8"))
            print(f"Using canonical Ubuntu HTTPS archive in {source}")


if __name__ == "__main__":
    configure_sources(Path("/etc/apt"))
