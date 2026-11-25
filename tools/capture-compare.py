"""Offscreen-only HTML reference raster and side-by-side acceptance sheets."""
from pathlib import Path
import base64
import re
from playwright.sync_api import sync_playwright
from PIL import Image, ImageDraw
import shutil

repo = Path(__file__).resolve().parents[1]
out = repo / "target/ui-snapshots/capture-remind"
out.mkdir(parents=True, exist_ok=True)
font = base64.b64encode((repo / "dock/assets/fonts/PretendardVariable.ttf").read_bytes()).decode()
with sync_playwright() as p:
    browser = p.chromium.launch(headless=True, channel="msedge")
    page = browser.new_page(viewport={"width":1440,"height":900}, device_scale_factor=1)
    page.route("http**/*", lambda route: route.abort())
    for name in ["HiCapture", "HiParts", "HiDesk", "HiLight", "PickCapture", "PickRemind"]:
        html = (repo / f"docs/design/widgets-2026-11-15/{name}.dc.html").read_text(encoding="utf-8")
        html = re.sub(r"<script\b[^>]*>.*?</script>", "", html, flags=re.S)
        html = re.sub(r"<link\b[^>]*>", "", html)
        html = html.replace("url('/_blob/236a248cb456c9cfc77f403d51f816b7')",f"url('data:font/ttf;base64,{font}')")
        html = html.replace("</head>", f"<style>@font-face{{font-family:'Noto Sans KR';src:url(data:font/ttf;base64,{font})}}x-dc{{display:block}}</style></head>")
        page.set_content(html, wait_until="load")
        page.evaluate("document.fonts.ready")
        page.screenshot(path=str(out / f"reference-{name}.png"))
        if name == "HiCapture":
            for i, state in enumerate(["empty","time","tab","send"]):
                page.locator(".lane").nth(i).screenshot(path=str(out / f"reference-capture-{state}.png"))
        if name == "HiParts":
            page.locator(".paper").first.screenshot(path=str(out / "reference-inbox-2x.png"))
    browser.close()

def sheet(names, target, columns=2, tile=(680,370)):
    images = [(n,Image.open(out/n).convert("RGB")) for n in names if (out/n).exists()]
    canvas=Image.new("RGB",(columns*tile[0],((len(images)+columns-1)//columns)*tile[1]),"#181818")
    d=ImageDraw.Draw(canvas)
    for i,(name,im) in enumerate(images):
        x=(i%columns)*tile[0];y=(i//columns)*tile[1]
        d.text((x+12,y+8),name,fill="white")
        im.thumbnail((tile[0]-24,tile[1]-34))
        canvas.paste(im,(x+12,y+28))
    canvas.save(out/target)

sheet([item for s in ["empty","time","tab","send"] for item in [f"reference-capture-{s}.png", f"capture-{s}-dark-100.png"]],"comparison-capture.png",tile=(1100,370))
reference_paper = Image.open(out/"reference-inbox-2x.png")
reference_paper.resize((260, round(reference_paper.height*260/reference_paper.width)), Image.Resampling.LANCZOS).save(out/"reference-inbox-1x.png")
sheet(["reference-inbox-1x.png"]+[f"inbox-{s}-{m}-100.png" for s in ["empty","three","overflow","folded","hover"] for m in ["dark","light"]],"comparison-inbox.png",columns=3,tile=(370,410))
sheet([f"reminder-{s}-{m}-{k}.png" for s in ["standing","bottom","side"] for m in ["dark","light"] for k in [28,56]],"comparison-reminders.png",columns=4,tile=(490,430))
sheet([f"capture-popover-{m}-{dpi}.png" for m in ["dark","light"] for dpi in [100,150]],"comparison-popover.png",columns=2,tile=(700,380))
sheet([f"capture-{s}-{m}-{dpi}.png" for m in ["dark","light"] for dpi in [100,150] for s in ["empty","time","tab","send"]],"matrix-capture.png",columns=4,tile=(680,220))
for source in (repo / "target/harmony-shots/notes-entry").glob("settings-*.png"):
    shutil.copy2(source, out/source.name)
sheet([f"settings-{m}-{d}x-{e}.png" for m in ["dark-glass","light-flat"] for d in [1,1.5] for e in ["on","off"]],"comparison-settings.png",columns=4,tile=(550,650))
sheet(["reference-PickRemind.png","reminder-standing-dark-28.png","reminder-standing-light-56.png"],"comparison-reminder-reference.png",columns=3,tile=(700,760))
print(out)
for variant in ["group", "say"]:
    names=[f"review-{variant}-{s}-{m}-{k}.png" for s in ["standing","bottom","side"] for m in ["dark","light"] for k in [28,56]]
    if all((out/name).exists() for name in names):
        sheet(names,f"comparison-review-{variant}.png",columns=4,tile=(490,430))
