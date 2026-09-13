"""Supplementary public geometry cases, not additions to frozen gold/gates.

The actual worker receives only PDF bytes/config/identity. Expected authored
order is read after recognition and never used to select/reorder its output.
"""
import argparse
import contextlib
import hashlib
import json
from pathlib import Path
import subprocess
import sys

from canonical_quality import metric


def authored_cases():
    vertical = [(48, y, 547, y + 50) for y in (72, 180, 288, 396)]
    columns = [(x, y, x + 225, y + 70) for x in (48, 320) for y in (72, 180)]
    return [
        ('scan_then_native', vertical, [False, False, True, True]),
        ('native_then_scan', vertical, [True, True, False, False]),
        ('scan_left_native_right', columns, [False, False, True, True]),
        ('native_left_scan_right', columns, [True, True, False, False]),
        ('alternating_vertical', vertical, [True, False, True, False]),
        ('alternating_columns', columns, [True, False, False, True]),
    ]


def generate(directory):
    # Supplementary only: no frozen fixture, font, manifest, or gold is touched.
    import pymupdf
    texts = [
        'Alpha paragraph describes the morning journey. The road follows the river.',
        'Bravo paragraph explains the second observation. Birds rest near the bridge.',
        'Charlie paragraph records the third experiment. Light enters the quiet room.',
        'Delta paragraph concludes the final discussion. Every result keeps its source.',
    ]
    paths = []
    for name, boxes, native in authored_cases():
        with pymupdf.open() as scan, pymupdf.open() as document:
            raster_page = scan.new_page(width=595, height=842)
            for bbox, text, is_native in zip(boxes, texts, native):
                if not is_native:
                    assert raster_page.insert_textbox(bbox, text, fontname='helv', fontsize=12) >= 0
            png = raster_page.get_pixmap(dpi=300, colorspace=pymupdf.csRGB, alpha=False).tobytes('png')
            page = document.new_page(width=595, height=842)
            page.insert_image(page.rect, stream=png)
            for bbox, text, is_native in zip(boxes, texts, native):
                if is_native:
                    assert page.insert_textbox(bbox, text, fontname='helv', fontsize=12) >= 0
            path = directory / (name + '.pdf')
            document.save(path)
            # This is supplementary authored truth, separate from engine input.
            path.with_suffix('.expected.json').write_text(json.dumps({
                'paragraphs': texts, 'native': native, 'rectangles': boxes,
                'font': 'PyMuPDF built-in Helvetica', 'dpi': 300,
            }, indent=2))
            paths.append(path)
    return paths


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    directory = args.output.parent / 'supplementary-mixed-order'
    directory.mkdir(exist_ok=True)
    worker = Path('src/parsing/pdf_layout_worker.py').read_text()
    namespace = {'__name__': 'mixed_order_worker'}
    exec(worker, namespace)
    config = {'enabled': True, 'engine_path': '/usr/bin/tesseract',
              'tessdata_path': '/usr/share/tesseract-ocr/5/tessdata',
              'languages': ['eng'], 'operator_revision': '1', 'dpi': 300,
              'oem': 1, 'psm': 3, 'detector_version': 'pdf-layout/v1',
              'protocol_version': 'pdf-layout/v1'}
    report = {'scope': 'supplementary public mixed-page diagnostic; not frozen acceptance', 'cases': {}}
    with contextlib.redirect_stdout(sys.stderr):
        paths = generate(directory)
    for path in paths:
        item = {}
        try:
            raw = path.read_bytes()
            identity = namespace['runtime_identity'](config, namespace['fingerprint_dependencies'](config))
            item.update({'raw_sha256': hashlib.sha256(raw).hexdigest(), 'identity': identity})
            result = subprocess.run([sys.executable, '-I', '-c', worker, '2000',
                '134217728', '16000000', json.dumps(config), json.dumps(identity)],
                input=raw, capture_output=True, timeout=90)
            item['returncode'] = result.returncode
            item['stderr'] = result.stderr.decode(errors='replace')[-4096:]
            payload = json.loads(result.stdout)
            item['worker_payload'] = payload
            # Recognition has finished. Only now read the expected source order.
            expected = json.loads(path.with_suffix('.expected.json').read_text())
            paragraphs = [block['text'] for section in payload.get('sections', [])
                          for block in section['blocks'] if block['kind'] == 'paragraph']
            item['canonical_paragraphs'] = paragraphs
            item['authored_expectation'] = expected
            item['cer'] = metric('\n\n'.join(expected['paragraphs']), '\n\n'.join(paragraphs))
            item['wer'] = metric('\n\n'.join(expected['paragraphs']), '\n\n'.join(paragraphs), True)
            item['exact_ordered_text'] = result.returncode == 0 and item['cer']['errors'] == 0
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            item['failure'] = str(error)[:1024]
            stderr = getattr(error, 'stderr', None)
            if stderr:
                item['stderr'] = stderr.decode(errors='replace')[-4096:]
        report['cases'][path.stem] = item
    encoded = json.dumps(report, ensure_ascii=False, indent=2)
    args.output.write_text(encoded)
    print(encoded)
    for name, item in report['cases'].items():
        print('MIXED_ORDER_RESULT', json.dumps({'case': name,
            'returncode': item.get('returncode'), 'failure': item.get('failure'),
            'cer': item.get('cer'), 'wer': item.get('wer'),
            'canonical_paragraphs': item.get('canonical_paragraphs')}, ensure_ascii=False))
    # Known gaps remain visible; this diagnostic never changes official gates.
    print('SUPPLEMENTARY_MIXED_ORDER_NONEXACT', json.dumps([
        name for name, item in report['cases'].items() if not item.get('exact_ordered_text', False)]))


if __name__ == '__main__':
    main()
