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

# Key table and settings table: the same rows in the same order.
for header, page in [("| Key |", "content/docs/keys.md"), ("| Variable |", "content/docs/settings.md")]:
    a, b = table_rows(readme, header), table_rows(read("site", page), header)
    if a is None or b is None:
        errors.append(f"table '{header}' missing in README.md or site/{page}")
    elif a != b:
        errors.append(f"table '{header}' differs between README.md and site/{page}:\n"
                      + "\n".join(f"  README: {x}\n  site:   {y}" for x, y in zip(a + [""] * len(b), b + [""] * len(a)) if x != y))

# Every documented variable is read by the code.
src = "".join(open(f).read() for f in glob.glob(os.path.join(ROOT, "src/**/*.rs"), recursive=True))
for var in sorted(set(re.findall(r"`(PEEKME_[A-Z_]+)`", readme))):
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
if site_v != cargo:
    errors.append(f"site/config.toml version {site_v} != Cargo.toml {cargo}")
if not readme_v or readme_v.group(1) != cargo:
    errors.append(f"README Status line version != Cargo.toml {cargo}")

# Tested agent versions on each agent page.
status = re.search(r"^Status:.*?(?=\n\n)", readme, re.M | re.S).group(0)
for name, page in [("Claude Code", "claude-code"), ("Codex", "codex"), ("Copilot CLI", "copilot-cli"), ("Kiro CLI", "kiro-cli")]:
    m = re.search(re.escape(name) + r" ([0-9][0-9.]*[0-9])", status)
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
