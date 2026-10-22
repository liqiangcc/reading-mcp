#!/usr/bin/env python3
"""PDF-2a, part 2: what lopdf 0.44 does with the `/ToUnicode` CMaps that lose text.

For every font the default backend loses CJK characters with (and, as controls, fonts it reads
completely), this prints the CMap's features, dumps the first 1,500 and the last 500 bytes of
the decompressed stream, and applies a **model of lopdf 0.44's CMap parser** to it, so the note
can say which construct the parser cannot handle.

The model is a port of the lopdf code, not lopdf itself:

- `lopdf-0.44.0/src/parser/cmap_parser.rs:29-36` (`cmap_stream`): the stream must be
  `/CIDInit /ProcSet|Procset findresource begin`, `<n> dict begin`, `begincmap`,
  1-7 metadata items, at least one `begincodespacerange`/`beginbfchar`/`beginbfrange` section,
  then exactly `endcmap CMapName currentdict /CMap defineresource pop end end`.
- `:243-257` (`source_code`, `hex_u16`): source codes are 1-4 bytes of hex; a **destination**
  unit is exactly two hex digits (`hex_u16`), so a destination string needs an even number of
  hex digits.
- `:269-306` (`target_string`, `range_target_array`).
- `src/encodings/cmap.rs:52-146` (`ToUnicodeCMap::parse`/`from_sections`): one
  `RangeInclusiveMap` per code length, later ranges overwrite earlier ones.
- `src/encodings/mod.rs:84-117` (`write_to_string`): bytes are decoded greedily, trying 1, 2, 3
  then 4 bytes per code, and an unmapped code becomes U+FFFD.

A parse error anywhere makes `get_font_encoding_inner` (`src/object.rs:452-463`) fall back to
`StandardEncoding`, which decodes the two bytes of an `Identity-H` code as two Latin-1
characters: that is the "garbage" mode.

Standard library plus PyMuPDF (`doc.xref_stream`, `page.get_text("rawdict")`).
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import re
import sys
import time
from collections import Counter, defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))


def load_sibling(name: str):
    path = os.path.join(HERE, name)
    spec = importlib.util.spec_from_file_location(name.replace("-", "_").replace(".py", ""), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CJK = load_sibling("pdf-cjk.py")

WS = b" \t\n\r\0\x0c"
HEX = "0123456789abcdefABCDEF"


# =============================================================================================
# Model of lopdf 0.44's CMap parser (see the module docstring for the file:line map)
# =============================================================================================


class Fail(Exception):
    pass


class Parser:
    def __init__(self, data: bytes, pos: int = 0):
        self.data = data
        self.i = pos

    def peek(self, tag: bytes) -> bool:
        return self.data[self.i:self.i + len(tag)] == tag

    def tag(self, tag: bytes):
        if not self.peek(tag):
            raise Fail(f"tag {tag!r} at {self.i}: {self.data[self.i:self.i + 24]!r}")
        self.i += len(tag)
        return tag

    def eol(self):
        for tag in (b"\r\n", b"\n", b"\r"):
            if self.peek(tag):
                self.i += len(tag)
                return
        raise Fail(f"eol at {self.i}")

    def comment(self):
        self.tag(b"%")
        while self.i < len(self.data) and self.data[self.i] not in b"\r\n":
            self.i += 1
        self.eol()

    def multispace(self, minimum: int):
        seen = 0
        while self.i < len(self.data):
            char = self.data[self.i:self.i + 1]
            if char in (b" ", b"\t"):
                self.i += 1
            elif char in (b"\r", b"\n"):
                self.eol()
            elif char == b"%":
                self.comment()
            else:
                break
            seen += 1
        if seen < minimum:
            raise Fail(f"multispace{minimum} at {self.i}: {self.data[self.i:self.i + 24]!r}")

    def space0(self):
        while self.i < len(self.data) and self.data[self.i:self.i + 1] in (b" ", b"\t"):
            self.i += 1

    def space1(self):
        start = self.i
        self.space0()
        if self.i == start:
            raise Fail(f"space1 at {self.i}: {self.data[self.i:self.i + 24]!r}")

    def digit1(self) -> int:
        start = self.i
        while self.i < len(self.data) and 0x30 <= self.data[self.i] <= 0x39:
            self.i += 1
        if self.i == start:
            raise Fail(f"digit1 at {self.i}")
        return int(self.data[start:self.i])

    def hex_char(self) -> int:
        pair = self.data[self.i:self.i + 2]
        if len(pair) != 2 or any(chr(c) not in HEX for c in pair):
            raise Fail(f"hex_char at {self.i}: {pair!r}")
        self.i += 2
        return int(pair, 16)

    def hex_u16(self) -> int:
        return self.hex_char() * 256 + self.hex_char()

    def source_code(self) -> tuple[int, int]:
        self.tag(b"<")
        start = self.i
        values = []
        while len(values) < 4:
            save = self.i
            try:
                values.append(self.hex_char())
            except Fail:
                self.i = save
                break
        if not values:
            raise Fail(f"empty source code at {start}")
        self.tag(b">")
        code = 0
        for value in values:
            code = code * 256 + value
        return code, len(values)

    def code_range_pair(self) -> tuple[int, int, int]:
        low, low_len = self.source_code()
        self.space0()
        high, high_len = self.source_code()
        if low_len != high_len:
            raise Fail("source code length mismatch")
        return low, high, low_len

    def target_string(self) -> list[int]:
        self.tag(b"<")
        units: list[int] = []
        while len(units) < 256:
            save = self.i
            try:
                unit = self.hex_u16()
            except Fail:
                self.i = save
                break
            try:
                self.multispace(0)
            except Fail:
                self.i = save
                break
            units.append(unit)
        if not units:
            raise Fail(f"empty destination at {self.i}")
        self.tag(b">")
        return units

    def range_target_array(self) -> list[list[int]]:
        self.tag(b"[")
        self.space0()
        out = [self.target_string()]
        while True:
            save = self.i
            try:
                self.space1()
                out.append(self.target_string())
            except Fail:
                self.i = save
                break
        self.space0()
        self.tag(b"]")
        return out

    def codespace_section(self) -> tuple:
        self.digit1()
        self.space1()
        self.tag(b"begincodespacerange")
        self.multispace(1)
        ranges = []
        while True:
            save = self.i
            try:
                self.space0()
                ranges.append(self.code_range_pair())
                self.multispace(1)
            except Fail:
                self.i = save
                break
        if not ranges:
            raise Fail("no codespace ranges")
        self.tag(b"endcodespacerange")
        self.multispace(1)
        return ("cs", ranges)

    def bfchar_section(self) -> tuple:
        self.digit1()
        self.space1()
        self.tag(b"beginbfchar")
        self.multispace(1)
        out = []
        while True:
            save = self.i
            try:
                self.space0()
                code, length = self.source_code()
                self.space0()
                destination = self.target_string()
                self.multispace(1)
                out.append(((code, length), destination))
            except Fail:
                self.i = save
                break
        self.tag(b"endbfchar")
        self.multispace(1)
        return ("bfchar", out)

    def bfrange_section(self) -> tuple:
        self.digit1()
        self.space1()
        self.tag(b"beginbfrange")
        self.multispace(1)
        out = []
        while True:
            save = self.i
            try:
                self.space0()
                low, high, length = self.code_range_pair()
                self.space0()
                try:
                    destination = [self.target_string()]
                except Fail:
                    destination = self.range_target_array()
                self.multispace(1)
                out.append(((low, high, length), destination))
            except Fail:
                self.i = save
                break
        self.tag(b"endbfrange")
        self.multispace(1)
        return ("bfrange", out)

    # The dictionary object parsers lopdf uses for the CMap metadata (`/CIDSystemInfo`).
    def name(self) -> bytes:
        self.tag(b"/")
        start = self.i
        while self.i < len(self.data):
            char = self.data[self.i]
            if char == 0x23:  # '#'
                self.i += 1
                self.hex_char()
                continue
            if char in WS or char in b"()<>[]{}/%":
                break
            self.i += 1
        return self.data[start:self.i]

    def integer(self) -> int:
        sign = 1
        if self.data[self.i:self.i + 1] in (b"+", b"-"):
            sign = -1 if self.data[self.i:self.i + 1] == b"-" else 1
            self.i += 1
        return sign * self.digit1()

    def literal_string(self) -> bytes:
        self.tag(b"(")
        depth = 1
        out = b""
        while self.i < len(self.data):
            char = self.data[self.i]
            self.i += 1
            if char == 0x5C:  # backslash
                self.i += 1
                continue
            if char == 0x28:
                depth += 1
            elif char == 0x29:
                depth -= 1
                if depth == 0:
                    return out
                out += b")"
                continue
            out += bytes([char])
        raise Fail("unterminated literal string")

    def direct_object(self, depth: int = 0):
        if depth > 64:
            raise Fail("nesting")
        char = self.data[self.i:self.i + 1]
        if char == b"/":
            return ("name", self.name())
        if char == b"<":
            if self.peek(b"<<"):
                return ("dict", self.dictionary())
            self.tag(b"<")
            start = self.i
            while self.i < len(self.data) and self.data[self.i] != 0x3E:
                self.i += 1
            self.tag(b">")
            return ("hex", self.data[start:self.i - 1])
        if char == b"(":
            return ("str", self.literal_string())
        if char == b"[":
            self.tag(b"[")
            self.multispace(0)
            items = []
            while True:
                save = self.i
                try:
                    items.append(self.direct_object(depth + 1))
                    self.multispace(0)
                except Fail:
                    self.i = save
                    break
            self.tag(b"]")
            return ("array", items)
        if char in (b"+", b"-") or (char and 0x30 <= self.data[self.i] <= 0x39):
            return ("int", self.integer())
        raise Fail(f"direct_object at {self.i}: {self.data[self.i:self.i + 24]!r}")

    def dictionary(self) -> dict:
        self.tag(b"<<")
        self.multispace(0)
        out: dict = {}
        while True:
            save = self.i
            try:
                key = self.name()
                self.multispace(0)
                value = self.direct_object()
                self.multispace(0)  # `_direct_object` is `terminated(_, space)`
                out[key] = value
            except Fail:
                self.i = save
                break
        self.tag(b">>")
        return out

    def dict_dup(self) -> dict:
        self.digit1()
        self.space1()
        self.tag(b"dict")
        self.space1()
        self.tag(b"dup")
        self.space1()
        self.tag(b"begin")
        self.multispace(1)
        out: dict = {}
        while True:
            save = self.i
            try:
                key = self.name()
                self.multispace(0)
                out[key] = self.direct_object()
                self.multispace(0)
                self.tag(b"def")
                self.multispace(1)
            except Fail:
                self.i = save
                break
        self.tag(b"end")
        self.multispace(0)
        self.tag(b"def")
        self.multispace(0)
        return out

    def cid_system_info(self):
        self.tag(b"/CIDSystemInfo")
        self.multispace(0)
        try:
            self.dictionary()
        except Fail:
            self.dict_dup()
        self.multispace(1)
        self.tag(b"def")
        self.multispace(1)

    def cmap_name(self):
        self.tag(b"/CMapName")
        self.space0()
        self.name()
        self.space1()
        self.tag(b"def")
        self.multispace(1)

    def cmap_type(self):
        self.tag(b"/CMapType")
        self.space1()
        self.digit1()
        self.space1()
        self.tag(b"def")
        self.multispace(1)

    def cmap_version(self):
        self.tag(b"/CMapVersion")
        self.space1()
        self.digit1()
        save = self.i
        try:
            self.tag(b".")
            self.digit1()
        except Fail:
            self.i = save
        self.space1()
        self.tag(b"def")
        self.multispace(1)

    def uid_offset(self):
        self.tag(b"/UIDOffset")
        self.space1()
        self.digit1()
        self.space1()
        self.tag(b"def")
        self.multispace(1)

    def wmode(self):
        self.tag(b"/WMode")
        self.space1()
        self.digit1()
        self.space1()
        self.tag(b"def")
        self.multispace(1)

    def xuid(self):
        self.tag(b"/XUID")
        self.space1()
        self.tag(b"[")
        self.digit1()
        while True:
            save = self.i
            try:
                self.space1()
                self.digit1()
            except Fail:
                self.i = save
                break
        self.tag(b"]")
        self.space1()
        self.tag(b"def")
        self.multispace(1)

    def metadata_item(self) -> bool:
        for method in (self.cid_system_info, self.cmap_name, self.cmap_type,
                       self.cmap_version, self.uid_offset, self.wmode, self.xuid):
            save = self.i
            try:
                method()
                return True
            except Fail:
                self.i = save
        return False


def lopdf_parse(data: bytes) -> tuple[list | None, str | None, int]:
    """Return (sections, error, byte offset where the parse stopped)."""
    parser = Parser(data)
    try:
        if data[:3] == b"\xef\xbb\xbf":
            parser.i = 3
        parser.multispace(0)
        parser.tag(b"/CIDInit")
        parser.space0()
        if parser.peek(b"/ProcSet"):
            parser.tag(b"/ProcSet")
        else:
            parser.tag(b"/Procset")
        parser.space1()
        parser.tag(b"findresource")
        parser.space1()
        parser.tag(b"begin")
        parser.multispace(1)
        parser.digit1()
        parser.space1()
        parser.tag(b"dict")
        parser.space1()
        parser.tag(b"begin")
        parser.multispace(1)
        parser.tag(b"begincmap")
        parser.multispace(1)
        metadata = 0
        while metadata < 7:
            save = parser.i
            if parser.metadata_item():
                metadata += 1
            else:
                parser.i = save
                break
        if metadata == 0:
            raise Fail(f"cmap_metadata: no metadata at {parser.i}")
        sections = []
        while True:
            save = parser.i
            for method in (parser.codespace_section, parser.bfchar_section,
                           parser.bfrange_section):
                parser.i = save
                try:
                    sections.append(method())
                    break
                except Fail:
                    continue
            else:
                parser.i = save
                break
        if not sections:
            raise Fail(f"no cmap sections at {parser.i}")
        parser.tag(b"endcmap")
        parser.multispace(1)
        parser.tag(b"CMapName")
        parser.space1()
        parser.tag(b"currentdict")
        parser.space1()
        parser.tag(b"/CMap")
        parser.space1()
        parser.tag(b"defineresource")
        parser.space1()
        parser.tag(b"pop")
        parser.multispace(1)
        parser.tag(b"end")
        parser.multispace(1)
        parser.tag(b"end")
        return sections, None, parser.i
    except Fail as error:
        return None, str(error), parser.i


class ToUnicodeMap:
    """`ToUnicodeCMap`: four maps, one per source-code length, later ranges overwrite."""

    REPLACEMENT = 0xFFFD

    def __init__(self):
        self.maps: list[dict[int, list[int]]] = [{}, {}, {}, {}]

    def put(self, low: int, high: int, length: int, kind: str, payload):
        if length == 0 or length > 4:
            return
        target = self.maps[length - 1]
        for code in range(low, high + 1):
            offset = code - low
            if kind == "cp":
                target[code] = [(code + payload) & 0xFFFF]
            elif kind == "hex":
                units = list(payload)
                units[-1] = (units[-1] + offset) & 0xFFFF
                target[code] = units
            else:
                target[code] = list(payload[offset]) if offset < len(payload) else \
                    [self.REPLACEMENT]

    def get(self, code: int, length: int) -> list[int] | None:
        if length == 0 or length > 4:
            return None
        return self.maps[length - 1].get(code)

    def decode(self, data: bytes) -> list[int]:
        """`write_to_string`: greedy 1..4 bytes per code, U+FFFD for an unmapped code."""
        out: list[int] = []
        length = 0
        code = 0
        for byte in data:
            if length == 4:
                out.extend(self.get(code, 4) or [self.REPLACEMENT])
                code, length = 0, 0
            length += 1
            code = code * 256 + byte
            value = self.get(code, length)
            if value is not None:
                code, length = 0, 0
                out.extend(value)
        if length:
            out.extend(self.get(code, length) or [self.REPLACEMENT])
        return out


def lopdf_build(sections: list) -> tuple[ToUnicodeMap | None, str | None]:
    """`ToUnicodeCMap::from_sections`."""
    cmap = ToUnicodeMap()
    for kind, items in sections:
        if kind == "cs":
            continue
        if kind == "bfchar":
            for (code, length), destination in items:
                if len(destination) == 1:
                    cmap.put(code, code, length, "cp", (destination[0] - code) & 0xFFFFFFFF)
                else:
                    cmap.put(code, code, length, "hex", destination)
            continue
        for (low, high, length), destinations in items:
            if high < low:
                return None, "InvalidCodeRange"
            if len(destinations) == 1 and len(destinations[0]) == 1:
                cmap.put(low, high, length, "cp", (destinations[0][0] - low) & 0xFFFFFFFF)
            elif len(destinations) == 1:
                cmap.put(low, high, length, "hex", destinations[0])
            elif not destinations:
                return None, "InvalidCodeRange"
            else:
                cmap.put(low, high, length, "arr", destinations)
    return cmap, None


# =============================================================================================
# The "intended" CMap: every destination read as the code point(s) its writer meant
# =============================================================================================

SECTION = re.compile(rb"(\d+)\s+begin(bfchar|bfrange|codespacerange)")
HEX_STRING = re.compile(rb"<([0-9A-Fa-f\s]*)>")


def hex_bytes(text: bytes) -> bytes:
    digits = re.sub(rb"\s+", b"", text)
    if len(digits) % 2:
        digits += b"0"
    return bytes.fromhex(digits.decode())


def destination_units(text: bytes) -> list[int]:
    """The code point(s) a destination hex string denotes, as the writer meant them.

    An even number of digits is UTF-16BE code units.  An odd number of digits cannot be UTF-16
    at all; the only reading that yields a character is the integer value of the digits, which
    is what MuPDF's writer meant for a code point above U+FFFF.
    """
    digits = re.sub(rb"\s+", b"", text).decode()
    if len(digits) % 4:
        return [int(digits, 16)]
    raw = hex_bytes(text)
    return [int.from_bytes(raw[i:i + 2], "big") for i in range(0, len(raw), 2)]


def mapping_lines(data: bytes):
    """Yield (kind, count, [(source strings, destination strings, array?)]) per mapping line.

    This is a permissive reader: it splits each section on newlines and takes the hex strings of
    each line in order, so it can read CMaps that lopdf's parser rejects.
    """
    for match in SECTION.finditer(data):
        count = int(match.group(1))
        kind = match.group(2)
        if kind == b"codespacerange":
            continue
        end = data.find(b"end" + kind, match.end())
        if end < 0:
            continue
        body = data[match.end():end]
        lines = [line for line in body.split(b"\n") if line.strip()]
        if count:
            lines = lines[:count]
        out = []
        for line in lines:
            matches = list(HEX_STRING.finditer(line))
            if len(matches) < 2:
                continue
            array = False
            if kind == b"bfrange" and len(matches) >= 3:
                between = line[matches[1].end():matches[2].start()]
                array = b"[" in between
            out.append(([item.group(1) for item in matches], array))
        yield kind.decode(), count, out


def intended_map(data: bytes) -> dict[tuple[int, int], list[int]]:
    """A permissive ToUnicode reader: {code, length} -> units, no trailer requirements."""
    mapping: dict[tuple[int, int], list[int]] = {}
    for kind, _count, lines in mapping_lines(data):
        for strings, array in lines:
            source = hex_bytes(strings[0])
            code = int.from_bytes(source, "big")
            length = len(source)
            if kind == "bfchar":
                if len(strings) >= 2:
                    mapping[(code, length)] = destination_units(strings[1])
                continue
            if len(strings) < 3:
                continue
            high = int.from_bytes(hex_bytes(strings[1]), "big")
            if array:
                targets = [destination_units(item) for item in strings[2:]]
                for index, value in enumerate(range(code, high + 1)):
                    if index < len(targets):
                        mapping[(value, length)] = targets[index]
            else:
                base = destination_units(strings[2])
                for value in range(code, high + 1):
                    units = list(base)
                    units[-1] = (units[-1] + (value - code)) & 0xFFFF
                    mapping[(value, length)] = units
    return mapping


# =============================================================================================
# Content-stream codes per font
# =============================================================================================

TF = re.compile(rb"/([^\s/<>\[\](){}]+)\s+[-\d.]+\s+Tf")
SHOW = re.compile(rb"\[(.*?)\]\s*TJ|(\((?:\\.|[^\\()])*\)|<[0-9A-Fa-f\s]*>)\s*(?:Tj|')", re.S)
DO = re.compile(rb"/([^\s/<>\[\](){}]+)\s+Do")
HEX_IN = re.compile(rb"<([0-9A-Fa-f\s]*)>")


def font_resources(doc, owner: int) -> dict[str, int]:
    kind, value = doc.xref_get_key(owner, "Resources/Font")
    if kind != "dict":
        return {}
    return {name: int(number) for name, number, _ in
            re.findall(r"/([^\s/<>\[\](){}]+)\s+(\d+)\s+(\d+)\s+R", value)}


def used_codes(doc, widths: dict[int, int]) -> dict[int, Counter]:
    """{font xref: Counter of source codes} over page streams and reachable Form XObjects.

    `widths` is the code width of each font xref, taken from its CMap codespace (or from the
    font type when there is no CMap): an `Identity-H` code is 2 bytes, a simple-font code is 1.
    """
    codes: dict[int, Counter] = defaultdict(Counter)

    def walk(owner: int, data: bytes, depth: int):
        fonts = font_resources(doc, owner)
        current = None
        for match in re.finditer(
                rb"/([^\s/<>\[\](){}]+)\s+[-\d.]+\s+Tf|\[(.*?)\]\s*TJ|"
                rb"(\((?:\\.|[^\\()])*\)|<[0-9A-Fa-f\s]*>)\s*(?:Tj|')", data, re.S):
            if match.group(1) is not None:
                current = match.group(1).decode("latin-1")
                continue
            if current is None or current not in fonts:
                continue
            xref = fonts[current]
            width = widths.get(xref, 2)
            for raw in strings_of(match):
                if width == 1:
                    for byte in raw:
                        codes[xref][byte] += 1
                else:
                    for index in range(0, len(raw) - 1, 2):
                        codes[xref][(raw[index] << 8) | raw[index + 1]] += 1
        if depth >= 4:
            return
        xobjects = doc.xref_get_key(owner, "Resources/XObject")
        if xobjects[0] != "dict":
            return
        table = {name: int(number) for name, number, _ in
                 re.findall(r"/([^\s/<>\[\](){}]+)\s+(\d+)\s+(\d+)\s+R", xobjects[1])}
        for name in DO.findall(data):
            xref = table.get(name.decode("latin-1"))
            if xref is None or not doc.xref_is_stream(xref):
                continue
            if doc.xref_get_key(xref, "Subtype") != ("name", "/Form"):
                continue
            try:
                stream = doc.xref_stream(xref)
            except Exception:  # noqa: BLE001 - an unreadable form carries no text
                continue
            owner_next = xref if doc.xref_get_key(xref, "Resources")[0] != "null" else owner
            walk(owner_next, stream, depth + 1)

    for page in doc:
        for content in page.get_contents():
            try:
                walk(page.xref, doc.xref_stream(content), 0)
            except Exception:  # noqa: BLE001 - an unreadable page stream carries no text
                continue
    return codes


def strings_of(match) -> list[bytes]:
    """The byte strings a `Tj`/`TJ`/`'` operation shows, as raw bytes."""
    out = []
    if match.group(2) is not None:
        for item in HEX_IN.findall(match.group(2)):
            out.append(hex_bytes(item))
        return out
    token = match.group(3)
    if token.startswith(b"<"):
        out.append(hex_bytes(token[1:-1]))
    else:
        out.append(token[1:-1])
    return out


# =============================================================================================
# CMap features
# =============================================================================================

def line_endings(data: bytes) -> dict:
    crlf = data.count(b"\r\n")
    bare_cr = data.count(b"\r") - crlf
    bare_lf = data.count(b"\n") - crlf
    style = "\\r\\n" if crlf and not bare_lf and not bare_cr else \
        ("\\n" if bare_lf and not crlf and not bare_cr else
         ("\\r" if bare_cr and not crlf and not bare_lf else "mixed"))
    return {"crlf": crlf, "cr": bare_cr, "lf": bare_lf, "style": style}


def features(data: bytes) -> dict:
    sections = Counter()
    declared = []
    mismatched = 0
    for match in SECTION.finditer(data):
        sections[match.group(2).decode()] += 1
        declared.append(int(match.group(1)))
    array_form = 0
    single_form = 0
    destinations = []
    for kind, count, lines in mapping_lines(data):
        for strings, array in lines:
            if kind == "bfchar":
                single_form += 1
                if len(strings) >= 2:
                    destinations.append(strings[1])
                continue
            if array:
                array_form += 1
                destinations.extend(strings[2:])
            else:
                single_form += 1
                if len(strings) >= 3:
                    destinations.append(strings[2])
        if count and len(lines) != count:
            mismatched += 1
    codespace = []
    for match in re.finditer(rb"begincodespacerange(.*?)endcodespacerange", data, re.S):
        for line in match.group(1).split(b"\n"):
            strings = HEX_STRING.findall(line)
            if len(strings) >= 2:
                codespace.append(len(re.sub(rb"\s+", b"", strings[0])) // 2)
    single_unit = surrogate = multi = odd = 0
    for digits in destinations:
        digits = re.sub(rb"\s+", b"", digits)
        if not digits:
            continue
        if len(digits) % 4:
            odd += 1
            continue
        units = len(digits) // 4
        if units == 1:
            single_unit += 1
        elif units == 2 and all(
                0xD800 <= int(digits[i:i + 4], 16) <= 0xDFFF for i in (0, 4)):
            surrogate += 1
        else:
            multi += 1
    return {
        "bytes": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
        "beginbfchar": sections.get("bfchar", 0),
        "beginbfrange": sections.get("bfrange", 0),
        "begincodespacerange": sections.get("codespacerange", 0),
        "declared_counts": declared,
        "sections_with_wrong_count": mismatched,
        "bfrange_array_form": array_form,
        "bfrange_single_destination": single_form,
        "destinations": len(destinations),
        "codespace_widths": sorted(set(codespace)),
        "dest_single_unit": single_unit,
        "dest_surrogate_pair": surrogate,
        "dest_multi_unit": multi,
        "dest_odd_nibbles": odd,
        "usecmap": b"usecmap" in data,
        "postscript_comments": data.lstrip().startswith(b"%!PS") or b"%%BeginResource" in data,
        "dict_boilerplate": b"dict begin" in data,
        "line_endings": line_endings(data),
    }


def dump_text(data: bytes) -> str:
    head = data[:1500]
    tail = data[-500:] if len(data) > 1500 else b""
    return ("--- first 1,500 bytes ---\n" + head.decode("latin-1") +
            ("\n--- last 500 bytes ---\n" + tail.decode("latin-1") if tail else "\n"))


# =============================================================================================
# Per font analysis
# =============================================================================================

def analyse_font(doc, font: dict, loss: dict, codes: Counter) -> dict:
    record = {
        "xref": font["xref"],
        "basefont": font["basefont"],
        "type": font["type"],
        "encoding": font["encoding"],
        "descendant_subtype": font["descendant_subtype"],
        "location": font["location"],
        "pages": len(font["pages"]),
        "group": font["group"],
        "owned_chars": loss.get("chars", 0),
        "recovered_chars": loss.get("chars_found", 0),
        "loss": None,
        "used_codes": len(codes),
        "used_code_instances": sum(codes.values()),
    }
    if record["owned_chars"]:
        record["loss"] = round(1.0 - record["recovered_chars"] / record["owned_chars"], 4)
    kind, value = doc.xref_get_key(font["xref"], "ToUnicode")
    record["to_unicode_xref"] = None
    record["cmap"] = None
    record["dump"] = None
    if kind != "xref":
        record["verdict"] = "no /ToUnicode stream: lopdf falls back to StandardEncoding"
        record["lopdf_parse"] = "no-cmap"
        record["lopdf_resolved_of_used"] = 0.0
        record["intended_resolved_of_used"] = 0.0
        return record
    xref = int(value.split()[0])
    data = doc.xref_stream(xref)
    record["to_unicode_xref"] = xref
    record["cmap"] = features(data)
    record["dump"] = dump_text(data)
    widths = record["cmap"]["codespace_widths"]
    width = widths[0] if len(widths) == 1 else (2 if font["type"] == "Type0" else 1)
    record["width"] = width

    sections, error, stopped = lopdf_parse(data)
    record["lopdf_parse"] = "ok" if sections is not None else "error"
    record["lopdf_error"] = error
    record["lopdf_stopped_at"] = stopped
    record["lopdf_sections"] = None if sections is None else [
        (name, len(items)) for name, items in sections]
    intended = intended_map(data)
    record["intended_entries"] = len(intended)
    used = sorted(codes)

    if sections is not None:
        cmap, build_error = lopdf_build(sections)
        record["lopdf_build_error"] = build_error
        mapped = set()
        for length, table in enumerate(cmap.maps, start=1):
            for code in table:
                mapped.add((code, length))
        record["lopdf_mapped_codes"] = len(mapped)
        resolved = [code for code in used if cmap.get(code, width) is not None]
        record["lopdf_resolved_of_used"] = (
            round(len(resolved) / len(used), 4) if used else None)
        record["lopdf_resolved_of_mapped"] = (
            round(len(mapped & {(code, width) for code in used}) / len(mapped), 4)
            if mapped else None)
        # What the decoder actually emits for the page text, per used code.
        record["lopdf_replacement_of_used"] = sum(
            1 for code in used
            if ToUnicodeMap.REPLACEMENT in cmap.decode(bytes([code >> 8, code & 0xFF])
                                                       if width == 2 else bytes([code])))
        record["_lopdf_cmap"] = cmap
    else:
        record["lopdf_mapped_codes"] = 0
        record["lopdf_resolved_of_used"] = 0.0
        record["lopdf_resolved_of_mapped"] = 0.0
        record["lopdf_replacement_of_used"] = len(used)
        record["_lopdf_cmap"] = None

    intended_resolved = [code for code in used if (code, width) in intended]
    record["intended_resolved_of_used"] = (
        round(len(intended_resolved) / len(used), 4) if used else None)
    record["used_codes_absent_from_cmap"] = len(used) - len(intended_resolved)
    unmapped_instances = sum(count for code, count in codes.items()
                             if (code, width) not in intended)
    record["unmapped_instances"] = unmapped_instances
    record["unmapped_instance_share"] = (
        round(unmapped_instances / sum(codes.values()), 4) if codes else None)
    record["absent_codes"] = [f"{code:0{2 * width}X}" for code in used
                              if (code, width) not in intended][:20]
    return record


def pick_fonts(kind: str, fonts: list[dict], stats: dict) -> list[dict]:
    buckets = {}
    for (xref, location), entry in stats.items():
        buckets[(xref, location)] = entry
    scored = []
    for font in fonts:
        for location in ("page", "xobject"):
            entry = buckets.get((font["xref"], location))
            if entry is None:
                continue
            if kind == "lossy":
                if entry["chars"] and entry["chars_found"] < entry["chars"]:
                    scored.append((entry["chars"] - entry["chars_found"], font, entry, location))
            elif kind == "control":
                if entry["chars"] and entry["chars_found"] == entry["chars"]:
                    scored.append((entry["chars"], font, entry, location))
            else:
                if entry["chars"]:
                    scored.append((entry["chars"], font, entry, location))
    scored.sort(key=lambda item: -item[0])
    seen = set()
    out = []
    for _score, font, entry, location in scored:
        if font["xref"] in seen:
            continue
        seen.add(font["xref"])
        out.append((dict(font, location=location), entry))
    return out


def probe_document(label: str, path: str, kind: str, args, probe, report: dict) -> None:
    import pymupdf

    state_dir = os.path.join(args.state_dir, label)
    os.makedirs(state_dir, exist_ok=True)
    record, default_text = CJK.read_backend(
        probe, args.binary, path, "default", "", state_dir, label,
        time.monotonic() + 1800)
    document = pymupdf.open(path)
    try:
        inventory = CJK.build_inventory(document)
        survival = CJK.per_font_survival(document, inventory, default_text)
        widths = {}
        for font in inventory:
            kind_, value_ = document.xref_get_key(font["xref"], "ToUnicode")
            width = 2 if font["type"] == "Type0" else 1
            if kind_ == "xref":
                stream = document.xref_stream(int(value_.split()[0]))
                widths_in_cmap = features(stream)["codespace_widths"]
                if len(widths_in_cmap) == 1:
                    width = widths_in_cmap[0]
            widths[font["xref"]] = width
        codes = used_codes(document, widths)
        fonts = pick_fonts(kind, inventory, survival["stats"])
        limit = args.max_fonts
        if kind == "control":
            limit = min(limit, 1)
        entries = []
        for font, loss in fonts[:limit]:
            entry = analyse_font(document, font, loss, codes.get(font["xref"], Counter()))
            entry["paper"] = label
            entry["kind"] = kind
            entries.append(entry)
            if entry.get("dump"):
                os.makedirs(args.dump_dir, exist_ok=True)
                safe = re.sub(r"[^A-Za-z0-9_.-]", "_", f"{label}-{font['xref']}")
                with open(os.path.join(args.dump_dir, f"{safe}.cmap.txt"), "w",
                          encoding="latin-1") as handle:
                    handle.write(f"# {label} font xref {font['xref']} "
                                 f"{font['basefont']} /ToUnicode xref {entry['to_unicode_xref']}\n")
                    handle.write(entry["dump"])
            entry.pop("dump", None)
            entry.pop("_lopdf_cmap", None)
        report["documents"].append({
            "label": label, "kind": kind, "path": os.path.basename(path),
            "pages": document.page_count, "backend": {
                key: record.get(key) for key in
                ("outcome", "error_code", "open_s", "sections", "units_read", "cjk_chars",
                 "chars", "capped")},
            "fonts": entries,
        })
        print(f"| {label} | {kind} | {document.page_count} | {record.get('cjk_chars')} | "
              f"{len(entries)} fonts |", flush=True)
    finally:
        document.close()


MINIMAL_HEAD = b"""/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /Adobe-Identity-UCS def
/CMapType 2 def
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
2 beginbfrange
<0001> <0002> <4E00>
"""

MINIMAL_TAIL = b"""endbfrange
endcmap
CMapName currentdict /CMap defineresource pop
end
end
"""


def selftest() -> dict:
    """The minimal repro: one destination line is enough to reject the whole CMap."""
    cases = {
        "valid-4-digit": b"<0003> <0003> <4E01>\n",
        "5-nibble": b"<0003> <0003> <1f100>\n",
        "surrogate-pair": b"<0003> <0003> <D83CDD00>\n",
    }
    out = {}
    for name, line in cases.items():
        data = MINIMAL_HEAD + line + MINIMAL_TAIL
        sections, error, stopped = lopdf_parse(data)
        out[name] = {
            "cmap": (MINIMAL_HEAD + line + MINIMAL_TAIL).decode("latin-1"),
            "lopdf_parse": "ok" if sections is not None else "error",
            "lopdf_error": error,
            "stopped_at": stopped,
            "bytes": len(data),
            "intended_entries": len(intended_map(data)),
        }
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--binary", required=True)
    parser.add_argument("--paper", action="append", default=[], help="label=path, lossy paper")
    parser.add_argument("--generated", action="append", default=[], help="label=path")
    parser.add_argument("--control", action="append", default=[], help="label=path")
    parser.add_argument("--state-dir", required=True)
    parser.add_argument("--dump-dir", default="")
    parser.add_argument("--max-fonts", type=int, default=4)
    parser.add_argument("--out", required=True)
    parser.add_argument("--md", default="")
    parser.add_argument("--selftest-out", default=None,
                        help="write the minimal-repro verdicts here and exit")
    args = parser.parse_args()
    if args.selftest_out:
        result = selftest()
        with open(args.selftest_out, "w", encoding="utf-8") as handle:
            json.dump(result, handle, indent=1, sort_keys=True)
        print(json.dumps({name: (value["lopdf_parse"], value["lopdf_error"])
                          for name, value in result.items()}, indent=1))
        return 0
    probe = CJK.CORPUS.load_probe()
    report = {"documents": [], "generated_by": "scripts/pdf-cmap-probe.py"}
    for kind, values in (("lossy", args.paper), ("generated", args.generated),
                         ("control", args.control)):
        for value in values:
            label, _, path = value.partition("=")
            if not path:
                raise SystemExit(f"want label=path, got {value!r}")
            probe_document(label, path, kind, args, probe, report)
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump(report, handle, indent=1, sort_keys=True)
    if args.md:
        with open(args.md, "w", encoding="utf-8") as handle:
            handle.write(render(report))
    print(json.dumps({document["label"]: len(document["fonts"])
                      for document in report["documents"]}, indent=1))
    return 0


def render(report: dict) -> str:
    lines = ["## CMap features per font", ""]
    for document in report["documents"]:
        lines.append(f"### {document['label']} ({document['kind']}, "
                     f"{document['pages']} pages, backend {document['backend'].get('outcome')})")
        lines.append("")
        if not document["fonts"]:
            lines.append("no font selected.")
            lines.append("")
            continue
        lines.append("| Font | Group | Location | Owned/recovered CJK | Loss | /ToUnicode xref "
                     "| bytes | bfchar/bfrange sections | array-form bfrange | dest shapes "
                     "| codespace | line endings | odd-nibble destinations | lopdf parse "
                     "| lopdf resolves used codes |")
        lines.append("| --- | --- | --- | --- | ---: | ---: | ---: | --- | ---: | --- | --- | "
                     "--- | ---: | --- | ---: |")
        for font in document["fonts"]:
            cmap = font.get("cmap") or {}
            shapes = (f"{cmap.get('dest_single_unit', 0)}u/"
                      f"{cmap.get('dest_surrogate_pair', 0)}surr/"
                      f"{cmap.get('dest_multi_unit', 0)}multi") if cmap else "-"
            loss = "-" if font["loss"] is None else f"{100 * font['loss']:.1f}%"
            resolved = ("-" if font["lopdf_resolved_of_used"] is None
                        else f"{100 * font['lopdf_resolved_of_used']:.1f}%")
            group = font["group"].replace("|", "\\|")
            lines.append(
                f"| {font['basefont']} ({font['xref']}) | `{group}` | "
                f"{font['location']} | {font['owned_chars']}/{font['recovered_chars']} | "
                f"{loss} | {font.get('to_unicode_xref')} | {cmap.get('bytes', '-')} | "
                f"{cmap.get('beginbfchar', '-')}/{cmap.get('beginbfrange', '-')} | "
                f"{cmap.get('bfrange_array_form', '-')} | {shapes} | "
                f"{cmap.get('codespace_widths', '-')} | "
                f"{cmap.get('line_endings', {}).get('style', '-')} | "
                f"{cmap.get('dest_odd_nibbles', '-')} | {font.get('lopdf_parse')} | "
                f"{resolved} |")
        lines.append("")
        for font in document["fonts"]:
            if font.get("lopdf_parse") == "error":
                lines.append(f"- **{font['basefont']}**: lopdf stops at byte "
                             f"{font['lopdf_stopped_at']} of {font['cmap']['bytes']}: "
                             f"`{font['lopdf_error']}`")
        lines.append("")
    return "\n".join(lines)


if __name__ == "__main__":
    sys.exit(main())
