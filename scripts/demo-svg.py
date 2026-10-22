#!/usr/bin/env python3
"""Builds demo.svg: the animated README demo of iRoof.top.

The desktop app opens on its splash screen, the cursor clicks "Climb up", a chat
continues (one message, the persona answers), the chat is shared by link, and a
friend opens that link on a phone. Phone and desktop show the same conversation.

Pure SVG + CSS keyframes (no scripts, no external files), so GitHub renders it
inside <img>. Every element runs one 26 s keyframe track, so they stay in sync.

    python3 scripts/demo-svg.py            # writes ./demo.svg
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
T = 26.0  # loop length, seconds

# The app's dark theme (web/app.css) and avatar colors (web/app.js).
BG, BG2, PANEL, CHAT = "#0f1419", "#17202a", "#182533", "#0e1621"
TEXT, MUTED, BORDER, ACCENT = "#e8eaed", "#8b98a5", "#22313f", "#3390ec"
OUT, OUT_META, LINK, OK = "#2b5278", "#9fc4ea", "#6ab3f3", "#4cc26b"
BAR = "#17212b"
FONT = "-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif"

css: list[str] = []
_n = 0


def pct(t: float) -> str:
    return f"{max(0.0, min(100.0, t / T * 100)):.3f}%"


def track(frames: list, prefix: str = "a") -> str:
    """A keyframe track: frames are (time, declarations[, timing function from here])."""
    global _n
    _n += 1
    name = f"{prefix}{_n}"
    body = "".join(f"{pct(f[0])}{{{f[1]}{';animation-timing-function:' + f[2] if len(f) > 2 else ''}}}" for f in frames)
    css.append(f"@keyframes {name}{{{body}}}.{name}{{animation:{name} {T}s linear infinite}}")
    return name


def show(t_in: float, t_out: float | None = None, fade: float = 0.25, rise: float = 0) -> str:
    """Fades in at t_in (optionally rising by `rise` px) and out at t_out."""
    def st(o, dy=0.0):
        return f"opacity:{o};transform:translateY({dy}px)" if rise else f"opacity:{o}"
    fr = [(0, st(0, rise)), (t_in, st(0, rise), "cubic-bezier(.2,.8,.2,1)"), (t_in + fade, st(1))]
    if t_out is not None:
        fr += [(t_out, st(1)), (t_out + fade, st(0))]
    fr.append((T, st(0 if t_out is not None else 1)))
    return track(fr)


def esc(s: str) -> str:
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def tw(s: str, size: float) -> float:
    """Estimated text width (the text is then fitted to it with textLength)."""
    w = 0.0
    for ch in s:
        if ord(ch) > 0x2000:
            w += 1.22
        elif ch in "il.,'!|:;·":
            w += 0.27
        elif ch in "fjrt":
            w += 0.36
        elif ch == " ":
            w += 0.28
        elif ch in "mwMW":
            w += 0.86
        elif ch.isupper():
            w += 0.66
        elif ch.isdigit():
            w += 0.56
        else:
            w += 0.54
    return w * size


def text(x, y, s, size=14, fill=TEXT, weight=400, anchor="start", fit=False, cls="", extra=""):
    fit_attr = f' textLength="{tw(s, size):.1f}" lengthAdjust="spacingAndGlyphs"' if fit else ""
    c = f' class="{cls}"' if cls else ""
    return (f'<text x="{x:.1f}" y="{y:.1f}" font-size="{size}" fill="{fill}" font-weight="{weight}" '
            f'text-anchor="{anchor}"{fit_attr}{c}{extra}>{esc(s)}</text>')


def avatar(cx, cy, r, initials, color):
    return (f'<circle cx="{cx}" cy="{cy}" r="{r}" fill="{color}"/>'
            + text(cx, cy + r * 0.36, initials, round(r * 0.78), "#fff", 600, "middle"))


def bubble(edge, y, s, mine, size=15, time="21:08", cls=""):
    """One-line chat bubble. `edge` is the right edge for own messages, else the left edge."""
    tw_ = tw(s, size)
    meta = tw(time + (" ✓✓" if mine else ""), size * 0.66) + (16 if mine else 12)
    w, h = tw_ + 24 + meta, size + 19
    x = edge - w if mine else edge
    fill, mfill = (OUT, OUT_META) if mine else (PANEL, MUTED)
    check = " ✓✓" if mine else ""
    g = [f'<g class="{cls}">' if cls else "<g>",
         f'<rect x="{x:.1f}" y="{y}" width="{w:.1f}" height="{h}" rx="{h / 2.3:.1f}" fill="{fill}"/>',
         text(x + 12, y + h / 2 + size * 0.36, s, size, "#fff" if mine else TEXT, fit=True),
         text(x + w - 10, y + h - 7, time + check, size * 0.66, mfill, anchor="end", fit=True),
         "</g>"]
    return "".join(g), (x, y, w, h)


def dots_bubble(edge, y, size, cls):
    h = size + 19
    out = [f'<g class="{cls}"><rect x="{edge}" y="{y}" width="62" height="{h}" rx="{h / 2.3:.1f}" fill="{PANEL}"/>']
    for i in range(3):
        out.append(f'<circle class="dot d{i}" cx="{edge + 18 + i * 13}" cy="{y + h / 2}" r="3.6" fill="{MUTED}"/>')
    out.append("</g>")
    return "".join(out)


def typer(x, y, s, size, t0, t1, bg, cls_text, field):
    """Text typed between t0 and t1: a cover with a caret slides off it, one character at a time.
    Clipped to `field` (x, y, w, h, rx), the input box, so the cover never shows outside it."""
    fx, fy, fw, fh, fr = field
    cid = f"field{int(x)}x{int(y)}"
    w = tw(s, size) + 6
    slide = track([(0, "transform:translateX(0)"), (t0, "transform:translateX(0)", f"steps({len(s)},end)"),
                   (t1, f"transform:translateX({w:.1f}px)"), (T, f"transform:translateX({w:.1f}px)")])
    return (f'<clipPath id="{cid}"><rect x="{fx}" y="{fy}" width="{fw}" height="{fh}" rx="{fr}"/></clipPath>'
            f'<g class="{cls_text}" clip-path="url(#{cid})">' + text(x, y, s, size, TEXT, fit=True)
            + f'<g class="{slide}"><rect x="{x - 1}" y="{y - size - 2}" width="{w + 4:.1f}" height="{size + 8}" fill="{bg}"/>'
            f'<rect x="{x - 1}" y="{y - size + 1}" width="1.6" height="{size + 3}" fill="{ACCENT}" class="caret"/></g></g>')


def ripple(cx, cy, t, color="#ffffff"):
    name = track([(0, "opacity:0;transform:scale(.3)"), (t, "opacity:0;transform:scale(.3)"), (t + 0.02, "opacity:.55;transform:scale(.3)", "ease-out"),
                  (t + 0.5, "opacity:0;transform:scale(1.6)"), (T, "opacity:0;transform:scale(1.6)")])
    return f'<circle cx="{cx}" cy="{cy}" r="16" fill="{color}" class="{name} pop"/>'


def logo_path() -> str:
    svg = (ROOT / "web" / "logo_dark.svg").read_text()
    return re.search(r'<path fill="[^"]+" transform="([^"]+)" d="([^"]+)"', svg).groups()


def build() -> str:
    lt, ld = logo_path()
    parts: list[str] = []
    P = parts.append

    # ------------------------------------------------------------- desktop window
    WX, WY, WW, WH = 30, 40, 1000, 680
    S = 0.88  # the desktop steps back when the phone arrives
    desk = track([(0, "transform:scale(1)"), (14.3, "transform:scale(1)", "cubic-bezier(.3,.7,.2,1)"), (15.1, f"transform:scale({S})"),
                  (25.3, f"transform:scale({S})"), (T, f"transform:scale({S})")])
    css.append(f".{desk}{{transform-origin:{WX}px {WY}px}}")

    def ds(x, y):  # desktop -> canvas coordinates once scaled
        return WX + (x - WX) * S, WY + (y - WY) * S

    P(f'<g class="{desk}">')
    P(f'<rect x="{WX}" y="{WY}" width="{WW}" height="{WH}" rx="14" fill="{BG}" stroke="#2a3744" stroke-width="1.5" filter="url(#shadow)"/>')
    P(f'<clipPath id="win"><rect x="{WX}" y="{WY}" width="{WW}" height="{WH}" rx="14"/></clipPath><g clip-path="url(#win)">')
    # title bar
    P(f'<rect x="{WX}" y="{WY}" width="{WW}" height="36" fill="#121a22"/>')
    for i, c in enumerate(["#ff5f57", "#febc2e", "#28c840"]):
        P(f'<circle cx="{WX + 22 + i * 20}" cy="{WY + 18}" r="6" fill="{c}"/>')
    P(text(WX + WW / 2, WY + 23, "iRoof.top", 13, MUTED, 500, "middle"))

    # splash
    splash = track([(0, "opacity:1;transform:translateY(0)"), (2.6, "opacity:1;transform:translateY(0)", "ease-in"), (3.0, "opacity:0;transform:translateY(-14px)"),
                    (T, "opacity:0;transform:translateY(-14px)")])
    P(f'<g class="{splash}"><rect x="{WX}" y="{WY + 36}" width="{WW}" height="{WH - 36}" fill="{BG}"/>')
    for i, (sx, sy, r) in enumerate([(220, 150, 1.6), (330, 230, 1.2), (760, 140, 1.8), (840, 260, 1.3), (180, 420, 1.1), (900, 470, 1.5), (620, 120, 1.2), (420, 120, 1)]):
        P(f'<circle cx="{sx}" cy="{sy}" r="{r}" fill="#cfd8e3" class="tw tw{i % 3}"/>')
    P(f'<svg x="380" y="104" width="300" height="262" viewBox="180 243 896 783"><path fill="{TEXT}" transform="{lt}" d="{ld}"/></svg>')
    P(text(530, 412, "Some people are far away.", 17, "#c3ccd6", 400, "middle", extra=' font-style="italic"'))
    btn = track([(0, f"fill:transparent"), (2.3, "fill:transparent"), (2.35, "fill:rgba(255,255,255,.14)"), (2.6, "fill:rgba(255,255,255,.14)"), (T, "fill:rgba(255,255,255,.14)")])
    P(f'<rect x="458" y="440" width="144" height="46" rx="23" stroke="{TEXT}" stroke-width="1.3" class="{btn}"/>')
    P(text(530, 469, "Climb up", 15.5, TEXT, 500, "middle"))
    P("</g>")

    # app shell
    app = show(2.85, fade=0.45)
    P(f'<g class="{app}">')
    SX, SW = WX, 290
    MX = SX + SW
    P(f'<rect x="{SX}" y="{WY + 36}" width="{SW}" height="{WH - 36}" fill="{BG2}"/>')
    P(f'<line x1="{MX}" y1="{WY + 36}" x2="{MX}" y2="{WY + WH}" stroke="#0b1118"/>')
    # sidebar header: the scene of the logo + wordmark
    P(f'<svg x="{SX + 12}" y="{WY + 46}" width="44" height="34" viewBox="300 280 680 440"><path fill="{TEXT}" transform="{lt}" d="{ld}"/></svg>')
    # The wordmark as in the app (.wordmark .amp): a small, light "&" sitting on the baseline.
    P(f'<text x="{SX + 62}" y="{WY + 70}" font-size="18" fill="{TEXT}" font-weight="400" letter-spacing=".36">'
      f'i<tspan font-size="9" font-weight="300" dx=".7" letter-spacing="0">&amp;</tspan><tspan dx="1.4">roof.top</tspan></text>')
    for i, ic in enumerate([0, 1]):
        cx = SX + SW - 54 + i * 30
        P(f'<circle cx="{cx}" cy="{WY + 64}" r="8" fill="none" stroke="{MUTED}" stroke-width="1.6"/>')
    P(f'<rect x="{SX + 12}" y="{WY + 94}" width="178" height="38" rx="10" fill="#1f2d3b"/>'
      + text(SX + 101, WY + 118, "+  New chat", 14, TEXT, 500, "middle"))
    P(f'<rect x="{SX + 198}" y="{WY + 94}" width="80" height="38" rx="10" fill="#1f2d3b"/>' + text(SX + 238, WY + 118, "Import", 14, TEXT, 500, "middle"))
    chats = [("Jonas Weber", "JW", "#6ec9cb", "yesss boss", "21:07"),
             ("Sofia Moreno", "SM", "#ee7aae", "❤️❤️❤️", "Oct 22"),
             ("Grandma Rosa", "GR", "#faa774", "did you eat? 🍲", "Sun"),
             ("Alex Kim", "AK", "#65aadd", "see you at 8 x", "Sat"),
             ("Dad", "D", "#7bc862", "call me when you land ✈️", "Fri")]
    y0 = WY + 146
    P(f'<rect x="{SX + 8}" y="{y0}" width="{SW - 16}" height="62" rx="12" fill="{OUT}"/>')
    for i, (name, ini, col, prev, when) in enumerate(chats):
        y = y0 + i * 66
        P(avatar(SX + 38, y + 31, 22, ini, col))
        P(text(SX + 70, y + 26, name, 14.5, "#fff" if i == 0 else TEXT, 600, fit=True))
        P(f'<rect x="{SX + 72 + tw(name, 14.5)}" y="{y + 14}" width="22" height="16" rx="8" fill="{"rgba(255,255,255,.2)" if i == 0 else BG}"/>'
          + text(SX + 83 + tw(name, 14.5), y + 26, "AI", 10, "#fff" if i == 0 else MUTED, 600, "middle"))
        P(text(SX + SW - 14, y + 25, when, 12, OUT_META if i == 0 else MUTED, anchor="end"))
        if i == 0:
            P(f'<g class="{show(0, 10.55, fade=0.01)}">' + text(SX + 70, y + 46, prev, 13, OUT_META) + "</g>")
            P(f'<g class="{show(10.55, fade=0.2)}">' + text(SX + 70, y + 46, "come over at 7 🍷", 13, OUT_META) + "</g>")
            # the share badge appears once a visitor arrives
            P(f'<g class="{show(14.6)}"><rect x="{SX + SW - 46}" y="{y + 34}" width="34" height="18" rx="9" fill="rgba(255,255,255,.2)"/>'
              + text(SX + SW - 29, y + 47, "🔗 1", 10.5, "#fff", 600, "middle") + "</g>")
        else:
            P(text(SX + 70, y + 46, prev, 13, MUTED))
    P(f'<line x1="{SX}" y1="{WY + WH - 44}" x2="{MX}" y2="{WY + WH - 44}" stroke="{BORDER}"/>'
      + text(SX + 18, WY + WH - 17, "▤  Imported exports", 13.5, MUTED, 500)
      + f'<rect x="{SX + SW - 46}" y="{WY + WH - 32}" width="30" height="19" rx="9.5" fill="{BG}"/>' + text(SX + SW - 31, WY + WH - 18, "5", 12, MUTED, 600, "middle"))

    # main: chat area, header, composer
    HY = WY + 36
    P(f'<rect x="{MX}" y="{HY}" width="{WX + WW - MX}" height="{WH - 36}" fill="{CHAT}"/>')
    for i in range(36):  # faint chat pattern
        P(f'<circle cx="{MX + 40 + (i * 97) % 680}" cy="{HY + 90 + (i * 53) % 470}" r="1.4" fill="#142131"/>')
    P(f'<rect x="{MX}" y="{HY}" width="{WX + WW - MX}" height="60" fill="{BAR}"/><line x1="{MX}" y1="{HY + 60}" x2="{WX + WW}" y2="{HY + 60}" stroke="#0b1118"/>')
    P(avatar(MX + 32, HY + 30, 20, "JW", "#6ec9cb"))
    P(text(MX + 62, HY + 25, "Jonas Weber", 15, TEXT, 600))
    sub_ai, sub_typing1, sub_visit, sub_typing2 = show(3.0, 7.9, 0.01), show(7.9, 10.55, 0.01), show(14.35, 19.2, 0.01), show(19.2, 21.65, 0.01)
    sub_visit2 = show(21.65, fade=0.01)
    sub_ai2 = show(10.55, 14.35, 0.01)
    for c in (sub_ai, sub_ai2):
        P(f'<g class="{c}">' + text(MX + 62, HY + 44, "AI in the style of Jonas Weber · you are Priya Nair", 12.5, MUTED) + "</g>")
    P(f'<g class="{sub_typing1}">' + text(MX + 62, HY + 44, "typing…", 12.5, LINK) + "</g>")
    for c in (sub_visit, sub_visit2):
        P(f'<g class="{c}">' + text(MX + 62, HY + 44, "Visitor conversation · Mia · iroof.top/s/jonas", 12.5, MUTED) + "</g>")
    P(f'<g class="{sub_typing2}">' + text(MX + 62, HY + 44, "typing…", 12.5, LINK) + "</g>")
    hx = WX + WW - 36
    hover = show(11.45, 12.2, 0.12)
    P(f'<circle cx="{hx}" cy="{HY + 30}" r="18" fill="rgba(255,255,255,.1)" class="{hover}"/>')
    # header icons: media, nudge, share
    P(f'<rect x="{hx - 88}" y="{HY + 22}" width="16" height="14" rx="3" fill="none" stroke="{MUTED}" stroke-width="1.6"/>')
    P(f'<path d="M{hx - 52} {HY + 22}h14v10h-8l-4 4v-4h-2z" fill="none" stroke="{MUTED}" stroke-width="1.6" stroke-linejoin="round"/>')
    P(f'<g fill="none" stroke="{TEXT}" stroke-width="1.7"><circle cx="{hx + 5}" cy="{HY + 23}" r="3"/><circle cx="{hx - 6}" cy="{HY + 30}" r="3"/><circle cx="{hx + 5}" cy="{HY + 37}" r="3"/>'
      f'<path d="M{hx - 3.5} {HY + 28.5}l6-4M{hx - 3.5} {HY + 31.5}l6 4"/></g>')
    CY = WY + WH - 56
    P(f'<rect x="{MX}" y="{CY}" width="{WX + WW - MX}" height="56" fill="{BAR}"/>')
    field_d = (MX + 14, CY + 10, WW - SW - 84, 36, 18)
    P(f'<rect x="{field_d[0]}" y="{field_d[1]}" width="{field_d[2]}" height="{field_d[3]}" rx="{field_d[4]}" fill="{CHAT}"/>')
    send_x, send_y = WX + WW - 34, CY + 28
    P(f'<circle cx="{send_x}" cy="{send_y}" r="18" fill="{ACCENT}"/><path d="M{send_x - 7} {send_y}h12m-5-5 5 5-5 5" stroke="#fff" stroke-width="2.2" fill="none" stroke-linecap="round" stroke-linejoin="round"/>')
    ph = track([(0, "opacity:1"), (4.3, "opacity:1"), (4.31, "opacity:0"), (7.4, "opacity:0"), (7.41, "opacity:1"), (T, "opacity:1")])
    P(f'<g class="{ph}">' + text(MX + 32, CY + 33, "Message", 15, MUTED) + "</g>")
    typed = track([(0, "opacity:0"), (4.3, "opacity:0"), (4.31, "opacity:1"), (7.4, "opacity:1"), (7.41, "opacity:0"), (T, "opacity:0")])
    P(typer(MX + 32, CY + 33, "Are you making the lasagna again this weekend?", 15, 4.4, 6.6, CHAT, typed, field_d))

    # Jonas chat (before the share), then the visitor conversation
    chat1 = show(0, 14.3, 0.01)
    P(f'<g class="{chat1}">')
    right, left = WX + WW - 18, MX + 18
    P(f'<rect x="{MX + (WW - SW) / 2 - 30}" y="{HY + 78}" width="60" height="22" rx="11" fill="rgba(0,0,0,.35)"/>' + text(MX + (WW - SW) / 2, HY + 93, "Today", 12, "#c3ccd6", 500, "middle"))
    for s, mine, y, t in [("made a little movie of our day 🎬", False, HY + 112, "21:04"), ("It turned out so good. Noted for next time.", True, HY + 154, "21:05"), ("yesss boss", False, HY + 196, "21:07")]:
        P(bubble(right if mine else left, y, s, mine, time=t)[0])
    P(bubble(right, HY + 250, "Are you making the lasagna again this weekend?", True, cls=show(7.45, rise=8))[0])
    P(dots_bubble(left, HY + 292, 15, show(7.95, 9.35, 0.15)))
    P(bubble(left, HY + 292, "obviously", False, time="21:09", cls=show(9.35, rise=8))[0])
    P(dots_bubble(left, HY + 334, 15, show(9.6, 10.5, 0.15)))
    P(bubble(left, HY + 334, "come over at 7, bring the wine 🍷", False, time="21:09", cls=show(10.5, rise=8))[0])
    P("</g>")

    visit = show(14.35, fade=0.3)
    P(f'<g class="{visit}">')
    P(f'<rect x="{MX + (WW - SW) / 2 - 100}" y="{HY + 78}" width="200" height="22" rx="11" fill="rgba(0,0,0,.35)"/>'
      + text(MX + (WW - SW) / 2, HY + 93, "Mia opened your link", 12, "#c3ccd6", 500, "middle"))
    v1, v1box = bubble(right, HY + 116, "hi! is it really you? 👀", True, time="21:12", cls=show(18.65, rise=8))
    P(v1)
    P(dots_bubble(left, HY + 158, 15, show(19.2, 20.55, 0.15)))
    v2, v2box = bubble(left, HY + 158, "mia!! of course it's me 😄", False, time="21:12", cls=show(20.55, rise=8))
    P(v2)
    P(dots_bubble(left, HY + 200, 15, show(20.8, 21.65, 0.15)))
    v3, v3box = bubble(left, HY + 200, "lasagna sunday. you in 🍝", False, time="21:12", cls=show(21.65, rise=8))
    P(v3)
    P("</g>")

    # share dialog
    dlg = show(12.0, 14.0, 0.25)
    P(f'<g class="{dlg}"><rect x="{WX}" y="{HY}" width="{WW}" height="{WH - 36}" fill="rgba(4,8,12,.55)"/>')
    DX, DY, DW, DH = MX + (WW - SW) / 2 - 220, HY + 120, 440, 236
    P(f'<rect x="{DX}" y="{DY}" width="{DW}" height="{DH}" rx="16" fill="{PANEL}" stroke="{BORDER}" filter="url(#shadow)"/>')
    P(text(DX + 24, DY + 38, "Share «Jonas Weber»", 17, TEXT, 600))
    P(f'<path d="M{DX + DW - 32} {DY + 26}l10 10m0-10-10 10" stroke="{MUTED}" stroke-width="1.8" stroke-linecap="round"/>')
    P(text(DX + 24, DY + 70, "Link", 12.5, MUTED, 500))
    P(f'<rect x="{DX + 24}" y="{DY + 80}" width="{DW - 48}" height="44" rx="10" fill="{CHAT}" stroke="{BORDER}"/>')
    making = show(12.0, 12.85, 0.01)
    P(f'<g class="{making}"><g class="spin" style="transform-origin:{DX + 44}px {DY + 102}px"><path d="M{DX + 44} {DY + 94}a8 8 0 1 1-8 8" stroke="{ACCENT}" stroke-width="2.2" fill="none" stroke-linecap="round"/></g>'
      + text(DX + 62, DY + 107, "Creating link…", 14.5, MUTED) + "</g>")
    ready = show(12.85, fade=0.2)
    P(f'<g class="{ready}">' + text(DX + 40, DY + 108, "iroof.top/s/jonas", 16, LINK, 500)
      + text(DX + DW - 40, DY + 107, "🔒 PIN 482193", 12.5, MUTED, anchor="end") + "</g>")
    chips = ["Private conversations", "History hidden", "200 answers"]
    cx = DX + 24
    for c in chips:
        w = tw(c, 12) + 22
        P(f'<rect x="{cx}" y="{DY + 138}" width="{w:.1f}" height="24" rx="12" fill="none" stroke="{BORDER}"/>' + text(cx + w / 2, DY + 154, c, 12, MUTED, anchor="middle"))
        cx += w + 8
    P(f'<rect x="{DX + 24}" y="{DY + 176}" width="{(DW - 58) / 2}" height="40" rx="10" fill="{ACCENT}"/>')
    copy_lbl, copied_lbl = show(0, 13.45, 0.01), show(13.45, fade=0.15)
    bx = DX + 24 + (DW - 58) / 4
    P(f'<g class="{copy_lbl}">' + text(bx, DY + 201, "Copy invite", 14.5, "#fff", 600, "middle") + "</g>")
    P(f'<g class="{copied_lbl}">' + text(bx, DY + 201, "✓ Copied", 14.5, "#fff", 600, "middle") + "</g>")
    P(f'<rect x="{DX + 34 + (DW - 58) / 2}" y="{DY + 176}" width="{(DW - 58) / 2}" height="40" rx="10" fill="#1f2d3b"/>'
      + text(DX + 34 + (DW - 58) * 0.75, DY + 201, "Done", 14.5, TEXT, 500, "middle"))
    P("</g>")
    P("</g>")  # app
    # clicks inside the desktop
    P(ripple(530, 463, 2.35))
    P(ripple(MX + 120, CY + 28, 4.2))
    P(ripple(send_x, send_y, 7.3))
    P(ripple(hx, HY + 30, 11.8))
    P(ripple(bx, DY + 196, 13.4))
    P("</g></g>")  # clip, desk

    # ------------------------------------------------------------- cursor
    # (time, x, y, opacity): every move eases in and out
    pts = [(0, 650, 640, 0), (0.7, 650, 640, 0), (0.95, 650, 640, 1), (2.1, 530, 463, 1), (2.9, 530, 463, 1),
           (3.9, MX + 120, CY + 28, 1), (6.7, MX + 120, CY + 28, 1), (7.2, send_x, send_y, 1), (11.0, send_x, send_y, 1),
           (11.7, hx, HY + 30, 1), (12.3, hx, HY + 30, 1), (13.25, bx, DY + 196, 1), (13.9, bx, DY + 196, 1),
           (14.2, bx, DY + 196, 0), (T, bx, DY + 196, 0)]
    fr = [(t, f"transform:translate({x}px,{y}px);opacity:{o}", "cubic-bezier(.45,.05,.25,1)") for t, x, y, o in pts]
    cur = track(fr)
    P(f'<g class="{cur}"><path d="M0 0v19l5-4.6 3.4 7.6 3.2-1.4-3.3-7.4h6.6z" fill="#fff" stroke="#111" stroke-width="1.3" stroke-linejoin="round"/></g>')

    # ------------------------------------------------------------- phone
    PX, PY, PW, PH = 902, 140, 290, 624
    SXp, SYp, SWp, SHp = PX + 10, PY + 10, PW - 20, PH - 20
    slide = track([(0, "transform:translateX(360px);opacity:0"), (14.45, "transform:translateX(360px);opacity:0", "cubic-bezier(.2,.8,.2,1)"),
                   (15.25, "transform:translateX(0);opacity:1"), (T, "transform:translateX(0);opacity:1")])
    P(f'<g class="{slide}">')
    P(f'<rect x="{PX}" y="{PY}" width="{PW}" height="{PH}" rx="46" fill="#05080b" stroke="#33414f" stroke-width="2" filter="url(#shadow)"/>')
    P(f'<clipPath id="scr"><rect x="{SXp}" y="{SYp}" width="{SWp}" height="{SHp}" rx="37"/></clipPath><g clip-path="url(#scr)">')
    P(f'<rect x="{SXp}" y="{SYp}" width="{SWp}" height="{SHp}" fill="{BG}"/>')
    P(text(SXp + 36, SYp + 25, "9:41", 13.5, TEXT, 600))
    P(f'<rect x="{PX + PW / 2 - 42}" y="{SYp + 8}" width="84" height="24" rx="12" fill="#000"/>')
    P(f'<rect x="{SXp + SWp - 62}" y="{SYp + 15}" width="22" height="11" rx="3" fill="none" stroke="{TEXT}" stroke-width="1.2"/><rect x="{SXp + SWp - 60}" y="{SYp + 17}" width="15" height="7" rx="1.5" fill="{TEXT}"/>')
    P(f'<rect x="{SXp + 12}" y="{SYp + 40}" width="{SWp - 24}" height="30" rx="15" fill="{BAR}"/>' + text(PX + PW / 2, SYp + 60, "🔒 iroof.top/s/jonas", 12.5, TEXT, 500, "middle"))
    top = SYp + 78
    # gate: who you are
    gate = show(14.6, 16.45, 0.2)
    P(f'<g class="{gate}">')
    P(avatar(PX + PW / 2, top + 84, 36, "JW", "#6ec9cb"))
    P(text(PX + PW / 2, top + 150, "Jonas Weber", 18, TEXT, 600, "middle"))
    P(text(PX + PW / 2, top + 172, "Try to tell if it's really me 😉", 12.5, MUTED, anchor="middle"))
    P(f'<rect x="{SXp + 28}" y="{top + 194}" width="{SWp - 56}" height="40" rx="10" fill="{BG2}" stroke="{BORDER}"/>' + text(SXp + 42, top + 219, "Mia", 14.5, TEXT))
    P(f'<rect x="{SXp + 28}" y="{top + 246}" width="{SWp - 56}" height="42" rx="21" fill="{ACCENT}"/>' + text(PX + PW / 2, top + 272, "Start chatting", 14.5, "#fff", 600, "middle"))
    P(text(PX + PW / 2, top + 312, "Replies are written by an AI that", 11, MUTED, anchor="middle") + text(PX + PW / 2, top + 327, "imitates a real person's style.", 11, MUTED, anchor="middle"))
    P("</g>")
    P(ripple(PX + PW / 2, top + 267, 16.15))
    # chat
    pchat = show(16.45, fade=0.25)
    P(f'<g class="{pchat}"><rect x="{SXp}" y="{top}" width="{SWp}" height="{SHp - (top - SYp)}" fill="{CHAT}"/>')
    P(f'<rect x="{SXp}" y="{top}" width="{SWp}" height="52" fill="{BAR}"/>')
    P(avatar(SXp + 34, top + 26, 17, "JW", "#6ec9cb"))
    P(text(SXp + 60, top + 22, "Jonas Weber", 14.5, TEXT, 600))
    for c, s, col in [(show(16.45, 19.2, 0.01), "online", MUTED), (show(19.2, 21.65, 0.01), "typing…", LINK), (show(21.65, fade=0.01), "online", MUTED)]:
        P(f'<g class="{c}">' + text(SXp + 60, top + 40, s, 12, col) + "</g>")
    pr, pl = SXp + SWp - 12, SXp + 12
    p1, p1box = bubble(pr, top + 72, "hi! is it really you? 👀", True, 13.5, "21:12", show(18.65, rise=8))
    P(p1)
    P(dots_bubble(pl, top + 112, 13.5, show(19.2, 20.55, 0.15)))
    p2, p2box = bubble(pl, top + 112, "mia!! of course it's me 😄", False, 13.5, "21:12", show(20.55, rise=8))
    P(p2)
    P(dots_bubble(pl, top + 152, 13.5, show(20.8, 21.65, 0.15)))
    p3, p3box = bubble(pl, top + 152, "lasagna sunday. you in 🍝", False, 13.5, "21:12", show(21.65, rise=8))
    P(p3)
    by = SYp + SHp - 50
    field_p = (SXp + 10, by + 9, SWp - 62, 32, 16)
    P(f'<rect x="{SXp}" y="{by}" width="{SWp}" height="50" fill="{BAR}"/><rect x="{field_p[0]}" y="{field_p[1]}" width="{field_p[2]}" height="{field_p[3]}" rx="{field_p[4]}" fill="{CHAT}"/>')
    psend = (SXp + SWp - 28, by + 25)
    P(f'<circle cx="{psend[0]}" cy="{psend[1]}" r="16" fill="{ACCENT}"/><path d="M{psend[0] - 6} {psend[1]}h11m-4.5-4.5 4.5 4.5-4.5 4.5" stroke="#fff" stroke-width="2" fill="none" stroke-linecap="round" stroke-linejoin="round"/>')
    pph = track([(0, "opacity:1"), (16.8, "opacity:1"), (16.81, "opacity:0"), (18.6, "opacity:0"), (18.61, "opacity:1"), (T, "opacity:1")])
    P(f'<g class="{pph}">' + text(SXp + 26, by + 30, "Message", 13.5, MUTED) + "</g>")
    ptyped = track([(0, "opacity:0"), (16.8, "opacity:0"), (16.81, "opacity:1"), (18.6, "opacity:1"), (18.61, "opacity:0"), (T, "opacity:0")])
    P(typer(SXp + 26, by + 30, "hi! is it really you? 👀", 13.5, 16.9, 18.2, CHAT, ptyped, field_p))
    P("</g>")
    P(ripple(psend[0], psend[1], 18.45))
    P("</g>")  # clip
    P("</g>")  # slide

    # ------------------------------------------------------------- sync arcs: the same message on both screens
    for (t, pbox, dbox, mine) in [(18.65, p1box, v1box, True), (20.55, p2box, v2box, False), (21.65, p3box, v3box, False)]:
        px_, py_ = pbox[0] - 4, pbox[1] + pbox[3] / 2
        dx_, dy_ = ds(dbox[0] + dbox[2] + 4, dbox[1] + dbox[3] / 2)
        midx = (px_ + dx_) / 2
        d = f"M{px_:.1f} {py_:.1f}C{midx:.1f} {py_ - 40:.1f} {midx:.1f} {dy_ - 40:.1f} {dx_:.1f} {dy_:.1f}"
        length = 2.2 * (abs(px_ - dx_) + abs(py_ - dy_) + 60)
        draw = track([(0, f"stroke-dashoffset:{length:.0f};opacity:0"), (t, f"stroke-dashoffset:{length:.0f};opacity:0", "ease-out"),
                      (t + 0.05, f"stroke-dashoffset:{length:.0f};opacity:1"), (t + 0.55, "stroke-dashoffset:0;opacity:1"), (t + 1.1, "stroke-dashoffset:0;opacity:0"),
                      (T, "stroke-dashoffset:0;opacity:0")])
        P(f'<path d="{d}" fill="none" stroke="{LINK}" stroke-width="2" stroke-linecap="round" stroke-dasharray="{length:.0f}" class="{draw}"/>')
        dot = track([(0, "opacity:0"), (t + 0.45, "opacity:0"), (t + 0.55, "opacity:1"), (t + 1.1, "opacity:0"), (T, "opacity:0")])
        P(f'<circle cx="{dx_:.1f}" cy="{dy_:.1f}" r="4" fill="{LINK}" class="{dot}"/>')
    sync = show(18.7, fade=0.3)
    P(f'<g class="{sync}"><rect x="{PX - 104}" y="{PY + PH - 34}" width="94" height="26" rx="13" fill="{PANEL}" stroke="{BORDER}"/>'
      f'<circle cx="{PX - 88}" cy="{PY + PH - 21}" r="4" fill="{OK}" class="live"/>' + text(PX - 76, PY + PH - 16.5, "live sync", 12.5, TEXT, 500) + "</g>")

    body = "".join(parts)
    loop = track([(0, "opacity:0"), (0.5, "opacity:1"), (25.3, "opacity:1"), (25.9, "opacity:0"), (T, "opacity:0")])
    style = (f"text{{font-family:{FONT}}}"
             ".pop{transform-box:fill-box;transform-origin:center}"
             ".spin{animation:spin .9s linear infinite}@keyframes spin{to{transform:rotate(360deg)}}"
             ".dot{animation:bounce 1.1s ease-in-out infinite}.d1{animation-delay:.15s}.d2{animation-delay:.3s}"
             "@keyframes bounce{0%,60%,100%{transform:translateY(0);opacity:.55}30%{transform:translateY(-4px);opacity:1}}"
             ".caret{animation:blink 1s steps(1) infinite}@keyframes blink{50%{opacity:0}}"
             ".tw{animation:twinkle 3.2s ease-in-out infinite}.tw1{animation-delay:1.1s}.tw2{animation-delay:2.1s}@keyframes twinkle{50%{opacity:.25}}"
             ".live{animation:blink 1.6s steps(1) infinite}"
             + "".join(css))
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1220 800" width="1220" height="800" role="img" '
            f'aria-label="iRoof.top demo: a chat continues in the style of a real person, then a friend opens the shared link on a phone">'
            f"<title>iRoof.top demo</title><defs><filter id=\"shadow\" x=\"-10%\" y=\"-10%\" width=\"120%\" height=\"130%\">"
            f'<feDropShadow dx="0" dy="14" stdDeviation="16" flood-color="#000" flood-opacity=".45"/></filter></defs>'
            f"<style>{style}</style><g class=\"{loop}\">{body}</g></svg>\n")


if __name__ == "__main__":
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "demo.svg"
    svg = build()
    out.write_text(svg)
    print(f"wrote {out} ({len(svg.encode()) / 1024:.0f} KB)")
