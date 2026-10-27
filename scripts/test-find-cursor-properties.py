#!/usr/bin/env python3
"""Test current logical Find cursor kernels without GTK, VTE or services.

Production types/functions and the render-stamp alias are copied verbatim into
one Rust module. The same property module is included by normal Cargo tests.
This does not emulate native matches, select terminal text or certify F01.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def top_level_item(source, signature, derive=False):
    if source.count(signature) != 1:
        raise ValueError(f"expected one production item: {signature}")
    start = source.index(signature)
    if start != 0 and source[start - 1] != '\n':
        raise ValueError(f"item is not top-level: {signature}")
    if derive:
        previous = source.rfind('\n', 0, start - 1) + 1
        if not source[previous:start].startswith('#[derive('):
            raise ValueError(f"missing original derive: {signature}")
        start = previous
    end = re.search(r'^}\s*$', source[start:], re.M)
    if end is None:
        raise ValueError(f"missing item boundary: {signature}")
    # Include the closing brace, excluding following whitespace.
    return source[start:start + end.start() + 1]


def build_harness():
    find_path = ROOT / 'src/block_view/find.rs'
    blocks_path = ROOT / 'src/block_view/blocks.rs'
    source, blocks = find_path.read_text(), blocks_path.read_text()
    wiring = '#[cfg(test)]\n#[path = "find_cursor_properties.rs"]\nmod cursor_properties;'
    if source.count(wiring) != 1:
        raise ValueError('property module must be wired once into the real Find module')
    aliases = re.findall(r'^pub\(crate\) type RenderStamp = [^;]+;$', blocks, re.M)
    if len(aliases) != 1:
        raise ValueError('expected one original render-stamp alias')
    types = ['pub(crate) struct FindSurface {', 'struct FindCursor {',
             'struct FindResume {', 'enum FindDirection {', 'enum NativeCursorAction {']
    functions = ['resumed_find_cursor', 'step_compressed_cursor',
                 'native_cursor_action', 'validated_native_cursor_action']
    items = [top_level_item(source, signature, True) for signature in types]
    items.extend(top_level_item(source, 'fn ' + name + '(') for name in functions)
    prop = ROOT / 'src/block_view/find_cursor_properties.rs'
    code = '#![allow(dead_code)]\nmod blocks {\n' + aliases[0] + '\n}\nmod find {\n'
    code += '\n\n'.join(items)
    code += '\n#[cfg(test)]\n#[path = ' + json.dumps(str(prop)) + ']\nmod cursor_properties;\n}\n'
    print(json.dumps({'production_find_sha256': hashlib.sha256(find_path.read_bytes()).hexdigest(),
                      'production_blocks_sha256': hashlib.sha256(blocks_path.read_bytes()).hexdigest(),
                      'extracted_items_sha256': [hashlib.sha256(item.encode()).hexdigest() for item in items]}), flush=True)
    return code


def main():
    with tempfile.TemporaryDirectory(prefix='forge-find-cursor-') as temp:
        root = Path(temp)
        source, binary = root / 'cursor.rs', root / 'cursor'
        source.write_text(build_harness())
        subprocess.run([os.environ.get('RUSTC', 'rustc'), '--edition=2021', '--test',
                        '-C', 'opt-level=1', str(source), '-o', str(binary)], check=True)
        subprocess.run([str(binary), '--nocapture'], check=True)
    print('Pure logical-cursor properties passed; native VTE selection was not exercised.')


if __name__ == '__main__':
    main()
