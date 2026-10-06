"""Reads the QR code on the host's screen, as a phone's camera would.

The screen is the text the host printed. Its QR code is drawn as an image,
light blocks on a dark background as a terminal shows them, and read with
zbar: another program than the one that drew it.

    python3 scan.py /shared/share.log     prints what the code says
"""

import subprocess
import sys
import tempfile

from PIL import Image

BLOCKS = {"█": (True, True), "▀": (True, False), "▄": (False, True), " ": (False, False)}
SCALE = 8


def code_lines(screen):
    """The lines of the screen that are lines of a QR code."""
    return [line for line in screen.splitlines() if len(line) > 20 and set(line) <= set(BLOCKS)]


def picture(lines):
    width = max(len(line) for line in lines)
    image = Image.new("L", (width * SCALE, len(lines) * 2 * SCALE), 0)
    for row, line in enumerate(lines):
        for column, character in enumerate(line.ljust(width)):
            for half, lit in enumerate(BLOCKS[character]):
                if lit:
                    x, y = column * SCALE, (row * 2 + half) * SCALE
                    image.paste(255, (x, y, x + SCALE, y + SCALE))
    return image


def main():
    with open(sys.argv[1], encoding="utf-8") as screen:
        lines = code_lines(screen.read())
    if not lines:
        sys.exit("no QR code on the screen")
    with tempfile.NamedTemporaryFile(suffix=".png") as file:
        picture(lines).save(file.name)
        read = subprocess.run(["zbarimg", "--quiet", "--raw", file.name], capture_output=True, text=True)
    if read.returncode != 0:
        sys.exit(f"the QR code could not be read: {read.stderr.strip()}")
    print(read.stdout.strip())


if __name__ == "__main__":
    main()
