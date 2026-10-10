#!/usr/bin/env python3
"""Generate the xlsx module's test fixtures: `python3 -I gen.py`.

The fixtures are checked in; this script documents and reproduces them. It
uses only the standard library (zlib, zipfile) and is deterministic: fixed
seeds and fixed ZIP timestamps. The shipped Rust code never runs it.
"""
import os
import random
import zipfile
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))


def out(path, data):
    path = os.path.join(HERE, path)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)


def raw_deflate(data, level=9, strategy=zlib.Z_DEFAULT_STRATEGY):
    c = zlib.compressobj(level, zlib.DEFLATED, -15, 9, strategy)
    return c.compress(data) + c.flush()


def words(rng, n):
    vocab = ["cell", "row", "column", "sheet", "formula", "value", "sum", "average",
             "the", "of", "and", "grid", "buffer", "macros", "excel", "number"]
    return " ".join(rng.choice(vocab) for _ in range(n)).encode()


# ---------------------------------------------------------------- inflate
def gen_inflate():
    rng = random.Random(1951)
    text = words(rng, 12000)
    cases = {}
    cases["empty"] = (b"", raw_deflate(b""))
    cases["stored"] = (text[:1000], raw_deflate(text[:1000], level=0))
    big = (text * 3)[:150000]
    cases["stored_multi"] = (big, raw_deflate(big, level=0))
    cases["fixed"] = (text[:20000], raw_deflate(text[:20000], strategy=zlib.Z_FIXED))
    cases["dynamic"] = (text, raw_deflate(text))
    long = b"a" * 300000 + b"abc" * 50000 + bytes(range(256)) * 400 + b"z" * 1000
    cases["long_matches"] = (long, raw_deflate(long))
    noise = bytes(rng.getrandbits(8) for _ in range(20000))
    binary = noise + noise[:5000] + bytes(rng.getrandbits(2) for _ in range(20000))
    cases["binary"] = (binary, raw_deflate(binary))
    # Fixed blocks, then stored, then dynamic, in one stream: each part ends
    # with a sync flush (byte-aligned, not final) so they concatenate.
    a, b, c = text[:5000], text[5000:9000], text[9000:30000]
    ca = zlib.compressobj(9, zlib.DEFLATED, -15, 9, zlib.Z_FIXED)
    cb = zlib.compressobj(0, zlib.DEFLATED, -15)
    cc = zlib.compressobj(6, zlib.DEFLATED, -15)
    mixed = (ca.compress(a) + ca.flush(zlib.Z_SYNC_FLUSH)
             + cb.compress(b) + cb.flush(zlib.Z_SYNC_FLUSH)
             + cc.compress(c[:10000]) + cc.flush(zlib.Z_FULL_FLUSH)
             + cc.compress(c[10000:]) + cc.flush())
    cases["mixed"] = (a + b + c, mixed)
    lines = []
    for name, (plain, packed) in cases.items():
        assert zlib.decompress(packed, -15) == plain
        out(f"inflate/{name}.deflate", packed)
        lines.append(f"{name} {len(plain)} {zlib.crc32(plain):08x}")
    out("inflate/manifest.txt", ("\n".join(lines) + "\n").encode())


# ---------------------------------------------------------------- zip
def write_zip(path, files, method=zipfile.ZIP_DEFLATED, comment=b""):
    full = os.path.join(HERE, path)
    os.makedirs(os.path.dirname(full), exist_ok=True)
    with zipfile.ZipFile(full, "w") as z:
        for name, data in files:
            info = zipfile.ZipInfo(name, date_time=(2024, 1, 2, 3, 4, 6))
            info.compress_type = method
            z.writestr(info, data)
        z.comment = comment


def gen_zip():
    big = "".join(f"line {i}\n" for i in range(40000)).encode()[:200000]
    write_zip("zip/deflated.zip", [
        ("hello.txt", b"Hello, zip!\n"),
        ("dir/big.txt", big),
        ("dir/empty.txt", b""),
        ("unicodé.txt", "naïve ☃\n".encode()),
    ])
    write_zip("zip/stored_comment.zip", [("a.txt", b"alpha"), ("b.txt", b"beta")],
              method=zipfile.ZIP_STORED, comment=b"a trailing archive comment PK")


# ---------------------------------------------------------------- packages
DECL = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
PR = "http://schemas.openxmlformats.org/package/2006/relationships"


def rels(items):
    body = "".join(
        f'<Relationship Id="{i}" Type="{R}/{t}" Target="{target}"/>' for i, t, target in items)
    return DECL + f'<Relationships xmlns="{PR}">{body}</Relationships>'


CONTENT_TYPES = DECL + (
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
    '<Default Extension="xml" ContentType="application/xml"/>'
    '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
    '</Types>')
ROOT_RELS = rels([("rId1", "officeDocument", "xl/workbook.xml"),
                  ("rId2", "extended-properties", "docProps/app.xml")])
APP = DECL + ('<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties">'
              '<Application>Microsoft Excel</Application><DocSecurity>0</DocSecurity></Properties>')
CELL_XFS_ONE = '<cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/></cellXfs>'
STYLES_DEFAULT = DECL + (
    f'<styleSheet xmlns="{MAIN}"><fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts>'
    '<fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills>'
    '<borders count="1"><border/></borders>'
    '<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>'
    f'{CELL_XFS_ONE}</styleSheet>')
STYLES_TWO_XF = STYLES_DEFAULT.replace(
    CELL_XFS_ONE,
    '<cellXfs count="2"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>'
    '<xf numFmtId="14" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/></cellXfs>')
STYLES_NUMFMT = STYLES_DEFAULT.replace(
    "<fonts", '<numFmts count="1"><numFmt numFmtId="164" formatCode="0.000"/></numFmts><fonts')


def workbook(sheets, extra="", pr='<workbookPr defaultThemeVersion="164011"/>'):
    body = "".join(
        f'<sheet name="{n}" sheetId="{i + 1}"{st} r:id="{rid}"/>' for i, (n, rid, st) in enumerate(sheets))
    return DECL + (f'<workbook xmlns="{MAIN}" xmlns:r="{R}"><fileVersion appName="xl"/>{pr}'
                   f'<bookViews><workbookView xWindow="0" yWindow="0"/></bookViews>'
                   f'<sheets>{body}</sheets>{extra}<calcPr calcId="191029"/></workbook>')


def sheet(data, before="", after="", tail=""):
    """A worksheet using an `x:` prefix for the main namespace throughout,
    to check that elements are matched by local name."""
    xml = (f'<worksheet xmlns:x="{MAIN}" xmlns:r="{R}"><dimension ref="A1"/>{before}'
           f'<sheetFormatPr defaultRowHeight="15"/><sheetData>{data}</sheetData>{after}'
           '<pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/>'
           f'{tail}</worksheet>')
    xml = xml.replace("</", "\0").replace("<", "<x:").replace("\0", "</x:")
    return DECL + xml


SHARED = DECL + (
    f'<sst xmlns="{MAIN}" count="6" uniqueCount="6">'
    '<si><t>plain</t></si>'
    '<si><r><rPr><b/></rPr><t>bold</t></r><r><t xml:space="preserve"> and </t></r>'
    '<r><rPr><i/></rPr><t>italic</t></r><rPh sb="0" eb="1"><t>PHONETIC</t></rPh></si>'
    '<si><t xml:space="preserve">  padded  </t></si>'
    '<si><t>\n  trimmed  </t></si>'
    '<si><t>tab_x0009_here_x000D__x005F_x0041_</t></si>'
    '<si><t>&#x6F22;字</t></si>'
    '</sst>')

BASIC_ROWS = (
    '<row r="1" spans="1:6"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c>'
    '<c r="C1" t="s"><v>2</v></c><c r="D1" t="s"><v>3</v></c><c r="E1" t="s"><v>4</v></c>'
    '<c r="F1" t="s" s="0"><v>5</v></c></row>'
    '<row r="2"><c r="A2"><v>1.5</v></c><c r="B2" t="n"><v>-2E-300</v></c><c r="C2" t="b"><v>1</v></c>'
    '<c r="D2" t="b"><v>0</v></c><c r="E2" t="e"><v>#DIV/0!</v></c>'
    '<c r="F2" t="d"><v>2024-03-01T00:00:00</v></c>'
    '<c r="G2" t="inlineStr"><is><r><t xml:space="preserve"> inline</t></r><r><t xml:space="preserve">\nrich</t></r></is></c></row>'
    '<row r="3"><c r="A3"><f>SUM(A2:B2)</f><v>1.5</v></c>'
    '<c r="B3" t="str"><f>A1&amp;"&lt;x&gt;"</f><v>plain&lt;x&gt;</v></c>'
    '<c r="C3"><f t="shared" ref="C3:C5" si="0">A2*2</f><v>3</v></c>'
    '<c r="D3"><f t="array" ref="D3">SUM(A2:B2*2)</f><v>3</v></c>'
    '<c r="E3"><f>NOW()</f></c></row>'
    '<row r="4"><c r="C4"><f t="shared" si="0"/><v>6</v></c></row>'
    '<row r="5"><c r="C5"><f t="shared" si="0"/><v>9</v></c></row>'
    '<row r="6"><c r="A6" s="0"/></row>'
    '<row><c><v>10</v></c><c><v>11</v></c><c r="D7"><v>13</v></c><c><v>14</v></c></row>'
    '<row><c><v>20</v></c></row>')


def package(path, files, method=zipfile.ZIP_DEFLATED):
    write_zip(path, list(files.items()), method=method)


def gen_packages():
    basic = {
        "[Content_Types].xml": CONTENT_TYPES,
        "_rels/.rels": ROOT_RELS,
        "docProps/app.xml": APP,
        "docProps/core.xml": DECL + '<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"/>',
        "xl/workbook.xml": workbook([("Data &amp; &lt;Stuff&gt;", "rId1", "")],
                                   pr='<workbookPr date1904="1"/>'),
        "xl/_rels/workbook.xml.rels": rels([
            ("rId1", "worksheet", "worksheets/sheet1.xml"),
            ("rId2", "sharedStrings", "sharedStrings.xml"),
            ("rId3", "styles", "styles.xml"),
            ("rId4", "theme", "theme/theme1.xml")]),
        "xl/worksheets/sheet1.xml": sheet(
            BASIC_ROWS, before='<sheetViews><sheetView workbookViewId="0"/></sheetViews>'),
        "xl/sharedStrings.xml": SHARED,
        "xl/styles.xml": STYLES_DEFAULT,
        "xl/theme/theme1.xml": DECL + '<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Office"/>',
        "xl/calcChain.xml": DECL + f'<calcChain xmlns="{MAIN}"><c r="A3" i="1"/></calcChain>',
    }
    package("pkg/basic.xlsx", basic)
    package("pkg/basic_stored.xlsx", basic, method=zipfile.ZIP_STORED)

    # Three sheets; the workbook rels list them out of tab order, one with an
    # absolute target and one through "..".
    multi = {
        "[Content_Types].xml": CONTENT_TYPES,
        "_rels/.rels": ROOT_RELS,
        "xl/workbook.xml": workbook(
            [("First", "rId9", ""), ("Second", "rId2", ""), ("Third", "rId5", ' state="hidden"')],
            extra='<definedNames><definedName name="Total">First!$A$1</definedName></definedNames>'),
        "xl/_rels/workbook.xml.rels": rels([
            ("rId2", "worksheet", "/xl/worksheets/second.xml"),
            ("rId5", "worksheet", "../xl/worksheets/third.xml"),
            ("rId9", "worksheet", "worksheets/sheet1.xml"),
            ("rId7", "sharedStrings", "sharedStrings.xml")]),
        "xl/worksheets/sheet1.xml": sheet('<row r="1"><c r="A1" t="s"><v>0</v></c></row>'),
        "xl/worksheets/second.xml": sheet('<row r="1"><c r="A1" t="s"><v>1</v></c></row>'),
        "xl/worksheets/third.xml": sheet('<row r="1"><c r="A1" t="inlineStr"><is><t>three</t></is></c></row>'),
        "xl/sharedStrings.xml": DECL + f'<sst xmlns="{MAIN}"><si><t>one</t></si><si><t>two</t></si></sst>',
    }
    package("pkg/multi.xlsx", multi)

    features = {
        "[Content_Types].xml": CONTENT_TYPES,
        "_rels/.rels": ROOT_RELS,
        "xl/workbook.xml": workbook([("Sheet1", "rId1", "")]),
        "xl/_rels/workbook.xml.rels": rels([
            ("rId1", "worksheet", "worksheets/sheet1.xml"),
            ("rId2", "styles", "styles.xml"),
            ("rId3", "vbaProject", "vbaProject.bin")]),
        "xl/styles.xml": STYLES_TWO_XF,
        "xl/worksheets/sheet1.xml": sheet(
            '<row r="1" ht="30" customHeight="1"><c r="A1" s="1"><v>45000</v></c><c r="B1"><v>2</v></c></row>',
            before=('<sheetViews><sheetView workbookViewId="0"><pane ySplit="1" topLeftCell="A2" state="frozen"/>'
                    '</sheetView></sheetViews><cols><col min="1" max="1" width="20" customWidth="1"/></cols>'),
            after=('<mergeCells count="1"><mergeCell ref="C1:D1"/></mergeCells>'
                   '<conditionalFormatting sqref="A1"><cfRule type="cellIs" dxfId="0" priority="1" operator="greaterThan">'
                   '<formula>1</formula></cfRule></conditionalFormatting>'
                   '<dataValidations count="1"><dataValidation type="whole" sqref="B1"><formula1>0</formula1>'
                   '</dataValidation></dataValidations>'
                   '<hyperlinks><hyperlink ref="A1" r:id="rId9"/></hyperlinks>'),
            tail=('<drawing r:id="rId1"/><legacyDrawing r:id="rId2"/>'
                  '<tableParts count="1"><tablePart r:id="rId3"/></tableParts>')),
        "xl/worksheets/_rels/sheet1.xml.rels": rels([
            ("rId1", "drawing", "../drawings/drawing1.xml"),
            ("rId2", "vmlDrawing", "../drawings/vmlDrawing1.vml"),
            ("rId3", "table", "../tables/table1.xml"),
            ("rId4", "comments", "../comments1.xml")]),
        "xl/comments1.xml": DECL + f'<comments xmlns="{MAIN}"/>',
        "xl/drawings/drawing1.xml": DECL + '<xdr:wsDr xmlns:xdr="d"/>',
        "xl/drawings/vmlDrawing1.vml": "<xml/>",
        "xl/charts/chart1.xml": DECL + '<c:chartSpace xmlns:c="c"/>',
        "xl/media/image1.png": b"\x89PNG\r\n\x1a\n",
        "xl/tables/table1.xml": DECL + f'<table xmlns="{MAIN}"/>',
        "xl/pivotTables/pivotTable1.xml": DECL + f'<pivotTableDefinition xmlns="{MAIN}"/>',
        "xl/vbaProject.bin": b"\xd0\xcf\x11\xe0 not really",
        "xl/foo.xml": "<foo/>",
        "xl/theme/theme1.xml": "<theme/>",
    }
    package("pkg/features.xlsx", features)

    numfmt = {
        "[Content_Types].xml": CONTENT_TYPES,
        "_rels/.rels": ROOT_RELS,
        "xl/workbook.xml": workbook([("S", "rId1", "")]),
        "xl/_rels/workbook.xml.rels": rels([
            ("rId1", "worksheet", "worksheets/sheet1.xml"), ("rId2", "styles", "styles.xml")]),
        "xl/styles.xml": STYLES_NUMFMT,
        "xl/worksheets/sheet1.xml": sheet('<row r="1"><c r="A1"><v>1</v></c></row>'),
    }
    package("pkg/numfmt.xlsx", numfmt)

    # No _rels/.rels: the workbook is found through [Content_Types].xml.
    norels = {
        "[Content_Types].xml": CONTENT_TYPES,
        "xl/workbook.xml": workbook([("Only", "rId1", "")]),
        "xl/_rels/workbook.xml.rels": rels([("rId1", "worksheet", "worksheets/sheet1.xml")]),
        "xl/worksheets/sheet1.xml": sheet('<row r="1"><c r="A1"><v>42</v></c></row>'),
    }
    package("pkg/norels.xlsx", norels)


if __name__ == "__main__":
    gen_inflate()
    gen_zip()
    gen_packages()
