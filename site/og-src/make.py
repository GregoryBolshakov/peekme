"""Render the 1280x640 social preview images into static/og/ with headless Chrome.

python3 og-src/make.py [path/to/chrome]
"""
import html, os, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SITE = os.path.dirname(HERE)
CHROME = sys.argv[1] if len(sys.argv) > 1 else "google-chrome"
MARK = open(os.path.join(SITE, "static/favicon.svg")).read()

IMAGES = [
    ("home", "claude", "Explain any text in your coding agent's terminal", "Claude Code, Codex CLI, Copilot CLI, Kiro CLI"),
    ("claude-code", "claude", "Explain any words in a Claude Code answer", "Select, press Alt+P, read it right under"),
    ("codex", "codex", "Explain any words in a Codex CLI answer", "Select, press Alt+P, read it right under"),
    ("copilot-cli", "copilot", "Explain any words in a Copilot CLI answer", "Select, press Alt+P, read it right under"),
    ("kiro-cli", "kiro", "Explain any words in a Kiro CLI answer", "Select, press Alt+P, read it right under"),
    ("side-questions", "claude", "/btw, /side, /ask and peekme", "Side questions in coding agents, compared"),
    ("how-it-works", "codex", "How peekme draws a box over a screen it doesn't own", "Pseudo-terminal, screen copy, context"),
]

PAGE = """<!doctype html><html><head><meta charset="utf-8"><style>
html,body{{margin:0;width:1280px;height:640px;background:#11111b;color:#cdd6f4;
font-family:system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;overflow:hidden}}
.wrap{{display:grid;grid-template-columns:470px 1fr;gap:40px;align-items:center;height:640px;padding:0 0 0 64px;box-sizing:border-box}}
.logo{{display:flex;align-items:center;gap:12px;font:700 30px ui-monospace,"DejaVu Sans Mono",monospace;margin-bottom:34px}}
.logo svg{{width:44px;height:44px}}
h1{{font-size:50px;line-height:1.12;margin:0 0 22px;letter-spacing:-0.5px}}
p{{font-size:25px;color:#a6adc8;margin:0 0 30px}}
.url{{font:600 22px ui-monospace,"DejaVu Sans Mono",monospace;color:#89b4fa}}
.shot{{width:790px;border-radius:14px;border:1px solid #313244;box-shadow:0 20px 60px rgba(0,0,0,.6)}}
</style></head><body><div class="wrap"><div>
<div class="logo">{mark}<span>peekme</span></div>
<h1>{title}</h1><p>{sub}</p><div class="url">peekme.dev</div>
</div><img class="shot" src="file://{shot}"></div></body></html>"""

out = os.path.join(SITE, "static/og")
os.makedirs(out, exist_ok=True)
with tempfile.TemporaryDirectory() as tmp:
    for name, media, title, sub in IMAGES:
        page = os.path.join(tmp, name + ".html")
        open(page, "w").write(PAGE.format(mark=MARK, title=html.escape(title), sub=html.escape(sub),
                                          shot=os.path.join(SITE, "static/media", media + ".webp")))
        subprocess.run([CHROME, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--allow-file-access-from-files",
                        "--window-size=1280,640", f"--screenshot={os.path.join(out, name + '.png')}", "file://" + page],
                       check=True, stderr=subprocess.DEVNULL, stdout=subprocess.DEVNULL)
        print(name)
