"""Check every local HTML link, fragment and resource in an mdBook artifact."""

from html.parser import HTMLParser
from pathlib import Path
import sys
from urllib.parse import unquote, urlsplit


class Page(HTMLParser):
    def __init__(self, source):
        super().__init__(convert_charrefs=True)
        self.ids = set()
        self.links = []
        self.feed(source)

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if attrs.get("id"):
            self.ids.add(attrs["id"])
        for name in ("href", "src"):
            if attrs.get(name):
                self.links.append(attrs[name])


def main():
    root = Path(sys.argv[1]).resolve()
    pages = {p: Page(p.read_text(encoding="utf-8")) for p in root.rglob("*.html")}
    if root / "index.html" not in pages:
        raise SystemExit(f"No generated index.html in {root}")
    failures = []
    count = 0
    for source, page in pages.items():
        for value in page.links:
            url = urlsplit(value)
            if url.scheme or url.netloc:
                continue
            # mdBook's 404 template uses host-root links for an unknown route.
            if source.name == "404.html" and url.path.startswith("/"):
                continue
            target = (source.parent / unquote(url.path)).resolve() if url.path else source
            if target.is_dir():
                target /= "index.html"
            count += 1
            if not target.is_relative_to(root) or not target.is_file():
                failures.append(f"{source.relative_to(root)}: missing local target {value}")
            elif url.fragment and target in pages:
                fragment = unquote(url.fragment)
                if fragment not in pages[target].ids:
                    failures.append(f"{source.relative_to(root)}: missing fragment {value}")
    if failures:
        raise SystemExit("\n".join(failures))
    print(f"Checked {len(pages)} HTML pages and {count} local links/resources/fragments.")


if __name__ == "__main__":
    main()
