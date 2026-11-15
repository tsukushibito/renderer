"""mdBook preprocessor: keep repository links usable in a portable HTML book.

Chapters and assets inside docs/ stay local. Links leaving docs/ point to the
same Git revision on GitHub, so the HTML artifact needs no checkout beside it.
Only Markdown link destinations are changed; fenced examples are preserved.
"""

import json
from pathlib import Path
import re
import subprocess
import sys
from urllib.parse import quote, unquote, urlsplit, urlunsplit


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "supports":
        return 0 if sys.argv[2:] == ["html"] else 1

    sys.stdin.reconfigure(encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")
    context, book = json.load(sys.stdin)
    docs = Path(context["root"]).resolve()
    repo = docs.parent
    revision = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=repo, text=True
    ).strip()
    repository = context["config"]["output"]["html"]["git-repository-url"]

    def destination(value, source):
        url = urlsplit(value)
        if url.scheme or url.netloc or not url.path or url.path.startswith("/"):
            return value
        target = (source.parent / unquote(url.path)).resolve()
        if target.is_relative_to(docs) and target != docs / "SUMMARY.md":
            return value
        if not target.is_relative_to(repo) or not target.exists():
            raise ValueError(f"Invalid repository link in {source}: {value}")
        path = quote(target.relative_to(repo).as_posix(), safe="/")
        remote = urlsplit(f"{repository}/blob/{revision}/{path}")
        return urlunsplit(remote._replace(query=url.query, fragment=url.fragment))

    def rewrite(content, source):
        lines = []
        fence = None
        for line in content.splitlines(keepends=True):
            marker = re.match(r"^\s{0,3}(`{3,}|~{3,})", line)
            if marker:
                token = marker.group(1)
                if fence is None:
                    fence = token
                elif token[0] == fence[0] and len(token) >= len(fence):
                    fence = None
                lines.append(line)
                continue
            if fence is None:
                line = re.sub(
                    r"(\]\()([^\s)]+)(\))",
                    lambda m: m[1] + destination(m[2], source) + m[3],
                    line,
                )
                line = re.sub(
                    r"^(\s{0,3}\[[^]]+\]:\s*)(\S+)",
                    lambda m: m[1] + destination(m[2], source),
                    line,
                )
            lines.append(line)
        return "".join(lines)

    def visit(sections):
        for section in sections:
            chapter = section.get("Chapter") if isinstance(section, dict) else None
            if chapter:
                source = docs / chapter["source_path"]
                chapter["content"] = rewrite(chapter["content"], source)
                visit(chapter["sub_items"])

    visit(book["items"])
    json.dump(book, sys.stdout, ensure_ascii=False)
    return 0


if __name__ == "__main__":
    sys.exit(main())

