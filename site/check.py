"""Fail when README.md and the site say different things. Run before a release.

python3 site/check.py
"""
import glob, os, re, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SITE = os.path.join(ROOT, "site")
read = lambda *p: open(os.path.join(ROOT, *p)).read()
errors = []


def table_rows(text, header):
    """Rows of the markdown table whose header line starts with `header`."""
    lines = text.splitlines()
    for i, line in enumerate(lines):
        if line.startswith(header):
            rows = []
            for row in lines[i + 2:]:
                if not row.startswith("|"):
                    break
                rows.append(row.strip())
            return rows
    return None


readme = read("README.md")

# The README keeps a short key table; the docs carry the full reference.
key_page = read("site", "content/docs/keys.md")
readme_keys = table_rows(readme, "| Key |")
if readme_keys is None or table_rows(key_page, "| Key |") is None:
    errors.append("key table missing in README.md or site/content/docs/keys.md")
else:
    for row in readme_keys:
        key = row.split("|")[1]
        for shortcut in re.findall(r"Alt\+[PQ]|PgUp|PgDn|Esc|Enter|Ctrl\+Home|Ctrl\+End", key):
            if shortcut not in key_page:
                errors.append(f"README key missing in site/content/docs/keys.md: {shortcut}")

# Every documented variable is read by the code.
src = "".join(open(f).read() for f in glob.glob(os.path.join(ROOT, "src/**/*.rs"), recursive=True))
for var in sorted(set(re.findall(r"`(PEEKME_[A-Z_]+)`", readme + read("site", "content/docs/settings.md")))):
    if var not in src:
        errors.append(f"{var} is documented but src/ never reads it")

# Install commands.
installs = re.findall(r"^(npm i -g peekme|peekme install)$", readme, re.M) + re.findall(
    r"`((?:brew|cargo) install [^`]+|curl -LsSf [^`]+)`", readme)
for cmd in installs:
    for page in ["templates/macros.html", "content/docs/install.md"]:
        if cmd not in read("site", page):
            errors.append(f"install command missing in site/{page}: {cmd}")

# Version.
cargo = re.search(r'^version = "([^"]+)"', read("Cargo.toml"), re.M).group(1)
site_v = re.search(r'^version = "([^"]+)"', read("site", "config.toml"), re.M).group(1)
readme_v = re.search(r"^Status: ([0-9][^ .]*\.[0-9]+\.[0-9]+)", readme, re.M)
# A beta (0.5.1-beta.1) goes to crates.io only: the README and the site
# keep the last release until the next one.
if "-" not in cargo and site_v != cargo:
    errors.append(f"site/config.toml version {site_v} != Cargo.toml {cargo}")
if "-" not in cargo and readme_v and readme_v.group(1) != cargo:
    errors.append(f"README Status line version != Cargo.toml {cargo}")

# A README may omit release and tested-version metadata and link to the docs.
# When it includes a Status paragraph, keep those versions in agreement.
status = re.search(r"^Status:.*?(?=\n\n)", readme, re.M | re.S)
if status:
    for name, page in [("Claude Code", "claude-code"), ("Codex", "codex"), ("Copilot CLI", "copilot-cli"), ("Kiro CLI", "kiro-cli")]:
        m = re.search(re.escape(name) + r" ([0-9][0-9.]*[0-9])", status.group(0))
        text = read("site", "content", page + ".md")
        if not m:
            errors.append(f"README Status line has no version for {name}")
        elif f"{name} {m.group(1)}" not in text:
            errors.append(f"site/content/{page}.md does not say tested with {name} {m.group(1)}")

if errors:
    print("README.md and the site differ:\n")
    print("\n\n".join(errors))
    sys.exit(1)
print("README.md and site agree")
