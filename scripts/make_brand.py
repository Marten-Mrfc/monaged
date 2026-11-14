"""Draws Monaged's brand files as SVG into assets/brand/ (no libraries needed).

- monaged-mark.svg       the app icon: near-black rounded square, dot-matrix M, red dot
- monaged-logo-*.svg     the mark and "Monaged" side by side, for dark and light backgrounds
- monaged-wordmark-*.svg the red dot and "Monaged", as in the app's title bar
- monaged-dots-*.svg     "MONAGED" in the app's 5x7 dot-matrix, for big display use

Same geometry as scripts/make_icon.py, so the icon, the app and the brand match.
Run: python scripts/make_brand.py
"""
from pathlib import Path

BG = "#0f0f0f"
EDGE = "#2f2f2f"
RED = "#d71921"
WHITE = "#ededed"
INK = "#0a0a0a"

# "Monaged" in Inter Display Medium (SIL Open Font License) at 100px, as outlines
# (fontTools), so the logo doesn't depend on installed fonts. Baseline at y=72.75.
WORD_PATH = "M6.6 72.8H17.3V33.1C17.3 28.2 17.1 21.2 17 14.6C19.6 22.2 22.1 29.1 23.6 33.1L39.1 72.8H47.9L63.3 33.1C64.8 29 67.3 21.5 70 13.6C69.9 20.5 69.7 27.9 69.7 33.1V72.8H80.3V0H65L48.9 42.6C47.7 46.3 45.3 53.2 43.6 58.3C41.9 53.2 39.6 46.3 38.2 42.6L21.9 0H6.6Z M113.9 73.9C128.7 73.9 138.6 62.9 138.6 47.1C138.6 31.1 128.7 20 113.9 20C99.2 20 89.2 31.1 89.2 47.1C89.2 62.9 99.2 73.9 113.9 73.9ZM113.9 65C105.1 65 99.7 58.1 99.7 47.1C99.7 36 105.2 28.9 113.9 28.9C122.7 28.9 128.2 36 128.2 47.1C128.2 58 122.8 65 113.9 65Z M157 43.1C157 33.5 162.5 29.4 169.5 29.4C176.6 29.4 180.8 33.6 180.8 41.9V72.8H191.1V40.6C191.1 27.1 183 20.2 172.4 20.2C165.6 20.2 160.4 23.1 156.8 29V21.2H146.7V72.8H157Z M217.4 73.6C226.3 73.6 230.5 69.6 232.6 65.4H232.8V72.8H243V37.4C243 26.9 235.2 20.2 222.5 20.2C209.6 20.2 201.3 27.1 200.8 36.9H210.9C211.3 32.2 215.8 28.6 222.4 28.6C228.9 28.6 232.8 32.2 232.8 37.2V37.6C232.8 41.4 229.6 41.3 219.9 42.5C209.4 43.7 199.5 46.2 199.5 58C199.5 68.3 207.2 73.6 217.4 73.6ZM219.5 65.3C213.5 65.3 209.8 62.6 209.8 58.2C209.8 53 214.9 51 220.5 50.1C226 49.4 231.4 48.5 232.8 47.4V53.5C232.8 59.8 228.6 65.3 219.5 65.3Z M276.1 93.8C289.1 93.8 299.6 87.7 299.6 72.1V21.2H289.5V29.1H289.4C285.9 22.9 280.1 20.2 273.4 20.2C260.1 20.2 251.1 31.1 251.1 46.8C251.1 62.5 260 73.4 273.4 73.4C280.2 73.4 285.8 70.7 289.4 64.6H289.4V72.5C289.4 80.8 284.4 85.4 276.1 85.4C269.9 85.4 265.2 82.9 263.9 77.9H253.6C255 88 263.5 93.8 276.1 93.8ZM275.5 64.6C267.2 64.6 261.5 58.3 261.5 46.8C261.5 35.2 267.2 29 275.5 29C284.3 29 290 36 290 46.8C290 57.6 284.3 64.6 275.5 64.6Z M332.3 73.9C343.9 73.9 353.1 67 355 57.1H345.1C343.6 62 339.1 65.3 332.4 65.3C323.4 65.3 318.2 59 317.9 49.8H355.8V47C355.8 31.1 346.3 20 331.7 20C317.7 20 307.7 31.3 307.7 47.1C307.7 62.6 317 73.9 332.3 73.9ZM317.9 41.9C318.8 33.7 324.1 28.6 331.8 28.6C339.6 28.6 344.9 33.7 345.7 41.9Z M383.5 73.8C390.2 73.8 396 70.9 399.5 64.9H399.6V72.8H409.7V0H399.4V28.7H399.3C396 22.9 390.1 20.2 383.5 20.2C370.1 20.2 361.1 31.1 361.1 47C361.1 62.9 370.1 73.8 383.5 73.8ZM385.6 64.9C377.3 64.9 371.6 58.6 371.6 47C371.6 35.3 377.3 29 385.6 29C394.4 29 400.1 36.1 400.1 47C400.1 57.8 394.4 64.9 385.6 64.9Z"
WORD_WIDTH = 415.48
WORD_CAP = 72.75

GLYPHS = {
    "M": ["10001", "11011", "10101", "10101", "10001", "10001", "10001"],
    "O": ["01110", "10001", "10001", "10001", "10001", "10001", "01110"],
    "N": ["10001", "10001", "11001", "10101", "10011", "10001", "10001"],
    "A": ["01110", "10001", "10001", "11111", "10001", "10001", "10001"],
    "G": ["01110", "10001", "10000", "10111", "10001", "10001", "01111"],
    "E": ["11111", "10000", "10000", "11110", "10000", "10000", "11111"],
    "D": ["11110", "10001", "10001", "10001", "10001", "10001", "11110"],
}


def mark(size=256, x=0.0, y=0.0):
    """The app icon as SVG elements, at (x, y)."""
    s = size
    r = s * 0.22
    edge = s * 0.012
    cell = s * 0.08
    dot = cell * 0.36
    left = s / 2 - 2 * cell + s * 0.02
    top = s * 0.27
    out = [
        f'<rect x="{x + edge / 2:.2f}" y="{y + edge / 2:.2f}" width="{s - edge:.2f}" height="{s - edge:.2f}" rx="{r:.2f}" fill="{BG}" stroke="{EDGE}" stroke-width="{edge:.2f}"/>'
    ]
    for row, line in enumerate(GLYPHS["M"]):
        for col, on in enumerate(line):
            if on == "1":
                out.append(f'<circle cx="{x + left + col * cell:.2f}" cy="{y + top + row * cell:.2f}" r="{dot:.2f}" fill="{WHITE}"/>')
    out.append(f'<circle cx="{x + s * 0.19:.2f}" cy="{y + s * 0.17:.2f}" r="{s * 0.065:.2f}" fill="{RED}"/>')
    return out


def word(text, cell, x, y, color):
    """Dot-matrix letters; returns (elements, width)."""
    out = []
    cursor = x
    dot = cell * 0.36
    for ch in text:
        glyph = GLYPHS[ch]
        for row, line in enumerate(glyph):
            for col, on in enumerate(line):
                if on == "1":
                    out.append(f'<circle cx="{cursor + col * cell + dot:.2f}" cy="{y + row * cell + dot:.2f}" r="{dot:.2f}" fill="{color}"/>')
        cursor += (len(glyph[0]) + 1) * cell
    width = cursor - x - cell + 2 * dot
    return out, width


def svg(width, height, body, title):
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width:.0f} {height:.0f}" width="{width:.0f}" height="{height:.0f}" role="img" aria-label="{title}">\n'
        f"<title>{title}</title>\n" + "\n".join(body) + "\n</svg>\n"
    )


def typeset(x, baseline, cap_height, color):
    """The wordmark, scaled to a cap height, with its baseline at `baseline`."""
    k = cap_height / WORD_CAP
    y = baseline - WORD_CAP * k
    return [f'<path transform="translate({x:.2f} {y:.2f}) scale({k:.4f})" fill="{color}" d="{WORD_PATH}"/>'], WORD_WIDTH * k


def main():
    out = Path(__file__).resolve().parent.parent / "assets" / "brand"
    out.mkdir(parents=True, exist_ok=True)
    (out / "monaged-mark.svg").write_text(svg(256, 256, mark(256), "Monaged"))
    for name, color in [("dark", WHITE), ("light", INK)]:
        # The logo: the mark, then "Monaged" with its capitals centred on the mark.
        size, gap, cap = 112.0, 30.0, 50.0
        text, width = typeset(size + gap, size / 2 + cap / 2, cap, color)
        # Room below the baseline for the g.
        (out / f"monaged-logo-{name}.svg").write_text(svg(size + gap + width + 2, size, mark(size) + text, "Monaged"))
        # The wordmark alone (with the red dot, as in the title bar).
        dot = 15.0
        text, width = typeset(dot * 2 + 14, cap + 2, cap, color)
        body = [f'<circle cx="{dot:.2f}" cy="{cap / 2 + 2:.2f}" r="{dot / 2 + 1:.2f}" fill="{RED}"/>'] + text
        (out / f"monaged-wordmark-{name}.svg").write_text(svg(dot * 2 + 14 + width + 2, cap + 22, body, "Monaged"))
        # The dot-matrix version, for big display use (headers, the social card).
        cell = 12
        letters, width = word("MONAGED", cell, 0, 0, color)
        (out / f"monaged-dots-{name}.svg").write_text(svg(width, 6 * cell + 2 * cell * 0.36, letters, "Monaged"))
    print(f"Wrote the brand files to {out}")


if __name__ == "__main__":
    main()
