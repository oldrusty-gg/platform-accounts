"""Content guard for the public source tree.

    python scripts/publish/content_guard.py <tree> [--iso <your.iso>] [--samples N]
        [--forbid <text>]... [--forbid-file <file>] [--max-bytes N]

Fails (exit 1) when any file in <tree> (except .git/ and target/ folders and
the git-ignored work/ and research/ folders at its top):
- is larger than --max-bytes (default 1 MB) or is binary (a NUL byte), or
  has an extension that only game data, captures or builds have;
- names a user folder (a Windows drive's Users folder, /Users/..., /home/...);
- refers to the private local work folder (a path starting with the dot
  folder the maintainers keep their imports and notes in);
- holds a public IPv4 address (documentation, private and loopback ranges
  are fine; Cargo.lock is skipped, its four-part versions look like
  addresses), a private key, or any --forbid text (case-insensitive);
- contains bytes sampled from the ISO (with --iso): distinctive 64-byte
  pieces of the user's own disc image must not occur anywhere.

The same check runs in CI (without --iso and --forbid) and in
scripts/publish/export-public.ps1 (with them).
"""

import argparse
import ipaddress
import random
import re
import sys
from pathlib import Path

SKIP_DIRS = {".git", "target", "node_modules"}
# The git-ignored local work folders at the top of a public checkout.
TOP_SKIP = {"work", "research"}
# Extensions only game data, captures, saves or builds have.
BAD_EXTENSIONS = {
    ".iso", ".bin", ".elf", ".img", ".chd", ".7z", ".zip", ".rar", ".exe", ".dll", ".pdb", ".lib",
    ".pss", ".ss2", ".mp4", ".wav", ".ogg", ".mp3", ".png", ".jpg", ".jpeg", ".bmp", ".tga",
    ".mcd", ".ps2", ".p2s", ".sav", ".db", ".sqlite", ".pem", ".key", ".crt", ".pyc", ".log",
}
USER_PATH = re.compile(rb"[A-Za-z]:[\\/]+Users[\\/]+[^\\/\s\"'`<>]+|/Users/[A-Za-z][^/\s\"'`]*/|/home/[a-z][a-z0-9_-]*/", re.I)
# The private work folder: "." + "local" as a path (not `x.local_addr()`
# or `q.local.iter()`), built from parts so this file does not match itself.
LOCAL_DIR = re.compile(rb"(?<![\w)\]])\." + b"lo" + rb"cal(?=[\\/'\"`\s,;:)]|$)", re.M)
IPV4 = re.compile(rb"(?<![\w.])(?:\d{1,3}\.){3}\d{1,3}(?![\w.])")
PRIVATE_KEY = re.compile(rb"-----BEGIN [A-Z ]*PRIVATE KEY-----")


def files(tree):
    for path in sorted(tree.rglob("*")):
        rel = path.relative_to(tree)
        if any(part in SKIP_DIRS for part in rel.parts[:-1]) or (len(rel.parts) > 1 and rel.parts[0] in TOP_SKIP) or not path.is_file():
            continue
        yield path, rel


def public_ip(text):
    try:
        ip = ipaddress.IPv4Address(text.decode())
    except ValueError:
        return False
    documentation = any(
        ip in ipaddress.IPv4Network(n) for n in ("192.0.2.0/24", "198.51.100.0/24", "203.0.113.0/24")
    )
    return not (ip.is_private or ip.is_loopback or ip.is_unspecified or ip.is_reserved or ip.is_multicast
                or ip.is_link_local or documentation or ip in ipaddress.IPv4Network("100.64.0.0/10"))


def iso_samples(iso, count, size=64):
    """Distinctive pieces of the ISO (not runs of one byte or two-byte patterns)."""
    rng = random.Random(1579)
    total = iso.stat().st_size
    samples = []
    with iso.open("rb") as f:
        tries = 0
        while len(samples) < count and tries < count * 50:
            tries += 1
            at = rng.randrange(0, max(1, total - size)) & ~3
            f.seek(at)
            chunk = f.read(size)
            if len(chunk) == size and len(set(chunk)) >= 24:
                samples.append((at, chunk))
    return samples


def line_of(data, offset):
    return data.count(b"\n", 0, offset) + 1


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("tree", type=Path)
    parser.add_argument("--iso", type=Path)
    parser.add_argument("--samples", type=int, default=4000)
    parser.add_argument("--forbid", action="append", default=[])
    parser.add_argument("--forbid-file", type=Path, help="one forbidden text per line")
    parser.add_argument("--max-bytes", type=int, default=1 << 20)
    args = parser.parse_args()
    forbid = [f for f in args.forbid if f]
    if args.forbid_file:
        forbid += [l.strip() for l in args.forbid_file.read_text(encoding="utf-8").splitlines() if l.strip()]
    forbid_bytes = [f.lower().encode() for f in forbid]
    samples = iso_samples(args.iso, args.samples) if args.iso else []

    hits = []
    count = total = 0
    for path, rel in files(args.tree):
        data = path.read_bytes()
        count += 1
        total += len(data)
        name = rel.as_posix()
        if len(data) > args.max_bytes:
            hits.append(f"{name}: {len(data)} bytes (over {args.max_bytes})")
        if path.suffix.lower() in BAD_EXTENSIONS:
            hits.append(f"{name}: {path.suffix} files do not belong in the source tree")
        if b"\0" in data[:8192]:
            hits.append(f"{name}: binary file")
        for m in USER_PATH.finditer(data):
            hits.append(f"{name}:{line_of(data, m.start())}: user folder path {m.group().decode(errors='replace')!r}")
        for m in LOCAL_DIR.finditer(data):
            hits.append(f"{name}:{line_of(data, m.start())}: private work folder reference")
        if path.name != "Cargo.lock":
            for m in IPV4.finditer(data):
                if public_ip(m.group()):
                    hits.append(f"{name}:{line_of(data, m.start())}: public IPv4 address")
        if PRIVATE_KEY.search(data):
            hits.append(f"{name}: private key")
        lower = data.lower()
        for needle, shown in zip(forbid_bytes, forbid):
            at = lower.find(needle)
            if at >= 0:
                hits.append(f"{name}:{line_of(data, at)}: forbidden text #{forbid.index(shown) + 1}")
        for at, chunk in samples:
            if chunk in data:
                hits.append(f"{name}: contains ISO bytes (ISO offset {at:#x})")
                break

    print(f"content guard: {count} files, {total / 1e6:.1f} MB scanned in {args.tree}")
    print(f"  ISO sample scan: {len(samples)} samples from {args.iso.name}" if samples else "  ISO sample scan: skipped (no --iso)")
    print(f"  forbidden texts: {len(forbid)}")
    if hits:
        print(f"FAILED: {len(hits)} hits")
        for h in hits[:200]:
            print("  " + h)
        return 1
    print("content guard passed: no user paths, private work folder references, public IPs, keys, forbidden texts, large or binary files"
          + (", or ISO bytes" if samples else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
