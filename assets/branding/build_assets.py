"""Build standalone app SVG and PNG/ICO exports from the approved vector logo.

Run: python3 assets/branding/build_assets.py
Requires Inkscape for raster exports and ImageMagick for the multi-size ICO.
The SVG artwork has no font, image, or network dependencies.
"""

from copy import deepcopy
from pathlib import Path
import shutil
import subprocess
import xml.etree.ElementTree as ET


ROOT = Path(__file__).resolve().parent
NS = "http://www.w3.org/2000/svg"
ET.register_namespace("", NS)
logo = ET.parse(ROOT / "regulus-logo.svg")
mark = logo.find(f".//{{{NS}}}g[@id='regulus-mark']")
assert mark is not None, "Missing canonical monogram"

# Keep the rounded square and highlights inside a transparent 1024 px canvas.
# The logo is copied as paths, never re-generated or rendered as lettering.
icon = ET.fromstring('''<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024" role="img" aria-labelledby="title desc">
  <title id="title">Regulus — icono de aplicación</title>
  <desc id="desc">R con estrella blanca sobre un cuadrado azul noche oscuro con esquinas continuas redondeadas, estilo iPhone.</desc>
  <defs>
    <radialGradient id="body" cx=".29" cy=".18" r=".94">
      <stop stop-color="#24354D"/>
      <stop offset=".26" stop-color="#152235"/>
      <stop offset=".57" stop-color="#0B1422"/>
      <stop offset=".85" stop-color="#070D16"/>
      <stop offset="1" stop-color="#04080F"/>
    </radialGradient>
    <radialGradient id="edge">
      <stop offset=".80" stop-color="#77A1D5" stop-opacity="0"/>
      <stop offset=".95" stop-color="#77A1D5" stop-opacity=".08"/>
      <stop offset=".986" stop-color="#A8CFFF" stop-opacity=".48"/>
      <stop offset="1" stop-color="#4776A9" stop-opacity=".12"/>
    </radialGradient>
    <radialGradient id="reflection" cx=".34" cy="0" r=".9">
      <stop stop-color="#9DB9DE" stop-opacity=".16"/>
      <stop offset=".45" stop-color="#7695BC" stop-opacity=".04"/>
      <stop offset="1" stop-color="#A0C6FF" stop-opacity="0"/>
    </radialGradient>
    <linearGradient id="rim" x1=".15" y1="0" x2=".85" y2="1">
      <stop stop-color="#A6C3E7" stop-opacity=".44"/>
      <stop offset=".36" stop-color="#7CA8DB" stop-opacity=".16"/>
      <stop offset=".67" stop-color="#152A44" stop-opacity=".12"/>
      <stop offset="1" stop-color="#84B6ED" stop-opacity=".22"/>
    </linearGradient>
    <linearGradient id="ink" x1="0" y1="0" x2=".6" y2="1">
      <stop stop-color="#FFFFFF"/>
      <stop offset="1" stop-color="#DFEDFF"/>
    </linearGradient>
    <linearGradient id="lower-light" x1="0" y1="0" x2="1" y2="0">
      <stop stop-color="#699FDA" stop-opacity="0"/>
      <stop offset=".6" stop-color="#86BCF8" stop-opacity=".15"/>
      <stop offset="1" stop-color="#699FDA" stop-opacity="0"/>
    </linearGradient>
    <filter id="shadow" x="-.25" y="-.25" width="1.5" height="1.5" color-interpolation-filters="sRGB">
      <feGaussianBlur in="SourceAlpha" stdDeviation="17" result="blur"/>
      <feOffset in="blur" dy="18" result="offset"/>
      <feFlood flood-color="#020812" flood-opacity=".38"/>
      <feComposite in2="offset" operator="in"/>
      <feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge>
    </filter>
    <filter id="mark-shadow" x="-.2" y="-.2" width="1.4" height="1.4" color-interpolation-filters="sRGB">
      <feGaussianBlur in="SourceAlpha" stdDeviation="7" result="blur"/>
      <feOffset in="blur" dy="7" result="offset"/>
      <feFlood flood-color="#030C1B" flood-opacity=".30"/>
      <feComposite in2="offset" operator="in"/>
      <feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge>
    </filter>
    <filter id="soft" x="-.5" y="-.5" width="2" height="2"><feGaussianBlur stdDeviation="10"/></filter>
    <path id="tile" d="M310 64H714C803 64 848 64 887 85C911 98 930 117 943 141C964 180 964 225 964 314V710C964 799 964 844 943 883C930 907 911 926 887 939C848 960 803 960 714 960H310C221 960 176 960 137 939C113 926 94 907 81 883C60 844 60 799 60 710V314C60 225 60 180 81 141C94 117 113 98 137 85C176 64 221 64 310 64Z"/>
    <clipPath id="tile-clip"><use href="#tile"/></clipPath>
  </defs>
  <use href="#tile" fill="url(#body)"/>
  <g clip-path="url(#tile-clip)">
    <path d="M60 64H964V215C674 147 342 197 60 357Z" fill="url(#reflection)" filter="url(#soft)"/>
    <path d="M160 931C325 966 752 966 876 923" fill="none" stroke="url(#lower-light)" stroke-width="12" filter="url(#soft)"/>
  </g>
  <use href="#tile" fill="none" stroke="url(#rim)" stroke-width="2"/>
  <g id="app-mark" fill="url(#ink)" filter="url(#mark-shadow)"/>
</svg>''')
holder = icon.find(f"{{{NS}}}g[@id='app-mark']")
# Original bounds: x=120..2281, y=90..2190; optically centered on tile.
vector = deepcopy(mark)
vector.set("transform", "translate(224 779) scale(.24 -.24)")
holder.append(vector)
ET.indent(icon, space="  ")
ET.ElementTree(icon).write(ROOT / "regulus-app-icon.svg", encoding="utf-8", xml_declaration=True)

inkscape = shutil.which("inkscape")
if not inkscape:
    raise SystemExit("SVG created. Install Inkscape to export PNG files.")

for source, target, width in [
    ("regulus-logo.svg", "regulus-logo.png", 1832),
    ("regulus-app-icon.svg", "regulus-app-icon.png", 1024),
    ("regulus-app-icon.svg", "regulus-app-icon-256.png", 256),
]:
    subprocess.run([
        inkscape, str(ROOT / source), f"--export-filename={ROOT / target}",
        f"--export-width={width}", "--export-background-opacity=0",
    ], check=True)

magick = shutil.which("magick")
if magick:
    subprocess.run([
        magick, str(ROOT / "regulus-app-icon.png"), "-define",
        "icon:auto-resize=256,128,64,48,32,16", str(ROOT / "regulus-app-icon.ico"),
    ], check=True)
print("Branding exports ready:", ROOT)
